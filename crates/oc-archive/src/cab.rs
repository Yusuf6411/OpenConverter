//! Microsoft Cabinet files: stored, MSZIP and LZX folders. Read only; multi-cabinet sets
//! are not followed.
use crate::entry::{Archive, Budget, Entry};
use crate::Context;
use oc_compress::{inflate, lzx::Lzx};
use oc_core::{Error, ErrorCode, Limits, Result};
use oc_io::ByteReader;

struct Folder {
    data_off: u64,
    n_data: u16,
    method: u16,
}

fn csum(data: &[u8], seed: u32) -> u32 {
    let mut c = seed;
    let mut chunks = data.chunks_exact(4);
    for w in &mut chunks {
        c ^= u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
    }
    let rem = chunks.remainder();
    let b = |i: usize| u32::from(rem.get(i).copied().unwrap_or(0));
    let ul = match rem.len() {
        3 => (b(0) << 16) | (b(1) << 8) | b(2),
        2 => (b(0) << 8) | b(1),
        1 => b(0),
        _ => 0,
    };
    c ^ ul
}

fn dos_time(date: u16, time: u16) -> Option<i64> {
    if date == 0 {
        return None;
    }
    let (y, m, d) = (i64::from(date >> 9) + 1980, i64::from((date >> 5) & 15), i64::from(date & 31));
    if m == 0 || d == 0 {
        return None;
    }
    Some(
        crate::zip::days_from_civil(y, m, d) * 86400
            + i64::from(time >> 11) * 3600
            + i64::from((time >> 5) & 63) * 60
            + i64::from(time & 31) * 2,
    )
}

fn decode_folder(data: &[u8], f: &Folder, reserve_data: usize, expected: u64, cx: &Context, budget: &Budget<'_>) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = usize::try_from(f.data_off).map_err(|_| Error::corrupt("CAB: bad data offset"))?;
    let kind = f.method & 0x0F;
    let mut lzx = if kind == 3 { Some(Lzx::new(u32::from((f.method >> 8) & 0x1F))?) } else { None };
    if kind == 2 {
        return Err(Error::new(ErrorCode::UnsupportedVariant, "CAB: Quantum compression is not supported"));
    }
    if kind > 3 {
        return Err(Error::new(ErrorCode::UnsupportedVariant, "CAB: unknown compression type"));
    }
    let lim = Limits {
        max_output: 1 << 17,
        ..budget.limits().clone()
    };
    for _ in 0..f.n_data {
        cx.progress.check_cancelled()?;
        let hdr = data
            .get(pos..pos + 8)
            .ok_or_else(|| Error::truncated("CAB: data block header cut short").at(pos as u64))?;
        let stored_csum = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let cb = usize::from(u16::from_le_bytes([hdr[4], hdr[5]]));
        let cu = usize::from(u16::from_le_bytes([hdr[6], hdr[7]]));
        let body_at = pos + 8 + reserve_data;
        let body = data
            .get(body_at..body_at + cb)
            .ok_or_else(|| Error::truncated("CAB: data block cut short").at(body_at as u64))?;
        if stored_csum != 0 {
            let sum = csum(&hdr[4..8], csum(body, 0));
            if sum != stored_csum {
                return Err(Error::corrupt("CAB: data block checksum mismatch").at(pos as u64));
            }
        }
        pos = body_at + cb;
        match kind {
            0 => out.extend_from_slice(body),
            1 => {
                if body.get(..2) != Some(b"CK") {
                    return Err(Error::corrupt("CAB: MSZIP block without 'CK' signature"));
                }
                let from = out.len().saturating_sub(32768);
                let dict = out.get(from..).unwrap_or(&[]).to_vec();
                let r = inflate::inflate_with_dict(body.get(2..).unwrap_or(&[]), &dict, &lim, cx.progress)?;
                out.extend_from_slice(&r.data);
            }
            _ => {
                if let Some(l) = lzx.as_mut() {
                    l.decode_frame(body, cu, &mut out)?;
                }
            }
        }
        if out.len() as u64 > expected.max(1) + 65536 {
            return Err(Error::corrupt("CAB: folder holds more data than its files declare"));
        }
        cx.limits.check_alloc(out.len() as u64)?;
    }
    Ok(out)
}

