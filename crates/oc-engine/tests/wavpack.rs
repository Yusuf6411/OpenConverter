//! WavPack read through the engine (there is no WavPack writer).
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::{Limits, NoProgress};
use oc_engine::{ConvertRequest, Engine};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/st16.wv")
}

fn convert(to: &str) -> Result<Vec<u8>, oc_core::Error> {
    let d = std::env::temp_dir().join(format!("oc-wv-{to}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let out = d.join(format!("out.{to}"));
    let e = Engine::new();
    let options = e.parse_options(&out, &[]).unwrap();
    let r = e.convert(
        &ConvertRequest {
            input: fixture(),
            output: out.clone(),
            overwrite: true,
            display_name: None,
            options,
        },
        &NoProgress,
    );
    let bytes = r.map(|_| std::fs::read(&out).unwrap());
    let _ = std::fs::remove_dir_all(d);
    bytes
}

#[test]
fn detected_by_content() {
    let data = std::fs::read(fixture()).unwrap();
    assert_eq!(
        Engine::new().detect(&data, Path::new("x.bin")).unwrap().0,
        "wv"
    );
}

#[test]
fn becomes_wav_and_flac_with_the_same_samples() {
    let wav = convert("wav").unwrap();
    let (a, _) = oc_audio::wav::read(&wav, &Limits::default()).unwrap();
    let wv = std::fs::read(fixture()).unwrap();
    let (b, _) = oc_audio::wavpack::read(&wv, &Limits::default()).unwrap();
    assert_eq!(a.samples, b.samples);
    let flac = convert("flac").unwrap();
    let (c, _) = oc_audio::flac::read(&flac, &Limits::default()).unwrap();
    assert_eq!(c.samples, b.samples);
}

#[test]
fn there_is_no_wavpack_writer() {
    assert!(convert("wv").is_err());
}
