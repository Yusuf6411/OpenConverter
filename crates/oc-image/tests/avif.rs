//! AVIF fixtures encoded by libaom (through ffmpeg); the expected pixels are ffmpeg's decode
//! (dav1d plus swscale) of the same files, so the comparison allows for the different chroma
//! upsampling filters.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;
use oc_image::{avif, Depth, Layout};
use std::path::PathBuf;

fn read(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/avif")
            .join(name),
    )
    .unwrap()
}

fn mean_error(a: &[u8], b: &[u8], sixteen: bool) -> f64 {
    assert_eq!(a.len(), b.len());
    let step = if sixteen { 2 } else { 1 };
    let mut sum = 0.0;
    let mut n = 0.0;
    for i in (0..a.len()).step_by(step) {
        let (x, y) = if sixteen {
            (
                f64::from(u16::from_be_bytes([a[i], a[i + 1]])) / 257.0,
                f64::from(u16::from_be_bytes([b[i], b[i + 1]])) / 257.0,
            )
        } else {
            (f64::from(a[i]), f64::from(b[i]))
        };
        sum += (x - y).abs();
        n += 1.0;
    }
    sum / n
}

#[test]
fn sniffs() {
    assert!(avif::sniff(&read("grad420.avif")));
    assert!(!avif::sniff(b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom"));
}

#[test]
fn yuv420_still() {
    let (img, _) = avif::decode(&read("grad420.avif"), &Limits::default()).unwrap();
    assert_eq!(
        (img.width, img.height, img.layout, img.depth),
        (96, 64, Layout::Rgb, Depth::Eight)
    );
    let e = mean_error(&img.data, &read("grad420.rgb"), false);
    assert!(e < 2.0, "{e}");
}

#[test]
fn yuv444_ten_bit_is_sixteen_bit_output() {
    let (img, _) = avif::decode(&read("grad444_10.avif"), &Limits::default()).unwrap();
    assert_eq!((img.layout, img.depth), (Layout::Rgb, Depth::Sixteen));
    let e = mean_error(&img.data, &read("grad444_10.rgb"), true);
    assert!(e < 1.5, "{e}");
}

#[test]
fn auxiliary_alpha_image() {
    let (img, _) = avif::decode(&read("alpha.avif"), &Limits::default()).unwrap();
    assert_eq!(img.layout, Layout::Rgba);
}

#[test]
fn truncation_and_damage_do_not_panic() {
    let data = read("alpha.avif");
    for n in 0..data.len() {
        let _ = avif::decode(&data[..n], &Limits::default());
    }
    for i in 0..data.len() {
        let mut d = data.clone();
        d[i] ^= 0x5a;
        let _ = avif::decode(&d, &Limits::default());
    }
}
