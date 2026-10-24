//! Streams made by a purpose-written encoder; each `.ref` file is the page as decoded by
//! MuPDF and pdfium (the two agreed on every stream), preceded by its width and height.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;
use oc_jbig2::{decode_embedded, Env};
use std::path::Path;

fn env() -> Env {
    Env::new(None, Limits::default())
}

#[test]
fn matches_independent_decoders() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jb2"))
        .collect();
    names.sort();
    assert!(names.len() > 20);
    let mut failures = Vec::new();
    for p in &names {
        let stream = std::fs::read(p).unwrap();
        let glob = std::fs::read(p.with_extension("glob")).ok();
        let want = std::fs::read(p.with_extension("ref")).unwrap();
        let (w, h) = (
            u32::from_be_bytes(want[0..4].try_into().unwrap()) as usize,
            u32::from_be_bytes(want[4..8].try_into().unwrap()) as usize,
        );
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        if name.contains("mmr") {
            continue; // needs the fax decoder from oc-image; see its tests
        }
        match decode_embedded(glob.as_deref(), &stream, &env()) {
            Ok(page) => {
                if page.width != w || page.height != h || page.bits != want[8..] {
                    failures.push(format!("{name}: pixels differ"));
                }
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn damaged_streams_do_not_panic() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let rounds: u64 = std::env::var("JBIG2_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let lim = Limits {
        max_pixels: 1 << 22,
        ..Limits::default()
    };
    let e = Env::new(None, lim);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut files = Vec::new();
    for en in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = en.path();
        if p.extension().is_some_and(|x| x == "jb2" || x == "jbf") {
            files.push((p.extension().unwrap() == "jbf", std::fs::read(&p).unwrap()));
        }
    }
    files.sort();
    let mut ok = 0u64;
    for _ in 0..rounds {
        let (is_file, d0) = &files[(next() % files.len() as u64) as usize];
        let mut d = d0.clone();
        for _ in 0..1 + next() % 4 {
            let i = (next() % d.len() as u64) as usize;
            match next() % 3 {
                0 => d[i] = next() as u8,
                1 => d[i] ^= 1 << (next() % 8),
                _ => d.truncate(i.max(1)),
            }
            if d.is_empty() {
                break;
            }
        }
        let r = if *is_file {
            oc_jbig2::decode_file(&d, &e)
        } else {
            decode_embedded(None, &d, &e)
        };
        ok += u64::from(r.is_ok());
        if let Err(e) = &r {
            if std::env::var("JBIG2_DBG").is_ok() {
                eprintln!("{e}");
            }
        }
    }
    assert!(
        ok > rounds / 20,
        "only {ok} of {rounds} damaged streams decoded at all"
    );
}

#[test]
fn standalone_files_sequential_and_random_access() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut n = 0;
    for en in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = en.path();
        if p.extension().is_none_or(|x| x != "jbf") {
            continue;
        }
        let data = std::fs::read(&p).unwrap();
        assert!(oc_jbig2::sniff(&data));
        let want = std::fs::read(p.with_extension("ref")).unwrap();
        let page = oc_jbig2::decode_file(&data, &env()).unwrap_or_else(|e| panic!("{p:?}: {e}"));
        assert_eq!(page.bits, want[8..], "{p:?}");
        n += 1;
    }
    assert_eq!(n, 10);
}
