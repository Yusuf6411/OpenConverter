//! Streams encoded by libaom, SVT-AV1 (through ffmpeg); the expected pictures are dav1d's own
//! decode of the same files, so every stream must match bit for bit.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use std::path::PathBuf;

fn read(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn check(name: &str) {
    let mut dec = oc_av1::Decoder::new();
    let pics = dec.decode(&read(&format!("{name}.obu"))).unwrap();
    let mut got = Vec::new();
    for p in &pics {
        for pl in &p.planes {
            for &v in pl {
                if p.bit_depth == 8 {
                    got.push(v as u8);
                } else {
                    got.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    assert!(
        got == read(&format!("{name}.yuv")),
        "{name} differs from dav1d"
    );
}

#[test]
fn lossless_420() {
    check("lossless_420");
}
#[test]
fn lossless_444() {
    check("lossless_444");
}
#[test]
fn lossy_with_cdef_and_restoration() {
    check("lossy_420");
}
#[test]
fn ten_bit() {
    check("lossy_10bit");
}
#[test]
fn film_grain() {
    check("grain");
}
#[test]
fn monochrome() {
    check("mono");
}
#[test]
fn svt_av1_hierarchical() {
    check("svt");
}

#[test]
fn damaged_streams_do_not_panic() {
    let n: usize = std::env::var("AV1_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    for name in ["lossy_420", "svt", "grain", "lossless_420"] {
        damage(&read(&format!("{name}.obu")), n);
    }
}

fn damage(data: &[u8], n: usize) {
    let mut seed = 0x1234_5678u32;
    for i in 0..n {
        let mut d = data.to_vec();
        if i % 3 == 0 {
            d.truncate(1 + (i * 13) % data.len());
        } else {
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = (seed >> 8) as usize % d.len();
                d[at] ^= 1 << (seed & 7);
            }
        }
        let _ = oc_av1::Decoder::new().decode(&d);
    }
}
