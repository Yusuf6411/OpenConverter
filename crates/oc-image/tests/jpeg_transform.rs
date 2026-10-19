//! Lossless JPEG transforms against jpegtran (tools/make_jpeg_transform_fixtures.py).
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use oc_core::{Limits, NoProgress};
use oc_image::jpeg::{self, Op, Transform};

fn transform_for(op: &str) -> Transform {
    match op {
        "flip_h" => Transform::rotate_flip(0, true, false),
        "flip_v" => Transform::rotate_flip(0, false, true),
        "transpose" => Some(Transform {
            ops: vec![Op::Transpose],
            crop: None,
            progressive: None,
        }),
        "transverse" => Some(Transform {
            ops: vec![Op::Transpose, Op::FlipH, Op::FlipV],
            crop: None,
            progressive: None,
        }),
        "rot90" => Transform::rotate_flip(90, false, false),
        "rot180" => Transform::rotate_flip(180, false, false),
        "rot270" => Transform::rotate_flip(270, false, false),
        "crop" => Some(Transform {
            ops: vec![],
            crop: Some([16, 8, 16, 16]),
            progressive: None,
        }),
        other => panic!("{other}"),
    }
    .unwrap()
}

#[test]
fn transforms_match_jpegtran() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/jpeg/transform");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        let Some(stem) = name.strip_suffix(".txt") else {
            continue;
        };
        let (src, op) = stem.split_once("__").unwrap();
        let input = std::fs::read(dir.join(format!("{src}.jpg"))).unwrap();
        let (bytes, _) =
            jpeg::transform(&input, &transform_for(op), &Limits::default(), &NoProgress).unwrap();
        let (img, _) = jpeg::decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        let dims: Vec<u32> = std::fs::read_to_string(&p)
            .unwrap()
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        assert_eq!((img.width, img.height), (dims[0], dims[1]), "{stem}");
        let ext = if src == "tgray" { "gray" } else { "rgb" };
        let want = std::fs::read(dir.join(format!("{stem}.{ext}"))).unwrap();
        assert_eq!(img.data, want, "{stem}");
        n += 1;
    }
    assert!(n >= 60, "found {n} fixtures");
}

#[test]
fn edge_trim_is_reported() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/jpeg/transform");
    let input = std::fs::read(dir.join("t420_odd.jpg")).unwrap();
    let (_, notes) = jpeg::transform(
        &input,
        &transform_for("flip_h"),
        &Limits::default(),
        &NoProgress,
    )
    .unwrap();
    assert!(notes.iter().any(|n| n.code == "jpeg.edge_trimmed"));
    let (_, notes) = jpeg::transform(
        &input,
        &transform_for("transpose"),
        &Limits::default(),
        &NoProgress,
    )
    .unwrap();
    assert!(notes.is_empty());
}

#[test]
fn four_rotations_restore_the_picture() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/jpeg/transform");
    let input = std::fs::read(dir.join("t420.jpg")).unwrap();
    let t = transform_for("rot90");
    let mut cur = input.clone();
    for _ in 0..4 {
        cur = jpeg::transform(&cur, &t, &Limits::default(), &NoProgress)
            .unwrap()
            .0;
    }
    let a = jpeg::decode(&input, &Limits::default(), &NoProgress)
        .unwrap()
        .0;
    let b = jpeg::decode(&cur, &Limits::default(), &NoProgress)
        .unwrap()
        .0;
    assert_eq!(a.data, b.data);
}
