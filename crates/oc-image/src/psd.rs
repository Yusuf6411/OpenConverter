//! Adobe Photoshop documents (PSD and PSB): the merged ("composite") image.
//!
//! Read: bitmap, grayscale, duotone (as gray), indexed, RGB and CMYK at 1, 8 and 16 bits,
//! raw, RLE and ZIP coded, with the first extra channel as alpha. Layers are not read; the
//! flattened image Photoshop saves with "maximise compatibility" is. CMYK is converted
//! without colour management. Write: gray, gray+alpha, RGB and RGBA at 8 or 16 bits, RLE or
//! raw, with ICC profile and resolution.
// Every index below is covered by a length check just before it: resource blocks by their
// `len() >= 12` loop guard, pixel planes because `raw` is exactly `channels * rows * row_bytes`.
#![allow(clippy::indexing_slicing, clippy::needless_range_loop)]
use crate::image::{Depth, Image, Layout};
use oc_compress::{packbits, zlib};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};
use oc_io::ByteReader;

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(b"8BPS") && matches!(data.get(4..6), Some([0, 1 | 2]))
}

fn skip_section(r: &mut ByteReader<'_>, wide: bool) -> Result<()> {
    let len = if wide {
        let hi = u64::from(r.u32_be()?);
        (hi << 32) | u64::from(r.u32_be()?)
    } else {
        u64::from(r.u32_be()?)
    };
    r.skip(usize::try_from(len).map_err(|_| Error::corrupt("PSD section is too long"))?)
}

