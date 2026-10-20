//! The lossless JPEG route of the engine: rotate and flip without decoding.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::{Limits, NoProgress};
use oc_engine::{ConvertRequest, Engine};
use std::path::PathBuf;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../oc-image/tests/data/jpeg/transform")
}

fn run(tag: &str, opts: &[(&str, &str)]) -> (Vec<u8>, oc_engine::ConvertReport) {
    let d = std::env::temp_dir().join(format!("oc-jl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let o = d.join("out.jpg");
    let e = Engine::new();
    let raw: Vec<(String, String)> = opts
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let options = e.parse_options(&o, &raw).unwrap();
    let report = e
        .convert(
            &ConvertRequest {
                input: data_dir().join("t420.jpg"),
                output: o.clone(),
                overwrite: true,
                display_name: None,
                options,
            },
            &NoProgress,
        )
        .unwrap();
    let out = std::fs::read(&o).unwrap();
    let _ = std::fs::remove_dir_all(d);
    (out, report)
}

#[test]
fn rotate_is_lossless() {
    let (out, r) = run("a", &[("rotate", "90"), ("lossless", "true")]);
    assert!(r.route.contains("Lossless"), "{}", r.route);
    let (img, _) = oc_image::jpeg::decode(&out, &Limits::default(), &NoProgress).unwrap();
    let want = std::fs::read(data_dir().join("t420__rot90.rgb")).unwrap();
    assert_eq!((img.width, img.height), (32, 48));
    assert_eq!(img.data, want);
}

#[test]
fn flip_then_rotate_order_matches_the_pixel_route() {
    let (lossless, _) = run(
        "b",
        &[
            ("flip", "horizontal"),
            ("rotate", "270"),
            ("lossless", "true"),
        ],
    );
    let (pixels, _) = run(
        "c",
        &[
            ("flip", "horizontal"),
            ("rotate", "270"),
            ("quality", "100"),
            ("subsampling", "4:4:4"),
        ],
    );
    let a = oc_image::jpeg::decode(&lossless, &Limits::default(), &NoProgress)
        .unwrap()
        .0;
    let b = oc_image::jpeg::decode(&pixels, &Limits::default(), &NoProgress)
        .unwrap()
        .0;
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mse: f64 = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(&x, &y)| f64::from(x.abs_diff(y)).powi(2))
        .sum::<f64>()
        / a.data.len() as f64;
    assert!(mse < 40.0, "mse {mse}");
}

#[test]
fn resizing_falls_back_to_pixels() {
    let (out, r) = run("d", &[("width", "24"), ("lossless", "true")]);
    assert!(r.notes.iter().any(|n| n.code == "jpeg.lossless_skipped"));
    let (img, _) = oc_image::jpeg::decode(&out, &Limits::default(), &NoProgress).unwrap();
    assert_eq!(img.width, 24);
}
