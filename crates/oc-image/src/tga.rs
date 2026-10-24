//! Truevision TGA.
//!
//! Read: colour-mapped (type 1/9), true-colour (2/10) and grayscale (3/11), raw or RLE,
//! 8/15/16/24/32 bits per pixel, any of the four pixel origins. The TGA 2.0 footer and
//! extension area are ignored.
//! Write: 24-bit BGR, 32-bit BGRA (when there is translucency) or 8-bit gray, top-left
//! origin, optionally RLE.
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_core::{Error, ErrorCode, Limits, Notes, Result};
use oc_io::ByteReader;

/// TGA has no magic number, so this is a structural check of the 18-byte header. It is
/// deliberately strict so that it rarely claims other formats' files.
pub fn sniff(data: &[u8]) -> bool {
    let Some(h) = data.get(..18) else {
        return false;
    };
    let (Some(&cmap_type), Some(&image_type), Some(&bpp), Some(&desc)) =
        (h.get(1), h.get(2), h.get(16), h.get(17))
    else {
        return false;
    };
    let w = u16::from_le_bytes([
        h.get(12).copied().unwrap_or(0),
        h.get(13).copied().unwrap_or(0),
    ]);
    let ht = u16::from_le_bytes([
        h.get(14).copied().unwrap_or(0),
        h.get(15).copied().unwrap_or(0),
    ]);
    cmap_type <= 1
        && matches!(image_type, 1 | 2 | 3 | 9 | 10 | 11)
        && matches!(bpp, 8 | 15 | 16 | 24 | 32)
        && desc & 0xC0 == 0
        && w > 0
        && ht > 0
        && (image_type & 7 != 1 || cmap_type == 1)
}

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorCode::UnsupportedVariant,
        format!("TGA: {what} is not supported"),
    )
}

/// Replicate the top bits so that 5-bit 31 becomes 255 exactly.
fn expand5(v: u16) -> u8 {
    let v = (v & 31) as u8;
    (v << 3) | (v >> 2)
}

