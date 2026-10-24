//! Colour handling: XYB to the file's own colour space, and from there to sRGB for output.
//!
//! Only the colour encodings JPEG XL can signal by enumeration are handled; files that carry an
//! ICC profile are treated as sRGB (the caller reports that).
use crate::header::{Colour, ImageHeader, Prim, Space, Tf, White};

type Mat = [[f64; 3]; 3];

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn inv(m: &Mat) -> Mat {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let d = 1.0 / det;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ]
}

fn white_xy(w: White) -> [f64; 2] {
    match w {
        White::D65 => [0.3127, 0.3290],
        White::E => [1.0 / 3.0, 1.0 / 3.0],
        White::Dci => [0.314, 0.351],
        White::Custom(xy) => xy,
    }
}

fn prim_xy(p: Prim) -> [[f64; 2]; 3] {
    match p {
        Prim::Srgb => [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]],
        Prim::Bt2100 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
        Prim::P3 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
        Prim::Custom(c) => c,
    }
}

fn xy_to_xyz(xy: [f64; 2]) -> [f64; 3] {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

/// Linear RGB with these primaries and white point to XYZ.
fn rgb_to_xyz(prim: Prim, white: White) -> Mat {
    let p = prim_xy(prim);
    let cols: Vec<[f64; 3]> = p.iter().map(|&xy| xy_to_xyz(xy)).collect();
    let m: Mat = [
        [cols[0][0], cols[1][0], cols[2][0]],
        [cols[0][1], cols[1][1], cols[2][1]],
        [cols[0][2], cols[1][2], cols[2][2]],
    ];
    let w = xy_to_xyz(white_xy(white));
    let mi = inv(&m);
    let s: [f64; 3] = std::array::from_fn(|i| (0..3).map(|k| mi[i][k] * w[k]).sum());
    [
        [m[0][0] * s[0], m[0][1] * s[1], m[0][2] * s[2]],
        [m[1][0] * s[0], m[1][1] * s[1], m[1][2] * s[2]],
        [m[2][0] * s[0], m[2][1] * s[1], m[2][2] * s[2]],
    ]
}

/// Bradford adaptation from one white point to another.
fn adapt(from: [f64; 2], to: [f64; 2]) -> Mat {
    const BRADFORD: Mat = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let (a, b) = (xy_to_xyz(from), xy_to_xyz(to));
    let ca: [f64; 3] = std::array::from_fn(|i| (0..3).map(|k| BRADFORD[i][k] * a[k]).sum());
    let cb: [f64; 3] = std::array::from_fn(|i| (0..3).map(|k| BRADFORD[i][k] * b[k]).sum());
    let scale: Mat = [
        [cb[0] / ca[0], 0.0, 0.0],
        [0.0, cb[1] / ca[1], 0.0],
        [0.0, 0.0, cb[2] / ca[2]],
    ];
    mul(&inv(&BRADFORD), &mul(&scale, &BRADFORD))
}

/// Matrix taking linear RGB of `c` to linear sRGB (D65).
fn to_srgb_matrix(c: &Colour) -> Mat {
    let d65 = white_xy(White::D65);
    let src = mul(&adapt(white_xy(c.white), d65), &rgb_to_xyz(c.prim, c.white));
    mul(&inv(&rgb_to_xyz(Prim::Srgb, White::D65)), &src)
}

fn is_srgb_like(c: &Colour) -> bool {
    c.prim == Prim::Srgb && c.white == White::D65
}

// ---------------------------------------------------------------------------------------------
// Transfer functions

fn srgb_decode(v: f64) -> f64 {
    let a = v.abs();
    (if a <= 0.04045 {
        a / 12.92
    } else {
        ((a + 0.055) / 1.055).powf(2.4)
    })
    .copysign(v)
}

fn srgb_encode(v: f64) -> f64 {
    let a = v.abs();
    (if a <= 0.003_130_8 {
        a * 12.92
    } else {
        1.055 * a.powf(1.0 / 2.4) - 0.055
    })
    .copysign(v)
}

fn bt709_decode(v: f64) -> f64 {
    let a = v.abs();
    (if a < 0.081 {
        a / 4.5
    } else {
        ((a + 0.099) / 1.099).powf(1.0 / 0.45)
    })
    .copysign(v)
}

fn bt709_encode(v: f64) -> f64 {
    let a = v.abs();
    (if a < 0.018 {
        a * 4.5
    } else {
        1.099 * a.powf(0.45) - 0.099
    })
    .copysign(v)
}

const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

/// Reference white of an SDR display, as a fraction of PQ's 10000 nits.
const SDR_WHITE: f64 = 203.0 / 10000.0;

fn pq_decode(v: f64) -> f64 {
    let p = v.max(0.0).powf(1.0 / PQ_M2);
    let num = (p - PQ_C1).max(0.0);
    (num / (PQ_C2 - PQ_C3 * p)).powf(1.0 / PQ_M1) / SDR_WHITE
}

fn pq_encode(l: f64) -> f64 {
    let p = (l.max(0.0) * SDR_WHITE).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * p) / (1.0 + PQ_C3 * p)).powf(PQ_M2)
}

