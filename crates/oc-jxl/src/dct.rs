//! Inverse (and, for the low-frequency part, forward) DCTs of the sizes JPEG XL uses, and the
//! block transforms that are not plain DCTs (DCT2, DCT4, DCT4x8, Hornuss, AFV).
//!
//! The 1-D transforms follow the recursive even/odd split that jxl-oxide (MIT/Apache-2.0) uses;
//! the forward direction is normalised by 1/N with a sqrt(2) on the AC terms, the inverse is
//! unnormalised, so `forward` followed by `inverse` is the identity.
use std::f32::consts::{FRAC_1_SQRT_2, SQRT_2};

/// `1 / (2 cos((2k + 1) pi / 2n))` for every power-of-two size from 4 to 256.
pub struct Tables {
    sec: [Vec<f32>; 9],
}

impl Tables {
    pub fn new() -> Self {
        let sec = std::array::from_fn(|log| {
            let n = 1usize << log;
            if n < 4 {
                return Vec::new();
            }
            (0..n / 2)
                .map(|k| {
                    let theta = (2 * k + 1) as f64 / (2 * n) as f64 * std::f64::consts::PI;
                    (1.0 / (2.0 * theta.cos())) as f32
                })
                .collect()
        });
        Self { sec }
    }

    fn sec(&self, n: usize) -> &[f32] {
        &self.sec[n.trailing_zeros() as usize]
    }
}

impl Default for Tables {
    fn default() -> Self {
        Self::new()
    }
}

/// In-place 1-D DCT of `io` (a power-of-two length); `scratch` has the same length.
fn dct(io: &mut [f32], scratch: &mut [f32], inverse: bool, t: &Tables) {
    let n = io.len();
    if n <= 1 {
        return;
    }
    if n == 2 {
        let (a, b) = (io[0] + io[1], io[0] - io[1]);
        if inverse {
            io[0] = a;
            io[1] = b;
        } else {
            io[0] = a / 2.0;
            io[1] = b / 2.0;
        }
        return;
    }
    let h = n / 2;
    let sec = t.sec(n);
    let (in0, in1) = scratch.split_at_mut(h);
    if !inverse {
        for i in 0..h {
            let (a, b) = (io[i], io[n - 1 - i]);
            in0[i] = (a + b) / 2.0;
            in1[i] = (a - b) / 2.0 * sec[i];
        }
        {
            let (o0, o1) = io.split_at_mut(h);
            dct(in0, o0, false, t);
            dct(in1, o1, false, t);
        }
        in1[0] *= SQRT_2;
        for i in 0..h - 1 {
            in1[i] += in1[i + 1];
        }
        for i in 0..h {
            io[2 * i] = in0[i];
            io[2 * i + 1] = in1[i];
        }
    } else {
        for i in 0..h {
            in0[i] = io[2 * i];
            in1[i] = io[2 * i + 1];
        }
        for i in 1..h {
            in1[h - i] += in1[h - i - 1];
        }
        in1[0] *= SQRT_2;
        {
            let (o0, o1) = io.split_at_mut(h);
            dct(in0, o0, true, t);
            dct(in1, o1, true, t);
        }
        for i in 0..h {
            in1[i] *= sec[i];
        }
        for i in 0..h {
            io[i] = in0[i] + in1[i];
            io[n - 1 - i] = in0[i] - in1[i];
        }
    }
}

/// Separable 2-D DCT of a `w` x `h` block stored row by row with the given stride.
pub fn dct2d(buf: &mut [f32], stride: usize, w: usize, h: usize, inverse: bool, t: &Tables) {
    if w * h <= 1 {
        return;
    }
    let mut scratch = vec![0f32; w.max(h)];
    for y in 0..h {
        dct(
            &mut buf[y * stride..y * stride + w],
            &mut scratch[..w],
            inverse,
            t,
        );
    }
    if h > 1 {
        let mut col = vec![0f32; h];
        for x in 0..w {
            for y in 0..h {
                col[y] = buf[y * stride + x];
            }
            dct(&mut col, &mut scratch[..h], inverse, t);
            for y in 0..h {
                buf[y * stride + x] = col[y];
            }
        }
    }
}

/// `ScaleF` of the low-frequency coefficients: entry `c << (5 - log2(blocks))` of a 32-entry
/// table for the 256-point transform.
pub fn scale_f(c: usize, log_blocks: u32) -> f32 {
    #[allow(clippy::excessive_precision)]
    const SCALE_F: [f32; 32] = [
        1.0,
        0.999_604_7,
        0.998_419_5,
        0.996_445_8,
        0.993_686_6,
        0.990_145_6,
        0.985_827_8,
        0.980_739_2,
        0.974_886_8,
        0.968_278_8,
        0.960_924_4,
        0.952_833_8,
        0.944_018_1,
        0.934_489_6,
        0.924_261_6,
        0.913_348_1,
        0.901_764_2,
        0.889_525_9,
        0.876_650_1,
        0.863_154_4,
        0.849_057_5,
        0.834_378_6,
        0.819_137_9,
        0.803_356_1,
        0.787_054_9,
        0.770_256_4,
        0.752_983_4,
        0.735_259_3,
        0.717_108_1,
        0.698_554_3,
        0.679_622_9,
        0.660_339_1,
    ];
    SCALE_F[c << (5 - log_blocks)]
}

