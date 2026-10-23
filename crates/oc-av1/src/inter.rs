//! Inter prediction (section 7.11.3).
use crate::consts::*;
use crate::dec::*;
use crate::hdr::*;
use crate::mvpred::round2signed;
use crate::tables as t;
use std::sync::OnceLock;

fn nb4w(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_WIDE[b])
}
fn nb4h(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_HIGH[b])
}

#[inline]
fn round2(x: i32, n: u32) -> i32 {
    if n == 0 {
        x
    } else {
        (x + (1 << (n - 1))) >> n
    }
}

const WEDGE_HORIZONTAL: usize = 0;
const WEDGE_VERTICAL: usize = 1;
const WEDGE_OBLIQUE27: usize = 2;
const WEDGE_OBLIQUE63: usize = 3;
const WEDGE_OBLIQUE117: usize = 4;
const WEDGE_OBLIQUE153: usize = 5;

/// WedgeMasks[bsize][flipSign][wedge] -> 32x32 (stride 32) byte arrays.
fn wedge_masks() -> &'static Vec<u8> {
    static MASKS: OnceLock<Vec<u8>> = OnceLock::new();
    MASKS.get_or_init(|| {
        const N: usize = 64;
        let mut master = vec![[[0u8; N]; N]; 6];
        for j in 0..N {
            let mut shift = (N / 4) as i32;
            let mut i = 0;
            while i < N {
                master[WEDGE_OBLIQUE63][i][j] = t::WEDGE_MASTER_OBLIQUE_EVEN
                    [(j as i32 - shift).clamp(0, N as i32 - 1) as usize];
                shift -= 1;
                master[WEDGE_OBLIQUE63][i + 1][j] =
                    t::WEDGE_MASTER_OBLIQUE_ODD[(j as i32 - shift).clamp(0, N as i32 - 1) as usize];
                master[WEDGE_VERTICAL][i][j] = t::WEDGE_MASTER_VERTICAL[j];
                master[WEDGE_VERTICAL][i + 1][j] = t::WEDGE_MASTER_VERTICAL[j];
                i += 2;
            }
        }
        for i in 0..N {
            for j in 0..N {
                let msk = master[WEDGE_OBLIQUE63][i][j];
                master[WEDGE_OBLIQUE27][j][i] = msk;
                master[WEDGE_OBLIQUE117][i][N - 1 - j] = 64 - msk;
                master[WEDGE_OBLIQUE153][N - 1 - j][i] = 64 - msk;
                master[WEDGE_HORIZONTAL][j][i] = master[WEDGE_VERTICAL][i][j];
            }
        }
        let mut out = vec![0u8; 22 * 2 * 16 * 1024];
        for bsize in BLOCK_8X8..22 {
            if t::WEDGE_BITS[bsize] == 0 {
                continue;
            }
            let w = block_width(bsize);
            let h = block_height(bsize);
            let (w4, h4) = (nb4w(bsize), nb4h(bsize));
            let shape = if h4 > w4 {
                0
            } else if h4 < w4 {
                1
            } else {
                2
            };
            for wedge in 0..16 {
                let cb = t::WEDGE_CODEBOOK[shape][wedge];
                let dir = usize::from(cb[0]);
                let xoff = N / 2 - ((usize::from(cb[1]) * w) >> 3);
                let yoff = N / 2 - ((usize::from(cb[2]) * h) >> 3);
                let mut sum = 0usize;
                for i in 0..w {
                    sum += usize::from(master[dir][yoff][xoff + i]);
                }
                for i in 1..h {
                    sum += usize::from(master[dir][yoff + i][xoff]);
                }
                let avg = (sum + (w + h - 1) / 2) / (w + h - 1);
                let flip = usize::from(avg < 32);
                for i in 0..h {
                    for j in 0..w {
                        let v = master[dir][yoff + i][xoff + j];
                        out[((bsize * 2 + flip) * 16 + wedge) * 1024 + i * 32 + j] = v;
                        out[((bsize * 2 + (1 - flip)) * 16 + wedge) * 1024 + i * 32 + j] = 64 - v;
                    }
                }
            }
        }
        out
    })
}

