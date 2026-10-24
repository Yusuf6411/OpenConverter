//! JPEG XL fixtures encoded by libjxl (through ffmpeg); the expected pixels are libjxl's own
//! decode of the same files, so lossless files must match bit for bit.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
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
        ("feat_resample2", 96, 72, Layout::Rgb),
        ("feat_resample8", 96, 72, Layout::Rgb),
        ("feat_ec_resample", 96, 72, Layout::Rgba),
        ("feat_noise", 96, 72, Layout::Rgb),
        ("feat_progressive_dc", 96, 72, Layout::Rgb),
        ("feat_progressive_ac", 96, 72, Layout::Rgb),
        ("feat_modular_lossy", 96, 72, Layout::Rgb),
        ("feat_palette", 96, 72, Layout::Rgb),
        ("feat_squeeze", 96, 72, Layout::Rgb),
        ("feat_rgba_lossless", 96, 72, Layout::Rgba),
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
fn embedded_icc_profile_is_rebuilt_and_pixels_stay_in_its_space() {
    let (img, _) = jxl::decode(&read("icc_test_ll.jxl"), &Limits::default()).unwrap();
    // the profile survives the file's compact encoding byte for byte
    assert!(img.metadata.icc_profile.as_deref() == Some(&read("icc_test.icc")[..]));
    assert!(img.data == read("icc_img.rgb"));
}

#[test]
fn lossy_file_with_custom_primaries_is_converted_to_srgb() {
    // an encoder-converted image: no profile is left, the pixels are sRGB
    let (img, _) = jxl::decode(&read("icc_test_d1.jxl"), &Limits::default()).unwrap();
    assert!(img.metadata.icc_profile.is_none());
    assert_eq!((img.width, img.height), (64, 48));
    let want = read("icc_test_d1.raw");
    let worst = img
        .data
        .iter()
        .zip(&want)
        .map(|(&a, &b)| (i32::from(a) - i32::from(b)).abs())
        .max()
        .unwrap();
    assert!(worst <= 2, "largest difference {worst}");
}

#[test]
fn damaged_input_never_panics() {
    for name in ["rgb_lossless", "vardct_d1", "jpeg_small_420", "icc_test_ll"] {
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

/// Long mutation run over every fixture: `cargo test -p oc-image --test jxl --release -- --ignored`
/// (set `JXL_STRESS_ROUNDS` to change the number of mutations per file).
#[test]
#[ignore]
fn stress_mutations_never_panic() {
    let rounds: u32 = std::env::var("JXL_STRESS_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/jxl");
    let limits = Limits {
        max_pixels: 1 << 20,
        max_alloc: 1 << 27,
        max_output: 1 << 26,
        ..Limits::default()
    };
    let mut x = 0x9e37_79b9u32;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "jxl") {
            continue;
        }
        let f = std::fs::read(&path).unwrap();
        for _ in 0..rounds {
            let mut b = f.clone();
            for _ in 0..1 + next() % 6 {
                let i = next() as usize % b.len();
                match next() % 3 {
                    0 => b[i] ^= 1 << (next() % 8),
                    1 => b[i] = next() as u8,
                    _ => b.truncate(i.max(1)),
                }
            }
            let _ = jxl::decode(&b, &limits);
        }
    }
}
