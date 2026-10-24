//! Intra prediction (RFC 6386 section 12). Prediction works on explicit edge samples, so the
//! caller decides what the borders look like (127 above the frame, 129 to its left).
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use crate::transform::clip8;

/// 16x16 and chroma modes share their numbers with the matching sub-block modes.
pub const DC_PRED: u8 = 0;
pub const TM_PRED: u8 = 1;
pub const V_PRED: u8 = 2;
pub const H_PRED: u8 = 3;
/// Sub-block only modes, in the order of the key-frame probability tables.
pub const B_RD_PRED: u8 = 4;
pub const B_VR_PRED: u8 = 5;
pub const B_LD_PRED: u8 = 6;
pub const B_VL_PRED: u8 = 7;
pub const B_HD_PRED: u8 = 8;
pub const B_HU_PRED: u8 = 9;

/// Predicts an `n` x `n` block (16 for luma, 8 for chroma) into `out`, row-major with
/// stride `n`. `has_top` and `has_left` only matter for DC, the one mode that adapts to
/// frame edges; the other modes read the border values as they are.
pub fn predict_block(
    mode: u8,
    n: usize,
    top: &[u8],
    left: &[u8],
    top_left: u8,
    has_top: bool,
    has_left: bool,
    out: &mut [u8],
) {
    match mode {
        DC_PRED => {
            let log2 = n.trailing_zeros();
            let st: u32 = top[..n].iter().map(|&v| u32::from(v)).sum();
            let sl: u32 = left[..n].iter().map(|&v| u32::from(v)).sum();
            let dc = match (has_top, has_left) {
                (true, true) => (st + sl + n as u32) >> (log2 + 1),
                (true, false) => (st + (n as u32 >> 1)) >> log2,
                (false, true) => (sl + (n as u32 >> 1)) >> log2,
                (false, false) => 128,
            };
            out[..n * n].fill(dc as u8);
        }
        TM_PRED => {
            for y in 0..n {
                for x in 0..n {
                    out[y * n + x] =
                        clip8(i32::from(left[y]) + i32::from(top[x]) - i32::from(top_left));
                }
            }
        }
        V_PRED => {
            for y in 0..n {
                out[y * n..(y + 1) * n].copy_from_slice(&top[..n]);
            }
        }
        _ => {
            for y in 0..n {
                out[y * n..(y + 1) * n].fill(left[y]);
            }
        }
    }
}

fn avg3(a: u8, b: u8, c: u8) -> u8 {
    ((u32::from(a) + 2 * u32::from(b) + u32::from(c) + 2) >> 2) as u8
}

fn avg2(a: u8, b: u8) -> u8 {
    ((u32::from(a) + u32::from(b) + 1) >> 1) as u8
}