fn floor_log2(x: u64) -> u32 {
    63 - x.leading_zeros()
}

fn resolve_divisor(d: i64) -> (u32, i64) {
    let n = floor_log2(d.unsigned_abs());
    let e = d.abs() - (1i64 << n);
    let f = if n > 8 {
        ((e + (1i64 << (n - 8 - 1))) >> (n - 8)) as usize
    } else {
        (e << (8 - n)) as usize
    };
    let div_shift = n + 14;
    let fac = i64::from(t::DIV_LUT[f]);
    (div_shift, if d < 0 { -fac } else { fac })
}

/// Setup shear process (7.11.3.6): (valid, alpha, beta, gamma, delta)
pub fn setup_shear(wp: &[i32; 6]) -> (bool, i32, i32, i32, i32) {
    let alpha0 = (wp[2] - (1 << WARPEDMODEL_PREC_BITS)).clamp(-32768, 32767);
    let beta0 = wp[3].clamp(-32768, 32767);
    let (div_shift, div_factor) = resolve_divisor(i64::from(wp[2]));
    let v = i64::from(wp[4]) << WARPEDMODEL_PREC_BITS;
    let gamma0 = round2signed(v * div_factor, div_shift).clamp(-32768, 32767) as i32;
    let w = i64::from(wp[3]) * i64::from(wp[4]);
    let delta0 = (i64::from(wp[5])
        - round2signed(w * div_factor, div_shift)
        - (1i64 << WARPEDMODEL_PREC_BITS))
        .clamp(-32768, 32767) as i32;
    let red = |x: i32| (round2signed(i64::from(x), 6) as i32) << 6;
    let (alpha, beta, gamma, delta) = (red(alpha0), red(beta0), red(gamma0), red(delta0));
    let mut valid = true;
    if 4 * alpha.abs() + 7 * beta.abs() >= (1 << WARPEDMODEL_PREC_BITS) {
        valid = false;
    }
    if 4 * gamma.abs() + 4 * delta.abs() >= (1 << WARPEDMODEL_PREC_BITS) {
        valid = false;
    }
    (valid, alpha, beta, gamma, delta)
}

struct RefInfo {
    planes: std::rc::Rc<Frame>,
    up_w: u32,
    h: u32,
}

impl Dec {
    fn rounding_vars(&self, is_compound: bool) -> (u32, u32, u32) {
        let mut r0 = 3;
        let mut r1 = if is_compound { 7 } else { 11 };
        if self.bit_depth == 12 {
            r0 += 2;
            if !is_compound {
                r1 -= 2;
            }
        }
        (r0, r1, 14 - (r0 + r1))
    }

    /// Motion vector scaling process (7.11.3.3): returns (startX, startY, stepX, stepY).
    fn mv_scaling(
        &self,
        plane: usize,
        ref_up_w: u32,
        ref_h: u32,
        x: usize,
        y: usize,
        mv: [i32; 2],
    ) -> (i32, i32, i32, i32) {
        let fw = self.hdr.frame_width;
        let fh = self.hdr.frame_height;
        let x_scale = i64::from(((ref_up_w << 14) + fw / 2) / fw);
        let y_scale = i64::from(((ref_h << 14) + fh / 2) / fh);
        let (sub_x, sub_y) = if plane == 0 {
            (0, 0)
        } else {
            (self.subx, self.suby)
        };
        let half = 8i64;
        let orig_x = ((x as i64) << 4) + ((2 * i64::from(mv[1])) >> sub_x) + half;
        let orig_y = ((y as i64) << 4) + ((2 * i64::from(mv[0])) >> sub_y) + half;
        let base_x = orig_x * x_scale - (half << 14);
        let base_y = orig_y * y_scale - (half << 14);
        let off = 32i64;
        let sx = round2signed(base_x, 14 + 4 - 10) + off;
        let sy = round2signed(base_y, 14 + 4 - 10) + off;
        let stx = round2signed(x_scale, 14 - 10);
        let sty = round2signed(y_scale, 14 - 10);
        (sx as i32, sy as i32, stx as i32, sty as i32)
    }

