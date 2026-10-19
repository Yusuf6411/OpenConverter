//! Decodes lossy WebP files made by libwebp and compares the planes with libwebp's own.
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]

use oc_core::NoProgress;
use std::fs;
use std::path::PathBuf;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// The payload of the `VP8 ` chunk of a simple lossy WebP file.
fn vp8_payload(webp: &[u8]) -> &[u8] {
    assert_eq!(&webp[..4], b"RIFF");
    assert_eq!(&webp[12..16], b"VP8 ", "fixture is not a simple lossy file");
    let size = u32::from_le_bytes(webp[16..20].try_into().unwrap()) as usize;
    &webp[20..20 + size]
}

#[test]
fn decodes_exactly_like_libwebp() {
    let mut names: Vec<_> = fs::read_dir(data_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "webp"))
        .collect();
    names.sort();
    assert!(names.len() > 30, "fixtures missing");
    let mut failures = Vec::new();
    for path in names {
        let webp = fs::read(&path).unwrap();
        let want = fs::read(path.with_extension("yuv")).unwrap();
        let frame = oc_vp8::decode(vp8_payload(&webp), &NoProgress).unwrap();
        let mut got = frame.y.clone();
        got.extend_from_slice(&frame.u);
        got.extend_from_slice(&frame.v);
        if got != want {
            let first = got.iter().zip(&want).position(|(a, b)| a != b);
            let wrong = got.iter().zip(&want).filter(|(a, b)| a != b).count();
            failures.push(format!(
                "{}: {}x{} {} bytes differ, first at {:?}",
                path.file_name().unwrap().to_string_lossy(),
                frame.width,
                frame.height,
                wrong,
                first
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
