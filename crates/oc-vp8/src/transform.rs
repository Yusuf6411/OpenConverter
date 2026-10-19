//! The 4x4 inverse DCT and Walsh-Hadamard transforms of VP8, and their forward counterparts.
//!
//! Block geometry is fixed (4x4 blocks inside planes sized in whole macroblocks), so
//! indexing here cannot go out of range for planes the codec itself allocated.
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

pub fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn mul1(a: i32) -> i32 {
    (a.wrapping_mul(20091) >> 16).wrapping_add(a)
}

fn mul2(a: i32) -> i32 {
    a.wrapping_mul(35468) >> 16
}

/// Adds the inverse DCT of `coeffs` (row-major 4x4) to the 4x4 pixels at `pos`.
///
/// Corrupt streams can carry coefficients large enough to overflow 32 bits; like libwebp's C
/// code the arithmetic wraps, so such streams decode to the same pixels everywhere.
pub fn idct_add(coeffs: &[i16], dst: &mut [u8], pos: usize, stride: usize) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let c = |k: usize| i32::from(coeffs[i + 4 * k]);
        let a = c(0).wrapping_add(c(2));
        let b = c(0).wrapping_sub(c(2));
        let cc = mul2(c(1)).wrapping_sub(mul1(c(3)));
        let d = mul1(c(1)).wrapping_add(mul2(c(3)));
        tmp[4 * i] = a.wrapping_add(d);
        tmp[4 * i + 1] = b.wrapping_add(cc);
        tmp[4 * i + 2] = b.wrapping_sub(cc);
        tmp[4 * i + 3] = a.wrapping_sub(d);
    }
    for i in 0..4 {
        let dc = tmp[i].wrapping_add(4);
        let a = dc.wrapping_add(tmp[i + 8]);
        let b = dc.wrapping_sub(tmp[i + 8]);
        let c = mul2(tmp[i + 4]).wrapping_sub(mul1(tmp[i + 12]));
        let d = mul1(tmp[i + 4]).wrapping_add(mul2(tmp[i + 12]));
        let row = pos + i * stride;
        let out = [
            a.wrapping_add(d),
            b.wrapping_add(c),
            b.wrapping_sub(c),
            a.wrapping_sub(d),
        ];
        for (x, v) in out.into_iter().enumerate() {
            dst[row + x] = clip8(i32::from(dst[row + x]).wrapping_add(v >> 3));
        }
    }
}

/// Inverse Walsh-Hadamard transform of the sixteen luma DC values; `out[16 * k]` receives the
/// DC of block `k`. Arithmetic wraps like `idct_add`'s.
pub fn iwht(input: &[i16], out: &mut [i16]) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let g = |k: usize| i32::from(input[k + i]);
        let a0 = g(0).wrapping_add(g(12));
        let a1 = g(4).wrapping_add(g(8));
        let a2 = g(4).wrapping_sub(g(8));
        let a3 = g(0).wrapping_sub(g(12));
        tmp[i] = a0.wrapping_add(a1);
        tmp[8 + i] = a0.wrapping_sub(a1);
        tmp[4 + i] = a3.wrapping_add(a2);
        tmp[12 + i] = a3.wrapping_sub(a2);
    }
    for i in 0..4 {
        let dc = tmp[i * 4].wrapping_add(3);
        let a0 = dc.wrapping_add(tmp[3 + i * 4]);
        let a1 = tmp[1 + i * 4].wrapping_add(tmp[2 + i * 4]);
        let a2 = tmp[1 + i * 4].wrapping_sub(tmp[2 + i * 4]);
        let a3 = dc.wrapping_sub(tmp[3 + i * 4]);
        let base = i * 64;
        out[base] = (a0.wrapping_add(a1) >> 3) as i16;
        out[base + 16] = (a3.wrapping_add(a2) >> 3) as i16;
        out[base + 32] = (a0.wrapping_sub(a1) >> 3) as i16;
        out[base + 48] = (a3.wrapping_sub(a2) >> 3) as i16;
    }
}