    /// Block inter prediction process (7.11.3.4).
    fn block_inter_pred(
        refp: &Plane,
        last_x: i32,
        last_y: i32,
        x: i32,
        y: i32,
        x_step: i32,
        y_step: i32,
        w: usize,
        h: usize,
        filt: [u8; 2],
        round0: u32,
        round1: u32,
    ) -> Vec<i32> {
        let inter_h = ((((h as i32 - 1) * y_step + (1 << 10) - 1) >> 10) + 8) as usize;
        let small = |f: u8, n: usize| -> usize {
            if n <= 4 {
                match f {
                    EIGHTTAP | EIGHTTAP_SHARP => 4,
                    EIGHTTAP_SMOOTH => 5,
                    _ => usize::from(f),
                }
            } else {
                usize::from(f)
            }
        };
        let fh = small(filt[1], w);
        let fv = small(filt[0], h);
        let mut inter = vec![0i32; inter_h * w];
        for r in 0..inter_h {
            let yy = ((y >> 10) + r as i32 - 3).clamp(0, last_y) as usize;
            let row = &refp.d[yy * refp.stride..];
            for c in 0..w {
                let p = x + x_step * c as i32;
                let filter = &t::SUBPEL_FILTERS[fh][((p >> 6) & 15) as usize];
                let base = (p >> 10) - 3;
                let mut s = 0i32;
                for tap in 0..8 {
                    let xx = (base + tap as i32).clamp(0, last_x) as usize;
                    s += i32::from(filter[tap]) * i32::from(row[xx]);
                }
                inter[r * w + c] = round2(s, round0);
            }
        }
        let mut pred = vec![0i32; w * h];
        for r in 0..h {
            let p = (y & 1023) + y_step * r as i32;
            let filter = &t::SUBPEL_FILTERS[fv][((p >> 6) & 15) as usize];
            let base = (p >> 10) as usize;
            for c in 0..w {
                let mut s = 0i32;
                for tap in 0..8 {
                    s += i32::from(filter[tap]) * inter[(base + tap) * w + c];
                }
                pred[r * w + c] = round2(s, round1);
            }
        }
        pred
    }

    /// Block warp process (7.11.3.5) for one 8x8 section.
    #[allow(clippy::too_many_arguments)]
    fn block_warp(
        &self,
        wp: &[i32; 6],
        refp: &Plane,
        last_x: i32,
        last_y: i32,
        plane: usize,
        x: usize,
        y: usize,
        i8_: usize,
        j8: usize,
        w: usize,
        h: usize,
        round0: u32,
        round1: u32,
        pred: &mut [i32],
    ) {
        let (sub_x, sub_y) = if plane == 0 {
            (0, 0)
        } else {
            (self.subx, self.suby)
        };
        let src_x = ((x + j8 * 8 + 4) << sub_x) as i64;
        let src_y = ((y + i8_ * 8 + 4) << sub_y) as i64;
        let dst_x = i64::from(wp[2]) * src_x + i64::from(wp[3]) * src_y + i64::from(wp[0]);
        let dst_y = i64::from(wp[4]) * src_x + i64::from(wp[5]) * src_y + i64::from(wp[1]);
        let (_, alpha, beta, gamma, delta) = setup_shear(wp);
        let x4 = dst_x >> sub_x;
        let y4 = dst_y >> sub_y;
        let ix4 = (x4 >> WARPEDMODEL_PREC_BITS) as i32;
        let sx4 = (x4 & ((1 << WARPEDMODEL_PREC_BITS) - 1)) as i32;
        let iy4 = (y4 >> WARPEDMODEL_PREC_BITS) as i32;
        let sy4 = (y4 & ((1 << WARPEDMODEL_PREC_BITS) - 1)) as i32;
        let mut inter = [[0i32; 8]; 15];
        for i1 in -7i32..8 {
            let yy = (iy4 + i1).clamp(0, last_y) as usize;
            let row = &refp.d[yy * refp.stride..];
            for i2 in -4i32..4 {
                let sx = sx4 + alpha * i2 + beta * i1;
                let offs = (round2(sx, 10) + 64) as usize;
                let mut s = 0i32;
                for i3 in 0..8 {
                    let xx = (ix4 + i2 - 3 + i3 as i32).clamp(0, last_x) as usize;
                    s += i32::from(t::WARPED_FILTERS[offs][i3]) * i32::from(row[xx]);
                }
                inter[(i1 + 7) as usize][(i2 + 4) as usize] = round2(s, round0);
            }
        }
        let lim_i = 4.min(h as i32 - i8_ as i32 * 8 - 4);
        let lim_j = 4.min(w as i32 - j8 as i32 * 8 - 4);
        for i1 in -4..lim_i {
            for i2 in -4..lim_j {
                let sy = sy4 + gamma * i2 + delta * i1;
                let offs = (round2(sy, 10) + 64) as usize;
                let mut s = 0i32;
                for i3 in 0..8 {
                    s += i32::from(t::WARPED_FILTERS[offs][i3])
                        * inter[(i1 + i3 as i32 + 4) as usize][(i2 + 4) as usize];
                }
                let py = (i8_ as i32 * 8 + i1 + 4) as usize;
                let px = (j8 as i32 * 8 + i2 + 4) as usize;
                pred[py * w + px] = round2(s, round1);
            }
        }
    }

