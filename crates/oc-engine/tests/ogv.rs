//! Ogg video (Theora with Vorbis) read through the engine.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::{Limits, NoProgress};
use oc_engine::{ConvertRequest, Engine};
use oc_video::{codec, Kind};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/av.ogv")
}

fn convert(to: &str) -> Vec<u8> {
    let d = std::env::temp_dir().join(format!("oc-ogv-{to}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let out = d.join(format!("out.{to}"));
    let e = Engine::new();
    let options = e.parse_options(&out, &[]).unwrap();
    e.convert(
        &ConvertRequest {
            input: fixture(),
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
fn detected_as_video_not_audio() {
    let data = std::fs::read(fixture()).unwrap();
    let id = Engine::new().detect(&data, Path::new("clip.ogg")).unwrap();
    assert_eq!(id.0, "ogv");
}

#[test]
fn streams_and_timing() {
    let data = std::fs::read(fixture()).unwrap();
    let m = oc_video::ogv::read(&data, &Limits::default()).unwrap();
    assert_eq!(m.streams.len(), 2);
    let v = &m.streams[0];
    assert_eq!((v.kind, v.codec.as_str()), (Kind::Video, codec::THEORA));
    assert_eq!((v.width, v.height, v.fps), (64, 48, Some((10, 1))));
    let a = &m.streams[1];
    assert_eq!(
        (a.kind, a.codec.as_str(), a.channels),
        (Kind::Audio, codec::VORBIS, 1)
    );
    assert_eq!(a.sample_rate, 22050);
    let frames: Vec<_> = m.packets_of(0).collect();
    assert_eq!(frames.len(), 8);
    assert!(frames[0].key);
    assert_eq!(frames[3].pts, 300_000_000);
    assert!(frames.iter().filter(|p| p.key).count() >= 2);
}

#[test]
fn copied_into_matroska_without_touching_the_packets() {
    let mkv = convert("mkv");
    let before =
        oc_video::ogv::read(&std::fs::read(fixture()).unwrap(), &Limits::default()).unwrap();
    let after = oc_video::mkv::read(&mkv, &Limits::default()).unwrap();
    assert_eq!(after.streams[0].codec, codec::THEORA);
    assert_eq!(after.streams[1].codec, codec::VORBIS);
    assert_eq!(after.streams[0].private, before.streams[0].private);
    let (a, b): (Vec<_>, Vec<_>) = (
        before.packets_of(0).collect(),
        after.packets_of(0).collect(),
    );
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.data, y.data);
    }
}

#[test]
fn decoded_into_webm_and_a_picture() {
    let webm = convert("webm");
    let m = oc_video::mkv::read(&webm, &Limits::default()).unwrap();
    assert_eq!(m.streams[0].codec, codec::VP8);
    assert_eq!(m.packets_of(0).count(), 8);
    let png = convert("png");
    let (img, _) = oc_image::png::decode(&png, &Limits::default(), &NoProgress).unwrap();
    assert_eq!((img.width, img.height), (64, 48));
}
