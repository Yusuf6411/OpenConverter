//! Coding of quantised coefficients (RFC 6386 section 13) and of key-frame sub-block modes,
//! written once and driven by a `Sink` so the same walk can write bits, count them for
//! probability statistics, or add up their cost for rate-distortion decisions.
#![allow(clippy::indexing_slicing)]

use crate::bool::BoolWriter;
use crate::tables::{BANDS, BMODE_PROBS};
use std::sync::OnceLock;

pub type Probs = [[[[u8; 11]; 3]; 8]; 4];

/// Receives the binary decisions of a token walk.
pub trait Sink {
    /// A decision with the adaptive probability at (`t`ype, `b`and, `c`ontext, `node`).
    fn adaptive(&mut self, bit: bool, t: usize, b: usize, c: usize, node: usize);
    /// A decision with a fixed probability of zero.
    fn fixed(&mut self, bit: bool, prob: u8);
}

const CAT3: [u8; 3] = [173, 148, 140];
const CAT4: [u8; 4] = [176, 155, 140, 135];
const CAT5: [u8; 5] = [180, 157, 141, 134, 130];
const CAT6: [u8; 11] = [254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129];

/// Codes one block of levels in zigzag order, starting at scan position `first`, for block
/// type `t` and neighbour context `ctx`. Returns whether any level is non-zero.
pub fn code_block<S: Sink>(
    s: &mut S,
    t: usize,
    ctx: usize,
    first: usize,
    levels: &[i16; 16],
) -> bool {
    let last = (first..16).rev().find(|&n| levels[n] != 0);
    let mut n = first;
    let (mut b, mut c) = (usize::from(BANDS[n]), ctx);
    s.adaptive(last.is_some(), t, b, c, 0);
    let Some(last) = last else { return false };
    while n < 16 {
        let level = levels[n];
        n += 1;
        let v = i32::from(level.unsigned_abs());
        if v == 0 {
            s.adaptive(false, t, b, c, 1);
            b = usize::from(BANDS[n]);
            c = 0;
            continue;
        }
        s.adaptive(true, t, b, c, 1);
        if v == 1 {
            s.adaptive(false, t, b, c, 2);
            c = 1;
        } else {
            s.adaptive(true, t, b, c, 2);
            if v <= 4 {
                s.adaptive(false, t, b, c, 3);
                s.adaptive(v != 2, t, b, c, 4);
                if v != 2 {
                    s.adaptive(v == 4, t, b, c, 5);
                }
            } else {
                s.adaptive(true, t, b, c, 3);
                if v <= 10 {
                    s.adaptive(false, t, b, c, 6);
                    if v <= 6 {
                        s.adaptive(false, t, b, c, 7);
                        s.fixed(v == 6, 159);
                    } else {
                        s.adaptive(true, t, b, c, 7);
                        s.fixed(v >= 9, 165);
                        s.fixed(v & 1 == 0, 145);
                    }
                } else {
                    s.adaptive(true, t, b, c, 6);
                    let (cat, base) = if v < 3 + (8 << 1) {
                        s.adaptive(false, t, b, c, 8);
                        s.adaptive(false, t, b, c, 9);
                        (&CAT3[..], 3 + 8)
                    } else if v < 3 + (8 << 2) {
                        s.adaptive(false, t, b, c, 8);
                        s.adaptive(true, t, b, c, 9);
                        (&CAT4[..], 3 + (8 << 1))
                    } else if v < 3 + (8 << 3) {
                        s.adaptive(true, t, b, c, 8);
                        s.adaptive(false, t, b, c, 10);
                        (&CAT5[..], 3 + (8 << 2))
                    } else {
                        s.adaptive(true, t, b, c, 8);
                        s.adaptive(true, t, b, c, 10);
                        (&CAT6[..], 3 + (8 << 3))
                    };
                    let extra = v - base;
                    for (i, &p) in cat.iter().enumerate() {
                        s.fixed((extra >> (cat.len() - 1 - i)) & 1 != 0, p);
                    }
                }
            }
            c = 2;
        }
        s.fixed(level < 0, 128);
        b = usize::from(BANDS[n]);
        if n == 16 {
            return true;
        }
        s.adaptive(n <= last, t, b, c, 0);
        if n > last {
            return true;
        }
    }
    true
}