fn hlg_decode(v: f64) -> f64 {
    const A: f64 = 0.178_832_77;
    const B: f64 = 0.284_668_92;
    const C: f64 = 0.559_910_73;
    let v = v.max(0.0);
    (if v <= 0.5 {
        v * v / 3.0
    } else {
        (((v - C) / A).exp() + B) / 12.0
    }) * 12.0
        / 3.77
}

fn hlg_encode(l: f64) -> f64 {
    const A: f64 = 0.178_832_77;
    const B: f64 = 0.284_668_92;
    const C: f64 = 0.559_910_73;
    let l = (l.max(0.0) * 3.77 / 12.0).max(0.0);
    if l <= 1.0 / 12.0 {
        (3.0 * l).sqrt()
    } else {
        A * (12.0 * l - B).ln() + C
    }
}

fn decode(tf: Tf, v: f64) -> f64 {
    match tf {
        Tf::Linear => v,
        Tf::Srgb | Tf::Unknown => srgb_decode(v),
        Tf::Bt709 => bt709_decode(v),
        Tf::Gamma(g) => v.abs().powf(g).copysign(v),
        Tf::Dci => v.abs().powf(2.6).copysign(v),
        Tf::Pq => pq_decode(v),
        Tf::Hlg => hlg_decode(v),
    }
}

fn encode(tf: Tf, v: f64) -> f64 {
    match tf {
        Tf::Linear => v,
        Tf::Srgb | Tf::Unknown => srgb_encode(v),
        Tf::Bt709 => bt709_encode(v),
        Tf::Gamma(g) => v.abs().powf(1.0 / g).copysign(v),
        Tf::Dci => v.abs().powf(1.0 / 2.6).copysign(v),
        Tf::Pq => pq_encode(v),
        Tf::Hlg => hlg_encode(v),
    }
}

/// Whether going from the file's encoding to sRGB changes any sample.
pub fn is_identity_to_srgb(c: &Colour) -> bool {
    is_srgb_like(c) && c.tf == Tf::Srgb || c.want_icc
}

/// Whether the encoding is high dynamic range, which an sRGB output can only approximate.
pub fn is_hdr(c: &Colour) -> bool {
    matches!(c.tf, Tf::Pq | Tf::Hlg)
}

/// Convert the planes of a frame between the encoded values of the file's colour space and
/// sRGB-encoded values, in place. `planes` are 1 (grey) or 3 (RGB) planes.
pub fn native_to_srgb(c: &Colour, planes: &mut [Vec<f32>]) {
    if is_identity_to_srgb(c) {
        return;
    }
    let m = to_srgb_matrix(c);
    let grey = planes.len() == 1;
    let n = planes[0].len();
    for i in 0..n {
        if grey {
            let l = decode(c.tf, f64::from(planes[0][i]));
            planes[0][i] = srgb_encode(l) as f32;
            continue;
        }
        let lin: [f64; 3] = std::array::from_fn(|k| decode(c.tf, f64::from(planes[k][i])));
        for r in 0..3 {
            let v = m[r][0] * lin[0] + m[r][1] * lin[1] + m[r][2] * lin[2];
            planes[r][i] = srgb_encode(v) as f32;
        }
    }
}