pub fn read(data: &[u8], cx: &Context) -> Result<Archive> {
    let mut r = ByteReader::new(data);
    if r.take(4)? != b"MSCF" {
        return Err(Error::corrupt("CAB: bad signature"));
    }
    r.skip(4)?;
    let _size = r.u32_le()?;
    r.skip(4)?;
    let files_off = r.u32_le()? as usize;
    r.skip(4)?;
    let _minor = r.u8()?;
    let _major = r.u8()?;
    let n_folders = usize::from(r.u16_le()?);
    let n_files = usize::from(r.u16_le()?);
    let flags = r.u16_le()?;
    r.skip(4)?; // set id, cabinet number
    let (mut res_folder, mut res_data) = (0usize, 0usize);
    if flags & 4 != 0 {
        let hdr_res = usize::from(r.u16_le()?);
        res_folder = usize::from(r.u8()?);
        res_data = usize::from(r.u8()?);
        r.skip(hdr_res)?;
    }
    if flags & 1 != 0 {
        r.until(0)?;
        r.until(0)?;
    }
    if flags & 2 != 0 {
        r.until(0)?;
        r.until(0)?;
    }
    cx.limits.check_entries(n_files as u64)?;
    let mut folders = Vec::new();
    for _ in 0..n_folders {
        let off = u64::from(r.u32_le()?);
        let n = r.u16_le()?;
        let m = r.u16_le()?;
        r.skip(res_folder)?;
        folders.push(Folder { data_off: off, n_data: n, method: m });
    }
    let mut fr = ByteReader::new(data.get(files_off..).ok_or_else(|| Error::truncated("CAB: file table beyond end"))?);
    struct F {
        size: u32,
        off: u32,
        folder: u16,
        mtime: Option<i64>,
        name: String,
    }
    let mut files = Vec::new();
    for _ in 0..n_files {
        let size = fr.u32_le()?;
        let off = fr.u32_le()?;
        let folder = fr.u16_le()?;
        let date = fr.u16_le()?;
        let time = fr.u16_le()?;
        let attr = fr.u16_le()?;
        let raw = fr.until(0)?;
        let name = if attr & 0x80 != 0 {
            String::from_utf8_lossy(raw).into_owned()
        } else {
            raw.iter().map(|&b| char::from(b)).collect()
        };
        files.push(F {
            size,
            off,
            folder,
            mtime: dos_time(date, time),
            name: name.replace('\\', "/"),
        });
    }
    let mut budget = Budget::new(cx.limits);
    let mut archive = Archive::default();
    let mut cache: Option<(usize, Vec<u8>)> = None;
    for f in &files {
        budget.entry()?;
        if f.folder >= 0xFFFD {
            return Err(Error::new(ErrorCode::UnsupportedVariant, "CAB: file continues from another cabinet"));
        }
        let fi = usize::from(f.folder);
        if cx.list_only {
            budget.add_output(u64::from(f.size))?;
            let mut e = Entry::listed(f.name.trim_start_matches('/').to_string(), u64::from(f.size));
            e.mtime = f.mtime;
            archive.entries.push(e);
            continue;
        }
        if cache.as_ref().map(|(k, _)| *k) != Some(fi) {
            let fold = folders.get(fi).ok_or_else(|| Error::corrupt("CAB: file refers to a missing folder"))?;
            let expected = files
                .iter()
                .filter(|x| usize::from(x.folder) == fi)
                .map(|x| u64::from(x.off) + u64::from(x.size))
                .max()
                .unwrap_or(0);
            cx.limits.check_expansion(data.len() as u64, expected)?;
            let d = decode_folder(data, fold, res_data, expected, cx, &budget)?;
            cache = Some((fi, d));
        }
        let fd = cache.as_ref().map(|(_, d)| d.as_slice()).unwrap_or(&[]);
        let a = f.off as usize;
        let body = fd
            .get(a..a + f.size as usize)
            .ok_or_else(|| Error::corrupt(format!("CAB: '{}' extends past its folder", f.name)))?;
        budget.add_output(body.len() as u64)?;
        let mut e = Entry::file(f.name.trim_start_matches('/').to_string(), body.to_vec());
        e.mtime = f.mtime;
        archive.entries.push(e);
    }
    Ok(archive)
}
