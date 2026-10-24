//! BC7 block decoding (Direct3D 11 texture format, also `DXGI_FORMAT_BC7_*` in DDS).
//!
//! The partition shapes and anchor positions in `bc7_tables.rs` were read back from an
//! independent decoder by `tools/gen_bc7_tables.py`.
// Mode parameters and table lookups use small fixed indices.
#![allow(clippy::indexing_slicing, clippy::needless_range_loop)]
use crate::bc7_tables::{ANCHOR2, ANCHOR3_1, ANCHOR3_2, PARTITION2, PARTITION3};

struct Bits {
    v: u128,
    at: u32,
}

impl Bits {
    fn take(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let r = ((self.v >> self.at) & ((1u128 << n) - 1)) as u32;
        self.at += n;
        r
    }
}

const W2: [u32; 4] = [0, 21, 43, 64];
const W3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
const W4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

fn weight(bits: u32, i: u32) -> u32 {
    match bits {
        2 => W2[i as usize],
        3 => W3[i as usize],
        _ => W4[i as usize],
    }
}

struct Mode {
    subsets: u32,
    partition_bits: u32,
    rotation_bits: u32,
    index_mode_bits: u32,
    /// Colour and alpha endpoint precision before the p-bit.
    color_bits: u32,
    alpha_bits: u32,
    /// 0 none, 1 per endpoint, 2 per subset.
    pbits: u32,
    index_bits: u32,
    alpha_index_bits: u32,
}

const MODES: [Mode; 8] = [
    Mode {
        subsets: 3,
        partition_bits: 4,
        rotation_bits: 0,
        index_mode_bits: 0,
        color_bits: 4,
        alpha_bits: 0,
        pbits: 1,
        index_bits: 3,
        alpha_index_bits: 0,
    },
    Mode {
        subsets: 2,
        partition_bits: 6,
        rotation_bits: 0,
        index_mode_bits: 0,
        color_bits: 6,
        alpha_bits: 0,
        pbits: 2,
        index_bits: 3,
        alpha_index_bits: 0,
    },
    Mode {
        subsets: 3,
        partition_bits: 6,
        rotation_bits: 0,
        index_mode_bits: 0,
        color_bits: 5,
        alpha_bits: 0,
        pbits: 0,
        index_bits: 2,
        alpha_index_bits: 0,
    },
    Mode {
        subsets: 2,
        partition_bits: 6,
        rotation_bits: 0,
        index_mode_bits: 0,
        color_bits: 7,
        alpha_bits: 0,
        pbits: 1,
        index_bits: 2,
        alpha_index_bits: 0,
    },
    Mode {
        subsets: 1,
        partition_bits: 0,
        rotation_bits: 2,
        index_mode_bits: 1,
        color_bits: 5,
        alpha_bits: 6,
        pbits: 0,
        index_bits: 2,
        alpha_index_bits: 3,
    },
    Mode {
        subsets: 1,
        partition_bits: 0,
        rotation_bits: 2,
        index_mode_bits: 0,
        color_bits: 7,
        alpha_bits: 8,
        pbits: 0,
        index_bits: 2,
        alpha_index_bits: 2,
    },
    Mode {
        subsets: 1,
        partition_bits: 0,
        rotation_bits: 0,
        index_mode_bits: 0,
        color_bits: 7,
        alpha_bits: 7,
        pbits: 1,
        index_bits: 4,
        alpha_index_bits: 0,
    },
    Mode {
        subsets: 2,
        partition_bits: 6,
        rotation_bits: 0,
        index_mode_bits: 0,
        color_bits: 5,
        alpha_bits: 5,
        pbits: 1,
        index_bits: 2,
        alpha_index_bits: 0,
    },
];

/// Expands an `n`-bit value to 8 bits.
fn expand(v: u32, n: u32) -> u32 {
    if n >= 8 {
        return v;
    }
    (v << (8 - n)) | (v >> (2 * n - 8))
}

