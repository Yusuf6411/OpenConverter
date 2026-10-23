//! Pyramid vector quantiser: codeword counting, indexing and the pulse search.
use std::sync::LazyLock;

const DIM: usize = 200;

/// `U(n, k)` of the reference implementation, symmetric in its arguments.
static U: LazyLock<Vec<u64>> = LazyLock::new(|| {
    let mut t = vec![0u64; DIM * DIM];
    for n in 1..DIM {
        t[n * DIM + 1] = 1;
        t[DIM + n] = 1;
    }
    for n in 2..DIM {
        for k in 2..DIM {
            let v = t[(n - 1) * DIM + k]
                .saturating_add(t[n * DIM + k - 1])
                .saturating_add(t[(n - 1) * DIM + k - 1]);
            t[n * DIM + k] = v;
        }
    }
    t
});

pub fn u(n: usize, k: usize) -> u64 {
    if n >= DIM || k >= DIM {
        return u64::MAX;
    }
    U[n * DIM + k]
}

/// Number of codewords of dimension `n` with `k` pulses.
pub fn v(n: usize, k: usize) -> u64 {
    u(n, k).saturating_add(u(n, k + 1))
}

/// Index of `y` (n >= 2) among the codewords with the same pulse count.
pub fn icwrs(y: &[i32]) -> u32 {
    let n = y.len();
    let mut j = n - 1;
    let mut i = u64::from(y[j] < 0);
    let mut k = y[j].unsigned_abs() as usize;
    while j > 0 {
        j -= 1;
        i += u(n - j, k);
        k += y[j].unsigned_abs() as usize;
        if y[j] < 0 {
            i += u(n - j, k + 1);
        }
    }
    i as u32
}

/// The codeword with index `i`; returns the sum of squares.
pub fn cwrsi(mut n: usize, mut k: usize, mut i: u32, y: &mut [i32]) -> f32 {
    let mut yy = 0f32;
    let mut o = 0usize;
    let mut i64v = u64::from(i);
    let _ = &mut i;
    while n > 2 {
        let val: i32;
        if k >= n {
            let p = u(n, k + 1);
            let s = -i64::from(i64v >= p);
            i64v -= p & (s as u64);
            let k0 = k;
            let q = u(n, n);
            let mut pp;
            if q > i64v {
                k = n;
                loop {
                    k -= 1;
                    pp = u(k, n);
                    if pp <= i64v {
                        break;
                    }
                }
            } else {
                pp = u(n, k);
                while pp > i64v {
                    k -= 1;
                    pp = u(n, k);
                }
            }
            i64v -= pp;
            val = ((k0 - k) as i64 + s) as i32 ^ s as i32;
        } else {
            let p = u(k, n);
            let q = u(k + 1, n);
            if p <= i64v && i64v < q {
                i64v -= p;
                val = 0;
            } else {
                let s = -i64::from(i64v >= q);
                i64v -= q & (s as u64);
                let k0 = k;
                let mut pp;
                loop {
                    k -= 1;
                    pp = u(k, n);
                    if pp <= i64v {
                        break;
                    }
                }
                i64v -= pp;
                val = ((k0 - k) as i64 + s) as i32 ^ s as i32;
            }
        }
        y[o] = val;
        o += 1;
        yy += (val * val) as f32;
        n -= 1;
    }
    // n == 2
    let p = (2 * k + 1) as u64;
    let s = -i64::from(i64v >= p);
    i64v -= p & (s as u64);
    let k0 = k;
    k = ((i64v + 1) >> 1) as usize;
    if k > 0 {
        i64v -= (2 * k - 1) as u64;
    }
    let val = ((k0 - k) as i64 + s) as i32 ^ s as i32;
    y[o] = val;
    yy += (val * val) as f32;
    // n == 1
    let s = -(i64v as i64);
    let val = (k as i64 + s) as i32 ^ s as i32;
    y[o + 1] = val;
    yy += (val * val) as f32;
    yy
}

/// Greedy search for the K-pulse vector closest in direction to `x`.
pub fn search(x: &[f32], iy: &mut [i32], k: usize) {
    let n = x.len();
    let mut y = vec![0f32; n];
    let mut xa = vec![0f32; n];
    let mut signx = vec![false; n];
    for j in 0..n {
        signx[j] = x[j] < 0.0;
        xa[j] = x[j].abs();
        iy[j] = 0;
    }
    let mut xy = 0f32;
    let mut yy = 0f32;
    let mut left = k as i32;
    if k > n >> 1 {
        let mut sum: f32 = xa.iter().sum();
        if !(sum > 1e-15 && sum < 64.0) {
            xa[0] = 1.0;
            for v in &mut xa[1..] {
                *v = 0.0;
            }
            sum = 1.0;
        }
        let rcp = (k as f32 - 1.0 + 1e-15) / sum;
        for j in 0..n {
            iy[j] = (rcp * xa[j]).floor() as i32;
            y[j] = iy[j] as f32;
            yy += y[j] * y[j];
            xy += xa[j] * y[j];
            y[j] *= 2.0;
            left -= iy[j];
        }
    }
    if left > n as i32 + 3 {
        let tmp = left as f32;
        yy += tmp * tmp + tmp * y[0];
        iy[0] += left;
        left = 0;
    }
    for _ in 0..left.max(0) {
        yy += 1.0;
        let mut best_id = 0usize;
        let mut best_num = -1f32;
        let mut best_den = 0f32;
        for j in 0..n {
            let rxy = xy + xa[j];
            let ryy = yy + y[j];
            let rxy2 = rxy * rxy;
            if best_den * rxy2 > ryy * best_num {
                best_den = ryy;
                best_num = rxy2;
                best_id = j;
            }
        }
        xy += xa[best_id];
        yy += y[best_id];
        y[best_id] += 2.0;
        iy[best_id] += 1;
    }
    for j in 0..n {
        if signx[j] {
            iy[j] = -iy[j];
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn counts_match_small_cases() {
        assert_eq!(v(2, 1), 4);
        assert_eq!(v(2, 3), 12);
        assert_eq!(v(3, 2), 18);
        assert_eq!(v(4, 1), 8);
        assert_eq!(v(1, 5), 2);
    }

    #[test]
    fn index_round_trip_is_a_bijection() {
        for (n, k) in [(2usize, 3usize), (3, 2), (4, 3), (5, 4), (6, 2), (8, 5)] {
            let total = v(n, k) as u32;
            let mut seen = std::collections::HashSet::new();
            for i in 0..total {
                let mut y = vec![0i32; n];
                cwrsi(n, k, i, &mut y);
                assert_eq!(y.iter().map(|v| v.abs()).sum::<i32>() as usize, k, "n={n} k={k} i={i} {y:?}");
                assert_eq!(icwrs(&y), i, "n={n} k={k} {y:?}");
                assert!(seen.insert(y));
            }
        }
    }
}