/// Turn the inverse-opsin result (linear sRGB, D65) into the file's encoded values.
fn linear_srgb_to_native(c: &Colour, planes: &mut [Vec<f32>]) {
    if c.want_icc || (is_srgb_like(c) && c.tf == Tf::Srgb) {
        for p in planes.iter_mut() {
            for v in p.iter_mut() {
                *v = srgb_encode(f64::from(*v)) as f32;
            }
        }
        return;
    }
    let m = inv(&to_srgb_matrix(c));
    let n = planes[0].len();
    for i in 0..n {
        let lin: [f64; 3] = std::array::from_fn(|k| f64::from(planes[k][i]));
        for r in 0..3 {
            let v = m[r][0] * lin[0] + m[r][1] * lin[1] + m[r][2] * lin[2];
            planes[r][i] = encode(c.tf, v) as f32;
        }
    }
}

/// XYB planes to the file's encoded colour values. Returns one plane for grey images.
pub fn xyb_to_native(img: &ImageHeader, x: Vec<f32>, y: Vec<f32>, b: Vec<f32>) -> Vec<Vec<f32>> {
    let ob = img.opsin.bias;
    let cbrt_ob = ob.map(f32::cbrt);
    let m = img.opsin.inv_mat;
    // linear values are relative to the intensity target; 255 nits is the sRGB white
    let scale = 255.0 / img.intensity_target;
    let n = x.len();
    let mut r = vec![0f32; n];
    let mut g = vec![0f32; n];
    let mut bl = vec![0f32; n];
    for i in 0..n {
        let gl = y[i] + x[i] - cbrt_ob[0];
        let gm = y[i] - x[i] - cbrt_ob[1];
        let gs = b[i] - cbrt_ob[2];
        let l = (gl * gl * gl + ob[0]) * scale;
        let mm = (gm * gm * gm + ob[1]) * scale;
        let s = (gs * gs * gs + ob[2]) * scale;
        r[i] = m[0][0] * l + m[0][1] * mm + m[0][2] * s;
        g[i] = m[1][0] * l + m[1][1] * mm + m[1][2] * s;
        bl[i] = m[2][0] * l + m[2][1] * mm + m[2][2] * s;
    }
    let mut planes = vec![r, g, bl];
    if img.colour.space == Space::Grey {
        let green = planes.swap_remove(1);
        let mut one = vec![green];
        for v in one[0].iter_mut() {
            *v = encode(img.colour.tf, f64::from(*v)) as f32;
        }
        return one;
    }
    linear_srgb_to_native(&img.colour, &mut planes);
    planes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_functions_round_trip() {
        for tf in [
            Tf::Srgb,
            Tf::Bt709,
            Tf::Gamma(2.2),
            Tf::Dci,
            Tf::Pq,
            Tf::Hlg,
            Tf::Linear,
        ] {
            for i in 1..=20 {
                let v = f64::from(i) / 20.0;
                let back = decode(tf, encode(tf, v));
                assert!((back - v).abs() < 1e-6, "{tf:?} {v} {back}");
            }
        }
    }

    #[test]
    fn display_p3_white_stays_white() {
        let c = Colour {
            want_icc: false,
            space: Space::Rgb,
            white: White::D65,
            prim: Prim::P3,
            tf: Tf::Srgb,
        };
        let mut planes = vec![vec![1.0f32], vec![1.0], vec![1.0]];
        native_to_srgb(&c, &mut planes);
        for p in &planes {
            assert!((p[0] - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn srgb_is_untouched() {
        let c = Colour {
            want_icc: false,
            space: Space::Rgb,
            white: White::D65,
            prim: Prim::Srgb,
            tf: Tf::Srgb,
        };
        let mut planes = vec![vec![0.3f32], vec![0.5], vec![0.7]];
        native_to_srgb(&c, &mut planes);
        assert_eq!(planes[0][0], 0.3);
    }
}
