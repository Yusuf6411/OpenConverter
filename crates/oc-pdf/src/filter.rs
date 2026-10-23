//! Stream filters: Flate, LZW, ASCII85, ASCIIHex, RunLength and the predictors. Image filters
//! (DCT, JPX, JBIG2, CCITT) are handed back to the caller.
use crate::doc::Pdf;
use crate::object::{Dict, Obj, Stream};
use oc_core::{Error, ErrorCode, Limits, NoProgress, Result};

#[derive(Debug, Clone)]
pub struct ImageFilter {
    pub name: String,
    pub parms: Dict,
}

fn canonical(name: &str) -> &str {
    match name {
        "AHx" => "ASCIIHexDecode",
        "A85" => "ASCII85Decode",
        "LZW" => "LZWDecode",
        "Fl" => "FlateDecode",
        "RL" => "RunLengthDecode",
        "CCF" => "CCITTFaxDecode",
        "DCT" => "DCTDecode",
        other => other,
    }
}

/// The filter names and their parameter dictionaries.
pub fn filter_chain(pdf: &Pdf, d: &Dict) -> Vec<(String, Dict)> {
    let f = d.get("Filter").or_else(|| d.get("F")).map(|o| pdf.resolve(o)).unwrap_or(Obj::Null);
    let p = d.get("DecodeParms").or_else(|| d.get("DP")).map(|o| pdf.resolve(o)).unwrap_or(Obj::Null);
    let names: Vec<String> = match &f {
        Obj::Name(n) => vec![n.clone()],
        Obj::Array(a) => a.iter().filter_map(|o| pdf.resolve(o).as_name().map(str::to_string)).collect(),
        _ => Vec::new(),
    };
    let parms: Vec<Dict> = match &p {
        Obj::Dict(d) => vec![d.clone()],
        Obj::Array(a) => a.iter().map(|o| pdf.resolve(o).as_dict().cloned().unwrap_or_default()).collect(),
        _ => Vec::new(),
    };
    names
        .into_iter()
        .enumerate()
        .map(|(i, n)| (canonical(&n).to_string(), parms.get(i).cloned().unwrap_or_default()))
        .collect()
}

fn limits() -> Limits {
    Limits { max_alloc: 1 << 31, max_output: 1 << 31, ..Limits::default() }
}

fn flate(data: &[u8]) -> Result<Vec<u8>> {
    let l = limits();
    match oc_compress::zlib::decompress(data, &l, &NoProgress) {
        Ok(v) => Ok(v),
        Err(_) => {
            // missing or damaged zlib header: try raw deflate after the two header bytes, then without
            if let Ok(r) = oc_compress::inflate::inflate(data.get(2..).unwrap_or_default(), &l, &NoProgress) {
                return Ok(r.data);
            }
            oc_compress::inflate::inflate(data, &l, &NoProgress)
                .map(|r| r.data)
                .map_err(|_| Error::corrupt("a Flate stream is damaged"))
        }
    }
}

fn ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in data {
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            b'>' => break,
            _ => continue,
        };
        match hi.take() {
            Some(h) => out.push(h * 16 + v),
            None => hi = Some(v),
        }
    }
    if let Some(h) = hi {
        out.push(h * 16);
    }
    out
}

fn ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut grp = [0u32; 5];
    let mut n = 0;
    let mut i = 0;
    // optional <~ prefix
    if data.starts_with(b"<~") {
        i = 2;
    }
    while let Some(&b) = data.get(i) {
        i += 1;
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                grp[n] = u32::from(b - b'!');
                n += 1;
                if n == 5 {
                    let v = grp.iter().fold(0u64, |a, x| a * 85 + u64::from(*x)) as u32;
                    out.extend_from_slice(&v.to_be_bytes());
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for g in grp.iter_mut().skip(n) {
            *g = 84;
        }
        let v = grp.iter().fold(0u64, |a, x| a * 85 + u64::from(*x)) as u32;
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    out
}

fn run_length(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&l) = data.get(i) {
        i += 1;
        match l {
            128 => break,
            0..=127 => {
                let n = usize::from(l) + 1;
                out.extend_from_slice(data.get(i..(i + n).min(data.len())).unwrap_or_default());
                i += n;
            }
            _ => {
                let n = 257 - usize::from(l);
                if let Some(&b) = data.get(i) {
                    out.extend(std::iter::repeat_n(b, n));
                }
                i += 1;
            }
        }
    }
    out
}

