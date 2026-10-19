//! Variable-width LZW as used by GIF (LSB-first) and TIFF (MSB-first, early change).
#![allow(clippy::indexing_slicing)] // table indices are checked against `next` before use

use oc_core::{Error, Limits, Result};
use oc_io::{BitOrder, BitReader, BitWriter};
use std::collections::HashMap;

const MAX_BITS: u32 = 12;
const TABLE_SIZE: usize = 1 << MAX_BITS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub order: BitOrder,
    /// Code width in bits right after a clear code (GIF: min code size + 1; TIFF: 9).
    pub start_bits: u32,
    /// TIFF's "early change": widen the codes one entry sooner.
    pub early_change: bool,
}

impl Config {
    /// GIF with the given minimum code size (2..=8).
    pub fn gif(min_code_size: u32) -> Self {
        Self {
            order: BitOrder::LsbFirst,
            start_bits: min_code_size + 1,
            early_change: false,
        }
    }
    pub fn tiff() -> Self {
        Self {
            order: BitOrder::MsbFirst,
            start_bits: 9,
            early_change: true,
        }
    }
    fn clear(&self) -> u16 {
        1 << (self.start_bits - 1)
    }
    fn eoi(&self) -> u16 {
        self.clear() + 1
    }
}

fn check(cfg: &Config) -> Result<()> {
    if !(3..=9).contains(&cfg.start_bits) {
        return Err(Error::invalid_option(
            "start_bits",
            "LZW start width must be 3..=9",
        ));
    }
    Ok(())
}

/// Decode an LZW stream until the end-of-information code or end of input.
pub fn decode(input: &[u8], cfg: Config, limits: &Limits) -> Result<Vec<u8>> {
    check(&cfg)?;
    let (clear, eoi) = (cfg.clear(), cfg.eoi());
    let early = u32::from(cfg.early_change);
    let mut prefix = vec![0u16; TABLE_SIZE];
    let mut suffix = vec![0u8; TABLE_SIZE];
    let mut length = vec![0u32; TABLE_SIZE];
    let init = |length: &mut Vec<u32>, suffix: &mut Vec<u8>| {
        for i in 0..usize::from(clear) {
            length[i] = 1;
            suffix[i] = i as u8;
        }
    };
    init(&mut length, &mut suffix);
    let mut r = BitReader::new(input, cfg.order);
    let mut out: Vec<u8> = Vec::new();
    let mut size = cfg.start_bits;
    let mut next = u32::from(eoi) + 1;
    let mut prev: Option<u16> = None;
    let expand = |code: u16, out: &mut Vec<u8>, prefix: &[u16], suffix: &[u8], length: &[u32]| {
        let len = length[usize::from(code)] as usize;
        let start = out.len();
        out.resize(start + len, 0);
        let mut c = usize::from(code);
        for k in (0..len).rev() {
            out[start + k] = suffix[c];
            c = usize::from(prefix[c]);
        }
    };
    // A missing EOI is tolerated: it is common in the wild.
    while let Ok(code) = r.read(size) {
        let code = code as u16;
        if code == clear {
            size = cfg.start_bits;
            next = u32::from(eoi) + 1;
            prev = None;
            continue;
        }
        if code == eoi {
            break;
        }
        match prev {
            None => {
                if code >= clear {
                    return Err(
                        Error::corrupt("LZW: first code after clear is not a literal")
                            .at(r.bit_position() / 8),
                    );
                }
                out.push(code as u8);
            }
            Some(p) => {
                let known = u32::from(code) < next;
                if !known && u32::from(code) != next {
                    return Err(Error::corrupt("LZW: code beyond table").at(r.bit_position() / 8));
                }
                let first_of = |c: u16, prefix: &[u16], suffix: &[u8], length: &[u32]| {
                    let mut c = usize::from(c);
                    for _ in 1..length[c] {
                        c = usize::from(prefix[c]);
                    }
                    suffix[c]
                };
                if known {
                    let f = first_of(code, &prefix, &suffix, &length);
                    expand(code, &mut out, &prefix, &suffix, &length);
                    if (next as usize) < TABLE_SIZE {
                        prefix[next as usize] = p;
                        suffix[next as usize] = f;
                        length[next as usize] = length[usize::from(p)] + 1;
                        next += 1;
                    }
                } else {
                    let f = first_of(p, &prefix, &suffix, &length);
                    if (next as usize) >= TABLE_SIZE {
                        return Err(Error::corrupt("LZW: KwKwK with full table"));
                    }
                    prefix[next as usize] = p;
                    suffix[next as usize] = f;
                    length[next as usize] = length[usize::from(p)] + 1;
                    next += 1;
                    expand(code, &mut out, &prefix, &suffix, &length);
                }
                if next + early >= (1 << size) && size < MAX_BITS {
                    size += 1;
                }
            }
        }
        prev = Some(code);
        limits.check_expansion(input.len() as u64, out.len() as u64)?;
    }
    Ok(out)
}

