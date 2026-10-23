//! Inverse transforms, written as the spec describes them (8.7).
use crate::tables::COS64_LOOKUP;

fn cos64(angle: i32) -> i64 {
    let a = angle & 127;
    let v = if a <= 32 {
        COS64_LOOKUP[a as usize]
    } else if a <= 64 {
        -COS64_LOOKUP[(64 - a) as usize]
    } else if a <= 96 {
        -COS64_LOOKUP[(a - 64) as usize]
    } else {
        COS64_LOOKUP[(128 - a) as usize]
    };
    i64::from(v)
}

fn sin64(angle: i32) -> i64 {
    cos64(angle - 32)
}

fn round2(x: i64, n: u32) -> i64 {
    (x + (1 << (n - 1))) >> n
}

fn brev(bits: u32, x: usize) -> usize {
    let mut t = 0;
    for i in 0..bits {
        t += ((x >> i) & 1) << (bits - 1 - i);
    }
    t
}

fn bfly(t: &mut [i64], a: usize, b: usize, angle: i32, flip: bool) {
    let (ta, tb) = (t[a], t[b]);
    let x = ta * cos64(angle) - tb * sin64(angle);
    let y = ta * sin64(angle) + tb * cos64(angle);
    t[a] = round2(x, 14);
    t[b] = round2(y, 14);
    if flip {
        t.swap(a, b);
    }
}

fn hada(t: &mut [i64], a: usize, b: usize, flip: bool) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let (x, y) = (t[a], t[b]);
    t[a] = x + y;
    t[b] = x - y;
}

fn sbfly(t: &[i64], s: &mut [i64], a: usize, b: usize, angle: i32, flip: bool) {
    let x = t[a] * cos64(angle) - t[b] * sin64(angle);
    let y = t[a] * sin64(angle) + t[b] * cos64(angle);
    s[a] = x;
    s[b] = y;
    if flip {
        s.swap(a, b);
    }
}

fn shada(t: &mut [i64], s: &[i64], a: usize, b: usize) {
    t[a] = round2(s[a] + s[b], 14);
    t[b] = round2(s[a] - s[b], 14);
}

fn idct_core(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let n1 = n0 >> 1;
    let n2 = n0 >> 2;
    let n3 = n0 >> 3;
    let nn = n as usize;
    if n == 2 {
        bfly(t, 0, 1, 16, true);
    } else {
        idct_core(t, n - 1);
    }
    for i in 0..n2 {
        bfly(t, n1 + i, n0 - 1 - i, 32 - brev(5, n1 + i) as i32, false);
    }
    if n >= 3 {
        for i in 0..n3 {
            for j in 0..2 {
                hada(t, n1 + 4 * i + 2 * j, n1 + 1 + 4 * i + 2 * j, j == 1);
            }
        }
    }
    if n == 5 {
        for i in 0..2usize {
            for j in 0..2usize {
                bfly(
                    t,
                    n0 - nn + 3 - n2 * j - 4 * i,
                    n1 + nn - 4 + n2 * j + 4 * i,
                    28 - 16 * i as i32 + 56 * j as i32,
                    true,
                );
            }
        }
        for i in 0..2usize {
            for j in 0..4usize {
                hada(t, n1 + n3 * j + i, n1 + n2 - 5 + n3 * j - i, j & 1 == 1);
            }
        }
    }
    if n >= 4 {
        for i in 0..=usize::from(n == 5) {
            for j in 0..2usize {
                bfly(
                    t,
                    n0 - nn + 2 - i - n2 * j,
                    n1 + nn - 3 + i + n2 * j,
                    24 + 48 * j as i32,
                    true,
                );
            }
        }
        for i in 0..=(2 * nn - 7) {
            for j in 0..2usize {
                hada(t, n1 + n2 * j + i, n1 + n2 - 1 + n2 * j - i, j & 1 == 1);
            }
        }
    }
    if n >= 3 {
        for i in 0..n3 {
            bfly(t, n0 - n3 - 1 - i, n1 + n3 + i, 16, true);
        }
    }
    for i in 0..n1 {
        hada(t, i, n0 - 1 - i, false);
    }
}

fn idct(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i64; 32];
    copy[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n0 {
        t[i] = copy[brev(n, i)];
    }
    idct_core(t, n);
}

fn adst_in_perm(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let n1 = n0 >> 1;
    let mut copy = [0i64; 16];
    copy[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n1 {
        t[2 * i] = copy[n0 - 1 - 2 * i];
        t[2 * i + 1] = copy[2 * i];
    }
}

fn adst_out_perm(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i64; 16];
    copy[..n0].copy_from_slice(&t[..n0]);
    if n == 4 {
        for a in 0..2usize {
            for b in 0..2usize {
                for c in 0..2usize {
                    for d in 0..2usize {
                        t[8 * a + 4 * b + 2 * c + d] =
                            copy[8 * (d ^ c) + 4 * (c ^ b) + 2 * (b ^ a) + a];
                    }
                }
            }
        }
    } else {
        for a in 0..2usize {
            for b in 0..2usize {
                for c in 0..2usize {
                    t[4 * a + 2 * b + c] = copy[4 * (c ^ b) + 2 * (b ^ a) + a];
                }
            }
        }
    }
}

