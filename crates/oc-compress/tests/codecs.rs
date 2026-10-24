//! Decoders against files made by CPython's bz2/lzma and the zstandard/lz4 modules
//! (`tools/make_fixtures.py`); encoders against the system tools when they are installed.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used)]
use oc_compress::bzip2;
use oc_core::{Limits, NoProgress};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn cases(codec: &str, ext: &str) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(codec);
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == ext) {
            let raw = std::fs::read(p.with_extension("raw")).unwrap();
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
                raw,
            ));
        }
    }
    assert!(!out.is_empty(), "no fixtures for {codec}");
    out.sort();
    out
}

/// Pipes `input` through `cmd args`; `None` when the tool is not installed.
fn tool(cmd: &str, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let data = input.to_vec();
    let t = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().ok()?;
    t.join().ok()?;
    out.status.success().then_some(out.stdout)
}

#[test]
fn bzip2_decodes_python_files() {
    for (name, comp, raw) in cases("bzip2", "bz2") {
        let got = bzip2::decompress(&comp, &Limits::default(), &NoProgress)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got == raw, "{name}");
    }
}

#[test]
fn bzip2_output_is_read_by_the_bzip2_tool() {
    for (name, _, raw) in cases("bzip2", "bz2") {
        for level in [1, 5, 9] {
            let z = bzip2::compress(&raw, level);
            if let Some(back) = tool("bzip2", &["-dc"], &z) {
                assert!(back == raw, "{name} level {level}");
            }
        }
    }
}

#[test]
fn bzip2_size_is_close_to_the_reference() {
    let (_, _, raw) = cases("bzip2", "bz2")
        .into_iter()
        .find(|c| c.0.starts_with("prose-9"))
        .unwrap();
    let ours = bzip2::compress(&raw, 9).len();
    if let Some(theirs) = tool("bzip2", &["-9c"], &raw) {
        assert!(
            ours * 100 <= theirs.len() * 103,
            "ours {ours} theirs {}",
            theirs.len()
        );
    }
}

use oc_compress::{lzma, xz};

#[test]
fn xz_decodes_python_files() {
    for (name, comp, raw) in cases("xz", "xz") {
        let got = xz::decompress(&comp, &Limits::default(), &NoProgress)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got == raw, "{name}");
    }
}

#[test]
fn lzma_alone_decodes_python_files() {
    for (name, comp, raw) in cases("lzma", "lzma") {
        let got = lzma::decode_alone(&comp, &Limits::default(), &NoProgress)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got == raw, "{name}");
    }
}

#[test]
fn xz_output_is_read_by_the_xz_tool() {
    for (name, _, raw) in cases("xz", "xz") {
        for level in [0, 3, 6, 9] {
            let z = xz::compress(&raw, level, xz::Check::Crc64);
            if let Some(back) = tool("xz", &["-dc"], &z) {
                assert!(back == raw, "{name} level {level}");
            }
        }
    }
}

#[test]
fn lzma_alone_output_is_read_by_the_xz_tool() {
    for (name, _, raw) in cases("lzma", "lzma") {
        let z = lzma::encode_alone(&raw, lzma::Level(5));
        if let Some(back) = tool("xz", &["--format=lzma", "-dc"], &z) {
            assert!(back == raw, "{name}");
        }
    }
}

#[test]
fn xz_size_against_the_reference() {
    for pick in ["prose", "tabular", "text"] {
        let (_, _, raw) = cases("xz", "xz")
            .into_iter()
            .find(|c| c.0.starts_with(pick))
            .unwrap();
        for (level, flag) in [(1u32, "-1"), (6, "-6")] {
            let ours = xz::compress(&raw, level, xz::Check::Crc64).len();
            if let Some(theirs) = tool("xz", &[flag, "-c"], &raw) {
                println!("{pick} level {level}: ours {ours} xz {}", theirs.len());
                assert!(
                    ours * 100 <= theirs.len() * 130 + 200,
                    "{pick} level {level}: ours {ours} theirs {}",
                    theirs.len()
                );
            }
        }
    }
}

#[test]
fn zstd_decodes_python_files() {
    for (name, comp, raw) in cases("zstd", "zst") {
        let got = oc_compress::zstd::decompress(&comp, &Limits::default(), &NoProgress)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got == raw, "{name}");
    }
}

#[test]
fn lz4_decodes_python_files() {
    for (name, comp, raw) in cases("lz4", "lz4") {
        let got = oc_compress::lz4::decompress(&comp, &Limits::default(), &NoProgress)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got == raw, "{name}");
    }
}

#[test]
fn compress_z_decodes_files_checked_with_uncompress() {
    for (name, comp, raw) in cases("compress", "Z") {
        let got = oc_compress::compress_z::decompress(&comp, &Limits::default(), &NoProgress)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got == raw, "{name}");
    }
}

/// PPMd7 streams made by pyppmd (the 7-Zip variant): several orders, memory sizes small
/// enough to force model restarts and free-block gluing, and binary data.
#[test]
fn ppmd7_decodes_reference_streams() {
    use sha2::{Digest, Sha256};
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/ppmd");
    let manifest = std::fs::read_to_string(dir.join("manifest.txt")).unwrap();
    for line in manifest.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let (name, order, mem, len, hash) = (
            f[0],
            f[1].parse::<u8>().unwrap(),
            f[2].parse::<u32>().unwrap(),
            f[3].parse::<usize>().unwrap(),
            f[4],
        );
        let data = std::fs::read(dir.join(format!("{name}.ppmd"))).unwrap();
        let mut props = vec![order];
        props.extend_from_slice(&mem.to_le_bytes());
        eprintln!("case {name}");
        let out = oc_compress::ppmd7::decode(&props, &data, len, &Limits::default(), &NoProgress)
            .unwrap();
        let got: String = Sha256::digest(&out)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(got, hash, "{name}");
        // A damaged stream must give an error or different bytes, never a panic.
        let mut bad = data.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0x5a;
        if let Ok(o) =
            oc_compress::ppmd7::decode(&props, &bad, len, &Limits::default(), &NoProgress)
        {
            assert_ne!(o, out);
        }
        let _ = oc_compress::ppmd7::decode(
            &props,
            &data[..data.len() / 2],
            len,
            &Limits::default(),
            &NoProgress,
        );
    }
}

#[test]
fn ppmd7_survives_random_streams() {
    let mut x = 12345u64;
    let mut next = || {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (x >> 33) as u32
    };
    for round in 0..400 {
        let len = 5 + (next() % 3000) as usize;
        let mut data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        data[0] = 0;
        // Mix in runs so the model sees repeated contexts.
        if round % 2 == 0 {
            for i in (10..len).step_by(3) {
                data[i] = data[i - 3];
            }
        }
        let order = 2 + (next() % 31) as u8;
        let mem = 2048u32 << (next() % 6);
        let mut props = vec![order];
        props.extend_from_slice(&mem.to_le_bytes());
        let _ = oc_compress::ppmd7::decode(&props, &data, 20_000, &Limits::default(), &NoProgress);
    }
}