    /// Warp estimation process (7.11.3.8).
    fn warp_estimation(&mut self) {
        let mut a = [[0i64; 2]; 2];
        let mut bx = [0i64; 2];
        let mut by = [0i64; 2];
        let w4 = nb4w(self.b.mi_size) as i64;
        let h4 = nb4h(self.b.mi_size) as i64;
        let mid_y = self.b.mi_row as i64 * 4 + h4 * 2 - 1;
        let mid_x = self.b.mi_col as i64 * 4 + w4 * 2 - 1;
        let suy = mid_y * 8;
        let sux = mid_x * 8;
        let duy = suy + i64::from(self.b.mv[0][0]);
        let dux = sux + i64::from(self.b.mv[0][1]);
        let ls = |a: i64, b: i64| ((a * b) >> 2) + (a + b);
        for i in 0..self.b.num_samples {
            let c = self.b.cand_list[i];
            let sy = i64::from(c[0]) - suy;
            let sx = i64::from(c[1]) - sux;
            let dy = i64::from(c[2]) - duy;
            let dx = i64::from(c[3]) - dux;
            if (sx - dx).abs() < 256 && (sy - dy).abs() < 256 {
                a[0][0] += ls(sx, sx) + 8;
                a[0][1] += ls(sx, sy) + 4;
                a[1][1] += ls(sy, sy) + 8;
                bx[0] += ls(sx, dx) + 8;
                bx[1] += ls(sy, dx) + 4;
                by[0] += ls(sx, dy) + 4;
                by[1] += ls(sy, dy) + 8;
            }
        }
        let det = a[0][0] * a[1][1] - a[0][1] * a[0][1];
        self.b.local_valid = det != 0;
        if det == 0 {
            return;
        }
        let (mut div_shift, mut div_factor) = resolve_divisor(det);
        let mut ds = div_shift as i32 - WARPEDMODEL_PREC_BITS as i32;
        if ds < 0 {
            div_factor <<= -ds;
            ds = 0;
        }
        div_shift = ds as u32;
        let r2s = |v: i128| -> i64 {
            let p = v * i128::from(div_factor);
            if div_shift == 0 {
                return p.clamp(i128::from(i64::MIN / 2), i128::from(i64::MAX / 2)) as i64;
            }
            let half = 1i128 << (div_shift - 1);
            let r = if p >= 0 {
                (p + half) >> div_shift
            } else {
                -((-p + half) >> div_shift)
            };
            r.clamp(i128::from(i64::MIN / 2), i128::from(i64::MAX / 2)) as i64
        };
        let nondiag = |v: i128| r2s(v).clamp(-(1 << 13) + 1, (1 << 13) - 1) as i32;
        let diag = |v: i128| {
            r2s(v).clamp(
                (1 << WARPEDMODEL_PREC_BITS) - (1 << 13) + 1,
                (1 << WARPEDMODEL_PREC_BITS) + (1 << 13) - 1,
            ) as i32
        };
        let (a00, a01, a11) = (
            i128::from(a[0][0]),
            i128::from(a[0][1]),
            i128::from(a[1][1]),
        );
        let p = &mut self.b.local_warp_params;
        p[2] = diag(a11 * i128::from(bx[0]) - a01 * i128::from(bx[1]));
        p[3] = nondiag(-a01 * i128::from(bx[0]) + a00 * i128::from(bx[1]));
        p[4] = nondiag(a11 * i128::from(by[0]) - a01 * i128::from(by[1]));
        p[5] = diag(-a01 * i128::from(by[0]) + a00 * i128::from(by[1]));
        let mvx = i64::from(self.b.mv[0][1]);
        let mvy = i64::from(self.b.mv[0][0]);
        let vx = mvx * (1 << (WARPEDMODEL_PREC_BITS - 3))
            - (mid_x * (i64::from(p[2]) - (1 << WARPEDMODEL_PREC_BITS)) + mid_y * i64::from(p[3]));
        let vy = mvy * (1 << (WARPEDMODEL_PREC_BITS - 3))
            - (mid_x * i64::from(p[4]) + mid_y * (i64::from(p[5]) - (1 << WARPEDMODEL_PREC_BITS)));
        p[0] = vx.clamp(-(1 << 23), (1 << 23) - 1) as i32;
        p[1] = vy.clamp(-(1 << 23), (1 << 23) - 1) as i32;
    }

