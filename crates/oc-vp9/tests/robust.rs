//! Damaged streams must produce errors (or odd pictures), never panics.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;
use oc_vp9::Decoder;

fn ivf_packets(data: &[u8]) -> Vec<Vec<u8>> {
    let mut at = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut out = Vec::new();
    while at + 12 <= data.len() {
        let n = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        out.push(data[at + 12..at + 12 + n].to_vec());
        at += 12 + n;
    }
    out
}

fn decode_all(pkts: &[Vec<u8>]) {
    let limits = Limits {
        max_pixels: 1 << 22,
        max_alloc: 1 << 28,
        ..Limits::default()
    };
    let mut d = Decoder::new(&limits);
    for p in pkts {
        let _ = d.decode(p);
    }
}

#[test]
fn bit_flips_and_truncation_never_panic() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in [
        "kf_odd",
        "inter_odd",
        "altref",
        "tiles",
        "p1_422",
        "p2_10",
        "seg",
        "resize",
        "lossless",
    ] {
        let pkts = ivf_packets(&std::fs::read(format!("{dir}{name}.ivf")).unwrap());
        for _ in 0..8 {
            let mut m = pkts.clone();
            for _ in 0..1 + next() % 4 {
                let i = (next() as usize) % m.len();
                if m[i].is_empty() {
                    continue;
                }
                let j = (next() as usize) % m[i].len();
                match next() % 3 {
                    0 => m[i][j] ^= 1 << (next() % 8),
                    1 => m[i][j] = next() as u8,
                    _ => m[i].truncate(j),
                }
            }
            decode_all(&m);
        }
    }
}

#[test]
fn garbage_is_rejected() {
    let mut d = Decoder::default();
    assert!(d.decode(&[]).is_err());
    assert!(d.decode(&[0; 40]).is_err());
    assert!(d.decode(&[0x82, 0x49, 0x83, 0x42]).is_err());
    // an inter frame before any key frame has nothing to refer to
    assert!(d.decode(&[0x86, 0x00, 0x00, 0x00, 0x00, 0x00]).is_err());
}