fn lzw(data: &[u8], early: bool) -> Result<Vec<u8>> {
    use oc_compress::lzw::{decode, Config};
    use oc_io::BitOrder;
    let cfg = Config { order: BitOrder::MsbFirst, start_bits: 9, early_change: early };
    decode(data, cfg, &limits())
}

fn predictor(data: Vec<u8>, parms: &Dict, pdf: &Pdf) -> Vec<u8> {
    let pred = pdf.dget_i64(parms, "Predictor").unwrap_or(1);
    if pred <= 1 {
        return data;
    }
    let colors = pdf.dget_i64(parms, "Colors").unwrap_or(1).clamp(1, 32) as usize;
    let bpc = pdf.dget_i64(parms, "BitsPerComponent").unwrap_or(8).clamp(1, 16) as usize;
    let cols = pdf.dget_i64(parms, "Columns").unwrap_or(1).clamp(1, 1 << 24) as usize;
    let bpp = (colors * bpc).div_ceil(8).max(1);
    let row = (colors * bpc * cols).div_ceil(8);
    if row == 0 {
        return data;
    }
    if pred == 2 {
        let mut out = data;
        if bpc == 8 {
            for r in out.chunks_mut(row) {
                for i in bpp..r.len() {
                    r[i] = r[i].wrapping_add(r[i - bpp]);
                }
            }
        } else if bpc == 16 {
            for r in out.chunks_mut(row) {
                let mut i = bpp;
                while i + 1 < r.len() {
                    let cur = u16::from_be_bytes([r[i], r[i + 1]]);
                    let prev = u16::from_be_bytes([r[i - bpp], r[i - bpp + 1]]);
                    r[i..i + 2].copy_from_slice(&cur.wrapping_add(prev).to_be_bytes());
                    i += 2;
                }
            }
        }
        return out;
    }
    // PNG predictors: each row starts with a filter byte
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        let Some((&ft, body)) = chunk.split_first() else { continue };
        let mut cur = body.to_vec();
        cur.resize(row, 0);
        for i in 0..row {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let add = match ft {
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => {
                    let p = i32::from(a) + i32::from(b) - i32::from(c);
                    let (pa, pb, pc) = ((p - i32::from(a)).abs(), (p - i32::from(b)).abs(), (p - i32::from(c)).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => 0,
            };
            cur[i] = cur[i].wrapping_add(add);
        }
        out.extend_from_slice(&cur);
        prev = cur;
    }
    out
}

/// Decode a stream. With `keep_image`, an image filter ends the chain and is returned with the
/// data as it stood before that filter.
pub fn decode_stream(pdf: &Pdf, s: &Stream, keep_image: bool) -> Result<(Vec<u8>, Option<ImageFilter>)> {
    let mut data = match &pdf.crypt {
        Some(c) if s.id.0 != 0 && !has_identity_crypt(pdf, &s.dict) => c.decrypt_stream(&s.raw, s.id.0, s.id.1, &s.dict),
        _ => s.raw.clone(),
    };
    for (name, parms) in filter_chain(pdf, &s.dict) {
        match name.as_str() {
            "FlateDecode" => data = predictor(flate(&data)?, &parms, pdf),
            "LZWDecode" => {
                let early = pdf.dget_i64(&parms, "EarlyChange").unwrap_or(1) != 0;
                data = predictor(lzw(&data, early)?, &parms, pdf);
            }
            "ASCII85Decode" => data = ascii85(&data),
            "ASCIIHexDecode" => data = ascii_hex(&data),
            "RunLengthDecode" => data = run_length(&data),
            "Crypt" => {}
            "DCTDecode" | "JPXDecode" | "JBIG2Decode" | "CCITTFaxDecode" => {
                if keep_image {
                    return Ok((data, Some(ImageFilter { name, parms })));
                }
                return Err(Error::new(ErrorCode::UnsupportedVariant, format!("{name} data cannot be used as plain bytes")));
            }
            other => return Err(Error::new(ErrorCode::UnsupportedVariant, format!("unknown stream filter {other}"))),
        }
    }
    Ok((data, None))
}

fn has_identity_crypt(pdf: &Pdf, d: &Dict) -> bool {
    filter_chain(pdf, d).iter().any(|(n, p)| n == "Crypt" && pdf.dget_name(p, "Name").as_deref().is_none_or(|n| n == "Identity"))
}
