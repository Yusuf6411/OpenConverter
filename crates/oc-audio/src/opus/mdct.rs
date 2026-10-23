//! MDCT through a mixed-radix complex FFT (sizes with factors 2, 3 and 5).
use std::f64::consts::PI;

#[derive(Clone, Copy, Default)]
struct C {
    re: f64,
    im: f64,
}
impl C {
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn add(self, o: C) -> C {
        C {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
}

struct Fft {
    n: usize,
    factors: Vec<usize>,
    tw: Vec<C>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let mut factors = Vec::new();
        let mut m = n;
        for p in [4usize, 2, 3, 5] {
            while m.is_multiple_of(p) && m > 1 {
                factors.push(p);
                m /= p;
            }
        }
        let mut f = 7;
        while m > 1 {
            while m.is_multiple_of(f) {
                factors.push(f);
                m /= f;
            }
            f += 2;
        }
        let tw = (0..n)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                C {
                    re: a.cos(),
                    im: a.sin(),
                }
            })
            .collect();
        Self { n, factors, tw }
    }

    /// Out-of-place transform of `n` points taken from `inp` with `stride`.
    fn run(&self, out: &mut [C], inp: &[C], stride: usize, level: usize, n: usize) {
        if n == 1 {
            out[0] = inp[0];
            return;
        }
        let p = self.factors[level];
        let m = n / p;
        for j in 0..p {
            let (a, b) = (j * m, (j + 1) * m);
            self.run(&mut out[a..b], &inp[j * stride..], stride * p, level + 1, m);
        }
        // combine: X[k + q m] = sum_j W_n^{j (k + q m)} Y_j[k]
        let tstep = self.n / n;
        let mut tmp = vec![C::default(); p];
        for k in 0..m {
            for j in 0..p {
                tmp[j] = out[j * m + k].mul(self.tw[(j * k * tstep) % self.n]);
            }
            for q in 0..p {
                let mut s = C::default();
                for (j, t) in tmp.iter().enumerate() {
                    s = s.add(t.mul(self.tw[((j * q * m) % n) * tstep % self.n]));
                }
                out[q * m + k] = s;
            }
        }
    }
}

/// An MDCT with `n` coefficients (windows of `2n` samples).
pub struct Mdct {
    n: usize,
    fft: Fft,
    pre: Vec<C>,
    twiddle: Vec<C>,
}

impl Mdct {
    pub fn new(n: usize) -> Self {
        let twiddle = (0..n / 2)
            .map(|k| {
                let a = -PI * (4 * k + 1) as f64 / (4 * n) as f64;
                C {
                    re: a.cos(),
                    im: a.sin(),
                }
            })
            .collect();
        let pre = (0..n / 2)
            .map(|k| {
                let a = -PI * k as f64 / n as f64;
                C {
                    re: a.cos(),
                    im: a.sin(),
                }
            })
            .collect();
        Self {
            n,
            fft: Fft::new(n / 2),
            pre,
            twiddle,
        }
    }

    /// DCT-IV of `u` (length n): X[k] = sum u[j] cos(pi/n (j+1/2)(k+1/2)).
    fn dct4(&self, u: &[f64], x: &mut [f64]) {
        let n = self.n;
        let h = n / 2;
        let mut t = vec![C::default(); h];
        for j in 0..h {
            t[j] = C {
                re: u[2 * j],
                im: u[n - 1 - 2 * j],
            }
            .mul(self.pre[j]);
        }
        let mut f = vec![C::default(); h];
        self.fft.run(&mut f, &t, 1, 0, h);
        for k in 0..h {
            let y = f[k].mul(self.twiddle[k]);
            x[2 * k] = y.re;
            x[n - 1 - 2 * k] = -y.im;
        }
    }