// ---------------------------------------------------------------------------------------------
// Block transforms that are not plain DCTs. `b` is an 8x8 block, row by row.

fn idct2_stage(b: &mut [f32; 64], size: usize) {
    let half = size / 2;
    let mut s = [[0f32; 8]; 8];
    for y in 0..half {
        for x in 0..half {
            let c00 = b[y * 8 + x];
            let c01 = b[y * 8 + x + half];
            let c10 = b[(y + half) * 8 + x];
            let c11 = b[(y + half) * 8 + x + half];
            s[2 * y][2 * x] = c00 + c01 + c10 + c11;
            s[2 * y][2 * x + 1] = c00 + c01 - c10 - c11;
            s[2 * y + 1][2 * x] = c00 - c01 + c10 - c11;
            s[2 * y + 1][2 * x + 1] = c00 - c01 - c10 + c11;
        }
    }
    for y in 0..size {
        b[y * 8..y * 8 + size].copy_from_slice(&s[y][..size]);
    }
}

pub fn transform_dct2(b: &mut [f32; 64]) {
    idct2_stage(b, 2);
    idct2_stage(b, 4);
    idct2_stage(b, 8);
}

pub fn transform_dct4(b: &mut [f32; 64], t: &Tables) {
    idct2_stage(b, 2);
    let mut scratch = [0f32; 64];
    for y in 0..2 {
        for x in 0..2 {
            let s = &mut scratch[(y * 2 + x) * 16..][..16];
            for iy in 0..4 {
                for ix in 0..4 {
                    // transposed gather, as the format defines it
                    s[ix * 4 + iy] = b[(y + iy * 2) * 8 + x + ix * 2];
                }
            }
            dct2d(s, 4, 4, 4, true, t);
        }
    }
    for y in 0..2 {
        for x in 0..2 {
            let s = &scratch[(y * 2 + x) * 16..][..16];
            for iy in 0..4 {
                for ix in 0..4 {
                    b[(y * 4 + iy) * 8 + x * 4 + ix] = s[iy * 4 + ix];
                }
            }
        }
    }
}

pub fn transform_hornuss(b: &mut [f32; 64]) {
    idct2_stage(b, 2);
    let mut scratch = [0f32; 64];
    for y in 0..2 {
        for x in 0..2 {
            let s = &mut scratch[(y * 2 + x) * 16..][..16];
            for iy in 0..4 {
                for ix in 0..4 {
                    s[iy * 4 + ix] = b[(y + iy * 2) * 8 + x + ix * 2];
                }
            }
            let residual: f32 = s[1..].iter().sum();
            let avg = s[0] - residual / 16.0;
            s[0] = s[5];
            s[5] = 0.0;
            for v in s.iter_mut() {
                *v += avg;
            }
        }
    }
    for y in 0..2 {
        for x in 0..2 {
            let s = &scratch[(y * 2 + x) * 16..][..16];
            for iy in 0..4 {
                for ix in 0..4 {
                    b[(y * 4 + iy) * 8 + x * 4 + ix] = s[iy * 4 + ix];
                }
            }
        }
    }
}

pub fn transform_dct4x8(b: &mut [f32; 64], transposed: bool, t: &Tables) {
    let (c0, c1) = (b[0], b[8]);
    b[0] = c0 + c1;
    b[8] = c0 - c1;
    let mut scratch = [0f32; 64];
    for idx in 0..2 {
        let s = &mut scratch[idx * 32..][..32];
        for iy in 0..4 {
            for ix in 0..8 {
                s[iy * 8 + ix] = b[(iy * 2 + idx) * 8 + ix];
            }
        }
        dct2d(s, 8, 8, 4, true, t);
    }
    if transposed {
        for y in 0..8 {
            for x in 0..8 {
                b[x * 8 + y] = scratch[y * 8 + x];
            }
        }
    } else {
        b.copy_from_slice(&scratch);
    }
}