/// Predicts a 4x4 sub-block. `top` holds the eight samples above (four above-right ones
/// last), `left` the four to the left, `x` the corner.
pub fn predict_4x4(mode: u8, x: u8, top: &[u8; 8], left: &[u8; 4]) -> [u8; 16] {
    let mut d = [0u8; 16];
    let (a, b, c, dd, e, f, g, h) = (
        top[0], top[1], top[2], top[3], top[4], top[5], top[6], top[7],
    );
    let (i, j, k, l) = (left[0], left[1], left[2], left[3]);
    // set(x, y, v)
    let mut set = |px: usize, py: usize, v: u8| d[px + py * 4] = v;
    match mode {
        DC_PRED => {
            let mut dc = 4u32;
            for q in 0..4 {
                dc += u32::from(top[q]) + u32::from(left[q]);
            }
            let v = (dc >> 3) as u8;
            for p in 0..16 {
                d[p] = v;
            }
        }
        TM_PRED => {
            for py in 0..4 {
                for px in 0..4 {
                    d[px + py * 4] = clip8(i32::from(left[py]) + i32::from(top[px]) - i32::from(x));
                }
            }
        }
        V_PRED => {
            let vals = [avg3(x, a, b), avg3(a, b, c), avg3(b, c, dd), avg3(c, dd, e)];
            for py in 0..4 {
                for px in 0..4 {
                    d[px + py * 4] = vals[px];
                }
            }
        }
        H_PRED => {
            let rows = [avg3(x, i, j), avg3(i, j, k), avg3(j, k, l), avg3(k, l, l)];
            for py in 0..4 {
                for px in 0..4 {
                    d[px + py * 4] = rows[py];
                }
            }
        }
        B_RD_PRED => {
            set(0, 3, avg3(j, k, l));
            let v = avg3(i, j, k);
            set(1, 3, v);
            set(0, 2, v);
            let v = avg3(x, i, j);
            set(2, 3, v);
            set(1, 2, v);
            set(0, 1, v);
            let v = avg3(a, x, i);
            set(3, 3, v);
            set(2, 2, v);
            set(1, 1, v);
            set(0, 0, v);
            let v = avg3(b, a, x);
            set(3, 2, v);
            set(2, 1, v);
            set(1, 0, v);
            let v = avg3(c, b, a);
            set(3, 1, v);
            set(2, 0, v);
            set(3, 0, avg3(dd, c, b));
        }
        B_VR_PRED => {
            let v = avg2(x, a);
            set(0, 0, v);
            set(1, 2, v);
            let v = avg2(a, b);
            set(1, 0, v);
            set(2, 2, v);
            let v = avg2(b, c);
            set(2, 0, v);
            set(3, 2, v);
            set(3, 0, avg2(c, dd));
            set(0, 3, avg3(k, j, i));
            set(0, 2, avg3(j, i, x));
            let v = avg3(i, x, a);
            set(0, 1, v);
            set(1, 3, v);
            let v = avg3(x, a, b);
            set(1, 1, v);
            set(2, 3, v);
            let v = avg3(a, b, c);
            set(2, 1, v);
            set(3, 3, v);
            set(3, 1, avg3(b, c, dd));
        }
        B_LD_PRED => {
            set(0, 0, avg3(a, b, c));
            let v = avg3(b, c, dd);
            set(1, 0, v);
            set(0, 1, v);
            let v = avg3(c, dd, e);
            set(2, 0, v);
            set(1, 1, v);
            set(0, 2, v);
            let v = avg3(dd, e, f);
            set(3, 0, v);
            set(2, 1, v);
            set(1, 2, v);
            set(0, 3, v);
            let v = avg3(e, f, g);
            set(3, 1, v);
            set(2, 2, v);
            set(1, 3, v);
            let v = avg3(f, g, h);
            set(3, 2, v);
            set(2, 3, v);
            set(3, 3, avg3(g, h, h));
        }
        B_VL_PRED => {
            set(0, 0, avg2(a, b));
            let v = avg2(b, c);
            set(1, 0, v);
            set(0, 2, v);
            let v = avg2(c, dd);
            set(2, 0, v);
            set(1, 2, v);
            let v = avg2(dd, e);
            set(3, 0, v);
            set(2, 2, v);
            set(0, 1, avg3(a, b, c));
            let v = avg3(b, c, dd);
            set(1, 1, v);
            set(0, 3, v);
            let v = avg3(c, dd, e);
            set(2, 1, v);
            set(1, 3, v);
            let v = avg3(dd, e, f);
            set(3, 1, v);
            set(2, 3, v);
            set(3, 2, avg3(e, f, g));
            set(3, 3, avg3(f, g, h));
        }
        B_HD_PRED => {
            let v = avg2(i, x);
            set(0, 0, v);
            set(2, 1, v);
            let v = avg2(j, i);
            set(0, 1, v);
            set(2, 2, v);
            let v = avg2(k, j);
            set(0, 2, v);
            set(2, 3, v);
            set(0, 3, avg2(l, k));
            set(3, 0, avg3(a, b, c));
            set(2, 0, avg3(x, a, b));
            let v = avg3(i, x, a);
            set(1, 0, v);
            set(3, 1, v);
            let v = avg3(j, i, x);
            set(1, 1, v);
            set(3, 2, v);
            let v = avg3(k, j, i);
            set(1, 2, v);
            set(3, 3, v);
            set(1, 3, avg3(l, k, j));
        }
        _ => {
            // B_HU_PRED
            set(0, 0, avg2(i, j));
            let v = avg2(j, k);
            set(2, 0, v);
            set(0, 1, v);
            let v = avg2(k, l);
            set(2, 1, v);
            set(0, 2, v);
            set(1, 0, avg3(i, j, k));
            let v = avg3(j, k, l);
            set(3, 0, v);
            set(1, 1, v);
            let v = avg3(k, l, l);
            set(3, 1, v);
            set(1, 2, v);
            for (px, py) in [(3, 2), (2, 2), (0, 3), (1, 3), (2, 3), (3, 3)] {
                set(px, py, l);
            }
        }
    }
    d
}
