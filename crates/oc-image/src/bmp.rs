//! Windows BMP / DIB.
//!
//! Read: 1/2?/4/8-bit palettised, 16/24/32-bit, BI_RGB, BI_BITFIELDS, BI_ALPHABITFIELDS,
//! BI_RLE4, BI_RLE8, bottom-up and top-down, OS/2 core headers and INFO/V4/V5 headers.
//! Write: 24-bit BGR, or 32-bit BGRA with a V4 header when the image has translucency.
//! Not supported (typed error): JPEG/PNG-in-BMP, OS/2 Huffman 1D and RLE24.
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_core::{Error, ErrorCode, Limits, Notes, Result};
use oc_io::ByteReader;

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(b"BM")
}

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorCode::UnsupportedVariant,
        format!("BMP: {what} is not supported"),
    )
}

/// Extract a channel from `v` using a contiguous bit mask, scaled to 8 bits.
fn masked(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 255;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let raw = (v & mask) >> shift;
    if bits >= 8 {
        (raw >> (bits - 8)) as u8
    } else {
        // Bit replication (what GDI does): 31 -> 255 and 0 -> 0 exactly.
        let mut v = raw << (8 - bits);
        let mut have = bits;
        while have < 8 {
            v |= v >> have;
            have *= 2;
        }
        v as u8
    }
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let mut r = ByteReader::new(data);
    if r.take(2)? != b"BM" {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a BMP file"));
    }
    r.skip(8)?; // file size, reserved
    let pixel_offset = r.u32_le()? as usize;
    let hdr_size = r.u32_le()?;
    let (width, raw_height, bpp, compression, colors_used);
    let mut masks = [0u32; 4];
    let palette_entry;
    if hdr_size == 12 {
        // BITMAPCOREHEADER
        width = i32::from(r.u16_le()?);
        raw_height = i32::from(r.u16_le()?);
        r.skip(2)?; // planes
        bpp = r.u16_le()?;
        compression = 0u32;
        colors_used = 0u32;
        palette_entry = 3usize;
    } else if hdr_size >= 40 {
        width = r.i32_le()?;
        raw_height = r.i32_le()?;
        r.skip(2)?;
        bpp = r.u16_le()?;
        compression = r.u32_le()?;
        r.skip(12)?; // image size, ppm x, ppm y
        colors_used = r.u32_le()?;
        r.skip(4)?; // important colours
        palette_entry = 4;
        if hdr_size >= 52 || compression == 3 || compression == 6 {
            // Masks follow the 40-byte header (inside it for V2+).
            for m in masks
                .iter_mut()
                .take(if compression == 6 || hdr_size >= 56 {
                    4
                } else {
                    3
                })
            {
                *m = r.u32_le()?;
            }
        }
        if compression == 4 || compression == 5 {
            return Err(unsupported("JPEG/PNG compression"));
        }
        if compression > 6 {
            return Err(Error::corrupt("unknown BMP compression"));
        }
    } else {
        return Err(unsupported("this DIB header size"));
    }
    if width <= 0 || raw_height == 0 {
        return Err(Error::corrupt("invalid BMP dimensions"));
    }
    let top_down = raw_height < 0;
    let height = raw_height.unsigned_abs();
    let width = width as u32;
    limits.check_pixels(width, height)?;
    limits.check_alloc(u64::from(width) * u64::from(height) * 4)?;
    if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(unsupported("this bit depth"));
    }

    // Palette sits between the header and the pixels.
    let mut palette: Vec<[u8; 4]> = Vec::new();
    if bpp <= 8 {
        let n = if colors_used == 0 {
            1usize << bpp
        } else {
            colors_used as usize
        };
        let n = n.min(256);
        let pal_start = 14 + hdr_size as usize;
        let mut pr = ByteReader::new(data.get(pal_start..).unwrap_or(&[]));
        for _ in 0..n {
            let e = pr.take(palette_entry)?;
            if let [b, g, r, ..] = *e {
                palette.push([r, g, b, 255]);
            }
        }
    }
    if compression == 0 && bpp == 16 && masks[0] == 0 {
        masks = [0x7C00, 0x03E0, 0x001F, 0];
    }
    if compression == 0 && bpp == 32 && masks[0] == 0 {
        masks = [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0];
    }
    if bpp == 24 {
        masks = [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0];
    }

    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    let put = |rgba: &mut Vec<u8>, x: usize, y: usize, px: [u8; 4]| {
        let row = if top_down { y } else { height as usize - 1 - y };
        if let Some(s) =
            rgba.get_mut((row * width as usize + x) * 4..(row * width as usize + x) * 4 + 4)
        {
            s.copy_from_slice(&px);
        }
    };
    let body = data
        .get(pixel_offset..)
        .ok_or_else(|| Error::truncated("BMP pixel data").at(pixel_offset as u64))?;
    let mut has_alpha_mask = masks[3] != 0;
    match compression {
        1 | 2 => {
            // RLE8 / RLE4. Pixels skipped by deltas stay palette entry 0.
            let background = palette.first().copied().unwrap_or([0, 0, 0, 255]);
            for y in 0..height as usize {
                for x in 0..width as usize {
                    put(&mut rgba, x, y, background);
                }
            }
            let mut br = ByteReader::new(body);
            let (mut x, mut y) = (0usize, 0usize);
            let rle4 = compression == 2;
            while y < height as usize && !br.is_empty() {
                let count = br.u8()? as usize;
                let val = br.u8()?;
                if count > 0 {
                    for i in 0..count {
                        let idx = if rle4 {
                            if i % 2 == 0 {
                                val >> 4
                            } else {
                                val & 15
                            }
                        } else {
                            val
                        };
                        if x < width as usize {
                            put(
                                &mut rgba,
                                x,
                                y,
                                palette.get(usize::from(idx)).copied().unwrap_or(background),
                            );
                        }
                        x += 1;
                    }
                } else {
                    match val {
                        0 => {
                            x = 0;
                            y += 1;
                        }
                        1 => break,
                        2 => {
                            x += br.u8()? as usize;
                            y += br.u8()? as usize;
                        }
                        n => {
                            let n = n as usize;
                            let bytes = if rle4 { n.div_ceil(2) } else { n };
                            let run = br.take(bytes)?;
                            for i in 0..n {
                                let idx = if rle4 {
                                    let b = run.get(i / 2).copied().unwrap_or(0);
                                    if i % 2 == 0 {
                                        b >> 4
                                    } else {
                                        b & 15
                                    }
                                } else {
                                    run.get(i).copied().unwrap_or(0)
                                };
                                if x < width as usize {
                                    put(
                                        &mut rgba,
                                        x,
                                        y,
                                        palette
                                            .get(usize::from(idx))
                                            .copied()
                                            .unwrap_or(background),
                                    );
                                }
                                x += 1;
                            }
                            if bytes % 2 == 1 {
                                br.skip(1)?;
                            }
                        }
                    }
                }
            }
        }
        0 | 3 | 6 => {
            let row_bytes = (width as usize * usize::from(bpp)).div_ceil(32) * 4;
            let need = row_bytes
                .checked_mul(height as usize)
                .ok_or_else(|| Error::limit("max_pixels", "BMP too large"))?;
            let px = body
                .get(..need)
                .ok_or_else(|| Error::truncated("BMP pixel data").at(pixel_offset as u64))?;
            for y in 0..height as usize {
                let row = px.get(y * row_bytes..(y + 1) * row_bytes).unwrap_or(&[]);
                for x in 0..width as usize {
                    let p = match bpp {
                        1 | 4 | 8 => {
                            let bit = x * usize::from(bpp);
                            let byte = row.get(bit / 8).copied().unwrap_or(0);
                            let idx = (byte >> (8 - usize::from(bpp) - bit % 8))
                                & (((1u16 << bpp) - 1) as u8);
                            palette
                                .get(usize::from(idx))
                                .copied()
                                .unwrap_or([0, 0, 0, 255])
                        }
                        16 => {
                            let v = u32::from(u16::from_le_bytes([
                                row.get(2 * x).copied().unwrap_or(0),
                                row.get(2 * x + 1).copied().unwrap_or(0),
                            ]));
                            [
                                masked(v, masks[0]),
                                masked(v, masks[1]),
                                masked(v, masks[2]),
                                if masks[3] != 0 {
                                    masked(v, masks[3])
                                } else {
                                    255
                                },
                            ]
                        }
                        24 => {
                            let o = 3 * x;
                            [
                                row.get(o + 2).copied().unwrap_or(0),
                                row.get(o + 1).copied().unwrap_or(0),
                                row.get(o).copied().unwrap_or(0),
                                255,
                            ]
                        }
                        _ => {
                            let o = 4 * x;
                            let v = u32::from_le_bytes([
                                row.get(o).copied().unwrap_or(0),
                                row.get(o + 1).copied().unwrap_or(0),
                                row.get(o + 2).copied().unwrap_or(0),
                                row.get(o + 3).copied().unwrap_or(0),
                            ]);
                            [
                                masked(v, masks[0]),
                                masked(v, masks[1]),
                                masked(v, masks[2]),
                                if masks[3] != 0 {
                                    masked(v, masks[3])
                                } else {
                                    255
                                },
                            ]
                        }
                    };
                    put(&mut rgba, x, y, p);
                }
            }
        }
        _ => return Err(unsupported("this compression")),
    }
    // A 32-bit BI_RGB file's fourth byte is unused unless a mask says otherwise.
    if bpp != 32 && bpp != 16 {
        has_alpha_mask = false;
    }
    let img = Image::new(width, height, Layout::Rgba, Depth::Eight, rgba)?;
    let layout = if has_alpha_mask && img.has_translucency() {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    // Drop an all-opaque alpha channel; the file never had real transparency.
    let (img, _) = ops::convert_layout(&img, layout, [0, 0, 0]);
    Ok((img, Notes::new()))
}

