//! VP9 video (WebM) decoded through the engine.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::{Limits, NoProgress};
use oc_engine::{ConvertRequest, Engine};
use oc_video::codec;
use std::path::PathBuf;

fn convert(to: &str) -> Vec<u8> {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/av_vp9.webm");
    let d = std::env::temp_dir().join(format!("oc-vp9-{to}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let out = d.join(format!("out.{to}"));
    let e = Engine::new();
    let options = e.parse_options(&out, &[]).unwrap();
    e.convert(
        &ConvertRequest {
            input,
            output: out.clone(),
            overwrite: true,
            display_name: None,
            options,
        },
        &NoProgress,
    )
    .unwrap();
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_dir_all(d);
    bytes
}

#[test]
fn vp9_clip_becomes_a_picture() {
    let png = convert("png");
    let (img, _) = oc_image::png::decode(&png, &Limits::default(), &NoProgress).unwrap();
    assert_eq!((img.width, img.height), (64, 48));
    // The test card is not flat: a decoder that returned grey would fail this.
    let first = img.data[0];
    assert!(img.data.iter().any(|&b| b.abs_diff(first) > 40));
}

#[test]
fn vp9_clip_becomes_a_gif() {
    let gif = convert("gif");
    assert_eq!(&gif[..3], b"GIF");
    let _ = codec::VP9;
}
