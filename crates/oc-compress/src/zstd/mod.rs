//! Zstandard (RFC 8878).
mod dec;
mod enc;
mod fse;
mod huf;

pub use dec::decompress;
pub use enc::compress;

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use oc_core::{Limits, NoProgress};

    fn words(n: usize) -> Vec<u8> {
        let mut s = 5u32;
        let mut r = move || {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (s >> 16) as usize
        };
        let ws: Vec<Vec<u8>> = (0..60).map(|_| (0..2 + r() % 8).map(|_| b'a' + (r() % 8) as u8).collect()).collect();
        let mut out = Vec::new();
        while out.len() < n {
            out.extend_from_slice(&ws[r() % 60]);
            out.push(b' ');
        }
        out.truncate(n);
        out
    }

    #[test]
    fn roundtrip_through_our_decoder() {
        for n in [0usize, 1, 100, 255, 256, 500, 3000, 100_000, 300_000] {
            let d = words(n);
            for level in [1, 3, 9, 19] {
                let z = compress(&d, level, n % 2 == 0);
                let back = decompress(&z, &Limits::default(), &NoProgress).unwrap_or_else(|e| panic!("n {n} level {level}: {e}"));
                assert!(back == d, "n {n} level {level}");
            }
        }
    }
}
