#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
//! The decoder reads streams made by the reference Brotli encoder (stored in tests/data/brotli,
//! made by the Python `brotli` module at several qualities and font/text/generic modes).
use oc_compress::brotli::{compress, decompress};
use oc_core::{Limits, NoProgress};

fn dec(b: &[u8]) -> oc_core::Result<Vec<u8>> {
    decompress(b, &Limits::default(), &NoProgress)
}

#[test]
fn reference_streams_decode() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/brotli");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "br") {
            let name = p.file_stem().unwrap().to_str().unwrap();
            let base = name.split('.').next().unwrap();
            let want = if base == "zeros" {
                vec![0u8; 70000]
            } else {
                std::fs::read(dir.join(format!("{base}.raw"))).unwrap()
            };
            let got = dec(&std::fs::read(&p).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(got == want, "{name}: output differs");
            n += 1;
        }
    }
    assert!(n >= 40, "only {n} streams found");
}

#[test]
fn own_streams_round_trip() {
    let mut x = 99u32;
    let data: Vec<u8> = (0..50_000)
        .map(|i| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            if i % 7 < 4 {
                b'a' + (i % 5) as u8
            } else {
                (x >> 24) as u8
            }
        })
        .collect();
    for level in [1, 5, 9] {
        assert_eq!(dec(&compress(&data, level)).unwrap(), data);
    }
}

#[test]
fn damaged_streams_fail_cleanly() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/brotli");
    let good = std::fs::read(dir.join("text.q11m0.br")).unwrap();
    assert!(dec(&good[..good.len() / 2]).is_err());
    let mut x = 7u32;
    for _ in 0..300 {
        let mut b = good.clone();
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        let i = (x >> 8) as usize % b.len();
        b[i] ^= 1 << (x % 8);
        let _ = dec(&b); // may succeed or fail, must not panic
    }
}
