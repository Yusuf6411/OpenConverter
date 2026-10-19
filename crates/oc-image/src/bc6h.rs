//! BC6H (DXGI 95 and 96): HDR colour in 4x4 blocks of 16 bytes, fourteen modes. Endpoints
//! are stored at 6 to 16 bits, mostly as deltas from the first one, and interpolated in
//! integers; the result is half-float bit patterns.
#![allow(clippy::indexing_slicing)] // indices come from small tables and 2..5 bit fields

use crate::bc6h_layout::LAYOUT;
use crate::bc7_tables::{ANCHOR2, PARTITION2};

struct Mode {
    transformed: bool,
    partitioned: bool,
    bits: u32,
    delta: [u32; 3],
}

/// The 5 bit selector (or 2 bit one, for the first two modes) names the mode.
fn mode_info(sel: u32) -> Option<Mode> {
    let m = |transformed, partitioned, bits, delta| {
        Some(Mode {
            transformed,
            partitioned,
            bits,
            delta,
        })
    };
    match sel {
        0 => m(true, true, 10, [5, 5, 5]),
        1 => m(true, true, 7, [6, 6, 6]),
        2 => m(true, true, 11, [5, 4, 4]),
        3 => m(false, false, 10, [10, 10, 10]),
        6 => m(true, true, 11, [4, 5, 4]),
        7 => m(true, false, 11, [9, 9, 9]),
        10 => m(true, true, 11, [4, 4, 5]),
        11 => m(true, false, 12, [8, 8, 8]),
        14 => m(true, true, 9, [5, 5, 5]),
        15 => m(true, false, 16, [4, 4, 4]),
        18 => m(true, true, 8, [6, 5, 5]),
        22 => m(true, true, 8, [5, 6, 5]),
        26 => m(true, true, 8, [5, 5, 6]),
        30 => m(false, true, 6, [6, 6, 6]),
        _ => None,
    }
}