/// Forward DCT of the difference between 4x4 blocks `src` and `pred` (both with their own
/// strides and positions).
pub fn fdct(
    src: &[u8],
    src_pos: usize,
    src_stride: usize,
    pred: &[u8],
    pred_pos: usize,
    pred_stride: usize,
    out: &mut [i16; 16],
) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let s = src_pos + i * src_stride;
        let p = pred_pos + i * pred_stride;
        let d = |k: usize| i32::from(src[s + k]) - i32::from(pred[p + k]);
        let (d0, d1, d2, d3) = (d(0), d(1), d(2), d(3));
        let a0 = d0 + d3;
        let a1 = d1 + d2;
        let a2 = d1 - d2;
        let a3 = d0 - d3;
        tmp[i * 4] = (a0 + a1) * 8;
        tmp[i * 4 + 1] = (a2 * 2217 + a3 * 5352 + 1812) >> 9;
        tmp[i * 4 + 2] = (a0 - a1) * 8;
        tmp[i * 4 + 3] = (a3 * 2217 - a2 * 5352 + 937) >> 9;
    }
    for i in 0..4 {
        let a0 = tmp[i] + tmp[12 + i];
        let a1 = tmp[4 + i] + tmp[8 + i];
        let a2 = tmp[4 + i] - tmp[8 + i];
        let a3 = tmp[i] - tmp[12 + i];
        out[i] = ((a0 + a1 + 7) >> 4) as i16;
        out[4 + i] = (((a2 * 2217 + a3 * 5352 + 12000) >> 16) + i32::from(a3 != 0)) as i16;
        out[8 + i] = ((a0 - a1 + 7) >> 4) as i16;
        out[12 + i] = ((a3 * 2217 - a2 * 5352 + 51000) >> 16) as i16;
    }
}

/// Forward Walsh-Hadamard transform of the sixteen luma DCs (`input[16 * k]`).
pub fn fwht(input: &[i16], out: &mut [i16; 16]) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let g = |k: usize| i32::from(input[i * 64 + k * 16]);
        let a0 = g(0) + g(2);
        let a1 = g(1) + g(3);
        let a2 = g(1) - g(3);
        let a3 = g(0) - g(2);
        tmp[i * 4] = a0 + a1;
        tmp[i * 4 + 1] = a3 + a2;
        tmp[i * 4 + 2] = a3 - a2;
        tmp[i * 4 + 3] = a0 - a1;
    }
    for i in 0..4 {
        let a0 = tmp[i] + tmp[8 + i];
        let a1 = tmp[4 + i] + tmp[12 + i];
        let a2 = tmp[4 + i] - tmp[12 + i];
        let a3 = tmp[i] - tmp[8 + i];
        out[i] = ((a0 + a1) >> 1) as i16;
        out[4 + i] = ((a3 + a2) >> 1) as i16;
        out[8 + i] = ((a3 - a2) >> 1) as i16;
        out[12 + i] = ((a0 - a1) >> 1) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dct_roundtrip_is_close() {
        let mut state = 7u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        for _ in 0..200 {
            let src: Vec<u8> = (0..16).map(|_| next()).collect();
            let pred: Vec<u8> = (0..16).map(|_| next()).collect();
            let mut c = [0i16; 16];
            fdct(&src, 0, 4, &pred, 0, 4, &mut c);
            // The forward transform has gain 8 per dimension relative to the inverse's 1/8,
            // so quantising by 1 here means undoing nothing: the inverse expects the same scale.
            let mut rec = pred.clone();
            idct_add(&c, &mut rec, 0, 4);
            for (a, b) in rec.iter().zip(&src) {
                assert!(a.abs_diff(*b) <= 1, "{a} vs {b}");
            }
        }
    }

    #[test]
    fn wht_roundtrip() {
        let mut dcs = [0i16; 256];
        let mut state = 99u32;
        for k in 0..16 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            dcs[k * 16] = ((state >> 20) as i16) - 2048;
        }
        let mut f = [0i16; 16];
        fwht(&dcs, &mut f);
        let mut back = [0i16; 256];
        iwht(&f, &mut back);
        for k in 0..16 {
            assert!(
                (i32::from(back[k * 16]) - i32::from(dcs[k * 16])).abs() <= 1,
                "block {k}: {} vs {}",
                back[k * 16],
                dcs[k * 16]
            );
        }
    }
}