pub fn encode(img: &Image) -> (Vec<u8>, Notes) {
    let mut notes = Notes::new();
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    // BMP has no gray+alpha: go through RGB(A).
    let wants_alpha = e8.layout.has_alpha() && e8.has_translucency();
    let target = if wants_alpha {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let (e, n) = ops::convert_layout(&e8, target, [255, 255, 255]);
    notes.extend(n);
    let bytes_pp = e.layout.channels();
    let row = (e.width as usize * bytes_pp).div_ceil(4) * 4;
    let hdr = if wants_alpha { 108u32 } else { 40 };
    let pix_off = 14 + hdr;
    let size = pix_off as usize + row * e.height as usize;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&pix_off.to_le_bytes());
    out.extend_from_slice(&hdr.to_le_bytes());
    out.extend_from_slice(&(e.width as i32).to_le_bytes());
    out.extend_from_slice(&(e.height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&((bytes_pp * 8) as u16).to_le_bytes());
    out.extend_from_slice(&(if wants_alpha { 3u32 } else { 0 }).to_le_bytes());
    out.extend_from_slice(&((row * e.height as usize) as u32).to_le_bytes());
    let ppm = |d: Option<f64>| d.map_or(2835i32, |v| (v / 0.0254).round() as i32);
    let (dx, dy) = img
        .metadata
        .dpi
        .map_or((None, None), |(x, y)| (Some(x), Some(y)));
    out.extend_from_slice(&ppm(dx).to_le_bytes());
    out.extend_from_slice(&ppm(dy).to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    if wants_alpha {
        for m in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000] {
            out.extend_from_slice(&m.to_le_bytes());
        }
        out.extend_from_slice(b"sRGB");
        out.extend_from_slice(&[0; 48]);
    }
    for y in (0..e.height).rev() {
        let src = e.row(y);
        let start = out.len();
        for px in src.chunks_exact(bytes_pp) {
            out.push(px.get(2).copied().unwrap_or(0));
            out.push(px.get(1).copied().unwrap_or(0));
            out.push(px.first().copied().unwrap_or(0));
            if wants_alpha {
                out.push(px.get(3).copied().unwrap_or(255));
            }
        }
        out.resize(start + row, 0);
    }
    (out, notes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::unwrap_used)]
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../tests/fixtures/images/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn matches_other_formats_for_the_same_picture() {
        let ppm = crate::netpbm::decode(&fixture("gradient.ppm"), &Limits::default())
            .unwrap()
            .0;
        let (bmp, _) = decode(&fixture("gradient.bmp"), &Limits::default()).unwrap();
        assert_eq!((bmp.layout, bmp.data), (Layout::Rgb, ppm.data));
        let q = crate::qoi::decode(&fixture("alpha.qoi"), &Limits::default()).unwrap();
        let (a, _) = decode(&fixture("alpha.bmp"), &Limits::default()).unwrap();
        assert_eq!((a.layout, a.data), (Layout::Rgba, q.data));
        let (odd, _) = decode(&fixture("odd_17x1.bmp"), &Limits::default()).unwrap();
        assert_eq!((odd.width, odd.height), (17, 1));
    }

    #[test]
    fn encode_roundtrips_rgb_and_rgba() {
        for name in ["gradient.bmp", "alpha.bmp"] {
            let (img, _) = decode(&fixture(name), &Limits::default()).unwrap();
            let (b, notes) = encode(&img);
            assert!(notes.is_empty());
            let (back, _) = decode(&b, &Limits::default()).unwrap();
            assert_eq!((back.layout, back.data), (img.layout, img.data), "{name}");
        }
    }

    /// Cross-check against ImageMagick-produced files: palettes, RLE, 16-bit, top-down.
    #[test]
    fn imagemagick_variants() {
        let dir = format!("{}/tests/data/bmp", env!("CARGO_MANIFEST_DIR"));
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|x| x.to_str()) != Some("bmp") {
                continue;
            }
            let (img, _) = decode(&std::fs::read(&p).unwrap(), &Limits::default()).unwrap();
            let (rgba, _) = ops::convert_layout(&img, Layout::Rgba, [0, 0, 0]);
            let want = std::fs::read(p.with_extension("rgba")).unwrap();
            assert_eq!(rgba.data, want, "{}", p.display());
            n += 1;
        }
        assert!(n >= 10, "expected the generated fixtures, found {n}");
    }

    #[test]
    fn unsupported_and_corrupt() {
        let good = fixture("gradient.bmp");
        for cut in [0, 1, 13, 30, 60, good.len() - 1] {
            assert!(
                decode(&good[..cut], &Limits::default()).is_err(),
                "cut {cut}"
            );
        }
        let mut jpeg = good.clone();
        jpeg[30] = 4; // BI_JPEG
        assert_eq!(
            decode(&jpeg, &Limits::default()).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedVariant)
        );
        for i in (0..good.len().min(200)).step_by(2) {
            let mut m = good.clone();
            m[i] ^= 0xA5;
            let _ = decode(&m, &Limits::default());
        }
    }
}
