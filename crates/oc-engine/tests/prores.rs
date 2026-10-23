//! ProRes in QuickTime files: read, decoded to pictures, and copied between containers.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::{Limits, NoProgress};
use oc_engine::{ConvertRequest, Engine};
use oc_video::{codec, frames::Decoder, Kind};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn convert(to: &str) -> Vec<u8> {
    let d = std::env::temp_dir().join(format!("oc-prores-{to}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let out = d.join(format!("out.{to}"));
    let e = Engine::new();
    let options = e.parse_options(&out, &[]).unwrap();
    e.convert(
        &ConvertRequest {
            input: fixture("clip.mov"),
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

fn source() -> oc_video::Media {
    oc_video::mp4::read(
        &std::fs::read(fixture("clip.mov")).unwrap(),
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn stream_is_described() {
    let m = source();
    let v = &m.streams[0];
    assert_eq!((v.kind, v.codec.as_str()), (Kind::Video, codec::PRORES));
    assert_eq!((v.width, v.height), (64, 48));
    assert_eq!(&v.private, b"apch");
    assert_eq!(m.packets_of(0).count(), 3);
}

#[test]
fn pictures_match_ffmpeg_8_bit_output() {
    let m = source();
    let want = std::fs::read(fixture("clip420.yuv")).unwrap();
    let mut dec = Decoder::new(&m.streams[0], &Limits::default()).unwrap();
    let mut at = 0;
    for p in m.packets_of(0) {
        let pic = dec.decode(&p.data).unwrap().unwrap();
        let planes: [&[u8]; 3] = [&pic.y, &pic.u, &pic.v];
        for (i, plane) in planes.iter().enumerate() {
            // Luma is a direct 10 to 8 bit rounding. ffmpeg's converter filters chroma
            // differently from a plain average, so only its mean must agree.
            let mut sum = 0i64;
            for (k, &v) in plane.iter().enumerate() {
                let diff = (i32::from(v) - i32::from(want[at + k])).abs();
                if i == 0 {
                    assert!(diff <= 1, "luma sample {k}: {v} vs {}", want[at + k]);
                }
                sum += i64::from(diff);
            }
            if i > 0 {
                assert!(
                    sum < 3 * plane.len() as i64,
                    "plane {i} mean difference {sum}"
                );
            }
            at += plane.len();
        }
    }
    assert_eq!(at, want.len());
}

#[test]
fn copied_to_another_mov_unchanged() {
    let out = convert("mov");
    let after = oc_video::mp4::read(&out, &Limits::default()).unwrap();
    let before = source();
    assert_eq!(after.streams[0].codec, codec::PRORES);
    assert_eq!(after.streams[0].private, before.streams[0].private);
    for (a, b) in before.packets_of(0).zip(after.packets_of(0)) {
        assert_eq!(a.data, b.data);
    }
}

#[test]
fn transcoded_into_webm() {
    let out = convert("webm");
    let m = oc_video::mkv::read(&out, &Limits::default()).unwrap();
    assert_eq!(m.streams[0].codec, codec::VP8);
    assert_eq!(m.packets_of(0).count(), 3);
}