/// Decodes one 16-byte block to 16 RGBA pixels in raster order. Reserved mode bits (a block
/// with no mode) decode to transparent black, as the specification requires.
pub fn decode_block(block: &[u8]) -> [[u8; 4]; 16] {
    let mut raw = [0u8; 16];
    raw.copy_from_slice(block.get(..16).unwrap_or(&[0; 16]));
    let mut b = Bits {
        v: u128::from_le_bytes(raw),
        at: 0,
    };
    let mode_no = (b.v.trailing_zeros()).min(8);
    if mode_no >= 8 {
        return [[0; 4]; 16];
    }
    b.at = mode_no + 1;
    let m = &MODES[mode_no as usize];
    let partition = b.take(m.partition_bits) as usize;
    let rotation = b.take(m.rotation_bits);
    let index_mode = b.take(m.index_mode_bits);
    let n_ends = (m.subsets * 2) as usize;
    let mut ends = [[0u32; 4]; 6];
    for c in 0..3 {
        for e in ends.iter_mut().take(n_ends) {
            e[c] = b.take(m.color_bits);
        }
    }
    if m.alpha_bits > 0 {
        for e in ends.iter_mut().take(n_ends) {
            e[3] = b.take(m.alpha_bits);
        }
    }
    // P-bits extend the precision of every endpoint they cover by one bit.
    let (cbits, abits) = if m.pbits > 0 {
        (
            m.color_bits + 1,
            if m.alpha_bits > 0 {
                m.alpha_bits + 1
            } else {
                0
            },
        )
    } else {
        (m.color_bits, m.alpha_bits)
    };
    if m.pbits == 1 {
        for e in ends.iter_mut().take(n_ends) {
            let p = b.take(1);
            for c in 0..4 {
                if c < 3 || m.alpha_bits > 0 {
                    e[c] = (e[c] << 1) | p;
                }
            }
        }
    } else if m.pbits == 2 {
        for s in 0..m.subsets as usize {
            let p = b.take(1);
            for e in &mut ends[2 * s..2 * s + 2] {
                for c in 0..3 {
                    e[c] = (e[c] << 1) | p;
                }
            }
        }
    }
    let mut rgba = [[0u32; 4]; 6];
    for (o, e) in rgba.iter_mut().zip(&ends).take(n_ends) {
        for c in 0..3 {
            o[c] = expand(e[c], cbits);
        }
        o[3] = if m.alpha_bits > 0 {
            expand(e[3], abits)
        } else {
            255
        };
    }

    let subset_of = |px: usize| -> usize {
        match m.subsets {
            1 => 0,
            2 => usize::from(PARTITION2[partition][px]),
            _ => usize::from(PARTITION3[partition][px]),
        }
    };
    let is_anchor = |px: usize| -> bool {
        if px == 0 {
            return true;
        }
        match m.subsets {
            2 => px == usize::from(ANCHOR2[partition]) && subset_of(px) == 1,
            3 => {
                (px == usize::from(ANCHOR3_1[partition]) && subset_of(px) == 1)
                    || (px == usize::from(ANCHOR3_2[partition]) && subset_of(px) == 2)
            }
            _ => false,
        }
    };
    // The first index field is always `index_bits` wide; modes 4 and 5 follow it with a
    // second field of `alpha_index_bits`. In mode 4 the index-mode bit says which of the
    // two drives the colour channels and which the alpha channel.
    let mut first = [0u32; 16];
    for (px, i) in first.iter_mut().enumerate() {
        *i = b.take(if is_anchor(px) {
            m.index_bits - 1
        } else {
            m.index_bits
        });
    }
    let mut second = first;
    if m.alpha_index_bits > 0 {
        for (px, i) in second.iter_mut().enumerate() {
            *i = b.take(if px == 0 {
                m.alpha_index_bits - 1
            } else {
                m.alpha_index_bits
            });
        }
    }
    let (cidx, cbits_idx, aidx, abits_idx) = if m.alpha_index_bits == 0 {
        (first, m.index_bits, first, m.index_bits)
    } else if index_mode == 1 {
        (second, m.alpha_index_bits, first, m.index_bits)
    } else {
        (first, m.index_bits, second, m.alpha_index_bits)
    };

    let mut out = [[0u8; 4]; 16];
    for px in 0..16 {
        let s = subset_of(px);
        let (e0, e1) = (&rgba[2 * s], &rgba[2 * s + 1]);
        let wc = weight(cbits_idx, cidx[px]);
        let wa = weight(abits_idx, aidx[px]);
        let mut v = [0u32; 4];
        for c in 0..3 {
            v[c] = (e0[c] * (64 - wc) + e1[c] * wc + 32) >> 6;
        }
        v[3] = (e0[3] * (64 - wa) + e1[3] * wa + 32) >> 6;
        if rotation != 0 {
            v.swap(3, rotation as usize - 1);
        }
        out[px] = [v[0] as u8, v[1] as u8, v[2] as u8, v[3] as u8];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_mode_is_transparent_black() {
        assert_eq!(decode_block(&[0; 16]), [[0; 4]; 16]);
    }
}