    /// Forward transform of a windowed block of `2n` samples.
    pub fn forward(&self, z: &[f32], out: &mut [f32]) {
        let n = self.n;
        let h = n / 2;
        let mut u = vec![0f64; n];
        for j in 0..h {
            // z = [a b c d]; u = (-c_r - d, a - b_r)
            u[j] = -f64::from(z[3 * h - 1 - j]) - f64::from(z[3 * h + j]);
            u[h + j] = f64::from(z[j]) - f64::from(z[n - 1 - j]);
        }
        let mut x = vec![0f64; n];
        self.dct4(&u, &mut x);
        for k in 0..n {
            out[k] = x[k] as f32;
        }
    }

    /// Inverse transform: `2n` time-aliased samples (to be windowed and overlap-added).
    /// `forward` followed by `inverse`, windowing with a Princen-Bradley window and
    /// overlap-adding at a hop of `n`, returns the input.
    pub fn inverse(&self, x: &[f32], out: &mut [f32]) {
        let n = self.n;
        let h = n / 2;
        let xs: Vec<f64> = x.iter().map(|&v| f64::from(v)).collect();
        let mut u = vec![0f64; n];
        self.dct4(&xs, &mut u);
        let s = 1.0 / n as f64;
        for j in 0..h {
            // inverse of the fold: [u2, -u2_r, -u1_r, -u1]
            out[j] = (u[h + j] * s) as f32;
            out[n - 1 - j] = (-u[h + j] * s) as f32;
            out[3 * h - 1 - j] = (-u[j] * s) as f32;
            out[3 * h + j] = (-u[j] * s) as f32;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn dct4_matches_definition() {
        for n in [8usize, 20, 120, 240] {
            let m = Mdct::new(n);
            let u: Vec<f64> = (0..n).map(|i| ((i * 7 + 3) % 11) as f64 - 5.0).collect();
            let mut x = vec![0.0; n];
            m.dct4(&u, &mut x);
            for k in 0..n {
                let r: f64 = (0..n)
                    .map(|j| u[j] * (PI / n as f64 * (j as f64 + 0.5) * (k as f64 + 0.5)).cos())
                    .sum();
                assert!((r - x[k]).abs() < 1e-9, "n={n} k={k}: {r} vs {}", x[k]);
            }
        }
    }

    #[test]
    fn mdct_matches_definition_and_reconstructs() {
        let n = 120;
        let m = Mdct::new(n);
        let z: Vec<f32> = (0..2 * n)
            .map(|i| (((i * 13 + 5) % 17) as f32 - 8.0) / 8.0)
            .collect();
        let mut x = vec![0f32; n];
        m.forward(&z, &mut x);
        for k in 0..n {
            let r: f64 = (0..2 * n)
                .map(|j| {
                    f64::from(z[j])
                        * (PI / n as f64 * (j as f64 + 0.5 + n as f64 / 2.0) * (k as f64 + 0.5))
                            .cos()
                })
                .sum();
            assert!((r - f64::from(x[k])).abs() < 1e-3, "k={k}: {r} vs {}", x[k]);
        }
        // overlap-add of three blocks with a sine window reconstructs the middle block
        let w: Vec<f32> = (0..2 * n)
            .map(|i| ((i as f64 + 0.5) * PI / (2 * n) as f64).sin() as f32)
            .collect();
        let sig: Vec<f32> = (0..4 * n)
            .map(|i| (((i * 31 + 7) % 23) as f32 - 11.0) / 11.0)
            .collect();
        let mut acc = vec![0f32; 4 * n];
        for b in 0..3 {
            let blk: Vec<f32> = (0..2 * n).map(|i| sig[b * n + i] * w[i]).collect();
            let mut c = vec![0f32; n];
            m.forward(&blk, &mut c);
            let mut y = vec![0f32; 2 * n];
            m.inverse(&c, &mut y);
            for i in 0..2 * n {
                acc[b * n + i] += y[i] * w[i] * 2.0;
            }
        }
        for i in n..2 * n {
            assert!(
                (acc[i] - sig[i]).abs() < 1e-4,
                "i={i}: {} vs {}",
                acc[i],
                sig[i]
            );
        }
    }
}
