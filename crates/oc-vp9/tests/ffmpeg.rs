//! Streams encoded by libvpx (through ffmpeg) and decoded by ffmpeg's VP9 decoder, which is
//! bit exact with libvpx. Every frame of ours must hash the same. When the matching `.yuv` is
//! present the first differing sample is reported too.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_vp9::{Decoder, Picture};

fn ivf_packets(data: &[u8]) -> Vec<&[u8]> {
    let hdr = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut at = hdr;
    let mut out = Vec::new();
    while at + 12 <= data.len() {
        let n = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        out.push(&data[at + 12..at + 12 + n]);
        at += 12 + n;
    }
    out
}

fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn bytes_of(p: &Picture) -> Vec<u8> {
    let mut v = Vec::new();
    for plane in &p.planes {
        for &s in plane {
            if p.bit_depth == 8 {
                v.push(s as u8);
            } else {
                v.extend_from_slice(&s.to_le_bytes());
            }
        }
    }
    v
}

fn check(name: &str) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");
    let ivf = std::fs::read(format!("{dir}{name}.ivf")).unwrap();
    let want: Vec<u64> = std::fs::read_to_string(format!("{dir}{name}.fnv"))
        .unwrap()
        .lines()
        .map(|l| u64::from_str_radix(l, 16).unwrap())
        .collect();
    let yuv = std::fs::read(format!("{dir}{name}.yuv")).ok();
    let mut dec = Decoder::default();
    let mut shown = Vec::new();
    for pkt in ivf_packets(&ivf) {
        shown.extend(dec.decode(pkt).unwrap());
    }
    assert_eq!(shown.len(), want.len(), "{name}: number of shown frames");
    let mut off = 0;
    for (i, pic) in shown.iter().enumerate() {
        let got = bytes_of(pic);
        if let Some(y) = &yuv {
            let r = &y[off..off + got.len()];
            if let Some(k) = got.iter().zip(r).position(|(a, b)| a != b) {
                let sample = if pic.bit_depth == 8 { 1 } else { 2 };
                let luma = pic.width * pic.height * sample;
                let (plane, rel) = if k < luma { (0, k) } else { (1, k - luma) };
                panic!(
                    "{name}: frame {i} differs first at byte {k} (plane {plane}, sample {}): {} vs {}",
                    rel / sample,
                    got[k],
                    r[k]
                );
            }
        }
        off += got.len();
        assert_eq!(fnv(got.into_iter()), want[i], "{name}: frame {i}");
    }
}

macro_rules! case {
    ($($n:ident),*) => { $( #[test] fn $n() { check(stringify!($n)); } )* };
}

case!(
    resize, kf, kf_odd, lossless, inter, inter_odd, altref, tiles, p1_444, p1_422, p2_10, seg,
    errres, parallel
);
