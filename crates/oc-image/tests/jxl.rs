//! JPEG XL fixtures encoded by libjxl (through ffmpeg); the expected pixels are libjxl's own
//! decode of the same files, so lossless files must match bit for bit.
use oc_core::Limits;
use oc_image::{jxl, Depth, Layout};
use std::path::PathBuf;

fn read(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/jxl").join(name);
    std::fs::read(p).unwrap()
}

#[test]
fn rgb_lossless_matches_libjxl() {
    let (img, _) = jxl::decode(&read("rgb_lossless.jxl"), &Limits::default()).unwrap();
    assert_eq!((img.width, img.height, img.layout, img.depth), (300, 200, Layout::Rgb, Depth::Eight));
    assert!(img.data == read("rgb_lossless.rgb"));
}

#[test]
fn gray_lossless_matches_libjxl() {
    let (img, _) = jxl::decode(&read("gray_lossless.jxl"), &Limits::default()).unwrap();
    assert_eq!(img.layout, Layout::Gray);
    assert!(img.data == read("gray_lossless.gray"));
}

#[test]
fn damaged_input_never_panics() {
    let f = read("rgb_lossless.jxl");
    for cut in [0, 1, 2, 5, 20, f.len() / 2, f.len() - 1] {
        let _ = jxl::decode(&f[..cut], &Limits::default());
    }
    let mut x = 0x1234_5678u32;
    for _ in 0..300 {
        let mut b = f.clone();
        for _ in 0..4 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let i = (x >> 8) as usize % b.len();
            b[i] ^= (x >> 3) as u8 | 1;
        }
        let _ = jxl::decode(&b, &Limits::default());
    }
}