/// Encode `data` (all bytes must be below `1 << (start_bits - 1)`).
pub fn encode(data: &[u8], cfg: Config) -> Result<Vec<u8>> {
    check(&cfg)?;
    let (clear, eoi) = (cfg.clear(), cfg.eoi());
    if data.iter().any(|&b| u16::from(b) >= clear) {
        return Err(Error::invalid_option(
            "start_bits",
            "symbol does not fit the code width",
        ));
    }
    let early = u32::from(cfg.early_change);
    let mut w = BitWriter::new(cfg.order);
    let mut size = cfg.start_bits;
    w.write(u32::from(clear), size);
    let mut dict: HashMap<(u16, u8), u16> = HashMap::new();
    let mut next = u32::from(eoi) + 1;
    let mut cur: Option<u16> = None;
    for &b in data {
        let Some(c) = cur else {
            cur = Some(u16::from(b));
            continue;
        };
        if let Some(&code) = dict.get(&(c, b)) {
            cur = Some(code);
            continue;
        }
        w.write(u32::from(c), size);
        dict.insert((c, b), next as u16);
        next += 1;
        if next + early > (1 << size) && size < MAX_BITS {
            size += 1;
        }
        if next as usize >= TABLE_SIZE {
            w.write(u32::from(clear), size);
            dict.clear();
            size = cfg.start_bits;
            next = u32::from(eoi) + 1;
        }
        cur = Some(u16::from(b));
    }
    if let Some(c) = cur {
        w.write(u32::from(c), size);
        // The decoder adds one more entry on this code and may widen before reading EOI.
        next += 1;
        if next + early > (1 << size) && size < MAX_BITS {
            size += 1;
        }
    }
    w.write(u32::from(eoi), size);
    Ok(w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(n: usize, mut s: u32, modulo: u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((s >> 16) % modulo) as u8
            })
            .collect()
    }

    fn roundtrip(data: &[u8], cfg: Config) {
        let enc = encode(data, cfg).ok().unwrap_or_default();
        let dec = decode(&enc, cfg, &Limits::default()).ok();
        assert_eq!(dec.as_deref(), Some(data), "{cfg:?} len {}", data.len());
    }

    #[test]
    fn roundtrips_across_table_resets_and_widths() {
        for mcs in [2u32, 4, 8] {
            let modulo = 1u32 << mcs;
            for n in [0usize, 1, 2, 100, 5_000, 200_000] {
                roundtrip(&lcg(n, 11, modulo), Config::gif(mcs));
                roundtrip(&vec![0u8; n], Config::gif(mcs));
            }
        }
        for n in [0usize, 1, 3, 1000, 100_000] {
            roundtrip(&lcg(n, 5, 256), Config::tiff());
            roundtrip(&vec![7u8; n], Config::tiff());
        }
    }

    #[test]
    fn corrupt_input_is_an_error_not_a_panic() {
        let cfg = Config::gif(8);
        assert!(decode(&[0xFF, 0xFF, 0xFF, 0xFF], cfg, &Limits::default()).is_err());
        assert!(decode(&[], cfg, &Limits::default()).is_ok());
        assert!(encode(&[200], Config::gif(2)).is_err());
        assert!(decode(
            &[0],
            Config {
                start_bits: 20,
                ..cfg
            },
            &Limits::default()
        )
        .is_err());
    }
}
