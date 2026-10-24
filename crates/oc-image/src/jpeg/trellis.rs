//! Rate-distortion optimised quantisation. For every block the quantised values that cost
//! the fewest bits at a given squared error are found by dynamic programming over the
//! position of the last non-zero coefficient, using the code lengths of the standard
//! Huffman tables as the price of each (run, size) symbol.
#![allow(clippy::indexing_slicing)] // positions are 0..64 by construction

use super::tables::ZIGZAG;

/// Trade-off between bits and squared error, relative to the square of the quantiser's
/// geometric-mean step. Larger values give smaller files and more distortion; 0.04 saves
/// about 10% at equal PSNR on the pictures tried (the theoretical high-rate value, 0.115,
/// overshoots at JPEG's rates).
const LAMBDA: f64 = 0.04;

const UNUSED_LEN: f64 = 24.0;

fn bit_length(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// The Lagrange multiplier for a quantiser: `LAMBDA` times the square of its geometric-mean
/// AC step. Every component of a picture uses the one derived from the luma table, so
/// distortion costs the same everywhere.
pub fn lambda(q: &[u16; 64]) -> f64 {
    let mean_ln = (1..64).map(|k| f64::from(q[k]).ln()).sum::<f64>() / 63.0;
    LAMBDA * mean_ln.exp().powi(2)
}

/// `coef` is the forward DCT scaled by 8, `q` the quantiser in natural order and `ac_len`
/// the code length of each (run << 4 | size) symbol; `lambda` comes from `lambda`. Returns quantised values in natural
/// order.
pub fn quantise(coef: &[i32; 64], q: &[u16; 64], ac_len: &[u8; 256], lambda: f64) -> [i32; 64] {
    let mut out = [0i32; 64];
    // DC is a difference between blocks and is rounded as usual.
    let div0 = i32::from(q[0]) << 3;
    out[0] = if coef[0] < 0 {
        -((-coef[0] + (div0 >> 1)) / div0)
    } else {
        (coef[0] + (div0 >> 1)) / div0
    };

    // Everything below is in zigzag order, in units of the quantiser step.
    let mut x = [0f64; 64];
    let mut step = [0f64; 64];
    for k in 1..64 {
        let nat = ZIGZAG[k];
        step[k] = f64::from(q[nat]);
        x[k] = f64::from(coef[nat].abs()) / (8.0 * step[k]);
    }
    // Cost of leaving coefficients j..k at zero, as distortion (prefix sums).
    let mut zero = [0f64; 65];
    for k in 1..64 {
        let d = step[k] * x[k];
        zero[k + 1] = zero[k] + d * d;
    }
    let zero_between = |from: usize, to: usize| zero[to] - zero[from + 1]; // (from, to) exclusive

    let len = |sym: usize| {
        let l = ac_len[sym];
        if l == 0 {
            UNUSED_LEN
        } else {
            f64::from(l)
        }
    };
    let rate = |run: usize, size: u32| {
        (run / 16) as f64 * len(0xF0) + len(((run % 16) << 4) | size as usize) + f64::from(size)
    };

    // best[k]: lowest cost of coding positions 1..=k with a non-zero value at k.
    let mut best = [f64::INFINITY; 64];
    let mut from = [0usize; 64];
    let mut value = [0i32; 64];
    best[0] = 0.0;
    for k in 1..64 {
        let n = (x[k] + 0.5).floor() as i32;
        if n == 0 {
            continue;
        }
        for v in [n, n - 1] {
            if v == 0 {
                continue;
            }
            let err = step[k] * (x[k] - f64::from(v));
            let dist = err * err;
            let size = bit_length(v as u32);
            for j in 0..k {
                if !best[j].is_finite() {
                    continue;
                }
                let cost = best[j] + rate(k - j - 1, size) + (zero_between(j, k) + dist) / lambda;
                if cost < best[k] {
                    best[k] = cost;
                    from[k] = j;
                    value[k] = v;
                }
            }
        }
    }
    let eob = len(0x00);
    let mut end = 0usize;
    let mut end_cost = f64::INFINITY;
    for j in 0..64 {
        if !best[j].is_finite() {
            continue;
        }
        let cost = best[j] + if j < 63 { eob } else { 0.0 } + (zero[64] - zero[j + 1]) / lambda;
        if cost < end_cost {
            end_cost = cost;
            end = j;
        }
    }
    let mut k = end;
    while k > 0 {
        let nat = ZIGZAG[k];
        out[nat] = if coef[nat] < 0 { -value[k] } else { value[k] };
        k = from[k];
    }
    out
}