    fn ref_info(&self, ref_frame: i8) -> RefInfo {
        let idx = self.hdr.ref_frame_idx[(ref_frame - LAST_FRAME) as usize];
        let rh = &self.refh[idx];
        RefInfo {
            planes: self.refd[idx]
                .as_ref()
                .map(|r| r.frame.clone())
                .unwrap_or_default(),
            up_w: rh.upscaled_width,
            h: rh.frame_height,
        }
    }

    pub fn predict_inter(
        &mut self,
        plane: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        cand_row: usize,
        cand_col: usize,
    ) {
        let m = *self.mi_at(cand_row, cand_col);
        let is_compound = m.ref_frame[1] > INTRA_FRAME;
        let (round0, round1, post_round) = self.rounding_vars(is_compound);
        if plane == 0 && self.b.motion_mode == LOCALWARP {
            self.warp_estimation();
            if self.b.local_valid {
                self.b.local_valid = setup_shear(&self.b.local_warp_params).0;
            }
        }
        let (sub_x, sub_y) = if plane == 0 {
            (0, 0)
        } else {
            (self.subx, self.suby)
        };
        let mut preds: [Vec<i32>; 2] = [Vec::new(), Vec::new()];
        for ref_list in 0..1 + usize::from(is_compound) {
            let ref_frame = m.ref_frame[ref_list];
            let mut global_valid = false;
            let is_global_mode = self.b.y_mode == GLOBALMV || self.b.y_mode == GLOBAL_GLOBALMV;
            if is_global_mode && self.hdr.gm_type[ref_frame as usize] > TRANSLATION {
                global_valid = setup_shear(&self.hdr.gm_params[ref_frame as usize]).0;
            }
            let use_warp = if w < 8 || h < 8 || self.hdr.force_integer_mv {
                0
            } else if self.b.motion_mode == LOCALWARP && self.b.local_valid {
                1
            } else if is_global_mode
                && self.hdr.gm_type[ref_frame as usize] > TRANSLATION
                && !self.is_scaled(ref_frame)
                && global_valid
            {
                2
            } else {
                0
            };
            let mv = [i32::from(m.mv[ref_list][0]), i32::from(m.mv[ref_list][1])];
            let intrabc = self.b.use_intrabc;
            let (ref_up_w, ref_h) = if intrabc {
                (self.hdr.upscaled_width, self.hdr.frame_height)
            } else {
                let ri = self.ref_info(ref_frame);
                (ri.up_w, ri.h)
            };
            let (start_x, start_y, step_x, step_y) =
                self.mv_scaling(plane, ref_up_w, ref_h, x, y, mv);
            let (last_w, last_h) = if intrabc {
                ((self.mi_cols * 4) as u32, (self.mi_rows * 4) as u32)
            } else {
                (ref_up_w, ref_h)
            };
            let last_x = (((last_w + sub_x as u32) >> sub_x) - 1) as i32;
            let last_y = (((last_h + sub_y as u32) >> sub_y) - 1) as i32;
            if use_warp != 0 {
                let ri = self.ref_info(ref_frame);
                let wp = if use_warp == 1 {
                    self.b.local_warp_params
                } else {
                    self.hdr.gm_params[ref_frame as usize]
                };
                let mut p = vec![0i32; w * h];
                for i8_ in 0..=((h - 1) >> 3) {
                    for j8 in 0..=((w - 1) >> 3) {
                        self.block_warp(
                            &wp,
                            &ri.planes.planes[plane],
                            last_x,
                            last_y,
                            plane,
                            x,
                            y,
                            i8_,
                            j8,
                            w,
                            h,
                            round0,
                            round1,
                            &mut p,
                        );
                    }
                }
                preds[ref_list] = p;
            } else if intrabc {
                preds[ref_list] = Self::block_inter_pred(
                    &self.cur.planes[plane],
                    last_x,
                    last_y,
                    start_x,
                    start_y,
                    step_x,
                    step_y,
                    w,
                    h,
                    m.interp_filter,
                    round0,
                    round1,
                );
            } else {
                let ri = self.ref_info(ref_frame);
                preds[ref_list] = Self::block_inter_pred(
                    &ri.planes.planes[plane],
                    last_x,
                    last_y,
                    start_x,
                    start_y,
                    step_x,
                    step_y,
                    w,
                    h,
                    m.interp_filter,
                    round0,
                    round1,
                );
            }
        }
        // masks
        let ct = self.b.compound_type;
        if ct == COMPOUND_WEDGE && plane == 0 {
            let wm = wedge_masks();
            let base = ((self.b.mi_size * 2 + self.b.wedge_sign) * 16 + self.b.wedge_index) * 1024;
            for i in 0..h {
                for j in 0..w {
                    self.mask[i * 128 + j] = wm[base + i * 32 + j];
                }
            }
        } else if ct == COMPOUND_INTRA {
            let size_scale = 128 / h.max(w);
            for i in 0..h {
                for j in 0..w {
                    self.mask[i * 128 + j] = match self.b.interintra_mode {
                        II_V_PRED => t::II_WEIGHTS_1D[i * size_scale],
                        II_H_PRED => t::II_WEIGHTS_1D[j * size_scale],
                        II_SMOOTH_PRED => t::II_WEIGHTS_1D[i.min(j) * size_scale],
                        _ => 32,
                    };
                }
            }
        } else if ct == COMPOUND_DIFFWTD && plane == 0 {
            for i in 0..h {
                for j in 0..w {
                    let diff = (preds[0][i * w + j] - preds[1][i * w + j]).abs();
                    let diff = round2(diff, (self.bit_depth - 8) + post_round);
                    let mm = (38 + diff / 16).clamp(0, 64);
                    self.mask[i * 128 + j] = if self.b.mask_type != 0 {
                        (64 - mm) as u8
                    } else {
                        mm as u8
                    };
                }
            }
        }
        let (mut fwd_w, mut bck_w) = (0, 0);
        if ct == COMPOUND_DISTANCE {
            let mut dist = [0i32; 2];
            for rl in 0..2 {
                let hh = self.hdr.order_hints[m.ref_frame[rl] as usize];
                dist[rl] = get_relative_dist(&self.seq, hh, self.hdr.order_hint)
                    .abs()
                    .clamp(0, 31);
            }
            let d0 = dist[1];
            let d1 = dist[0];
            let order = usize::from(d0 <= d1);
            if d0 == 0 || d1 == 0 {
                fwd_w = i32::from(t::QUANT_DIST_LOOKUP[3][order]);
                bck_w = i32::from(t::QUANT_DIST_LOOKUP[3][1 - order]);
            } else {
                let mut i = 0;
                while i < 3 {
                    let c0 = i32::from(t::QUANT_DIST_WEIGHT[i][order]);
                    let c1 = i32::from(t::QUANT_DIST_WEIGHT[i][1 - order]);
                    if order == 1 {
                        if d0 * c0 > d1 * c1 {
                            break;
                        }
                    } else if d0 * c0 < d1 * c1 {
                        break;
                    }
                    i += 1;
                }
                fwd_w = i32::from(t::QUANT_DIST_LOOKUP[i][order]);
                bck_w = i32::from(t::QUANT_DIST_LOOKUP[i][1 - order]);
            }
        }
        let maxv = (1i32 << self.bit_depth) - 1;
        let is_inter_intra = self.b.is_inter_intra;
        {
            let pl = &mut self.cur.planes[plane];
            let stride = pl.stride;
            if !is_compound && !is_inter_intra {
                for i in 0..h {
                    for j in 0..w {
                        pl.d[(y + i) * stride + x + j] = preds[0][i * w + j].clamp(0, maxv) as u16;
                    }
                }
            } else if ct == COMPOUND_AVERAGE {
                for i in 0..h {
                    for j in 0..w {
                        let v = round2(preds[0][i * w + j] + preds[1][i * w + j], 1 + post_round);
                        pl.d[(y + i) * stride + x + j] = v.clamp(0, maxv) as u16;
                    }
                }
            } else if ct == COMPOUND_DISTANCE {
                for i in 0..h {
                    for j in 0..w {
                        let v = round2(
                            fwd_w * preds[0][i * w + j] + bck_w * preds[1][i * w + j],
                            4 + post_round,
                        );
                        pl.d[(y + i) * stride + x + j] = v.clamp(0, maxv) as u16;
                    }
                }
            } else {
                let mask = &self.mask;
                let interintra = self.b.interintra;
                let wedge_ii = self.b.wedge_interintra;
                for yy in 0..h {
                    for xx in 0..w {
                        let mk = |r: usize, c: usize| i32::from(mask[r * 128 + c]);
                        let mval = if (sub_x == 0 && sub_y == 0) || (interintra && !wedge_ii) {
                            mk(yy, xx)
                        } else if sub_x != 0 && sub_y == 0 {
                            round2(mk(yy, 2 * xx) + mk(yy, 2 * xx + 1), 1)
                        } else if sub_x == 0 && sub_y != 0 {
                            round2(mk(2 * yy, xx) + mk(2 * yy + 1, xx), 1)
                        } else {
                            round2(
                                mk(2 * yy, 2 * xx)
                                    + mk(2 * yy, 2 * xx + 1)
                                    + mk(2 * yy + 1, 2 * xx)
                                    + mk(2 * yy + 1, 2 * xx + 1),
                                2,
                            )
                        };
                        let di = (y + yy) * stride + x + xx;
                        if interintra {
                            let p0 = round2(preds[0][yy * w + xx], post_round).clamp(0, maxv);
                            let p1 = i32::from(pl.d[di]);
                            pl.d[di] = round2(mval * p1 + (64 - mval) * p0, 6) as u16;
                        } else {
                            let p0 = preds[0][yy * w + xx];
                            let p1 = preds[1][yy * w + xx];
                            pl.d[di] = round2(mval * p0 + (64 - mval) * p1, 6 + post_round)
                                .clamp(0, maxv) as u16;
                        }
                    }
                }
            }
        }
        if self.b.motion_mode == OBMC {
            self.overlapped_motion_compensation(plane, w, h);
        }
    }

