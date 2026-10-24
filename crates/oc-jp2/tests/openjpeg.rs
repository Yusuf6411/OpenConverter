//! Files written by OpenJPEG (through glymur); the expected pixels are OpenJPEG's own decode of the
//! same files. Lossless files must match exactly; the 9/7 file may differ by one level.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;
use std::path::PathBuf;

fn read(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn decode_bytes(name: &str) -> (oc_jp2::Picture, Vec<u8>) {
    let p = oc_jp2::decode(&read(&format!("{name}.jp2")), &Limits::default()).unwrap();
    let mut b = Vec::new();
    for &v in &p.samples {
        if p.sixteen {
            b.extend_from_slice(&v.to_be_bytes());
        } else {
            b.push(v as u8);
        }
    }
    (p, b)
}

fn exact(name: &str, channels: usize) {
    let (p, got) = decode_bytes(name);
    assert_eq!(p.channels, channels, "{name}");
    assert!(
        got == read(&format!("{name}.ref")),
        "{name} differs from OpenJPEG"
    );
}

#[test]
fn lossless_rgb() {
    exact("rgb_lossless", 3);
}
#[test]
fn every_code_block_style() {
    exact("styles_all", 3);
    exact("bypass", 3);
    exact("termall_vsc", 3);
}
#[test]
fn tiles_layers_and_markers() {
    exact("tiles_layers_rpcl", 3);
}
#[test]
fn precincts_and_progressions() {
    exact("precincts_pcrl", 3);
    exact("cprl_cb", 3);
}
#[test]
fn grey_and_alpha() {
    exact("gray8", 1);
    exact("rgba", 4);
    let (p, _) = decode_bytes("gray16");
    assert!(p.sixteen);
    exact("gray16", 1);
}
#[test]
fn irreversible_is_within_one_level() {
    let (_, got) = decode_bytes("irreversible");
    let want = read("irreversible.ref");
    assert_eq!(got.len(), want.len());
    assert!(got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= 1));
}

#[test]
fn damaged_files_do_not_panic() {
    let n: usize = std::env::var("JP2_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    for name in [
        "tiles_layers_rpcl",
        "styles_all",
        "precincts_pcrl",
        "irreversible",
    ] {
        damage(&read(&format!("{name}.jp2")), n);
    }
}

fn damage(data: &[u8], n: usize) {
    let mut seed = 77u32;
    for i in 0..n {
        let mut d = data.to_vec();
        if i % 4 == 0 {
            d.truncate(1 + (i * 37) % data.len());
        } else {
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = (seed >> 8) as usize % d.len();
                d[at] ^= 1 << (seed & 7);
            }
        }
        let _ = oc_jp2::decode(&d, &Limits::default());
    }
}
