//! Inverse discrete wavelet transforms (T.800 Annex F): reversible 5/3 and irreversible 9/7.

fn refl(i: isize, n: usize) -> usize {
    let n = n as isize;
    let mut i = i;
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    i = i.rem_euclid(period);
    if i >= n {
        i = period - i;
    }
    i as usize
}

/// One line, interleaved: `par` is the parity of the absolute index of element 0 (0 = low-pass).
pub fn inv53(a: &mut [i32], par: usize) {
    let n = a.len();
    if n == 1 {
        if par == 1 {
            a[0] >>= 1;
        }
        return;
    }
    // even absolute positions: indices i with (i + par) even
    let first_even = par; // index of first even-absolute element: i + par even -> i = par (0 or 1)
    let mut i = first_even;
    while i < n {
        let l = a[refl(i as isize - 1, n)];
        let r = a[refl(i as isize + 1, n)];
        a[i] -= (l + r + 2) >> 2;
        i += 2;
    }
    let mut i = 1 - first_even;
    while i < n {
        let l = a[refl(i as isize - 1, n)];
        let r = a[refl(i as isize + 1, n)];
        a[i] += (l + r) >> 1;
        i += 2;
    }
}

const ALPHA: f32 = -1.586_134_3;
const BETA: f32 = -0.052_980_12;
const GAMMA: f32 = 0.882_911_1;
const DELTA: f32 = 0.443_506_85;
const K: f32 = 1.230_174_1;

pub fn inv97(a: &mut [f32], par: usize) {
    let n = a.len();
    if n == 1 {
        if par == 1 {
            a[0] *= 0.5;
        }
        return;
    }
    let even0 = par;
    let odd0 = 1 - par;
    let mut i = even0;
    while i < n {
        a[i] *= K;
        i += 2;
    }
    let mut i = odd0;
    while i < n {
        a[i] *= 1.0 / K;
        i += 2;
    }
    let steps: [(usize, f32); 4] = [(even0, DELTA), (odd0, GAMMA), (even0, BETA), (odd0, ALPHA)];
    for (start, c) in steps {
        let mut i = start;
        while i < n {
            let l = a[refl(i as isize - 1, n)];
            let r = a[refl(i as isize + 1, n)];
            a[i] -= c * (l + r);
            i += 2;
        }
    }
}

/// Generic over the two sample types so tile-components can be handled uniformly.
pub trait Sample: Copy + Default {
    fn inv(line: &mut [Self], par: usize, reversible: bool);
}
impl Sample for i32 {
    fn inv(line: &mut [i32], par: usize, _r: bool) {
        inv53(line, par)
    }
}
impl Sample for f32 {
    fn inv(line: &mut [f32], par: usize, _r: bool) {
        inv97(line, par)
    }
}

/// Inverse transform of one decomposition level in place. The array (row stride `stride`) holds the
/// level's sub-bands in the Mallat layout: LL (`lw` x `lh`) top left, HL to its right, LH below,
/// HH diagonal; the result is the `rw` x `rh` resolution. `x0`/`y0` are the resolution's origin.
pub fn inverse_level<T: Sample>(
    buf: &mut [T],
    stride: usize,
    rw: usize,
    rh: usize,
    lw: usize,
    lh: usize,
    x0: usize,
    y0: usize,
    reversible: bool,
) {
    let mut line = vec![T::default(); rw.max(rh)];
    // horizontal
    for y in 0..rh {
        let row = &mut buf[y * stride..y * stride + rw];
        let par = x0 & 1;
        let (mut li, mut hi) = (0, lw);
        for (i, out) in line.iter_mut().take(rw).enumerate() {
            if (i + par) % 2 == 0 {
                *out = row[li];
                li += 1;
            } else {
                *out = row[hi];
                hi += 1;
            }
        }
        T::inv(&mut line[..rw], par, reversible);
        row.copy_from_slice(&line[..rw]);
    }
    // vertical
    for x in 0..rw {
        let par = y0 & 1;
        let (mut li, mut hi) = (0, lh);
        for (i, out) in line.iter_mut().take(rh).enumerate() {
            if (i + par) % 2 == 0 {
                *out = buf[li * stride + x];
                li += 1;
            } else {
                *out = buf[hi * stride + x];
                hi += 1;
            }
        }
        T::inv(&mut line[..rh], par, reversible);
        for (i, v) in line.iter().take(rh).enumerate() {
            buf[i * stride + x] = *v;
        }
    }
}
