//! The restoration filters of a VarDCT frame: the Gabor-like 3x3 smoothing and the
//! edge-preserving filter (EPF), both working on the XYB planes.
use crate::header::Epf;

/// 3x3 convolution with edge replication; `w` are the side and diagonal weights.
pub fn gabor(plane: &mut [f32], width: usize, height: usize, w: [f32; 2]) {
    let [w0, w1] = w;
    let norm = (1.0 + 4.0 * w0 + 4.0 * w1).recip();
    let src = plane.to_vec();
    let at = |x: isize, y: isize| -> f32 {
        let x = x.clamp(0, width as isize - 1) as usize;
        let y = y.clamp(0, height as isize - 1) as usize;
        src[y * width + x]
    };
    for y in 0..height {
        for x in 0..width {
            let (xi, yi) = (x as isize, y as isize);
            let side = at(xi - 1, yi) + at(xi + 1, yi) + at(xi, yi - 1) + at(xi, yi + 1);
            let diag =
                at(xi - 1, yi - 1) + at(xi + 1, yi - 1) + at(xi - 1, yi + 1) + at(xi + 1, yi + 1);
            plane[y * width + x] = (src[y * width + x] + side * w0 + diag * w1) * norm;
        }
    }
}

fn mirror(mut i: isize, len: usize) -> usize {
    loop {
        if i < 0 {
            i = -(i + 1);
        } else if i as usize >= len {
            i = 2 * len as isize - 1 - i;
        } else {
            return i as usize;
        }
    }
}

const KERNEL_1: [(isize, isize); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];
const KERNEL_2: [(isize, isize); 12] = [
    (0, -2),
    (-1, -1),
    (0, -1),
    (1, -1),
    (-2, 0),
    (-1, 0),
    (1, 0),
    (2, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
    (0, 2),
];
const DIST_0: [(isize, isize); 5] = [(0, -1), (1, 0), (0, 0), (-1, 0), (0, 1)];
const DIST_1: [(isize, isize); 5] = [(0, -1), (0, 0), (0, 1), (-1, 0), (1, 0)];
const DIST_2: [(isize, isize); 1] = [(0, 0)];

const PAD: usize = 3;

fn step(
    input: &[Vec<f32>; 3],
    output: &mut [Vec<f32>; 3],
    width: usize,
    height: usize,
    sigma: &[f32],
    sigma_stride: usize,
    e: &Epf,
    which: usize,
) {
    let (kernel, dist_offsets): (&[(isize, isize)], &[(isize, isize)]) = match which {
        0 => (&KERNEL_2, &DIST_0),
        1 => (&KERNEL_1, &DIST_1),
        _ => (&KERNEL_1, &DIST_2),
    };
    let step_mul = match which {
        0 => e.pass0_sigma_scale,
        2 => e.pass2_sigma_scale,
        _ => 1.0,
    };
    let pw = width + 2 * PAD;
    let ph = height + 2 * PAD;
    // edge-mirrored copies so the taps need no bounds logic
    let padded: [Vec<f32>; 3] = std::array::from_fn(|c| {
        let mut p = vec![0f32; pw * ph];
        for y in 0..ph {
            let sy = mirror(y as isize - PAD as isize, height);
            for x in 0..pw {
                p[y * pw + x] = input[c][sy * width + mirror(x as isize - PAD as isize, width)];
            }
        }
        p
    });
    let neg = 6.6 * (std::f32::consts::FRAC_1_SQRT_2 - 1.0);
    for y in 0..height {
        let y_border = (y + 1) & 6 == 0;
        for x in 0..width {
            let s = sigma[(y / 8) * sigma_stride + x / 8];
            let at = (y + PAD) * pw + x + PAD;
            if s < 0.3 {
                for c in 0..3 {
                    output[c][y * width + x] = input[c][y * width + x];
                }
                continue;
            }
            let mut sm = step_mul;
            if y_border || x & 7 == 0 || x & 7 == 7 {
                sm *= e.border_sad_mul;
            }
            let inv = neg / s * sm;
            let mut sum_w = 1.0f32;
            let mut sum = [padded[0][at], padded[1][at], padded[2][at]];
            for &(kx, ky) in kernel {
                let k = at as isize + ky * pw as isize + kx;
                let mut d = 0f32;
                for c in 0..3 {
                    let mut acc = 0f32;
                    for &(ix, iy) in dist_offsets {
                        let off = iy * pw as isize + ix;
                        acc += (padded[c][(k + off) as usize]
                            - padded[c][(at as isize + off) as usize])
                            .abs();
                    }
                    d += e.channel_scale[c] * acc;
                }
                let wt = (1.0 + d * inv).max(0.0);
                sum_w += wt;
                for c in 0..3 {
                    sum[c] += wt * padded[c][k as usize];
                }
            }
            for c in 0..3 {
                output[c][y * width + x] = sum[c] / sum_w;
            }
        }
    }
}

/// Run the EPF passes the frame header asks for.
pub fn epf(
    planes: &mut [Vec<f32>; 3],
    width: usize,
    height: usize,
    sigma: &[f32],
    sigma_stride: usize,
    e: &Epf,
) {
    let steps: &[usize] = match e.iters {
        1 => &[1],
        2 => &[1, 2],
        _ => &[0, 1, 2],
    };
    let mut out: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; width * height]);
    for &s in steps {
        step(planes, &mut out, width, height, sigma, sigma_stride, e, s);
        std::mem::swap(planes, &mut out);
    }
}