/// Pulls the ICC profile and the resolution out of the image resource blocks.
fn read_resources(mut data: &[u8], meta: &mut crate::image::Metadata) {
    while data.len() >= 12 && data.starts_with(b"8BIM") {
        let id = u16::from_be_bytes([data[4], data[5]]);
        let name_len = usize::from(data[6]);
        // Pascal string padded so that length byte + text is even.
        let name_total = (1 + name_len + 1) & !1;
        let at = 6 + name_total;
        let Some(size) = data
            .get(at..at + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(|b| u32::from_be_bytes(b) as usize)
        else {
            return;
        };
        let Some(body) = data.get(at + 4..at + 4 + size) else {
            return;
        };
        match id {
            1039 => meta.icc_profile = Some(body.to_vec()),
            1005 if body.len() >= 16 => {
                let fixed = |i: usize| {
                    f64::from(u32::from_be_bytes([
                        body[i],
                        body[i + 1],
                        body[i + 2],
                        body[i + 3],
                    ])) / 65536.0
                };
                // Unit 1 is pixels per inch, 2 is pixels per centimetre.
                let scale = |unit: u8| if unit == 2 { 2.54 } else { 1.0 };
                let (hx, vx) = (fixed(0) * scale(body[5]), fixed(8) * scale(body[13]));
                if hx > 0.0 && vx > 0.0 {
                    meta.dpi = Some((hx, vx));
                }
            }
            _ => {}
        }
        let next = at + 4 + size + (size & 1);
        match data.get(next..) {
            Some(rest) => data = rest,
            None => return,
        }
    }
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a PSD file"));
    }
    let mut r = ByteReader::new(data);
    r.skip(4)?;
    let version = r.u16_be()?;
    r.skip(6)?;
    let channels = usize::from(r.u16_be()?);
    let height = r.u32_be()? as usize;
    let width = r.u32_be()? as usize;
    let depth = usize::from(r.u16_be()?);
    let mode = r.u16_be()?;
    let big = version == 2;
    let max_side = if big { 300_000 } else { 30_000 };
    if width == 0 || height == 0 || width > max_side || height > max_side {
        return Err(Error::corrupt("PSD has an invalid size"));
    }
    if !(1..=56).contains(&channels) {
        return Err(Error::corrupt("PSD has an invalid channel count"));
    }
    limits.check_pixels(width as u32, height as u32)?;
    limits.check_alloc((width as u64) * (height as u64) * channels as u64 * 2 * 2)?;
    if ![1, 8, 16].contains(&depth) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!("{depth}-bit Photoshop files are not supported"),
        ));
    }
    let needed = match mode {
        0 | 1 | 2 | 8 => 1,
        3 => 3,
        4 => 4,
        other => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!(
                    "Photoshop colour mode {other} ({}) is not supported",
                    match other {
                        7 => "multichannel",
                        9 => "Lab",
                        _ => "unknown",
                    }
                ),
            ))
        }
    };
    if channels < needed || (depth == 1 && mode != 0) {
        return Err(Error::corrupt(
            "PSD channel count does not fit its colour mode",
        ));
    }

    // Colour mode data: the palette of indexed images.
    let cm_len = r.u32_be()? as usize;
    let cm = r.take(cm_len)?;
    let palette: Vec<[u8; 3]> = if mode == 2 && cm.len() >= 768 {
        (0..256)
            .map(|i| [cm[i], cm[256 + i], cm[512 + i]])
            .collect()
    } else {
        Vec::new()
    };
    if mode == 2 && palette.is_empty() {
        return Err(Error::corrupt("indexed PSD has no palette"));
    }
    let res_len = r.u32_be()? as usize;
    let mut meta = crate::image::Metadata::default();
    read_resources(r.take(res_len)?, &mut meta);
    skip_section(&mut r, big)?; // layer and mask information

    let compression = r.u16_be()?;
    let rest = r.take(r.remaining())?;
    let row_bytes = (width * depth).div_ceil(8);
    let plane = row_bytes * height;
    let total = plane * channels;
    let raw: Vec<u8> = match compression {
        0 => rest
            .get(..total)
            .ok_or_else(|| Error::truncated("PSD pixel data"))?
            .to_vec(),
        1 => {
            let entry = if big { 4 } else { 2 };
            let rows = channels * height;
            let table = rest
                .get(..rows * entry)
                .ok_or_else(|| Error::truncated("PSD row table"))?;
            let mut body = rest.get(rows * entry..).unwrap_or(&[]);
            let mut out = Vec::with_capacity(total);
            for i in 0..rows {
                progress.check_cancelled()?;
                let n = if big {
                    u32::from_be_bytes([
                        table[4 * i],
                        table[4 * i + 1],
                        table[4 * i + 2],
                        table[4 * i + 3],
                    ]) as usize
                } else {
                    usize::from(u16::from_be_bytes([table[2 * i], table[2 * i + 1]]))
                };
                let packed = body
                    .get(..n)
                    .ok_or_else(|| Error::truncated("PSD pixel data"))?;
                out.extend(packbits::decode(packed, Some(row_bytes))?);
                body = body.get(n..).unwrap_or(&[]);
            }
            out
        }
        2 | 3 => {
            let lim = Limits {
                max_output: total as u64,
                max_ratio: u64::MAX,
                ..limits.clone()
            };
            let mut z = zlib::decompress(rest, &lim, progress)?;
            if z.len() < total {
                return Err(Error::truncated("PSD pixel data"));
            }
            z.truncate(total);
            if compression == 3 {
                // Each row stores differences from the sample to its left.
                for row in z.chunks_exact_mut(row_bytes) {
                    match depth {
                        8 => {
                            for i in 1..row.len() {
                                row[i] = row[i].wrapping_add(row[i - 1]);
                            }
                        }
                        16 => {
                            for i in (2..row.len().saturating_sub(1)).step_by(2) {
                                let prev = u16::from_be_bytes([row[i - 2], row[i - 1]]);
                                let cur = u16::from_be_bytes([row[i], row[i + 1]]);
                                let sum = prev.wrapping_add(cur).to_be_bytes();
                                row[i] = sum[0];
                                row[i + 1] = sum[1];
                            }
                        }
                        _ => {}
                    }
                }
            }
            z
        }
        other => return Err(Error::corrupt(format!("unknown PSD compression {other}"))),
    };

    // Planar samples to the interleaved image.
    let npx = width * height;
    let sample = |c: usize, p: usize| -> u16 {
        match depth {
            16 => {
                let i = c * plane + 2 * p;
                u16::from_be_bytes([raw[i], raw[i + 1]])
            }
            8 => u16::from(raw[c * plane + p]),
            _ => {
                let (y, x) = (p / width, p % width);
                let byte = raw[c * plane + y * row_bytes + x / 8];
                // In a PSD bitmap a set bit is black.
                if byte >> (7 - x % 8) & 1 == 1 {
                    0
                } else {
                    255
                }
            }
        }
    };
    let out_depth = if depth == 16 {
        Depth::Sixteen
    } else {
        Depth::Eight
    };
    let top = u32::from(out_depth.max() as u16);
    let mut out: Vec<u8> = Vec::new();
    let push = |out: &mut Vec<u8>, v: u16| match out_depth {
        Depth::Sixteen => out.extend_from_slice(&v.to_be_bytes()),
        Depth::Eight => out.push(v as u8),
    };
    let has_alpha = channels > needed;
    let layout = match (mode, has_alpha) {
        (0 | 1 | 8, false) => Layout::Gray,
        (0 | 1 | 8, true) => Layout::GrayAlpha,
        (2, false) | (3 | 4, false) => Layout::Rgb,
        _ => Layout::Rgba,
    };
    if mode == 4 {
        notes.push(Note::new(
            Severity::Warning,
            "psd.cmyk_converted",
            "the file is CMYK; it was converted to RGB without colour management",
        ));
    }
    if mode == 8 {
        notes.push(Note::new(
            Severity::Info,
            "psd.duotone_as_gray",
            "the duotone image was read as grayscale",
        ));
    }
    if channels > needed + 1 {
        notes.push(Note::new(
            Severity::Info,
            "psd.extra_channels_ignored",
            "channels beyond the first alpha channel were ignored",
        ));
    }
    for p in 0..npx {
        match mode {
            0 | 1 | 8 => push(&mut out, sample(0, p)),
            2 => {
                let [pr, pg, pb] = palette
                    .get(usize::from(sample(0, p)))
                    .copied()
                    .unwrap_or([0; 3]);
                for v in [pr, pg, pb] {
                    push(&mut out, u16::from(v));
                }
            }
            3 => {
                for c in 0..3 {
                    push(&mut out, sample(c, p));
                }
            }
            _ => {
                // Photoshop stores CMYK inverted: 255 means no ink.
                let k = u32::from(sample(3, p));
                for c in 0..3 {
                    let v = (u32::from(sample(c, p)) * k + top / 2) / top;
                    push(&mut out, v as u16);
                }
            }
        }
        if has_alpha {
            push(&mut out, sample(needed, p));
        }
    }
    let mut img = Image::new(width as u32, height as u32, layout, out_depth, out)?;
    img.metadata = meta;
    Ok((img, notes))
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    /// Run-length code the rows (smaller for most images) instead of storing them raw.
    pub rle: bool,
    /// Write the large document format (PSB): sizes up to 300000 pixels.
    pub large: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            rle: true,
            large: false,
        }
    }
}

