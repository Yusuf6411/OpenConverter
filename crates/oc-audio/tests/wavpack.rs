//! WavPack streams made by ffmpeg's encoder decode to the samples ffmpeg's decoder produces.
//! `tools/make_wavpack_fixtures.py` writes the streams and the hashes in `MANIFEST`.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_audio::{wavpack, Samples};
use oc_core::Limits;
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/wavpack")
}

fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn cases() -> Vec<(String, u16, u8, usize, u64)> {
    std::fs::read_to_string(dir().join("MANIFEST"))
        .unwrap()
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (
                f[0].to_string(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
                f[3].parse().unwrap(),
                u64::from_str_radix(f[4], 16).unwrap(),
            )
        })
        .collect()
}

#[test]
fn matches_ffmpeg() {
    for (name, channels, bits, frames, hash) in cases() {
        let data = std::fs::read(dir().join(format!("{name}.wv"))).unwrap();
        assert!(wavpack::sniff(&data));
        let (a, _) =
            wavpack::read(&data, &Limits::default()).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!((a.channels, a.frames()), (channels, frames), "{name}");
        let Samples::Int { bits: b, data } = &a.samples else {
            panic!("{name}: not integer")
        };
        assert_eq!(*b, bits, "{name}");
        let shift = 32 - u32::from(*b);
        let got = fnv(data
            .iter()
            .flat_map(|&s| (((s as u32) << shift) as i32).to_le_bytes()));
        assert_eq!(got, hash, "{name}");
    }
}

#[test]
fn damaged_files_are_errors_not_panics() {
    for (name, ..) in cases() {
        let data = std::fs::read(dir().join(format!("{name}.wv"))).unwrap();
        for step in [0usize, 1, 2, 3, 5, 7] {
            let mut d = data.clone();
            let at = (data.len() / 8) * (step + 1) % data.len();
            d[at] ^= 0x5a;
            let _ = wavpack::read(&d, &Limits::default());
            let _ = wavpack::read(&data[..data.len() * (step + 1) / 8], &Limits::default());
        }
    }
    assert!(wavpack::read(b"wvpk", &Limits::default()).is_err());
}
