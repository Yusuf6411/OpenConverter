//! 8x8 integer DCT, the "slow but accurate" algorithm of the IJG library (Loeffler,
//! Ligtenberg and Moschytz, 12 multiplies). Using the same arithmetic as libjpeg makes
//! decoded pixels match it exactly, which is what other software produces.
#![allow(clippy::indexing_slicing)] // all indices are < 64 by construction

const CONST_BITS: i32 = 13;
const PASS1_BITS: i32 = 2;

const F_0_298: i32 = 2446;
const F_0_390: i32 = 3196;
const F_0_541: i32 = 4433;
const F_0_765: i32 = 6270;
const F_0_899: i32 = 7373;
const F_1_175: i32 = 9633;
const F_1_501: i32 = 12299;
const F_1_847: i32 = 15137;
const F_1_961: i32 = 16069;
const F_2_053: i32 = 16819;
const F_2_562: i32 = 20995;
const F_3_072: i32 = 25172;

#[inline]
fn descale(x: i32, n: i32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

/// One 8-point inverse transform. Results carry a factor of 2^CONST_BITS. Arithmetic is
/// 64-bit because damaged files can hold coefficients far outside the legal range.
fn idct_1d(d: [i64; 8]) -> [i64; 8] {
    let (f541, f765, f1847) = (i64::from(F_0_541), i64::from(F_0_765), i64::from(F_1_847));
    let z1 = (d[2] + d[6]) * f541;
    let tmp2 = z1 - d[6] * f1847;
    let tmp3 = z1 + d[2] * f765;
    let tmp0 = (d[0] + d[4]) << CONST_BITS;
    let tmp1 = (d[0] - d[4]) << CONST_BITS;
    let (tmp10, tmp13, tmp11, tmp12) = (tmp0 + tmp3, tmp0 - tmp3, tmp1 + tmp2, tmp1 - tmp2);
    let (mut t0, mut t1, mut t2, mut t3) = (d[7], d[5], d[3], d[1]);
    let (z1, z2, z3, z4) = (t0 + t3, t1 + t2, t0 + t2, t1 + t3);
    let z5 = (z3 + z4) * i64::from(F_1_175);
    t0 *= i64::from(F_0_298);
    t1 *= i64::from(F_2_053);
    t2 *= i64::from(F_3_072);
    t3 *= i64::from(F_1_501);
    let (z1, z2) = (z1 * -i64::from(F_0_899), z2 * -i64::from(F_2_562));
    let z3 = z3 * -i64::from(F_1_961) + z5;
    let z4 = z4 * -i64::from(F_0_390) + z5;
    t0 += z1 + z3;
    t1 += z2 + z4;
    t2 += z2 + z3;
    t3 += z1 + z4;
    [
        tmp10 + t3,
        tmp11 + t2,
        tmp12 + t1,
        tmp13 + t0,
        tmp13 - t0,
        tmp12 - t1,
        tmp11 - t2,
        tmp10 - t3,
    ]
}

/// Inverse DCT of one block of dequantised coefficients (natural order) into samples,
/// level-shifted by +128 and clamped to 0..=255.
pub fn idct(coef: &[i32; 64], out: &mut [u8; 64]) {
    let round = |x: i64, n: i32| (x + (1 << (n - 1))) >> n;
    let mut ws = [0i64; 64];
    for col in 0..8 {
        let mut d = [0i64; 8];
        for (row, v) in d.iter_mut().enumerate() {
            *v = i64::from(coef[row * 8 + col].clamp(-(1 << 20), 1 << 20));
        }
        for (row, v) in idct_1d(d).into_iter().enumerate() {
            ws[row * 8 + col] = round(v, CONST_BITS - PASS1_BITS);
        }
    }
    for row in 0..8 {
        let mut d = [0i64; 8];
        d.copy_from_slice(&ws[row * 8..row * 8 + 8]);
        for (x, v) in idct_1d(d).into_iter().enumerate() {
            out[row * 8 + x] = (round(v, CONST_BITS + PASS1_BITS + 3) + 128).clamp(0, 255) as u8;
        }
    }
}

/// Forward DCT of one block of samples (0..=255, level-shifted inside). The result is
/// scaled up by 8 relative to the orthonormal DCT, as in libjpeg.
pub fn fdct(samples: &[u8; 64], out: &mut [i32; 64]) {
    let mut d = [0i32; 64];
    for (o, &s) in d.iter_mut().zip(samples) {
        *o = i32::from(s) - 128;
    }
    // Rows.
    for row in 0..8 {
        let r = &mut d[row * 8..row * 8 + 8];
        let (t0, t7) = (r[0] + r[7], r[0] - r[7]);
        let (t1, t6) = (r[1] + r[6], r[1] - r[6]);
        let (t2, t5) = (r[2] + r[5], r[2] - r[5]);
        let (t3, t4) = (r[3] + r[4], r[3] - r[4]);
        let (t10, t13, t11, t12) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
        r[0] = (t10 + t11) << PASS1_BITS;
        r[4] = (t10 - t11) << PASS1_BITS;
        let z1 = (t12 + t13) * F_0_541;
        let sh = CONST_BITS - PASS1_BITS;
        r[2] = descale(z1 + t13 * F_0_765, sh);
        r[6] = descale(z1 + t12 * -F_1_847, sh);
        let (z1, z2, z3, z4) = (t4 + t7, t5 + t6, t4 + t6, t5 + t7);
        let z5 = (z3 + z4) * F_1_175;
        let (t4, t5, t6, t7) = (t4 * F_0_298, t5 * F_2_053, t6 * F_3_072, t7 * F_1_501);
        let (z1, z2) = (z1 * -F_0_899, z2 * -F_2_562);
        let z3 = z3 * -F_1_961 + z5;
        let z4 = z4 * -F_0_390 + z5;
        r[7] = descale(t4 + z1 + z3, sh);
        r[5] = descale(t5 + z2 + z4, sh);
        r[3] = descale(t6 + z2 + z3, sh);
        r[1] = descale(t7 + z1 + z4, sh);
    }
    // Columns.
    for col in 0..8 {
        let g = |row: usize| d[row * 8 + col];
        let (t0, t7) = (g(0) + g(7), g(0) - g(7));
        let (t1, t6) = (g(1) + g(6), g(1) - g(6));
        let (t2, t5) = (g(2) + g(5), g(2) - g(5));
        let (t3, t4) = (g(3) + g(4), g(3) - g(4));
        let (t10, t13, t11, t12) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
        out[col] = descale(t10 + t11, PASS1_BITS);
        out[32 + col] = descale(t10 - t11, PASS1_BITS);
        let z1 = (t12 + t13) * F_0_541;
        let sh = CONST_BITS + PASS1_BITS;
        out[16 + col] = descale(z1 + t13 * F_0_765, sh);
        out[48 + col] = descale(z1 + t12 * -F_1_847, sh);
        let (z1, z2, z3, z4) = (t4 + t7, t5 + t6, t4 + t6, t5 + t7);
        let z5 = (z3 + z4) * F_1_175;
        let (t4, t5, t6, t7) = (t4 * F_0_298, t5 * F_2_053, t6 * F_3_072, t7 * F_1_501);
        let (z1, z2) = (z1 * -F_0_899, z2 * -F_2_562);
        let z3 = z3 * -F_1_961 + z5;
        let z4 = z4 * -F_0_390 + z5;
        out[56 + col] = descale(t4 + z1 + z3, sh);
        out[40 + col] = descale(t5 + z2 + z4, sh);
        out[24 + col] = descale(t6 + z2 + z3, sh);
        out[8 + col] = descale(t7 + z1 + z4, sh);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Orthonormal 2-D DCT in floating point, times 8 to match `fdct`'s scale.
    fn reference(samples: &[u8; 64]) -> [f64; 64] {
        let mut out = [0.0; 64];
        for v in 0..8 {
            for u in 0..8 {
                let mut s = 0.0;
                for y in 0..8 {
                    for x in 0..8 {
                        s += (f64::from(samples[y * 8 + x]) - 128.0)
                            * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()
                            * (((2 * y + 1) * v) as f64 * std::f64::consts::PI / 16.0).cos();
                    }
                }
                let cu = if u == 0 { 0.5f64.sqrt() } else { 1.0 };
                let cv = if v == 0 { 0.5f64.sqrt() } else { 1.0 };
                out[v * 8 + u] = s * cu * cv / 4.0 * 8.0;
            }
        }
        out
    }

    fn block(seed: u32) -> [u8; 64] {
        let mut b = [0u8; 64];
        let mut s = seed;
        for v in &mut b {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            *v = (s >> 24) as u8;
        }
        b
    }

    #[test]
    fn forward_matches_the_definition() {
        for seed in 0..50 {
            let b = block(seed);
            let mut got = [0i32; 64];
            fdct(&b, &mut got);
            let want = reference(&b);
            for i in 0..64 {
                assert!(
                    (f64::from(got[i]) - want[i]).abs() < 3.0,
                    "seed {seed} coef {i}"
                );
            }
        }
    }

    #[test]
    fn inverse_undoes_forward() {
        for seed in 0..50 {
            let b = block(seed);
            let mut c = [0i32; 64];
            fdct(&b, &mut c);
            // fdct output is 8x the orthonormal scale; the inverse expects unscaled input.
            for v in &mut c {
                *v = (*v as f64 / 8.0).round() as i32;
            }
            let mut back = [0u8; 64];
            idct(&c, &mut back);
            for i in 0..64 {
                assert!(
                    back[i].abs_diff(b[i]) <= 3,
                    "seed {seed} px {i}: {} vs {}",
                    back[i],
                    b[i]
                );
            }
        }
    }

    #[test]
    fn dc_only_block_is_flat() {
        let mut c = [0i32; 64];
        c[0] = 80; // 80/8 = +10 above the mid-gray level
        let mut out = [0u8; 64];
        idct(&c, &mut out);
        assert!(out.iter().all(|&v| v == 138));
    }
}