fn resource(out: &mut Vec<u8>, id: u16, body: &[u8]) {
    out.extend_from_slice(b"8BIM");
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // empty Pascal name, padded
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    if body.len() & 1 == 1 {
        out.push(0);
    }
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let notes = Notes::new();
    let max_side = if opts.large { 300_000 } else { 30_000 };
    if img.width > max_side || img.height > max_side {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!(
                "{} images are at most {max_side} pixels on a side",
                if opts.large { "PSB" } else { "PSD" }
            ),
        ));
    }
    let channels = img.layout.channels();
    let (w, h) = (img.width as usize, img.height as usize);
    let bytes = img.depth.bytes();
    let mut out = Vec::new();
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&(if opts.large { 2u16 } else { 1 }).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(&(channels as u16).to_be_bytes());
    out.extend_from_slice(&img.height.to_be_bytes());
    out.extend_from_slice(&img.width.to_be_bytes());
    out.extend_from_slice(&(if bytes == 2 { 16u16 } else { 8 }).to_be_bytes());
    out.extend_from_slice(&(if img.layout.is_gray() { 1u16 } else { 3 }).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    let mut res = Vec::new();
    if let Some((dx, dy)) = img.metadata.dpi {
        let fixed = |v: f64| ((v * 65536.0).round() as u32).to_be_bytes();
        let mut body = Vec::new();
        for d in [dx, dy] {
            body.extend_from_slice(&fixed(d));
            body.extend_from_slice(&1u16.to_be_bytes()); // pixels per inch
            body.extend_from_slice(&1u16.to_be_bytes()); // width/height shown in inches
        }
        // Layout is hRes, hUnit, widthUnit, vRes, vUnit, heightUnit.
        resource(&mut res, 1005, &body);
    }
    if let Some(icc) = &img.metadata.icc_profile {
        resource(&mut res, 1039, icc);
    }
    out.extend_from_slice(&(res.len() as u32).to_be_bytes());
    out.extend_from_slice(&res);
    // No layers: a zero length, eight bytes wide in a PSB.
    out.extend_from_slice(if opts.large {
        &[0u8; 8][..]
    } else {
        &[0u8; 4][..]
    });

    // Planar rows.
    let row_len = w * bytes;
    let mut rows: Vec<Vec<u8>> = Vec::with_capacity(channels * h);
    for c in 0..channels {
        for y in 0..h {
            let src = img.row(y as u32);
            let mut row = Vec::with_capacity(row_len);
            for x in 0..w {
                let at = (x * channels + c) * bytes;
                row.extend_from_slice(src.get(at..at + bytes).unwrap_or(&[0; 2][..bytes]));
            }
            rows.push(row);
        }
    }
    if opts.rle {
        out.extend_from_slice(&1u16.to_be_bytes());
        let packed: Vec<Vec<u8>> = rows.iter().map(|r| packbits::encode(r)).collect();
        for p in &packed {
            if opts.large {
                out.extend_from_slice(&(p.len() as u32).to_be_bytes());
            } else if let Ok(n) = u16::try_from(p.len()) {
                out.extend_from_slice(&n.to_be_bytes());
            } else {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "a row is too long for PSD run-length coding; use compression=none or PSB",
                ));
            }
        }
        for p in &packed {
            out.extend_from_slice(p);
        }
    } else {
        out.extend_from_slice(&0u16.to_be_bytes());
        for r in &rows {
            out.extend_from_slice(r);
        }
    }
    Ok((out, notes))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn picture(layout: Layout, depth: Depth) -> Image {
        let (w, h) = (19usize, 11usize);
        let ch = layout.channels();
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                for c in 0..ch {
                    let v = ((x * 13 + y * 7 + c * 50) % 256) as u16;
                    match depth {
                        Depth::Eight => d.push(v as u8),
                        Depth::Sixteen => d.extend_from_slice(&(v * 257).to_be_bytes()),
                    }
                }
            }
        }
        Image::new(w as u32, h as u32, layout, depth, d).unwrap()
    }

    #[test]
    fn every_layout_roundtrips_raw_and_rle() {
        for layout in [Layout::Gray, Layout::GrayAlpha, Layout::Rgb, Layout::Rgba] {
            for depth in [Depth::Eight, Depth::Sixteen] {
                for (rle, large) in [(false, false), (true, false), (true, true)] {
                    let mut img = picture(layout, depth);
                    img.metadata.dpi = Some((300.0, 150.0));
                    img.metadata.icc_profile = Some(vec![1, 2, 3]);
                    let (bytes, _) = encode(&img, &EncodeOptions { rle, large }).unwrap();
                    assert!(sniff(&bytes));
                    let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
                    assert_eq!(
                        back.data, img.data,
                        "{layout:?} {depth:?} rle={rle} large={large}"
                    );
                    assert_eq!(back.layout, img.layout);
                    assert_eq!(back.metadata.icc_profile, img.metadata.icc_profile);
                    let (dx, dy) = back.metadata.dpi.unwrap();
                    assert!((dx - 300.0).abs() < 0.01 && (dy - 150.0).abs() < 0.01);
                }
            }
        }
    }

    #[test]
    fn damaged_and_unsupported_files() {
        let (good, _) = encode(
            &picture(Layout::Rgb, Depth::Eight),
            &EncodeOptions::default(),
        )
        .unwrap();
        for cut in (0..good.len()).step_by(9) {
            assert!(
                decode(&good[..cut], &Limits::default(), &NoProgress).is_err(),
                "cut {cut}"
            );
        }
        let mut lab = good.clone();
        lab[24..26].copy_from_slice(&9u16.to_be_bytes());
        assert_eq!(
            decode(&lab, &Limits::default(), &NoProgress)
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedVariant
        );
        let tight = Limits {
            max_pixels: 10,
            ..Limits::default()
        };
        assert!(decode(&good, &tight, &NoProgress).is_err());
    }
}
