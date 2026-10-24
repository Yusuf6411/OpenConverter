//! Windows icons and cursors (ICO / CUR).
//!
//! Read: the largest image in the file, stored either as a PNG or as a DIB with an AND
//! transparency mask. Write: PNG-compressed entries, one per requested size.
use crate::bmp;
use crate::image::{Depth, Image, Layout};
use crate::ops;
use crate::png;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};
use oc_io::ByteReader;

pub fn sniff(data: &[u8]) -> bool {
    matches!(data, [0, 0, 1 | 2, 0, n, 0, ..] if *n > 0)
}

struct Entry {
    width: u32,
    height: u32,
    bpp: u16,
    size: usize,
    offset: usize,
}

fn le16(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(i)?, *d.get(i + 1)?]))
}

fn le32(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *d.get(i)?,
        *d.get(i + 1)?,
        *d.get(i + 2)?,
        *d.get(i + 3)?,
    ]))
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let mut r = ByteReader::new(data);
    if r.u16_le()? != 0 {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "not an ICO or CUR file",
        ));
    }
    let kind = r.u16_le()?;
    if kind != 1 && kind != 2 {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "not an ICO or CUR file",
        ));
    }
    let count = usize::from(r.u16_le()?);
    if count == 0 {
        return Err(Error::corrupt("icon file contains no images"));
    }
    limits.check_entries(count as u64)?;
    let mut entries = Vec::new();
    for _ in 0..count {
        let e = r.take(16)?;
        let &[w, h, _, _, _, _, _, _, ..] = e else {
            return Err(Error::truncated("icon directory"));
        };
        entries.push(Entry {
            width: if w == 0 { 256 } else { u32::from(w) },
            height: if h == 0 { 256 } else { u32::from(h) },
            bpp: le16(e, 6).unwrap_or(0),
            size: le32(e, 8).unwrap_or(0) as usize,
            offset: le32(e, 12).unwrap_or(0) as usize,
        });
    }
    let best = entries
        .iter()
        .enumerate()
        .max_by_key(|(i, e)| (e.width * e.height, e.bpp, std::cmp::Reverse(*i)))
        .map(|(i, _)| i)
        .ok_or_else(|| Error::corrupt("icon file contains no images"))?;
    if count > 1 {
        notes.push(
            Note::new(
                Severity::Info,
                "ico.largest_image",
                format!("the file holds {count} images; the largest was used"),
            )
            .with_detail(count.to_string()),
        );
    }
    let e = entries
        .get(best)
        .ok_or_else(|| Error::corrupt("icon directory"))?;
    let body = e
        .offset
        .checked_add(e.size)
        .and_then(|end| data.get(e.offset..end))
        .ok_or_else(|| Error::truncated("icon image data"))?;
    let img = if png::sniff(body) {
        png::decode(body, limits, progress)?.0
    } else {
        decode_dib(body, limits)?
    };
    Ok((img, notes))
}

