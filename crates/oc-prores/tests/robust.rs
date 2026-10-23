//! Damaged input must give an error or a frame, never a panic or a runaway allocation.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;

fn first_frame(name: &str) -> Vec<u8> {
    let p = format!("{}/tests/data/{name}.prores", env!("CARGO_MANIFEST_DIR"));
    let d = std::fs::read(p).unwrap();
    let n = u32::from_be_bytes(d[..4].try_into().unwrap()) as usize;
    d[..n].to_vec()
}

#[test]
fn truncation_never_panics() {
    for name in ["prof2", "alpha", "il_tff"] {
        let f = first_frame(name);
        for cut in (0..f.len()).step_by(7) {
            let _ = oc_prores::decode(&f[..cut], &Limits::default());
        }
    }
}

#[test]
fn bit_flips_never_panic() {
    for name in ["prof1", "yuv444", "alpha", "il_bff"] {
        let f = first_frame(name);
        let mut seed = 0x9E37_79B9u32;
        for _ in 0..600 {
            let mut g = f.clone();
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let i = (seed >> 8) as usize % g.len();
            g[i] ^= 1 << (seed & 7);
            let _ = oc_prores::decode(&g, &Limits::default());
        }
    }
}

#[test]
fn huge_dimensions_are_refused() {
    let mut f = first_frame("prof0");
    f[16..18].copy_from_slice(&[0xFF, 0xFF]);
    f[18..20].copy_from_slice(&[0xFF, 0xFF]);
    let tight = Limits {
        max_pixels: 1 << 20,
        ..Limits::default()
    };
    assert!(oc_prores::decode(&f, &tight).is_err());
}