/// "Has coefficients" flags of the blocks along one edge of a macroblock, used as context.
#[derive(Clone, Copy, Default)]
pub struct Nz {
    pub y: [bool; 4],
    pub u: [bool; 2],
    pub v: [bool; 2],
    pub dc: bool,
}

/// Codes the luma part of a macroblock predicted as a whole: the Y2 block, then sixteen
/// blocks without their DC.
pub fn walk_i16<S: Sink>(
    s: &mut S,
    top: &mut Nz,
    left: &mut Nz,
    y2: &[i16; 16],
    y: &[[i16; 16]; 16],
) {
    let ctx = usize::from(top.dc) + usize::from(left.dc);
    let nz = code_block(s, 1, ctx, 0, y2);
    top.dc = nz;
    left.dc = nz;
    for by in 0..4 {
        let mut l = left.y[by];
        for bx in 0..4 {
            let ctx = usize::from(l) + usize::from(top.y[bx]);
            l = code_block(s, 0, ctx, 1, &y[by * 4 + bx]);
            top.y[bx] = l;
        }
        left.y[by] = l;
    }
}

/// Codes the luma blocks of a macroblock predicted in 4x4 blocks.
pub fn walk_i4<S: Sink>(s: &mut S, top: &mut Nz, left: &mut Nz, y: &[[i16; 16]; 16]) {
    for by in 0..4 {
        let mut l = left.y[by];
        for bx in 0..4 {
            let ctx = usize::from(l) + usize::from(top.y[bx]);
            l = code_block(s, 3, ctx, 0, &y[by * 4 + bx]);
            top.y[bx] = l;
        }
        left.y[by] = l;
    }
}

/// Codes the eight chroma blocks (four of U, then four of V).
pub fn walk_uv<S: Sink>(s: &mut S, top: &mut Nz, left: &mut Nz, uv: &[[i16; 16]; 8]) {
    for ch in 0..2 {
        let (tn, ln) = if ch == 0 {
            (&mut top.u, &mut left.u)
        } else {
            (&mut top.v, &mut left.v)
        };
        for by in 0..2 {
            let mut l = ln[by];
            for bx in 0..2 {
                let ctx = usize::from(l) + usize::from(tn[bx]);
                l = code_block(s, 2, ctx, 0, &uv[ch * 4 + by * 2 + bx]);
                tn[bx] = l;
            }
            ln[by] = l;
        }
    }
}

/// Counts decisions per probability, for choosing the probabilities to transmit.
/// Zero and one counts per [type][band][context][node].
pub type Counts = [[[[[u32; 2]; 11]; 3]; 8]; 4];

pub struct Stats {
    pub counts: Box<Counts>,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            counts: Box::new([[[[[0; 2]; 11]; 3]; 8]; 4]),
        }
    }
}

impl Sink for Stats {
    fn adaptive(&mut self, bit: bool, t: usize, b: usize, c: usize, node: usize) {
        self.counts[t][b][c][node][usize::from(bit)] += 1;
    }
    fn fixed(&mut self, _bit: bool, _prob: u8) {}
}

/// Cost in 1/256 bit of coding a zero with probability `p`/256, indexed by `p`.
pub fn cost_table() -> &'static [u16; 257] {
    static T: OnceLock<[u16; 257]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u16; 257];
        for (p, e) in t.iter_mut().enumerate().skip(1) {
            *e = (-(p as f64 / 256.0).log2() * 256.0).round() as u16;
        }
        t[0] = t[1];
        t
    })
}

/// Adds up the cost of a walk, in 1/256 bit.
pub struct Cost<'a> {
    pub probs: &'a Probs,
    pub total: u32,
    table: &'static [u16; 257],
}

impl<'a> Cost<'a> {
    pub fn new(probs: &'a Probs) -> Self {
        Self {
            probs,
            total: 0,
            table: cost_table(),
        }
    }

    pub fn bit(&mut self, bit: bool, prob: u8) {
        let p = usize::from(prob);
        self.total += u32::from(self.table[if bit { 256 - p } else { p }]);
    }
}

impl Sink for Cost<'_> {
    fn adaptive(&mut self, bit: bool, t: usize, b: usize, c: usize, node: usize) {
        self.bit(bit, self.probs[t][b][c][node]);
    }
    fn fixed(&mut self, bit: bool, prob: u8) {
        self.bit(bit, prob);
    }
}