const WEIGHTS3: [i32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
const WEIGHTS4: [i32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

fn sign_extend(v: i32, bits: u32) -> i32 {
    let m = 1i32 << (bits - 1);
    (v ^ m) - m
}

fn unquantize(v: i32, signed: bool, bits: u32) -> i32 {
    if bits >= 16 || (!signed && bits >= 15) {
        return v;
    }
    if signed {
        let a = v.abs();
        let u = if a == 0 {
            0
        } else if a >= (1 << (bits - 1)) - 1 {
            0x7FFF
        } else {
            ((a << 15) + 0x4000) >> (bits - 1)
        };
        if v < 0 {
            -u
        } else {
            u
        }
    } else if v == 0 {
        0
    } else if v >= (1 << bits) - 1 {
        0xFFFF
    } else {
        ((v << 15) + 0x4000) >> (bits - 1)
    }
}

/// Half-float bit pattern for an interpolated endpoint value.
fn finish(v: i32, signed: bool) -> u16 {
    if signed {
        let m = ((v.unsigned_abs() * 31) >> 5) as u16;
        if v < 0 && m != 0 {
            0x8000 | m
        } else {
            m
        }
    } else {
        ((v.unsigned_abs() * 31) >> 6) as u16
    }
}

pub(crate) fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let e = i32::from((h >> 10) & 31);
    let m = f32::from(h & 0x3FF);
    sign * match e {
        0 => m * 2f32.powi(-24),
        31 => {
            if m == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Half-float bit patterns of the 16 texels of a block, row by row. Reserved modes give
/// black, as in Direct3D.
pub(crate) fn decode_block_bits(block: &[u8], signed: bool) -> [[u16; 3]; 16] {
    let mut raw = [0u8; 16];
    for (r, b) in raw.iter_mut().zip(block) {
        *r = *b;
    }
    let mut bits = u128::from_le_bytes(raw);
    let mut take = |n: u32| -> i32 {
        let v = (bits & ((1u128 << n) - 1)) as i32;
        bits >>= n;
        v
    };
    let mut sel = take(2) as u32;
    if sel & 2 != 0 {
        sel |= (take(3) as u32) << 2;
    }
    let Some(mode) = mode_info(sel) else {
        return [[0; 3]; 16];
    };
    let Some((_, fields)) = LAYOUT.iter().find(|(m, _)| u32::from(*m) == sel) else {
        return [[0; 3]; 16];
    };
    let mut ep = [[0i32; 4]; 3];
    for &(c, e, n, shift) in *fields {
        ep[usize::from(c)][usize::from(e)] |= take(u32::from(n)) << shift;
    }
    let count = if mode.partitioned { 4 } else { 2 };
    let mask = (1i32 << mode.bits) - 1;
    for (c, e) in ep.iter_mut().enumerate() {
        if signed {
            e[0] = sign_extend(e[0], mode.bits);
        }
        for i in 1..count {
            if signed || mode.transformed {
                e[i] = sign_extend(e[i], mode.delta[c]);
            }
            if mode.transformed {
                e[i] = (e[i] + e[0]) & mask;
                if signed {
                    e[i] = sign_extend(e[i], mode.bits);
                }
            }
        }
        for v in e.iter_mut().take(count) {
            *v = unquantize(*v, signed, mode.bits);
        }
    }
    let part = if mode.partitioned {
        take(5) as usize
    } else {
        0
    };
    let (index_bits, weights): (u32, &[i32]) = if mode.partitioned {
        (3, &WEIGHTS3)
    } else {
        (4, &WEIGHTS4)
    };
    let mut out = [[0u16; 3]; 16];
    for (i, px) in out.iter_mut().enumerate() {
        let (subset, anchor) = if mode.partitioned {
            let s = usize::from(PARTITION2[part][i]);
            (
                s,
                if s == 1 {
                    usize::from(ANCHOR2[part])
                } else {
                    0
                },
            )
        } else {
            (0, 0)
        };
        let n = index_bits - u32::from(i == anchor);
        let w = weights.get(take(n) as usize).copied().unwrap_or(64);
        for (c, o) in px.iter_mut().enumerate() {
            let v = (ep[c][2 * subset] * (64 - w) + ep[c][2 * subset + 1] * w + 32) >> 6;
            *o = finish(v, signed);
        }
    }
    out
}

/// Linear RGB of the 16 texels of a block.
pub(crate) fn decode_block(block: &[u8], signed: bool) -> [[f32; 3]; 16] {
    decode_block_bits(block, signed).map(|p| p.map(half_to_f32))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Fixtures from tools/make_bc6h_fixtures.py: random blocks of every mode and the
    /// half-floats bcdec decodes from them.
    #[test]
    fn matches_bcdec() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/bc6h");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let Some(stem) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(stem) = stem.strip_suffix(".blocks") else {
                continue;
            };
            let signed = stem.starts_with("sf16");
            let blocks = std::fs::read(&p).unwrap();
            let want = std::fs::read(p.with_file_name(format!("{stem}.half"))).unwrap();
            let mut bad = 0;
            for (b, blk) in blocks.chunks(16).enumerate() {
                let got = decode_block_bits(blk, signed);
                for (i, px) in got.iter().enumerate() {
                    for (c, &h) in px.iter().enumerate() {
                        let at = ((b * 16 + i) * 3 + c) * 2;
                        let exp = u16::from_le_bytes([want[at], want[at + 1]]);
                        if h != exp {
                            bad += 1;
                            if bad < 4 {
                                eprintln!("{stem} block {b} px {i} c {c}: {h:04x} vs {exp:04x}");
                            }
                        }
                    }
                }
            }
            assert_eq!(bad, 0, "{stem}");
            n += 1;
        }
        assert!(n >= 30, "found {n} fixtures");
    }

    #[test]
    fn reserved_modes_are_black() {
        // Selector 0b00100 (mode 4 in the 5 bit space) is reserved.
        let mut b = [0u8; 16];
        b[0] = 0b10011;
        assert!(decode_block(&b, false).iter().all(|p| *p == [0.0; 3]));
    }
}