fn decode_dib(dib: &[u8], limits: &Limits) -> Result<Image> {
    let hdr = le32(dib, 0).ok_or_else(|| Error::truncated("icon DIB"))? as usize;
    if hdr < 40 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "icon DIB with an old-style header",
        ));
    }
    let width = le32(dib, 4).ok_or_else(|| Error::truncated("icon DIB"))? as i32;
    let height = le32(dib, 8).ok_or_else(|| Error::truncated("icon DIB"))? as i32;
    let bpp = le16(dib, 14).ok_or_else(|| Error::truncated("icon DIB"))?;
    let compression = le32(dib, 16).unwrap_or(0);
    let colors = le32(dib, 32).unwrap_or(0) as usize;
    if width <= 0 || height <= 0 {
        return Err(Error::corrupt("icon DIB has invalid dimensions"));
    }
    let (w, full_h) = (width as u32, height as u32);
    // The DIB height covers the colour bitmap and the mask together.
    let has_mask = full_h == w * 2;
    let h = if has_mask { full_h / 2 } else { full_h };
    let palette_bytes = if bpp <= 8 {
        4 * if colors == 0 {
            1usize << bpp
        } else {
            colors.min(256)
        }
    } else if compression == 3 {
        12
    } else {
        0
    };
    let pixel_offset = hdr + palette_bytes;
    let mut file = Vec::with_capacity(14 + dib.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&((14 + pixel_offset) as u32).to_le_bytes());
    file.extend_from_slice(dib);
    if has_mask {
        file.get_mut(14 + 8..14 + 12)
            .ok_or_else(|| Error::truncated("icon DIB"))?
            .copy_from_slice(&(h as i32).to_le_bytes());
    }
    let mut img = bmp::decode_icon_dib(&file, limits)?;
    let wi = w as usize;
    if bpp == 32 {
        // Some old icons store 32-bit colour with an unused (zero) alpha byte.
        if img.data.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            for p in img.data.as_chunks_mut::<4>().0 {
                p[3] = 255;
            }
        } else {
            return Ok(drop_opaque_alpha(img));
        }
    }
    if has_mask {
        let xor_stride = (wi * usize::from(bpp)).div_ceil(32) * 4;
        let mask_off = pixel_offset + xor_stride * h as usize;
        let mask_stride = wi.div_ceil(32) * 4;
        for y in 0..h as usize {
            // DIB rows are bottom-up; the image rows are top-down.
            let src = h as usize - 1 - y;
            let Some(row) =
                dib.get(mask_off + src * mask_stride..mask_off + (src + 1) * mask_stride)
            else {
                break; // no (or short) mask: treat the rest as opaque
            };
            for x in 0..wi {
                let bit = row.get(x / 8).is_some_and(|b| b & (0x80 >> (x % 8)) != 0);
                if bit {
                    if let Some(a) = img.data.get_mut((y * wi + x) * 4 + 3) {
                        *a = 0;
                    }
                }
            }
        }
    }
    Ok(drop_opaque_alpha(img))
}

fn drop_opaque_alpha(img: Image) -> Image {
    if img.has_translucency() {
        img
    } else {
        ops::convert_layout(&img, Layout::Rgb, [0, 0, 0]).0
    }
}

#[derive(Debug, Clone, Default)]
pub struct EncodeOptions {
    /// Square sizes to write, 1..=256. Empty keeps the image's own size (shrunk to fit 256).
    pub sizes: Vec<u32>,
    /// Write a cursor (type 2, hotspot 0,0) instead of an icon.
    pub cursor: bool,
}