pub fn transform_afv(b: &mut [f32; 64], n: usize, t: &Tables) {
    let flip_x = n % 2;
    let flip_y = n / 2;

    let mut coeff_afv = [0f32; 16];
    coeff_afv[0] = (b[0] + b[1] + b[8]) * 4.0;
    for (idx, v) in coeff_afv.iter_mut().enumerate().skip(1) {
        *v = b[2 * (idx / 4) * 8 + 2 * (idx % 4)];
    }
    let mut samples = [0f32; 16];
    for (c, basis) in coeff_afv.iter().zip(AFV_BASIS.iter()) {
        for (s, bv) in samples.iter_mut().zip(basis) {
            *s += c * bv;
        }
    }

    let mut s44 = [0f32; 16];
    s44[0] = b[0] - b[1] + b[8];
    for iy in 0..4 {
        for ix in 0..4 {
            if ix | iy != 0 {
                s44[ix * 4 + iy] = b[2 * iy * 8 + 2 * ix + 1];
            }
        }
    }
    dct2d(&mut s44, 4, 4, 4, true, t);

    let mut s48 = [0f32; 32];
    s48[0] = b[0] - b[8];
    for iy in 0..4 {
        for ix in 0..8 {
            if ix | iy != 0 {
                s48[iy * 8 + ix] = b[(2 * iy + 1) * 8 + ix];
            }
        }
    }
    dct2d(&mut s48, 8, 8, 4, true, t);

    for iy in 0..4 {
        let ay = if flip_y == 0 { iy } else { 3 - iy };
        for ix in 0..4 {
            let ax = if flip_x == 0 { ix } else { 3 - ix };
            b[(flip_y * 4 + iy) * 8 + flip_x * 4 + ix] = samples[ay * 4 + ax];
        }
    }
    for iy in 0..4 {
        for ix in 0..4 {
            b[(flip_y * 4 + iy) * 8 + (1 - flip_x) * 4 + ix] = s44[iy * 4 + ix];
        }
    }
    for iy in 0..4 {
        let y = (1 - flip_y) * 4 + iy;
        b[y * 8..y * 8 + 8].copy_from_slice(&s48[iy * 8..iy * 8 + 8]);
    }
}