/// Convert one stored pixel (`bytes`, little-endian) to RGBA.
fn to_rgba(bytes: &[u8], bpp: u8, palette: &[[u8; 4]], gray: bool, use_attr: bool) -> [u8; 4] {
    match (bytes, bpp) {
        (&[b, g, r, a], 32) => [r, g, b, if use_attr { a } else { 255 }],
        (&[b, g, r], 24) => [r, g, b, 255],
        (&[lo, hi], 15 | 16) => {
            let v = u16::from_le_bytes([lo, hi]);
            let a = if bpp == 16 && use_attr && v & 0x8000 == 0 {
                0
            } else {
                255
            };
            [expand5(v >> 10), expand5(v >> 5), expand5(v), a]
        }
        (&[i], 8) => {
            if gray {
                [i, i, i, 255]
            } else {
                palette
                    .get(usize::from(i))
                    .copied()
                    .unwrap_or([0, 0, 0, 255])
            }
        }
        _ => [0, 0, 0, 255],
    }
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let mut r = ByteReader::new(data);
    let id_len = usize::from(r.u8()?);
    let cmap_type = r.u8()?;
    let image_type = r.u8()?;
    let cmap_first = usize::from(r.u16_le()?);
    let cmap_len = usize::from(r.u16_le()?);
    let cmap_bits = r.u8()?;
    r.skip(4)?; // x/y origin
    let width = u32::from(r.u16_le()?);
    let height = u32::from(r.u16_le()?);
    let bpp = r.u8()?;
    let desc = r.u8()?;

    if !matches!(image_type, 1 | 2 | 3 | 9 | 10 | 11) {
        return Err(if image_type == 0 {
            Error::corrupt("TGA file contains no image data")
        } else {
            unsupported("this image type")
        });
    }
    let rle = image_type >= 9;
    let kind = image_type & 7; // 1 mapped, 2 colour, 3 gray
    if width == 0 || height == 0 {
        return Err(Error::corrupt("TGA has zero width or height"));
    }
    let ok_bpp = match kind {
        1 | 3 => bpp == 8,
        _ => matches!(bpp, 15 | 16 | 24 | 32),
    };
    if !ok_bpp {
        return Err(unsupported("this bit depth"));
    }
    limits.check_pixels(width, height)?;
    limits.check_alloc(u64::from(width) * u64::from(height) * 4)?;

    r.skip(id_len)?;
    let mut palette: Vec<[u8; 4]> = Vec::new();
    if cmap_type == 1 {
        let entry = match cmap_bits {
            15 | 16 => 2,
            24 => 3,
            32 => 4,
            _ => return Err(unsupported("this colour-map entry size")),
        };
        let raw = r.take(cmap_len * entry)?;
        // Entries before `cmap_first` are not stored; pad so that pixel indices line up.
        palette.resize(cmap_first, [0, 0, 0, 255]);
        for e in raw.chunks_exact(entry) {
            palette.push(to_rgba(e, cmap_bits, &[], false, false));
        }
    } else if kind == 1 {
        return Err(Error::corrupt("TGA colour-mapped image has no colour map"));
    }

    // Raw stored pixels, in file order.
    let bytes_pp = usize::from(bpp.div_ceil(8));
    let n = width as usize * height as usize;
    let mut raw = Vec::with_capacity(n * bytes_pp);
    if rle {
        while raw.len() < n * bytes_pp {
            let head = r.u8()?;
            let count = usize::from(head & 0x7F) + 1;
            let remaining = (n * bytes_pp - raw.len()) / bytes_pp;
            // A packet running past the end of the image is tolerated by truncating it.
            let count = count.min(remaining);
            if head & 0x80 != 0 {
                let px = r.take(bytes_pp)?;
                for _ in 0..count {
                    raw.extend_from_slice(px);
                }
            } else {
                raw.extend_from_slice(r.take(count * bytes_pp)?);
            }
        }
    } else {
        raw.extend_from_slice(r.take(n * bytes_pp)?);
    }

    let alpha_bits = desc & 0x0F;
    let mut use_attr = bpp == 32 || (bpp == 16 && alpha_bits == 1);
    if bpp == 32 && alpha_bits == 0 {
        // Many writers leave the attribute bits at zero and still store real alpha, others
        // store zeros. All-zero alpha with no declared alpha channel is read as opaque.
        if raw.as_chunks::<4>().0.iter().all(|&[_, _, _, a]| a == 0) {
            use_attr = false;
            notes.push(oc_core::Note::new(
                oc_core::Severity::Info,
                "tga.alpha-ignored",
                "the alpha channel was all zero and no alpha was declared; read as opaque",
            ));
        }
    }
    let gray = kind == 3;
    let right_to_left = desc & 0x10 != 0;
    let top_to_bottom = desc & 0x20 != 0;
    let mut rgba = vec![0u8; n * 4];
    for (i, px) in raw.chunks_exact(bytes_pp).enumerate() {
        let (sy, sx) = (i / width as usize, i % width as usize);
        let y = if top_to_bottom {
            sy
        } else {
            height as usize - 1 - sy
        };
        let x = if right_to_left {
            width as usize - 1 - sx
        } else {
            sx
        };
        let o = (y * width as usize + x) * 4;
        if let Some(d) = rgba.get_mut(o..o + 4) {
            d.copy_from_slice(&to_rgba(px, bpp, &palette, gray, use_attr));
        }
    }
    let img = Image::new(width, height, Layout::Rgba, Depth::Eight, rgba)?;
    let target = if gray {
        Layout::Gray
    } else if img.has_translucency() {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let (img, _) = ops::convert_layout(&img, target, [0, 0, 0]);
    Ok((img, notes))
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    /// Run-length encode the pixel data.
    pub rle: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self { rle: true }
    }
}

