//! Dequantization, inverse transforms and reconstruction (sections 7.12.3 and 7.13).
use crate::consts::*;
use crate::dec::*;
use crate::tables as t;

fn cos128(angle: i32) -> i64 {
    let a = (angle & 255) as usize;
    let v = if a <= 64 {
        i64::from(t::COS128_LOOKUP[a])
    } else if a <= 128 {
        -i64::from(t::COS128_LOOKUP[128 - a])
    } else if a <= 192 {
        -i64::from(t::COS128_LOOKUP[a - 128])
    } else {
        i64::from(t::COS128_LOOKUP[256 - a])
    };
    v
}

fn sin128(angle: i32) -> i64 {
    cos128(angle - 64)
}

fn brev(bits: u32, x: usize) -> usize {
    let mut r = 0;
    for i in 0..bits {
        r += ((x >> i) & 1) << (bits - 1 - i);
    }
    r
}

#[inline]
fn bf(tt: &mut [i32], a: usize, b: usize, angle: i32, flip: bool) {
    let ta = i64::from(tt[a]);
    let tb = i64::from(tt[b]);
    let x = ta * cos128(angle) - tb * sin128(angle);
    let y = ta * sin128(angle) + tb * cos128(angle);
    let x = ((x + 2048) >> 12) as i32;
    let y = ((y + 2048) >> 12) as i32;
    if flip {
        tt[a] = y;
        tt[b] = x;
    } else {
        tt[a] = x;
        tt[b] = y;
    }
}

#[inline]
fn hd(tt: &mut [i32], a: usize, b: usize, flip: bool, r: u32) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let x = tt[a];
    let y = tt[b];
    let lo = -(1i32 << (r - 1));
    let hi = (1i32 << (r - 1)) - 1;
    tt[a] = (x + y).clamp(lo, hi);
    tt[b] = (x - y).clamp(lo, hi);
}