/// Fit `img` inside a `size` x `size` box, centred on transparency.
pub(crate) fn fit_square(img: &Image, size: u32) -> Image {
    let scale = f64::from(size) / f64::from(img.width.max(img.height));
    let nw = ((f64::from(img.width) * scale).round() as u32).clamp(1, size);
    let nh = ((f64::from(img.height) * scale).round() as u32).clamp(1, size);
    let scaled = if (nw, nh) == (img.width, img.height) {
        img.clone()
    } else {
        ops::resize(img, nw, nh, ops::Filter::Bilinear)
    };
    let (rgba, _) = ops::convert_layout(&scaled, Layout::Rgba, [0, 0, 0]);
    let mut canvas = vec![0u8; size as usize * size as usize * 4];
    let (ox, oy) = ((size - nw) / 2, (size - nh) / 2);
    for y in 0..nh as usize {
        let src = rgba.row(y as u32);
        let dst = ((oy as usize + y) * size as usize + ox as usize) * 4;
        if let Some(d) = canvas.get_mut(dst..dst + src.len()) {
            d.copy_from_slice(src);
        }
    }
    Image {
        width: size,
        height: size,
        layout: Layout::Rgba,
        depth: Depth::Eight,
        data: canvas,
        metadata: Default::default(),
    }
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    let mut images: Vec<Image> = Vec::new();
    if opts.sizes.is_empty() {
        let longest = e8.width.max(e8.height);
        let target = if longest > 256 {
            let s = 256.0 / f64::from(longest);
            let nw = ((f64::from(e8.width) * s).round() as u32).max(1);
            let nh = ((f64::from(e8.height) * s).round() as u32).max(1);
            notes.push(Note::new(
                Severity::Info,
                "ico.resized",
                format!("icons are at most 256x256; resized to {nw}x{nh}"),
            ));
            ops::resize(&e8, nw, nh, ops::Filter::Bilinear)
        } else {
            e8
        };
        images.push(ops::convert_layout(&target, Layout::Rgba, [0, 0, 0]).0);
    } else {
        let mut sizes = opts.sizes.clone();
        sizes.sort_unstable();
        sizes.dedup();
        for s in sizes {
            if !(1..=256).contains(&s) {
                return Err(Error::invalid_option("sizes", "sizes must be 1 to 256"));
            }
            images.push(fit_square(&e8, s));
        }
    }
    let encoded: Vec<Vec<u8>> = images
        .iter()
        .map(|i| png::encode(i, &png::EncodeOptions::default()).0)
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, if opts.cursor { 2 } else { 1 }, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (i, e) in images.iter().zip(&encoded) {
        out.push((i.width % 256) as u8);
        out.push((i.height % 256) as u8);
        out.extend_from_slice(&[0, 0]);
        // Planes / bit count, or the cursor hotspot.
        out.extend_from_slice(&if opts.cursor {
            [0, 0, 0, 0]
        } else {
            [1, 0, 32, 0]
        });
        out.extend_from_slice(&(e.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += e.len();
    }
    for e in &encoded {
        out.extend_from_slice(e);
    }
    Ok((out, notes))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn rgba_of(img: &Image) -> Vec<u8> {
        ops::convert_layout(img, Layout::Rgba, [0, 0, 0]).0.data
    }

    #[test]
    fn imagemagick_and_pillow_icons() {
        let dir = format!("{}/tests/data/ico", env!("CARGO_MANIFEST_DIR"));
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|x| x.to_str()) != Some("ico") {
                continue;
            }
            let data = std::fs::read(&p).unwrap();
            assert!(sniff(&data), "{}", p.display());
            let (img, _) = decode(&data, &Limits::default(), &NoProgress).unwrap();
            let want = std::fs::read(p.with_extension("rgba")).unwrap();
            assert_eq!(rgba_of(&img), want, "{}", p.display());
            n += 1;
        }
        assert!(n >= 5, "expected the generated fixtures, found {n}");
    }

    fn picture(w: u32, h: u32) -> Image {
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let a = if (x + y) % 7 == 0 { 0 } else { 255 };
                d.extend_from_slice(&[(x * 5) as u8, (y * 9) as u8, (x + y) as u8, a]);
            }
        }
        Image::new(w, h, Layout::Rgba, Depth::Eight, d).unwrap()
    }

    #[test]
    fn roundtrip_single_and_multiple_sizes() {
        let img = picture(24, 24);
        let (bytes, _) = encode(&img, &EncodeOptions::default()).unwrap();
        let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        assert_eq!(back.data, img.data);

        let (bytes, _) = encode(
            &img,
            &EncodeOptions {
                sizes: vec![48, 16, 32],
                cursor: false,
            },
        )
        .unwrap();
        let (back, notes) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        assert_eq!((back.width, back.height), (48, 48));
        assert!(notes.iter().any(|n| n.code == "ico.largest_image"));
    }

    #[test]
    fn big_images_shrink_and_non_square_sizes_are_padded() {
        let (bytes, notes) = encode(&picture(400, 200), &EncodeOptions::default()).unwrap();
        let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        assert_eq!((back.width, back.height), (256, 128));
        assert!(notes.iter().any(|n| n.code == "ico.resized"));
        let (bytes, _) = encode(
            &picture(40, 20),
            &EncodeOptions {
                sizes: vec![32],
                cursor: true,
            },
        )
        .unwrap();
        assert_eq!(bytes.get(2), Some(&2)); // cursor type
        let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        assert_eq!((back.width, back.height), (32, 32));
        // Top and bottom rows are padding.
        assert_eq!(back.data.get(3), Some(&0));
    }

    #[test]
    fn bad_input_errors() {
        let (good, _) = encode(&picture(16, 16), &EncodeOptions::default()).unwrap();
        for cut in [0, 3, 6, 20, 30, good.len() - 1] {
            assert!(
                decode(&good[..cut], &Limits::default(), &NoProgress).is_err(),
                "cut {cut}"
            );
        }
        let mut bad = good.clone();
        bad[4] = 0; // zero images
        assert!(decode(&bad, &Limits::default(), &NoProgress).is_err());
        assert!(encode(
            &picture(4, 4),
            &EncodeOptions {
                sizes: vec![300],
                cursor: false
            }
        )
        .is_err());
    }
}