fn iadst4(t: &mut [i64]) {
    const S1: i64 = 5283;
    const S2: i64 = 9929;
    const S3: i64 = 13377;
    const S4: i64 = 15212;
    let s0 = S1 * t[0];
    let s1 = S2 * t[0];
    let s2 = S3 * t[1];
    let s3 = S4 * t[2];
    let s4 = S1 * t[2];
    let s5 = S2 * t[3];
    let s6 = S4 * t[3];
    let v = t[0] - t[2] + t[3];
    let s7 = S3 * v;
    let x0 = s0 + s3 + s5;
    let x1 = s1 - s4 - s6;
    let x2 = s7;
    let x3 = s2;
    t[0] = round2(x0 + x3, 14);
    t[1] = round2(x1 + x3, 14);
    t[2] = round2(x2, 14);
    t[3] = round2(x0 + x1 - x3, 14);
}

fn iadst8(t: &mut [i64]) {
    let mut s = [0i64; 8];
    adst_in_perm(t, 3);
    for i in 0..4 {
        sbfly(t, &mut s, 2 * i, 1 + 2 * i, 30 - 8 * i as i32, true);
    }
    for i in 0..4 {
        shada(t, &s, i, 4 + i);
    }
    for i in 0..2 {
        sbfly(t, &mut s, 4 + 3 * i, 5 + i, 24 - 16 * i as i32, true);
    }
    for i in 0..2 {
        shada(t, &s, 4 + i, 6 + i);
    }
    for i in 0..2 {
        hada(t, i, 2 + i, false);
    }
    for i in 0..2 {
        bfly(t, 2 + 4 * i, 3 + 4 * i, 16, true);
    }
    adst_out_perm(t, 3);
    for i in 0..4 {
        t[1 + 2 * i] = -t[1 + 2 * i];
    }
}

fn iadst16(t: &mut [i64]) {
    let mut s = [0i64; 16];
    adst_in_perm(t, 4);
    for i in 0..8 {
        sbfly(t, &mut s, 2 * i, 1 + 2 * i, 31 - 4 * i as i32, true);
    }
    for i in 0..8 {
        shada(t, &s, i, 8 + i);
    }
    for i in 0..4 {
        sbfly(t, &mut s, 8 + 2 * i, 9 + 2 * i, 28 - 16 * i as i32, true);
    }
    for i in 0..4 {
        shada(t, &s, 8 + i, 12 + i);
    }
    for i in 0..4 {
        hada(t, i, 4 + i, false);
    }
    for i in 0..2 {
        for j in 0..2 {
            sbfly(
                t,
                &mut s,
                4 + 8 * i + 3 * j,
                5 + 8 * i + j,
                24 - 16 * j as i32,
                true,
            );
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            shada(t, &s, 4 + 8 * j + i, 6 + 8 * j + i);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            hada(t, 8 * j + i, 2 + 8 * j + i, false);
        }
    }
    for i in 0..2usize {
        for j in 0..2usize {
            bfly(
                t,
                2 + 4 * j + 8 * i,
                3 + 4 * j + 8 * i,
                48 + 64 * (i ^ j) as i32,
                false,
            );
        }
    }
    adst_out_perm(t, 4);
    for i in 0..2 {
        for j in 0..2 {
            t[1 + 12 * j + 2 * i] = -t[1 + 12 * j + 2 * i];
        }
    }
}

fn iadst(t: &mut [i64], n: u32) {
    match n {
        2 => iadst4(t),
        3 => iadst8(t),
        _ => iadst16(t),
    }
}

fn iwht(t: &mut [i64], shift: u32) {
    let mut a = t[0] >> shift;
    let mut c = t[1] >> shift;
    let mut d = t[2] >> shift;
    let mut b = t[3] >> shift;
    a += c;
    d -= b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;
    t[0] = a;
    t[1] = b;
    t[2] = c;
    t[3] = d;
}

pub const DCT_DCT: u8 = 0;
pub const ADST_DCT: u8 = 1;
pub const DCT_ADST: u8 = 2;

/// 2D inverse transform of the `n0 x n0` block in `block` (row major), in place.
/// `tx_type` is one of DCT_DCT, ADST_DCT (ADST on columns), DCT_ADST (ADST on rows), ADST_ADST.
pub fn inverse_2d(block: &mut [i32], n: u32, tx_type: u8, lossless: bool) {
    let n0 = 1usize << n;
    let mut t = [0i64; 32];
    for i in 0..n0 {
        for j in 0..n0 {
            t[j] = i64::from(block[i * n0 + j]);
        }
        if lossless {
            iwht(&mut t, 2);
        } else if tx_type == DCT_DCT || tx_type == ADST_DCT {
            idct(&mut t, n);
        } else {
            iadst(&mut t, n);
        }
        for j in 0..n0 {
            block[i * n0 + j] = t[j] as i32;
        }
    }
    for j in 0..n0 {
        for i in 0..n0 {
            t[i] = i64::from(block[i * n0 + j]);
        }
        if lossless {
            iwht(&mut t, 0);
        } else if tx_type == DCT_DCT || tx_type == DCT_ADST {
            idct(&mut t, n);
        } else {
            iadst(&mut t, n);
        }
        for i in 0..n0 {
            block[i * n0 + j] = if lossless {
                t[i] as i32
            } else {
                round2(t[i], (n + 2).min(6)) as i32
            };
        }
    }
}
