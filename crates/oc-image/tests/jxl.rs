//! JPEG XL fixtures encoded by libjxl (through ffmpeg); the expected pixels are libjxl's own
//! decode of the same files, so lossless files must match bit for bit.
use oc_core::Limits;
use oc_image::{jxl, Depth, Layout};
use std::path::PathBuf;

fn read(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/jxl")
        .join(name);
    std::fs::read(p).unwrap()
}

#[test]
fn rgb_lossless_matches_libjxl() {
    let (img, _) = jxl::decode(&read("rgb_lossless.jxl"), &Limits::default()).unwrap();
    assert_eq!(
        (img.width, img.height, img.layout, img.depth),
        (300, 200, Layout::Rgb, Depth::Eight)
    );
    assert!(img.data == read("rgb_lossless.rgb"));
}

#[test]
fn gray_lossless_matches_libjxl() {
    let (img, _) = jxl::decode(&read("gray_lossless.jxl"), &Limits::default()).unwrap();
    assert_eq!(img.layout, Layout::Gray);
    assert!(img.data == read("gray_lossless.gray"));
}

/// Lossy files: libjxl's decode is the reference; ours may differ by rounding of the float
/// pipeline only.
#[test]
fn lossy_files_match_libjxl_within_rounding() {
    let cases = [
        ("vardct_d1", 96, 80, Layout::Rgb),
        ("vardct_d4_e9", 96, 80, Layout::Rgb),
        ("vardct_gray", 96, 80, Layout::Gray),
        ("vardct_rgba", 96, 80, Layout::Rgba),
        ("vardct_multigroup", 300, 260, Layout::Rgb),
        ("vardct_16bit", 96, 80, Layout::Rgb),
        ("jpeg_small_420", 37, 23, Layout::Rgb),
        ("jpeg_small_422", 37, 23, Layout::Rgb),
        ("jpeg_small_444", 37, 23, Layout::Rgb),
        ("jpeg_big_420", 131, 97, Layout::Rgb),
        ("jpeg_gray", 131, 97, Layout::Gray),
    ];
    for (name, w, h, layout) in cases {
        let (img, _) = jxl::decode(&read(&format!("{name}.jxl")), &Limits::default()).unwrap();
        assert_eq!(
            (img.width, img.height, img.layout),
            (w, h, layout),
            "{name}"
        );
        let want = read(&format!("{name}.raw"));
        let (got, tol): (Vec<i32>, i32) = match img.depth {
            Depth::Eight => (img.data.iter().map(|&v| i32::from(v)).collect(), 2),
            _ => (
                img.data
                    .chunks(2)
                    .map(|c| i32::from(u16::from_be_bytes([c[0], c[1]])))
                    .collect(),
                8,
            ),
        };
        let want: Vec<i32> = match img.depth {
            Depth::Eight => want.iter().map(|&v| i32::from(v)).collect(),
            _ => want
                .chunks(2)
                .map(|c| i32::from(u16::from_le_bytes([c[0], c[1]])))
                .collect(),
        };
        assert_eq!(got.len(), want.len(), "{name}");
        let worst = got
            .iter()
            .zip(&want)
            .map(|(a, b)| (a - b).abs())
            .max()
            .unwrap();
        assert!(worst <= tol, "{name}: largest difference {worst}");
    }
}

#[test]
fn damaged_input_never_panics() {
    for name in ["rgb_lossless", "vardct_d1", "jpeg_small_420"] {
        damage(&read(&format!("{name}.jxl")));
    }
}

fn damage(f: &[u8]) {
    for cut in [0, 1, 2, 5, 20, f.len() / 2, f.len() - 1] {
        let _ = jxl::decode(&f[..cut], &Limits::default());
    }
    let mut x = 0x1234_5678u32;
    for _ in 0..300 {
        let mut b = f.to_vec();
        for _ in 0..4 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let i = (x >> 8) as usize % b.len();
            b[i] ^= (x >> 3) as u8 | 1;
        }
        let _ = jxl::decode(&b, &Limits::default());
    }
}