fn idct(tt: &mut [i32], n: u32, r: u32) {
    let len = 1usize << n;
    let mut copy = [0i32; 64];
    copy[..len].copy_from_slice(&tt[..len]);
    for i in 0..len {
        tt[i] = copy[brev(n, i)];
    }
    let u = |x: usize| x as i32;
    if n == 6 {
        for i in 0..16 {
            bf(tt, 32 + i, 63 - i, 63 - 4 * u(brev(4, i)), false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            bf(tt, 16 + i, 31 - i, 6 + (u(brev(3, 7 - i)) << 3), false);
        }
    }
    if n == 6 {
        for i in 0..16 {
            hd(tt, 32 + i * 2, 33 + i * 2, i & 1 == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            bf(tt, 8 + i, 15 - i, 12 + (u(brev(2, 3 - i)) << 4), false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            hd(tt, 16 + 2 * i, 17 + 2 * i, i & 1 == 1, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                bf(
                    tt,
                    62 - i * 4 - j,
                    33 + i * 4 + j,
                    60 - 16 * u(brev(2, i)) + 64 * u(j),
                    true,
                );
            }
        }
    }
    if n >= 3 {
        for i in 0..2 {
            bf(tt, 4 + i, 7 - i, 56 - 32 * u(i), false);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            hd(tt, 8 + 2 * i, 9 + 2 * i, i & 1 == 1, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                bf(
                    tt,
                    30 - 4 * i - j,
                    17 + 4 * i + j,
                    24 + (u(j) << 6) + ((1 - u(i)) << 5),
                    true,
                );
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hd(tt, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 == 1, r);
            }
        }
    }
    for i in 0..2 {
        bf(tt, 2 * i, 2 * i + 1, 32 + 16 * u(i), i == 0);
    }
    if n >= 3 {
        for i in 0..2 {
            hd(tt, 4 + 2 * i, 5 + 2 * i, i == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            bf(tt, 14 - i, 9 + i, 48 + 64 * u(i), true);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hd(tt, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                bf(
                    tt,
                    61 - i * 8 - j,
                    34 + i * 8 + j,
                    56 - u(i) * 32 + u(j >> 1) * 64,
                    true,
                );
            }
        }
    }
    for i in 0..2 {
        hd(tt, i, 3 - i, false, r);
    }
    if n >= 3 {
        bf(tt, 6, 5, 32, true);
    }
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hd(tt, 8 + 4 * i + j, 11 + 4 * i - j, i == 1, r);
            }
        }
    }
    if n >= 5 {
        for i in 0..4 {
            bf(tt, 29 - i, 18 + i, 48 + u(i >> 1) * 64, true);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hd(tt, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..4 {
            hd(tt, i, 7 - i, false, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            bf(tt, 13 - i, 10 + i, 32, true);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hd(tt, 16 + i * 8 + j, 23 + i * 8 - j, i == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            bf(tt, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true);
        }
    }
    if n >= 4 {
        for i in 0..8 {
            hd(tt, i, 15 - i, false, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            bf(tt, 27 - i, 20 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..8 {
            hd(tt, 32 + i, 47 - i, false, r);
            hd(tt, 48 + i, 63 - i, true, r);
        }
    }
    if n >= 5 {
        for i in 0..16 {
            hd(tt, i, 31 - i, false, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            bf(tt, 55 - i, 40 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..32 {
            hd(tt, i, 63 - i, false, r);
        }
    }
}

fn adst_in_perm(tt: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i32; 16];
    copy[..n0].copy_from_slice(&tt[..n0]);
    for i in 0..n0 {
        let idx = if i & 1 == 1 { i - 1 } else { n0 - i - 1 };
        tt[i] = copy[idx];
    }
}

fn adst_out_perm(tt: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i32; 16];
    copy[..n0].copy_from_slice(&tt[..n0]);
    for i in 0..n0 {
        let a = (i >> 3) & 1;
        let b = ((i >> 2) & 1) ^ ((i >> 3) & 1);
        let c = ((i >> 1) & 1) ^ ((i >> 2) & 1);
        let d = (i & 1) ^ ((i >> 1) & 1);
        let idx = ((d << 3) | (c << 2) | (b << 1) | a) >> (4 - n);
        tt[i] = if i & 1 == 1 { -copy[idx] } else { copy[idx] };
    }
}

fn iadst4(tt: &mut [i32]) {
    const S1: i64 = 1321;
    const S2: i64 = 2482;
    const S3: i64 = 3344;
    const S4: i64 = 3803;
    let t0 = i64::from(tt[0]);
    let t1 = i64::from(tt[1]);
    let t2 = i64::from(tt[2]);
    let t3 = i64::from(tt[3]);
    let mut s = [0i64; 7];
    s[0] = S1 * t0;
    s[1] = S2 * t0;
    s[2] = S3 * t1;
    s[3] = S4 * t2;
    s[4] = S1 * t2;
    s[5] = S2 * t3;
    s[6] = S4 * t3;
    let a7 = t0 - t2;
    let b7 = a7 + t3;
    s[0] += s[3];
    s[1] -= s[4];
    s[3] = s[2];
    s[2] = S3 * b7;
    s[0] += s[5];
    s[1] -= s[6];
    let x0 = s[0] + s[3];
    let x1 = s[1] + s[3];
    let x2 = s[2];
    let x3 = s[0] + s[1] - s[3];
    tt[0] = ((x0 + 2048) >> 12) as i32;
    tt[1] = ((x1 + 2048) >> 12) as i32;
    tt[2] = ((x2 + 2048) >> 12) as i32;
    tt[3] = ((x3 + 2048) >> 12) as i32;
}

fn iadst8(tt: &mut [i32], r: u32) {
    adst_in_perm(tt, 3);
    for i in 0..4 {
        bf(tt, 2 * i, 2 * i + 1, 60 - 16 * i as i32, true);
    }
    for i in 0..4 {
        hd(tt, i, 4 + i, false, r);
    }
    for i in 0..2 {
        bf(tt, 4 + 3 * i, 5 + i, 48 - 32 * i as i32, true);
    }
    for i in 0..2 {
        for j in 0..2 {
            hd(tt, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..2 {
        bf(tt, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_out_perm(tt, 3);
}

fn iadst16(tt: &mut [i32], r: u32) {
    adst_in_perm(tt, 4);
    for i in 0..8 {
        bf(tt, 2 * i, 2 * i + 1, 62 - 8 * i as i32, true);
    }
    for i in 0..8 {
        hd(tt, i, 8 + i, false, r);
    }
    for i in 0..2 {
        bf(tt, 8 + 2 * i, 9 + 2 * i, 56 - 32 * i as i32, true);
        bf(tt, 13 + 2 * i, 12 + 2 * i, 8 + 32 * i as i32, true);
    }
    for i in 0..4 {
        for j in 0..2 {
            hd(tt, 8 * j + i, 4 + 8 * j + i, false, r);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            bf(
                tt,
                4 + 8 * j + 3 * i,
                5 + 8 * j + i,
                48 - 32 * i as i32,
                true,
            );
        }
    }
    for i in 0..2 {
        for j in 0..4 {
            hd(tt, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..4 {
        bf(tt, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_out_perm(tt, 4);
}

fn iadst(tt: &mut [i32], n: u32, r: u32) {
    match n {
        2 => iadst4(tt),
        3 => iadst8(tt, r),
        _ => iadst16(tt, r),
    }
}

fn iwht(tt: &mut [i32], shift: u32) {
    let mut a = tt[0] >> shift;
    let mut c = tt[1] >> shift;
    let mut d = tt[2] >> shift;
    let mut b = tt[3] >> shift;
    a += c;
    d -= b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;
    tt[0] = a;
    tt[1] = b;
    tt[2] = c;
    tt[3] = d;
}

fn iidentity(tt: &mut [i32], n: u32) {
    let len = 1usize << n;
    for v in tt.iter_mut().take(len) {
        *v = match n {
            2 => ((i64::from(*v) * 5793 + 2048) >> 12) as i32,
            3 => *v * 2,
            4 => ((i64::from(*v) * 11586 + 2048) >> 12) as i32,
            _ => *v * 4,
        };
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Dct,
    Adst,
    Id,
}

fn row_kind(tt: u8) -> Kind {
    match tt {
        DCT_DCT | ADST_DCT | FLIPADST_DCT | H_DCT => Kind::Dct,
        DCT_ADST | ADST_ADST | DCT_FLIPADST | FLIPADST_FLIPADST | ADST_FLIPADST | FLIPADST_ADST
        | H_ADST | H_FLIPADST => Kind::Adst,
        _ => Kind::Id,
    }
}

fn col_kind(tt: u8) -> Kind {
    match tt {
        DCT_DCT | DCT_ADST | DCT_FLIPADST | V_DCT => Kind::Dct,
        ADST_DCT | ADST_ADST | FLIPADST_DCT | FLIPADST_FLIPADST | ADST_FLIPADST | FLIPADST_ADST
        | V_ADST | V_FLIPADST => Kind::Adst,
        _ => Kind::Id,
    }
}

/// 2D inverse transform (7.13.3). `dq` is the `th x tw` dequantized block (row stride `tw`);
/// the result is written to `res` with row stride `w`.
pub fn inverse_transform_2d(
    dq: &[i32],
    res: &mut [i32],
    tx_sz: usize,
    tx_type: u8,
    lossless: bool,
    bit_depth: u32,
) {
    let log2w = u32::from(t::TX_WIDTH_LOG2[tx_sz]);
    let log2h = u32::from(t::TX_HEIGHT_LOG2[tx_sz]);
    let w = 1usize << log2w;
    let h = 1usize << log2h;
    let tw = w.min(32);
    let row_shift = if lossless {
        0
    } else {
        u32::from(t::TRANSFORM_ROW_SHIFT[tx_sz])
    };
    let col_shift = if lossless { 0 } else { 4 };
    let row_clamp = bit_depth + 8;
    let col_clamp = (bit_depth + 6).max(16);
    let rk = row_kind(tx_type);
    let ck = col_kind(tx_type);
    let rect = (log2w as i32 - log2h as i32).abs() == 1;
    let mut tt = [0i32; 64];
    let clo = -(1i32 << (col_clamp - 1));
    let chi = (1i32 << (col_clamp - 1)) - 1;
    for i in 0..h {
        if i >= 32 {
            for j in 0..w {
                res[i * w + j] = 0;
            }
            continue;
        }
        for j in 0..w {
            tt[j] = if j < 32 { dq[i * tw + j] } else { 0 };
        }
        if rect {
            for v in tt.iter_mut().take(w) {
                *v = ((i64::from(*v) * 2896 + 2048) >> 12) as i32;
            }
        }
        if lossless {
            iwht(&mut tt, 2);
        } else {
            match rk {
                Kind::Dct => idct(&mut tt, log2w, row_clamp),
                Kind::Adst => iadst(&mut tt, log2w, row_clamp),
                Kind::Id => iidentity(&mut tt, log2w),
            }
        }
        for j in 0..w {
            let v = if row_shift > 0 {
                (tt[j] + (1 << (row_shift - 1))) >> row_shift
            } else {
                tt[j]
            };
            res[i * w + j] = v.clamp(clo, chi);
        }
    }
    for j in 0..w {
        for i in 0..h {
            tt[i] = res[i * w + j];
        }
        if lossless {
            iwht(&mut tt, 0);
        } else {
            match ck {
                Kind::Dct => idct(&mut tt, log2h, col_clamp),
                Kind::Adst => iadst(&mut tt, log2h, col_clamp),
                Kind::Id => iidentity(&mut tt, log2h),
            }
        }
        for i in 0..h {
            res[i * w + j] = if col_shift > 0 {
                (tt[i] + (1 << (col_shift - 1))) >> col_shift
            } else {
                tt[i]
            };
        }
    }
}

impl Dec {
    fn dc_q(&self, b: i32) -> i32 {
        i32::from(t::DC_QLOOKUP[((self.bit_depth - 8) >> 1) as usize][b.clamp(0, 255) as usize])
    }
    fn ac_q(&self, b: i32) -> i32 {
        i32::from(t::AC_QLOOKUP[((self.bit_depth - 8) >> 1) as usize][b.clamp(0, 255) as usize])
    }

    fn get_dc_quant(&self, plane: usize) -> i32 {
        let q = self
            .hdr
            .get_qindex(false, self.b.segment_id, self.current_q_index);
        match plane {
            0 => self.dc_q(q + self.hdr.delta_q_y_dc),
            1 => self.dc_q(q + self.hdr.delta_q_u_dc),
            _ => self.dc_q(q + self.hdr.delta_q_v_dc),
        }
    }

    fn get_ac_quant(&self, plane: usize) -> i32 {
        let q = self
            .hdr
            .get_qindex(false, self.b.segment_id, self.current_q_index);
        match plane {
            0 => self.ac_q(q),
            1 => self.ac_q(q + self.hdr.delta_q_u_ac),
            _ => self.ac_q(q + self.hdr.delta_q_v_ac),
        }
    }

    pub fn reconstruct(&mut self, plane: usize, x: usize, y: usize, tx_sz: usize) {
        let dq_denom = match tx_sz {
            TX_32X32 | TX_16X32 | TX_32X16 | TX_16X64 | TX_64X16 => 2,
            TX_64X64 | TX_32X64 | TX_64X32 => 4,
            _ => 1,
        };
        let log2w = usize::from(t::TX_WIDTH_LOG2[tx_sz]);
        let log2h = usize::from(t::TX_HEIGHT_LOG2[tx_sz]);
        let w = 1usize << log2w;
        let h = 1usize << log2h;
        let tw = w.min(32);
        let th = h.min(32);
        let tx_type = self.b.plane_tx_type;
        let flip_ud = matches!(
            tx_type,
            FLIPADST_DCT | FLIPADST_ADST | V_FLIPADST | FLIPADST_FLIPADST
        );
        let flip_lr = matches!(
            tx_type,
            DCT_FLIPADST | ADST_FLIPADST | H_FLIPADST | FLIPADST_FLIPADST
        );
        let dc = self.get_dc_quant(plane);
        let ac = self.get_ac_quant(plane);
        let qm_level = self.hdr.seg_qm_level[plane][self.b.segment_id] as usize;
        let use_qm = self.hdr.using_qmatrix && tx_type < IDTX && qm_level < 15;
        let lim = 1i64 << (7 + self.bit_depth);
        let mut dqb = vec![0i32; tw * th];
        for i in 0..th {
            for j in 0..tw {
                let qv = self.quant[i * tw + j];
                if qv == 0 {
                    continue;
                }
                let q = if i == 0 && j == 0 { dc } else { ac };
                let q2 = if use_qm {
                    let m = i32::from(
                        t::QUANTIZER_MATRIX[qm_level][usize::from(plane > 0)]
                            [usize::from(t::QM_OFFSET[tx_sz]) + i * tw + j],
                    );
                    (q * m + 16) >> 5
                } else {
                    q
                };
                let d = i64::from(qv) * i64::from(q2);
                let mag = (d.abs() & 0xFF_FFFF) / dq_denom;
                let d2 = if d < 0 { -mag } else { mag };
                dqb[i * tw + j] = d2.clamp(-lim, lim - 1) as i32;
            }
        }
        let mut res = vec![0i32; w * h];
        inverse_transform_2d(
            &dqb,
            &mut res,
            tx_sz,
            tx_type,
            self.b.lossless,
            self.bit_depth,
        );
        let maxv = (1i32 << self.bit_depth) - 1;
        let pl = &mut self.cur.planes[plane];
        for i in 0..h {
            let yy = if flip_ud { h - i - 1 } else { i };
            for j in 0..w {
                let xx = if flip_lr { w - j - 1 } else { j };
                let px = x + xx;
                let py = y + yy;
                if px >= pl.stride || py >= pl.h {
                    continue;
                }
                let idx = py * pl.stride + px;
                pl.d[idx] = (i32::from(pl.d[idx]) + res[i * w + j]).clamp(0, maxv) as u16;
            }
        }
    }
}