    fn overlapped_motion_compensation(&mut self, plane: usize, w: usize, h: usize) {
        let (sub_x, sub_y) = if plane == 0 {
            (0, 0)
        } else {
            (self.subx, self.suby)
        };
        let (mi_row, mi_col) = (self.b.mi_row, self.b.mi_col);
        if self.b.avail_u && self.get_plane_residual_size(self.b.mi_size, plane) >= BLOCK_8X8 {
            let w4 = nb4w(self.b.mi_size);
            let mut x4 = mi_col;
            let y4 = mi_row;
            let mut n_count = 0;
            let n_limit = 4.min(usize::from(t::MI_WIDTH_LOG2[self.b.mi_size]));
            while n_count < n_limit && x4 < self.mi_cols.min(mi_col + w4) {
                let cand_row = mi_row - 1;
                let cand_col = x4 | 1;
                let cand_sz = usize::from(self.mi_at(cand_row, cand_col).mi_size);
                let step4 = nb4w(cand_sz).clamp(2, 16);
                if self.mi_at(cand_row, cand_col).ref_frame[0] > INTRA_FRAME {
                    n_count += 1;
                    let pred_w = w.min((step4 * 4) >> sub_x);
                    let pred_h = (h >> 1).min(32 >> sub_y);
                    self.predict_overlap(plane, cand_row, cand_col, x4, y4, pred_w, pred_h, 0);
                }
                x4 += step4;
            }
        }
        if self.b.avail_l {
            let h4 = nb4h(self.b.mi_size);
            let x4 = mi_col;
            let mut y4 = mi_row;
            let mut n_count = 0;
            let n_limit = 4.min(usize::from(t::MI_HEIGHT_LOG2[self.b.mi_size]));
            while n_count < n_limit && y4 < self.mi_rows.min(mi_row + h4) {
                let cand_col = mi_col - 1;
                let cand_row = y4 | 1;
                let cand_sz = usize::from(self.mi_at(cand_row, cand_col).mi_size);
                let step4 = nb4h(cand_sz).clamp(2, 16);
                if self.mi_at(cand_row, cand_col).ref_frame[0] > INTRA_FRAME {
                    n_count += 1;
                    let pred_w = (w >> 1).min(32 >> sub_x);
                    let pred_h = h.min((step4 * 4) >> sub_y);
                    self.predict_overlap(plane, cand_row, cand_col, x4, y4, pred_w, pred_h, 1);
                }
                y4 += step4;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn predict_overlap(
        &mut self,
        plane: usize,
        cand_row: usize,
        cand_col: usize,
        x4: usize,
        y4: usize,
        pred_w: usize,
        pred_h: usize,
        pass: usize,
    ) {
        let (sub_x, sub_y) = if plane == 0 {
            (0, 0)
        } else {
            (self.subx, self.suby)
        };
        let m = *self.mi_at(cand_row, cand_col);
        let mv = [i32::from(m.mv[0][0]), i32::from(m.mv[0][1])];
        let ri = self.ref_info(m.ref_frame[0]);
        let pred_x = (x4 * 4) >> sub_x;
        let pred_y = (y4 * 4) >> sub_y;
        let (sx, sy, stx, sty) = self.mv_scaling(plane, ri.up_w, ri.h, pred_x, pred_y, mv);
        let (round0, round1, _) = self.rounding_vars(false);
        let last_x = (((ri.up_w + sub_x as u32) >> sub_x) - 1) as i32;
        let last_y = (((ri.h + sub_y as u32) >> sub_y) - 1) as i32;
        let obmc = Self::block_inter_pred(
            &ri.planes.planes[plane],
            last_x,
            last_y,
            sx,
            sy,
            stx,
            sty,
            pred_w,
            pred_h,
            m.interp_filter,
            round0,
            round1,
        );
        let maxv = (1i32 << self.bit_depth) - 1;
        let len = if pass == 0 { pred_h } else { pred_w };
        let mask: &[u8] = match len {
            2 => &t::OBMC_MASK_2,
            4 => &t::OBMC_MASK_4,
            8 => &t::OBMC_MASK_8,
            16 => &t::OBMC_MASK_16,
            _ => &t::OBMC_MASK_32,
        };
        let pl = &mut self.cur.planes[plane];
        for i in 0..pred_h {
            for j in 0..pred_w {
                let mm = i32::from(if pass == 0 { mask[i] } else { mask[j] });
                let o = obmc[i * pred_w + j].clamp(0, maxv);
                let di = (pred_y + i) * pl.stride + pred_x + j;
                pl.d[di] = round2(mm * i32::from(pl.d[di]) + (64 - mm) * o, 6) as u16;
            }
        }
    }
}
