//! QOI ("Quite OK Image") read/write.
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_core::{Error, ErrorCode, Limits, Notes, Result};
use oc_io::ByteReader;

const MAGIC: &[u8; 4] = b"qoif";
const END: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

fn hash(p: [u8; 4]) -> usize {
    (usize::from(p[0]) * 3 + usize::from(p[1]) * 5 + usize::from(p[2]) * 7 + usize::from(p[3]) * 11)
        % 64
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<Image> {
    let mut r = ByteReader::new(data);
    if r.take(4)? != MAGIC {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a QOI file"));
    }
    let width = r.u32_be()?;
    let height = r.u32_be()?;
    let channels = r.u8()?;
    let _colorspace = r.u8()?;
    if channels != 3 && channels != 4 {
        return Err(Error::corrupt("QOI channels must be 3 or 4").at(12));
    }
    if width == 0 || height == 0 {
        return Err(Error::corrupt("zero image size"));
    }
    limits.check_pixels(width, height)?;
    let n = width as usize * height as usize;
    limits.check_alloc((n * 4) as u64)?;
    let mut px = [0u8, 0, 0, 255];
    let mut index = [[0u8; 4]; 64];
    let mut out = Vec::with_capacity(n * usize::from(channels));
    let mut run = 0u32;
    for _ in 0..n {
        if run > 0 {
            run -= 1;
        } else {
            let b = r.u8()?;
            match b {
                0xFE => {
                    px[0] = r.u8()?;
                    px[1] = r.u8()?;
                    px[2] = r.u8()?;
                }
                0xFF => {
                    px = [r.u8()?, r.u8()?, r.u8()?, r.u8()?];
                }
                _ => match b >> 6 {
                    0 => px = index.get(usize::from(b & 63)).copied().unwrap_or(px),
                    1 => {
                        px[0] = px[0].wrapping_add(((b >> 4) & 3).wrapping_sub(2));
                        px[1] = px[1].wrapping_add(((b >> 2) & 3).wrapping_sub(2));
                        px[2] = px[2].wrapping_add((b & 3).wrapping_sub(2));
                    }
                    2 => {
                        let b2 = r.u8()?;
                        let dg = (b & 63).wrapping_sub(32);
                        px[0] = px[0].wrapping_add(dg.wrapping_sub(8).wrapping_add(b2 >> 4));
                        px[1] = px[1].wrapping_add(dg);
                        px[2] = px[2].wrapping_add(dg.wrapping_sub(8).wrapping_add(b2 & 15));
                    }
                    _ => run = u32::from(b & 63),
                },
            }
            if let Some(slot) = index.get_mut(hash(px)) {
                *slot = px;
            }
        }
        out.extend_from_slice(px.get(..usize::from(channels)).unwrap_or(&px));
    }
    let layout = if channels == 4 {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    Image::new(width, height, layout, Depth::Eight, out)
}

#[allow(clippy::indexing_slicing)] // chunk size is `ch`; table index is hash % 64
pub fn encode(img: &Image) -> (Vec<u8>, Notes) {
    let mut notes = Notes::new();
    // QOI stores 8-bit RGB or RGBA only.
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    let target = if e8.layout.has_alpha() {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let (e, n) = ops::convert_layout(&e8, target, [255, 255, 255]);
    notes.extend(n);
    let ch = e.layout.channels();
    let mut out = Vec::with_capacity(e.data.len() / 2 + 32);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&e.width.to_be_bytes());
    out.extend_from_slice(&e.height.to_be_bytes());
    out.push(ch as u8);
    out.push(0);
    let mut index = [[0u8; 4]; 64];
    let mut prev = [0u8, 0, 0, 255];
    let mut run = 0u8;
    let total = e.pixel_count();
    for (i, chunk) in e.data.chunks_exact(ch).enumerate() {
        let px = [
            chunk[0],
            chunk[1],
            chunk[2],
            if ch == 4 { chunk[3] } else { 255 },
        ];
        if px == prev {
            run += 1;
            if run == 62 || i == total - 1 {
                out.push(0xC0 | (run - 1));
                run = 0;
            }
        } else {
            if run > 0 {
                out.push(0xC0 | (run - 1));
                run = 0;
            }
            let h = hash(px);
            if index[h] == px {
                out.push(h as u8);
            } else {
                index[h] = px;
                if px[3] == prev[3] {
                    let dr = px[0].wrapping_sub(prev[0]);
                    let dg = px[1].wrapping_sub(prev[1]);
                    let db = px[2].wrapping_sub(prev[2]);
                    let (vr, vg, vb) = (dr.wrapping_add(2), dg.wrapping_add(2), db.wrapping_add(2));
                    let (lr, lb) = (
                        dr.wrapping_sub(dg).wrapping_add(8),
                        db.wrapping_sub(dg).wrapping_add(8),
                    );
                    let lg = dg.wrapping_add(32);
                    if vr < 4 && vg < 4 && vb < 4 {
                        out.push(0x40 | (vr << 4) | (vg << 2) | vb);
                    } else if lg < 64 && lr < 16 && lb < 16 {
                        out.push(0x80 | lg);
                        out.push((lr << 4) | lb);
                    } else {
                        out.extend_from_slice(&[0xFE, px[0], px[1], px[2]]);
                    }
                } else {
                    out.extend_from_slice(&[0xFF, px[0], px[1], px[2], px[3]]);
                }
            }
        }
        prev = px;
    }
    out.extend_from_slice(&END);
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
    fn decodes_1x_fixture_and_reencodes_it_losslessly() {
        let img = decode(&fixture("alpha.qoi"), &Limits::default()).unwrap();
        assert_eq!(img.layout, Layout::Rgba);
        let (bytes, notes) = encode(&img);
        assert!(notes.is_empty());
        assert_eq!(decode(&bytes, &Limits::default()).unwrap().data, img.data);
    }

    #[test]
    fn roundtrips_runs_diffs_lumas_and_alpha() {
        let mut d = Vec::new();
        let mut s = 1u32;
        for i in 0..5000u32 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let px = match i % 4 {
                0 => [10, 20, 30, 255],
                1 => [
                    (s >> 24) as u8,
                    (s >> 16) as u8,
                    (s >> 8) as u8,
                    (s >> 20) as u8,
                ],
                2 => [11, 21, 29, 255],
                _ => [30, 40, 50, 255],
            };
            d.extend(px);
        }
        let img = Image::new(100, 50, Layout::Rgba, Depth::Eight, d).unwrap();
        let (b, _) = encode(&img);
        assert_eq!(decode(&b, &Limits::default()).unwrap().data, img.data);
        let flat = Image::new(100, 50, Layout::Rgb, Depth::Eight, vec![7; 15000]).unwrap();
        let (b, _) = encode(&flat);
        assert!(b.len() < 200);
        assert_eq!(decode(&b, &Limits::default()).unwrap().data, flat.data);
    }

    #[test]
    fn sixteen_bit_input_is_reduced_with_a_note() {
        let img = Image::new(
            1,
            1,
            Layout::Rgb,
            Depth::Sixteen,
            vec![255, 255, 0, 0, 128, 0],
        )
        .unwrap();
        let (b, notes) = encode(&img);
        assert!(notes.has_loss());
        assert_eq!(
            decode(&b, &Limits::default()).unwrap().data,
            vec![255, 0, 128]
        );
    }

    #[test]
    fn corrupt_input_errors() {
        let good = fixture("alpha.qoi");
        for cut in [0, 5, 13, 14, 20, good.len() / 2] {
            assert!(
                decode(&good[..cut], &Limits::default()).is_err(),
                "cut {cut}"
            );
        }
        assert!(decode(b"qoif\0\0\0\x01\0\0\0\x01\x07\0", &Limits::default()).is_err());
        assert!(decode(
            b"qoif\xff\xff\xff\xff\xff\xff\xff\xff\x04\0",
            &Limits::default()
        )
        .is_err());
    }
}