/// Writes a walk to a boolean coder.
pub struct Writer<'a> {
    pub bw: &'a mut BoolWriter,
    pub probs: &'a Probs,
}

impl Sink for Writer<'_> {
    fn adaptive(&mut self, bit: bool, t: usize, b: usize, c: usize, node: usize) {
        self.bw.put(bit, self.probs[t][b][c][node]);
    }
    fn fixed(&mut self, bit: bool, prob: u8) {
        self.bw.put(bit, prob);
    }
}

/// The sub-block mode tree of key frames, as in the decoder.
pub const BMODE_TREE: [i8; 18] = [
    0, 1, -1, 2, -2, 3, 4, 6, -3, 5, -4, -5, -6, 7, -7, 8, -8, -9,
];

/// For each sub-block mode, the (probability index, bit) decisions that lead to it.
pub fn bmode_paths() -> &'static [Vec<(usize, bool)>; 10] {
    static P: OnceLock<[Vec<(usize, bool)>; 10]> = OnceLock::new();
    P.get_or_init(|| {
        fn walk(node: usize, path: &mut Vec<(usize, bool)>, out: &mut [Vec<(usize, bool)>; 10]) {
            for bit in [false, true] {
                path.push((node, bit));
                let child = BMODE_TREE[2 * node + usize::from(bit)];
                if child > 0 {
                    walk(child as usize, path, out);
                } else {
                    out[(-child) as usize] = path.clone();
                }
                path.pop();
            }
        }
        let mut out: [Vec<(usize, bool)>; 10] = Default::default();
        walk(0, &mut Vec::new(), &mut out);
        out
    })
}

/// Cost in 1/256 bit of sub-block `mode` given the modes above and to the left.
pub fn bmode_cost(above: u8, left: u8, mode: u8) -> u32 {
    let probs = &BMODE_PROBS[usize::from(above)][usize::from(left)];
    let t = cost_table();
    bmode_paths()[usize::from(mode)]
        .iter()
        .map(|&(i, bit)| {
            let p = usize::from(probs[i]);
            u32::from(t[if bit { 256 - p } else { p }])
        })
        .sum()
}

pub fn put_bmode(bw: &mut BoolWriter, above: u8, left: u8, mode: u8) {
    let probs = &BMODE_PROBS[usize::from(above)][usize::from(left)];
    for &(i, bit) in &bmode_paths()[usize::from(mode)] {
        bw.put(bit, probs[i]);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::bool::BoolReader;
    use crate::tables::COEFF_PROBS;

    #[test]
    fn every_mode_has_a_path_that_reads_back() {
        let paths = bmode_paths();
        for (mode, path) in paths.iter().enumerate() {
            assert!(!path.is_empty(), "mode {mode}");
            let mut bw = BoolWriter::new();
            put_bmode(&mut bw, 3, 7, mode as u8);
            let bytes = bw.finish();
            let probs = &BMODE_PROBS[3][7];
            let mut br = BoolReader::new(&bytes);
            let mut i = BMODE_TREE[usize::from(br.get(probs[0]))];
            while i > 0 {
                i = BMODE_TREE[2 * i as usize + usize::from(br.get(probs[i as usize]))];
            }
            assert_eq!(-i, mode as i8);
        }
    }

    #[test]
    fn cost_is_roughly_one_bit_for_even_odds() {
        let t = cost_table();
        assert!((250..=262).contains(&t[128]));
        assert!(t[255] < 3);
    }

    #[test]
    fn stats_and_cost_see_the_same_decisions() {
        let levels: [i16; 16] = [3, -1, 0, 0, 7, 0, 0, 0, -12, 0, 0, 0, 0, 0, 0, 2];
        let mut stats = Stats::default();
        assert!(code_block(&mut stats, 3, 1, 0, &levels));
        let total: u32 = stats
            .counts
            .iter()
            .flatten()
            .flatten()
            .flatten()
            .flatten()
            .sum();
        assert!(total > 20);
        let mut cost = Cost::new(&COEFF_PROBS);
        code_block(&mut cost, 3, 1, 0, &levels);
        assert!(cost.total > 0);
    }
}
