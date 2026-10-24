//! Inverse MDCT through a DCT-IV computed with a complex FFT of a quarter of the window.
use std::f64::consts::PI;

pub struct Imdct {
    /// Window length N; the transform takes N/2 coefficients and gives N samples.
    n: usize,
    twiddle: Vec<(f32, f32)>, // pre-rotation, N/4 entries
    post: Vec<(f32, f32)>,    // post-rotation, N/4 entries
    fft: Fft,
}

struct Fft {
    n: usize,
    rev: Vec<u32>,
    tw: Vec<(f32, f32)>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let bits = n.trailing_zeros();
        let rev = (0..n as u32)
            .map(|i| {
                if bits == 0 {
                    0
                } else {
                    i.reverse_bits() >> (32 - bits)
                }
            })
            .collect();
        let tw = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        Self { n, rev, tw }
    }
    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = self.tw[k * step];
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

impl Imdct {
    pub fn new(n: usize) -> Self {
        assert!(n >= 16 && n.is_power_of_two());
        let m = n / 2; // DCT-IV length
        let twiddle = (0..m / 2)
            .map(|k| {
                let a = -PI * (4 * k + 1) as f64 / (4 * m) as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        let post = (0..m / 2)
            .map(|k| {
                let a = -PI * k as f64 / m as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        Self {
            n,
            twiddle,
            post,
            fft: Fft::new(m / 2),
        }
    }

    /// DCT-IV: t[n] = sum_k x[k] cos(pi/M (n + 1/2)(k + 1/2)).
    fn dct4(&self, x: &[f32], t: &mut [f32]) {
        let m = self.n / 2;
        let h = m / 2;
        let mut re = vec![0.0f32; h];
        let mut im = vec![0.0f32; h];
        for k in 0..h {
            let (c, s) = self.twiddle[k];
            let a = x[2 * k];
            let b = x[m - 1 - 2 * k];
            re[k] = a * c - b * s;
            im[k] = a * s + b * c;
        }
        self.fft.run(&mut re, &mut im);
        for k in 0..h {
            let (c, s) = self.post[k];
            let yr = re[k] * c - im[k] * s;
            let yi = re[k] * s + im[k] * c;
            t[2 * k] = yr;
            t[m - 1 - 2 * k] = -yi;
        }
    }

    /// Forward MDCT of N windowed samples to N/2 coefficients, scaled so that this inverse
    /// followed by a power-complementary window and overlap-add restores the input.
    pub fn forward(&self, x: &[f32], out: &mut [f32]) {
        let m = self.n / 2;
        let h = m / 2;
        let mut u = vec![0.0f32; m];
        for i in 0..h {
            u[h + i] += x[i];
        }
        for i in h..3 * h {
            u[3 * h - 1 - i] -= x[i];
        }
        for i in 3 * h..4 * h {
            u[i - 3 * h] -= x[i];
        }
        self.dct4(&u, out);
        let scale = 2.0 / m as f32;
        for v in out.iter_mut() {
            *v *= scale;
        }
    }

    /// N/2 coefficients to N samples, with the Vorbis phase, without any scaling.
    pub fn inverse(&self, x: &[f32], out: &mut [f32]) {
        let m = self.n / 2;
        let mut t = vec![0.0f32; m];
        self.dct4(x, &mut t);
        let h = m / 2;
        out[..h].copy_from_slice(&t[h..2 * h]);
        for i in h..3 * h {
            out[i] = -t[3 * h - 1 - i];
        }
        for i in 3 * h..4 * h {
            out[i] = -t[i - 3 * h];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_direct_formula() {
        for n in [16usize, 64, 256, 2048] {
            let m = n / 2;
            let x: Vec<f32> = (0..m)
                .map(|i| ((i * 37 + 11) % 101) as f32 / 50.0 - 1.0)
                .collect();
            let mut y = vec![0.0f32; n];
            Imdct::new(n).inverse(&x, &mut y);
            for (i, &got) in y.iter().enumerate() {
                let mut acc = 0.0f64;
                for (k, &xk) in x.iter().enumerate() {
                    acc += f64::from(xk)
                        * (2.0 * PI / n as f64
                            * (i as f64 + 0.5 + n as f64 / 4.0)
                            * (k as f64 + 0.5))
                            .cos();
                }
                assert!(
                    (acc - f64::from(got)).abs() < 1e-3 * (1.0 + acc.abs()),
                    "n {n} i {i}: {acc} vs {got}"
                );
            }
        }
    }
}
