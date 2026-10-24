#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
//! Synthetic DNG files (tools/make_dng_fixtures.py): a scene with known colours is turned into
//! a camera mosaic with a known colour matrix, so the rendered result can be compared with it.
use oc_core::{Limits, NoProgress};
use oc_image::dng;

fn read(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/dng")
            .join(name),
    )
    .unwrap()
}

/// The scene as 8-bit RGB, 64 by 48.
fn scene() -> Vec<u8> {
    let d = read("scene.ppm");
    d[d.len() - 64 * 48 * 3..].to_vec()
}

/// Mean and maximum difference between the render and `want` (8-bit levels) over a rectangle.
fn compare(name: &str, want: &[u8], ww: usize, wh: usize, margin: usize) -> (f64, u32) {
    let (img, _) = dng::decode(&read(name), &Limits::default(), &NoProgress).unwrap();
    assert_eq!(
        (img.width as usize, img.height as usize),
        (ww, wh),
        "{name}: size"
    );
    let (mut sum, mut max, mut n) = (0u64, 0u32, 0u64);
    for y in margin..wh - margin {
        for x in margin..ww - margin {
            for c in 0..3 {
                let got = u32::from(img.sample((y * ww + x) * 3 + c)) >> 8;
                let d = got.abs_diff(u32::from(want[(y * ww + x) * 3 + c]));
                sum += u64::from(d);
                max = max.max(d);
                n += 1;
            }
        }
    }
    (sum as f64 / n as f64, max)
}

fn check(name: &str) {
    let (mean, max) = compare(name, &scene(), 64, 48, 3);
    assert!(mean < 0.5 && max <= 2, "{name}: mean {mean}, max {max}");
}

#[test]
fn uncompressed_16_bit_strips() {
    check("rggb_16bit_strips.dng");
}

#[test]
fn packed_12_bit_big_endian_gbrg() {
    check("gbrg_12bit_packed_be.dng");
}

#[test]
fn lossless_jpeg_tiles_14_bit() {
    check("ljpeg_tiles_14bit.dng");
}

#[test]
fn deflate_with_two_illuminants_and_forward_matrices() {
    check("deflate_two_illuminants.dng");
}

#[test]
fn orientation_is_applied() {
    let s = scene();
    // Rotate 90 degrees clockwise: the new image is 48 wide and 64 tall.
    let mut want = vec![0u8; s.len()];
    for y in 0..48 {
        for x in 0..64 {
            let (dx, dy) = (47 - y, x);
            want[(dy * 48 + dx) * 3..(dy * 48 + dx) * 3 + 3]
                .copy_from_slice(&s[(y * 64 + x) * 3..(y * 64 + x) * 3 + 3]);
        }
    }
    let (mean, max) = compare("orientation6.dng", &want, 48, 64, 3);
    assert!(mean < 0.5 && max <= 2, "mean {mean}, max {max}");
}

#[test]
fn active_area_and_default_crop() {
    let s = scene();
    let (cx, cy, cw, ch) = (2usize, 2usize, 60usize, 44usize);
    let mut want = Vec::new();
    for y in cy..cy + ch {
        want.extend_from_slice(&s[(y * 64 + cx) * 3..(y * 64 + cx + cw) * 3]);
    }
    let (mean, max) = compare("active_area_crop.dng", &want, cw, ch, 3);
    assert!(mean < 0.5 && max <= 2, "mean {mean}, max {max}");
}

#[test]
fn sniffing_and_damage() {
    let f = read("rggb_16bit_strips.dng");
    assert!(dng::sniff(&f));
    assert!(!dng::sniff(b"II*\0\x08\0\0\0\0\0\0\0\0\0"));
    let mut x = 11u32;
    for _ in 0..400 {
        let mut b = f.clone();
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        let i = (x >> 8) as usize % b.len();
        b[i] ^= 1 << (x % 8);
        let _ = dng::decode(&b, &Limits::default(), &NoProgress);
    }
    assert!(dng::decode(&f[..f.len() / 2], &Limits::default(), &NoProgress).is_err());
}