#[allow(clippy::excessive_precision)]
#[rustfmt::skip]
const AFV_BASIS: [[f32; 16]; 16] = [
    [0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25],
    [0.876902929799142, 0.2206518106944235, -0.10140050393753763, -0.1014005039375375, 0.2206518106944236, -0.10140050393753777, -0.10140050393753772, -0.10140050393753763, -0.10140050393753758, -0.10140050393753769, -0.1014005039375375, -0.10140050393753768, -0.10140050393753768, -0.10140050393753759, -0.10140050393753763, -0.10140050393753741],
    [0.0, 0.0, 0.40670075830260755, 0.44444816619734445, 0.0, 0.0, 0.19574399372042936, 0.2929100136981264, -0.40670075830260716, -0.19574399372042872, 0.0, 0.11379074460448091, -0.44444816619734384, -0.29291001369812636, -0.1137907446044814, 0.0],
    [0.0, 0.0, -0.21255748058288748, 0.3085497062849767, 0.0, 0.4706702258572536, -0.1621205195722993, 0.0, -0.21255748058287047, -0.16212051957228327, -0.47067022585725277, -0.1464291867126764, 0.3085497062849487, 0.0, -0.14642918671266536, 0.4251149611657548],
    [0.0, -0.7071067811865474, 0.0, 0.0, FRAC_1_SQRT_2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    [-0.4105377591765233, 0.6235485373547691, -0.06435071657946274, -0.06435071657946266, 0.6235485373547694, -0.06435071657946284, -0.0643507165794628, -0.06435071657946274, -0.06435071657946272, -0.06435071657946279, -0.06435071657946266, -0.06435071657946277, -0.06435071657946277, -0.06435071657946273, -0.06435071657946274, -0.0643507165794626],
    [0.0, 0.0, -0.4517556589999482, 0.15854503551840063, 0.0, -0.04038515160822202, 0.0074182263792423875, 0.39351034269210167, -0.45175565899994635, 0.007418226379244351, 0.1107416575309343, 0.08298163094882051, 0.15854503551839705, 0.3935103426921022, 0.0829816309488214, -0.45175565899994796],
    [0.0, 0.0, -0.304684750724869, 0.5112616136591823, 0.0, 0.0, -0.290480129728998, -0.06578701549142804, 0.304684750724884, 0.2904801297290076, 0.0, -0.23889773523344604, -0.5112616136592012, 0.06578701549142545, 0.23889773523345467, 0.0],
    [0.0, 0.0, 0.3017929516615495, 0.25792362796341184, 0.0, 0.16272340142866204, 0.09520022653475037, 0.0, 0.3017929516615503, 0.09520022653475055, -0.16272340142866173, -0.35312385449816297, 0.25792362796341295, 0.0, -0.3531238544981624, -0.6035859033230976],
    [0.0, 0.0, 0.40824829046386274, 0.0, 0.0, 0.0, 0.0, -0.4082482904638628, -0.4082482904638635, 0.0, 0.0, -0.40824829046386296, 0.0, 0.4082482904638634, 0.408248290463863, 0.0],
    [0.0, 0.0, 0.1747866975480809, 0.0812611176717539, 0.0, 0.0, -0.3675398009862027, -0.307882213957909, -0.17478669754808135, 0.3675398009862011, 0.0, 0.4826689115059883, -0.08126111767175039, 0.30788221395790305, -0.48266891150598584, 0.0],
    [0.0, 0.0, -0.21105601049335784, 0.18567180916109802, 0.0, 0.0, 0.49215859013738733, -0.38525013709251915, 0.21105601049335806, -0.49215859013738905, 0.0, 0.17419412659916217, -0.18567180916109904, 0.3852501370925211, -0.1741941265991621, 0.0],
    [0.0, 0.0, -0.14266084808807264, -0.3416446842253372, 0.0, 0.7367497537172237, 0.24627107722075148, -0.08574019035519306, -0.14266084808807344, 0.24627107722075137, 0.14883399227113567, -0.04768680350229251, -0.3416446842253373, -0.08574019035519267, -0.047686803502292804, -0.14266084808807242],
    [0.0, 0.0, -0.13813540350758585, 0.3302282550303788, 0.0, 0.08755115000587084, -0.07946706605909573, -0.4613374887461511, -0.13813540350758294, -0.07946706605910261, 0.49724647109535086, 0.12538059448563663, 0.3302282550303805, -0.4613374887461554, 0.12538059448564315, -0.13813540350758452],
    [0.0, 0.0, -0.17437602599651067, 0.0702790691196284, 0.0, -0.2921026642334881, 0.3623817333531167, 0.0, -0.1743760259965108, 0.36238173335311646, 0.29210266423348785, -0.4326608024727445, 0.07027906911962818, 0.0, -0.4326608024727457, 0.34875205199302267],
    [0.0, 0.0, 0.11354987314994337, -0.07417504595810355, 0.0, 0.19402893032594343, -0.435190496523228, 0.21918684838857466, 0.11354987314994257, -0.4351904965232251, 0.5550443808910661, -0.25468277124066463, -0.07417504595810233, 0.2191868483885728, -0.25468277124066413, 0.1135498731499429],
];

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_forward(x: &[f32]) -> Vec<f32> {
        let n = x.len();
        (0..n)
            .map(|k| {
                let mut s = 0f64;
                for (i, &v) in x.iter().enumerate() {
                    s += f64::from(v)
                        * (k as f64 * (2 * i + 1) as f64 * std::f64::consts::PI / (2 * n) as f64)
                            .cos();
                }
                let s = s / n as f64;
                (if k == 0 {
                    s
                } else {
                    s * std::f64::consts::SQRT_2
                }) as f32
            })
            .collect()
    }

    fn naive_inverse(x: &[f32]) -> Vec<f32> {
        let n = x.len();
        (0..n)
            .map(|i| {
                let mut s = f64::from(x[0]);
                for (k, &v) in x.iter().enumerate().skip(1) {
                    s += f64::from(v)
                        * std::f64::consts::SQRT_2
                        * (k as f64 * (2 * i + 1) as f64 * std::f64::consts::PI / (2 * n) as f64)
                            .cos();
                }
                s as f32
            })
            .collect()
    }

    #[test]
    fn matches_the_definition() {
        let t = Tables::new();
        let mut seed = 12345u32;
        for log in 1..=8 {
            let n = 1usize << log;
            let x: Vec<f32> = (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 16) as f32 / 65536.0 - 0.5
                })
                .collect();
            let mut a = x.clone();
            let mut s = vec![0f32; n];
            dct(&mut a, &mut s, false, &t);
            for (p, q) in a.iter().zip(naive_forward(&x)) {
                assert!((p - q).abs() < 1e-4, "forward n={n}: {p} vs {q}");
            }
            let mut a = x.clone();
            dct(&mut a, &mut s, true, &t);
            for (p, q) in a.iter().zip(naive_inverse(&x)) {
                assert!((p - q).abs() < 1e-3, "inverse n={n}: {p} vs {q}");
            }
        }
    }

    #[test]
    fn two_d_round_trip() {
        let t = Tables::new();
        for (w, h) in [(8usize, 8usize), (16, 8), (8, 32), (64, 64)] {
            let x: Vec<f32> = (0..w * h).map(|i| ((i * 37 % 101) as f32) / 50.0).collect();
            let mut a = x.clone();
            dct2d(&mut a, w, w, h, false, &t);
            dct2d(&mut a, w, w, h, true, &t);
            for (p, q) in a.iter().zip(&x) {
                assert!((p - q).abs() < 1e-4, "{w}x{h}");
            }
        }
    }
}