fn rle_encode(pixels: &[u8], bytes_pp: usize, width: usize, out: &mut Vec<u8>) {
    // Packets never cross a scanline, which keeps the file readable by strict decoders.
    for row in pixels.chunks(width * bytes_pp) {
        let px: Vec<&[u8]> = row.chunks_exact(bytes_pp).collect();
        let mut i = 0;
        while i < px.len() {
            let mut run = 1;
            while i + run < px.len() && run < 128 && px.get(i + run) == px.get(i) {
                run += 1;
            }
            if run >= 2 {
                out.push(0x80 | (run as u8 - 1));
                out.extend_from_slice(px.get(i).copied().unwrap_or(&[]));
                i += run;
            } else {
                // Literal packet: until the next repeat of two, at most 128 pixels.
                let start = i;
                let mut len = 0;
                while i < px.len() && len < 128 {
                    if px.get(i) == px.get(i + 1) {
                        break;
                    }
                    i += 1;
                    len += 1;
                }
                out.push(len as u8 - 1);
                for p in px.get(start..start + len).unwrap_or(&[]) {
                    out.extend_from_slice(p);
                }
            }
        }
    }
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> (Vec<u8>, Notes) {
    let mut notes = Notes::new();
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    let target = match e8.layout {
        Layout::Gray => Layout::Gray,
        Layout::GrayAlpha | Layout::Rgba if e8.has_translucency() => Layout::Rgba,
        _ => Layout::Rgb,
    };
    let (e, n) = ops::convert_layout(&e8, target, [255, 255, 255]);
    notes.extend(n);
    let bytes_pp = e.layout.channels();
    let (bpp, image_type, alpha_bits) = match e.layout {
        Layout::Gray => (8u8, 3u8, 0u8),
        Layout::Rgba => (32, 2, 8),
        _ => (24, 2, 0),
    };
    // Stored as BGR(A), top-left origin.
    let mut pixels = Vec::with_capacity(e.data.len());
    for px in e.data.chunks_exact(bytes_pp) {
        match *px {
            [r, g, b] => pixels.extend_from_slice(&[b, g, r]),
            [r, g, b, a] => pixels.extend_from_slice(&[b, g, r, a]),
            [v] => pixels.push(v),
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(18 + pixels.len());
    out.push(0); // no image id
    out.push(0); // no colour map
    out.push(if opts.rle { image_type + 8 } else { image_type });
    out.extend_from_slice(&[0; 5]); // colour map spec
    out.extend_from_slice(&[0; 4]); // origin
    out.extend_from_slice(&(e.width as u16).to_le_bytes());
    out.extend_from_slice(&(e.height as u16).to_le_bytes());
    out.push(bpp);
    out.push(0x20 | alpha_bits);
    if opts.rle {
        rle_encode(&pixels, bytes_pp, e.width as usize, &mut out);
    } else {
        out.extend_from_slice(&pixels);
    }
    (out, notes)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rgba_of(img: &Image) -> Vec<u8> {
        ops::convert_layout(img, Layout::Rgba, [0, 0, 0]).0.data
    }

    #[test]
    fn pillow_variants() {
        let dir = format!("{}/tests/data/tga", env!("CARGO_MANIFEST_DIR"));
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|x| x.to_str()) != Some("tga") {
                continue;
            }
            let (img, _) = decode(&std::fs::read(&p).unwrap(), &Limits::default()).unwrap();
            let want = std::fs::read(p.with_extension("rgba")).unwrap();
            assert_eq!(rgba_of(&img), want, "{}", p.display());
            assert!(sniff(&std::fs::read(&p).unwrap()), "{}", p.display());
            n += 1;
        }
        assert!(n >= 10, "expected the generated fixtures, found {n}");
    }

    fn sample() -> Image {
        let mut d = Vec::new();
        for y in 0..9u32 {
            for x in 0..13u32 {
                let flat = y < 3;
                d.extend_from_slice(&[
                    if flat { 7 } else { (x * 19) as u8 },
                    (y * 27) as u8,
                    (x * y) as u8,
                    if x == 4 { 128 } else { 255 },
                ]);
            }
        }
        Image::new(13, 9, Layout::Rgba, Depth::Eight, d).unwrap()
    }

    #[test]
    fn roundtrips_with_and_without_rle() {
        let img = sample();
        let flat = ops::convert_layout(&img, Layout::Rgb, [0, 0, 0]).0;
        let gray = ops::convert_layout(&img, Layout::Gray, [0, 0, 0]).0;
        for rle in [false, true] {
            let o = EncodeOptions { rle };
            for src in [&img, &flat, &gray] {
                let (bytes, _) = encode(src, &o);
                let (back, _) = decode(&bytes, &Limits::default()).unwrap();
                assert_eq!(back.layout, src.layout, "rle={rle}");
                assert_eq!(back.data, src.data, "rle={rle} {:?}", src.layout);
            }
        }
    }

    #[test]
    fn rle_shrinks_flat_images() {
        let img = Image::new(64, 64, Layout::Rgb, Depth::Eight, vec![9; 64 * 64 * 3]).unwrap();
        let (raw, _) = encode(&img, &EncodeOptions { rle: false });
        let (packed, _) = encode(&img, &EncodeOptions { rle: true });
        assert!(packed.len() < raw.len() / 20);
    }

    #[test]
    fn corrupt_input_errors() {
        let (good, _) = encode(&sample(), &EncodeOptions { rle: true });
        for cut in [0, 5, 17, 18, 30, good.len() - 1] {
            assert!(
                decode(&good[..cut], &Limits::default()).is_err(),
                "cut {cut}"
            );
        }
        let mut bad = good.clone();
        bad[2] = 0;
        assert!(decode(&bad, &Limits::default()).is_err());
        bad[2] = 7;
        assert_eq!(
            decode(&bad, &Limits::default()).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedVariant)
        );
        // A dimension bomb is stopped by the limits, not by running out of memory.
        let mut huge = good.clone();
        huge[12..16].copy_from_slice(&[0xFF; 4]);
        let tight = Limits {
            max_pixels: 1000,
            ..Limits::default()
        };
        assert!(decode(&huge, &tight).is_err());
    }
}
