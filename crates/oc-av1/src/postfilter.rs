//! In-loop post filters: deblocking (7.14), CDEF (7.15), super-resolution (7.16) and loop restoration (7.17).
use crate::consts::*;
use crate::dec::*;
use crate::hdr::RESTORE_NONE;
use crate::tables as t;

fn floor_log2(v: u32) -> i32 {
    31 - v.leading_zeros() as i32
}

fn round2(x: i32, n: u32) -> i32 {
    if n == 0 {
        x
    } else {
        (x + (1 << (n - 1))) >> n
    }
}

impl Dec {
    fn plane_sub(&self, plane: usize) -> (usize, usize) {
        if plane == 0 {
            (0, 0)
        } else {
            (self.subx, self.suby)
        }
    }

    /// Adaptive filter strength (7.14.4 / 7.14.5): returns lvl for the block at (row, col).
    fn lf_level(&self, row: usize, col: usize, plane: usize, pass: usize) -> i32 {
        let m = self.mi[row * self.mi_stride + col];
        let i = if plane == 0 { pass } else { plane + 1 };
        let mode = m.y_mode;
        let mode_type =
            usize::from(mode >= NEARESTMV && mode != GLOBALMV && mode != GLOBAL_GLOBALMV);
        let delta = if self.hdr.delta_lf_multi {
            m.delta_lf[i]
        } else {
            m.delta_lf[0]
        };
        let seg = usize::from(m.segment_id);
        let mut lvl = (i32::from(delta) + self.hdr.loop_filter_level[i]).clamp(0, MAX_LOOP_FILTER);
        let feature = SEG_LVL_ALT_LF_Y_V + i;
        if self.hdr.seg_feature_active_idx(seg, feature) {
            lvl = (self.hdr.feature_data[seg][feature] + lvl).clamp(0, MAX_LOOP_FILTER);
        }
        if self.hdr.loop_filter_delta_enabled {
            let n_shift = lvl >> 5;
            let r = m.ref_frame[0];
            if r <= INTRA_FRAME {
                lvl += self.hdr.loop_filter_ref_deltas[INTRA_FRAME as usize] << n_shift;
            } else {
                lvl += (self.hdr.loop_filter_ref_deltas[r as usize] << n_shift)
                    + (self.hdr.loop_filter_mode_deltas[mode_type] << n_shift);
            }
            lvl = lvl.clamp(0, MAX_LOOP_FILTER);
        }
        lvl
    }

    pub fn loop_filter_frame(&mut self) {
        let frame_w = self.hdr.frame_width as usize;
        let frame_h = self.hdr.frame_height as usize;
        let sharp = self.hdr.loop_filter_sharpness;
        for plane in 0..self.num_planes {
            if plane != 0 && self.hdr.loop_filter_level[1 + plane] == 0 {
                continue;
            }
            let (sub_x, sub_y) = self.plane_sub(plane);
            for pass in 0..2 {
                let row_step = if plane == 0 { 1 } else { 1 << sub_y };
                let col_step = if plane == 0 { 1 } else { 1 << sub_x };
                let (dx, dy) = if pass == 0 { (1usize, 0usize) } else { (0, 1) };
                let mut row0 = 0;
                while row0 < self.mi_rows {
                    let mut col0 = 0;
                    while col0 < self.mi_cols {
                        let x = col0 * 4;
                        let y = row0 * 4;
                        let on_screen = !(x >= frame_w
                            || y >= frame_h
                            || (pass == 0 && x == 0)
                            || (pass == 1 && y == 0));
                        if on_screen {
                            let row = row0 | sub_y;
                            let col = col0 | sub_x;
                            let xp = x >> sub_x;
                            let yp = y >> sub_y;
                            let prev_row = row - (dy << sub_y);
                            let prev_col = col - (dx << sub_x);
                            let mi = self.mi[row * self.mi_stride + col];
                            let tx_sz = usize::from(
                                self.lf_tx_sizes[plane]
                                    [(row >> sub_y) * self.mi_stride + (col >> sub_x)],
                            );
                            let plane_size =
                                self.get_plane_residual_size(usize::from(mi.mi_size), plane);
                            let prev_tx = usize::from(
                                self.lf_tx_sizes[plane]
                                    [(prev_row >> sub_y) * self.mi_stride + (prev_col >> sub_x)],
                            );
                            let (is_block_edge, is_tx_edge) = if pass == 0 {
                                (
                                    xp % block_width(plane_size) == 0,
                                    xp % usize::from(t::TX_WIDTH[tx_sz]) == 0,
                                )
                            } else {
                                (
                                    yp % block_height(plane_size) == 0,
                                    yp % usize::from(t::TX_HEIGHT[tx_sz]) == 0,
                                )
                            };
                            let is_intra = mi.ref_frame[0] <= INTRA_FRAME;
                            let apply = is_tx_edge && (is_block_edge || !mi.skip || is_intra);
                            if apply {
                                let base = if pass == 0 {
                                    usize::from(t::TX_WIDTH[prev_tx])
                                        .min(usize::from(t::TX_WIDTH[tx_sz]))
                                } else {
                                    usize::from(t::TX_HEIGHT[prev_tx])
                                        .min(usize::from(t::TX_HEIGHT[tx_sz]))
                                };
                                let filter_size = if plane == 0 {
                                    base.min(16)
                                } else {
                                    base.min(8)
                                };
                                let mut lvl = self.lf_level(row, col, plane, pass);
                                if lvl == 0 {
                                    lvl = self.lf_level(prev_row, prev_col, plane, pass);
                                }
                                if lvl > 0 {
                                    let shift = if sharp > 4 {
                                        2
                                    } else if sharp > 0 {
                                        1
                                    } else {
                                        0
                                    };
                                    let limit = if sharp > 0 {
                                        (lvl >> shift).clamp(1, 9 - sharp)
                                    } else {
                                        (lvl >> shift).max(1)
                                    };
                                    let blimit = 2 * (lvl + 2) + limit;
                                    let thresh = lvl >> 4;
                                    for i in 0..4 {
                                        sample_filter(
                                            &mut self.cur.planes[plane],
                                            xp + dy * i,
                                            yp + dx * i,
                                            plane,
                                            limit,
                                            blimit,
                                            thresh,
                                            dx,
                                            dy,
                                            filter_size,
                                            self.bit_depth,
                                        );
                                    }
                                }
                            }
                        }
                        col0 += col_step;
                    }
                    row0 += row_step;
                }
            }
        }
    }

    pub fn post_filters(&mut self) {
        let hdr = &self.hdr;
        let do_cdef = self.seq.enable_cdef && !hdr.coded_lossless && !hdr.allow_intrabc;
        let cdef_frame = if do_cdef {
            Some(self.cdef_frame())
        } else {
            None
        };
        let (cur_up, cdef_up) = if self.hdr.use_superres {
            let cu = self.upscale(&self.cur);
            let cd = match &cdef_frame {
                Some(f) => self.upscale(f),
                None => cu.clone(),
            };
            (cu, cd)
        } else {
            let cd = match cdef_frame {
                Some(f) => f,
                None => self.cur.clone(),
            };
            (std::mem::take(&mut self.cur), cd)
        };
        if self.hdr.uses_lr {
            self.cur = self.loop_restoration(&cur_up, &cdef_up);
        } else {
            self.cur = cdef_up;
        }
    }

    // ---- CDEF (7.15) ----

    fn cdef_frame(&self) -> Frame {
        let mut out = self.cur.clone();
        let bd = self.bit_depth;
        let coeff_shift = bd - 8;
        let mut r = 0;
        while r < self.mi_rows {
            let mut c = 0;
            while c < self.mi_cols {
                let idx = self.cdef_idx[(r >> 4) * self.cdef_stride + (c >> 4)];
                if idx >= 0 {
                    let idx = idx as usize;
                    let s = |rr: usize, cc: usize| self.mi[rr * self.mi_stride + cc].skip;
                    let skip = s(r, c) && s(r + 1, c) && s(r, c + 1) && s(r + 1, c + 1);
                    if !skip {
                        let (y_dir, var) = self.cdef_direction(r, c);
                        let mut pri = self.hdr.cdef_y_pri_strength[idx] << coeff_shift;
                        let sec = self.hdr.cdef_y_sec_strength[idx] << coeff_shift;
                        let dir = if pri == 0 { 0 } else { y_dir };
                        let var_str = if (var >> 6) != 0 {
                            floor_log2((var >> 6) as u32).min(12)
                        } else {
                            0
                        };
                        pri = if var != 0 {
                            (pri * (4 + var_str) + 8) >> 4
                        } else {
                            0
                        };
                        let damping = self.hdr.cdef_damping + coeff_shift as i32;
                        self.cdef_filter(&mut out, 0, r, c, pri, sec, damping, dir);
                        if self.num_planes > 1 {
                            let pri = self.hdr.cdef_uv_pri_strength[idx] << coeff_shift;
                            let sec = self.hdr.cdef_uv_sec_strength[idx] << coeff_shift;
                            let dir = if pri == 0 {
                                0
                            } else {
                                usize::from(t::CDEF_UV_DIR[self.subx][self.suby][y_dir])
                            };
                            let damping = self.hdr.cdef_damping + coeff_shift as i32 - 1;
                            self.cdef_filter(&mut out, 1, r, c, pri, sec, damping, dir);
                            self.cdef_filter(&mut out, 2, r, c, pri, sec, damping, dir);
                        }
                    }
                }
                c += 2;
            }
            r += 2;
        }
        out
    }

    fn cdef_direction(&self, r: usize, c: usize) -> (usize, i32) {
        let mut cost = [0i32; 8];
        let mut partial = [[0i32; 15]; 8];
        let x0 = c * 4;
        let y0 = r * 4;
        let p = &self.cur.planes[0];
        for i in 0..8 {
            for j in 0..8 {
                let x = (i32::from(p.get(x0 + j, y0 + i)) >> (self.bit_depth - 8)) - 128;
                partial[0][i + j] += x;
                partial[1][i + j / 2] += x;
                partial[2][i] += x;
                partial[3][3 + i - j / 2] += x;
                partial[4][7 + i - j] += x;
                partial[5][3 - i / 2 + j] += x;
                partial[6][j] += x;
                partial[7][i / 2 + j] += x;
            }
        }
        let dt = |i: usize| i32::from(t::DIV_TABLE[i]);
        for i in 0..8 {
            cost[2] += partial[2][i] * partial[2][i];
            cost[6] += partial[6][i] * partial[6][i];
        }
        cost[2] *= dt(8);
        cost[6] *= dt(8);
        for i in 0..7 {
            cost[0] += (partial[0][i] * partial[0][i] + partial[0][14 - i] * partial[0][14 - i])
                * dt(i + 1);
            cost[4] += (partial[4][i] * partial[4][i] + partial[4][14 - i] * partial[4][14 - i])
                * dt(i + 1);
        }
        cost[0] += partial[0][7] * partial[0][7] * dt(8);
        cost[4] += partial[4][7] * partial[4][7] * dt(8);
        let mut i = 1;
        while i < 8 {
            for j in 0..5 {
                cost[i] += partial[i][3 + j] * partial[i][3 + j];
            }
            cost[i] *= dt(8);
            for j in 0..3 {
                cost[i] += (partial[i][j] * partial[i][j]
                    + partial[i][10 - j] * partial[i][10 - j])
                    * dt(2 * j + 2);
            }
            i += 2;
        }
        let mut best = 0;
        let mut y_dir = 0;
        for (i, &cv) in cost.iter().enumerate() {
            if cv > best {
                best = cv;
                y_dir = i;
            }
        }
        (y_dir, (best - cost[(y_dir + 4) & 7]) >> 10)
    }

    #[allow(clippy::too_many_arguments)]
    fn cdef_filter(
        &self,
        out: &mut Frame,
        plane: usize,
        r: usize,
        c: usize,
        pri: i32,
        sec: i32,
        damping: i32,
        dir: usize,
    ) {
        let coeff_shift = self.bit_depth - 8;
        let (sub_x, sub_y) = self.plane_sub(plane);
        let x0 = (c * 4) >> sub_x;
        let y0 = (r * 4) >> sub_y;
        let w = 8 >> sub_x;
        let h = 8 >> sub_y;
        let src = &self.cur.planes[plane];
        let mi_rows = self.mi_rows as i32;
        let mi_cols = self.mi_cols as i32;
        let pri_taps = &t::CDEF_PRI_TAPS[((pri >> coeff_shift) & 1) as usize];
        let sec_taps = &t::CDEF_SEC_TAPS[((pri >> coeff_shift) & 1) as usize];
        let get = |i: usize, j: usize, d: usize, k: usize, sign: i32| -> Option<i32> {
            let y = (y0 + i) as i32 + sign * i32::from(t::CDEF_DIRECTIONS[d][k][0]);
            let x = (x0 + j) as i32 + sign * i32::from(t::CDEF_DIRECTIONS[d][k][1]);
            let cr = (y << sub_y) >> 2;
            let cc = (x << sub_x) >> 2;
            if cc >= 0 && cc < mi_cols && cr >= 0 && cr < mi_rows {
                Some(i32::from(src.get(x as usize, y as usize)))
            } else {
                None
            }
        };
        let constrain = |diff: i32, threshold: i32| -> i32 {
            if threshold == 0 {
                return 0;
            }
            let adj = (damping - floor_log2(threshold as u32)).max(0);
            let m = (threshold - (diff.abs() >> adj)).clamp(0, diff.abs());
            if diff < 0 {
                -m
            } else {
                m
            }
        };
        let dst = &mut out.planes[plane];
        for i in 0..h {
            for j in 0..w {
                let x = i32::from(src.get(x0 + j, y0 + i));
                let mut sum = 0;
                let mut mx = x;
                let mut mn = x;
                for k in 0..2 {
                    for sign in [-1, 1] {
                        if let Some(p) = get(i, j, dir, k, sign) {
                            sum += i32::from(pri_taps[k]) * constrain(p - x, pri);
                            mx = mx.max(p);
                            mn = mn.min(p);
                        }
                        for off in [6usize, 2] {
                            if let Some(s) = get(i, j, (dir + off) & 7, k, sign) {
                                sum += i32::from(sec_taps[k]) * constrain(s - x, sec);
                                mx = mx.max(s);
                                mn = mn.min(s);
                            }
                        }
                    }
                }
                let v = (x + ((8 + sum - i32::from(sum < 0)) >> 4)).clamp(mn, mx);
                dst.set(x0 + j, y0 + i, v as u16);
            }
        }
    }

    // ---- Super-resolution (7.16) ----

    fn upscale(&self, input: &Frame) -> Frame {
        let up_w = self.hdr.upscaled_width as usize;
        let frame_w = self.hdr.frame_width as usize;
        let frame_h = self.hdr.frame_height as usize;
        let max_v = (1i32 << self.bit_depth) - 1;
        let mut planes = Vec::new();
        for (plane, src) in input.planes.iter().enumerate() {
            let (sub_x, sub_y) = self.plane_sub(plane);
            let down_w = ((frame_w + sub_x) >> sub_x) as i64;
            let upw = ((up_w + sub_x) >> sub_x) as i64;
            let plane_h = (frame_h + sub_y) >> sub_y;
            let step_x = ((down_w << 14) + upw / 2) / upw;
            let err = upw * step_x - (down_w << 14);
            let mut init = (-((upw - down_w) << 13) + upw / 2) / upw + (1 << 7) - err / 2;
            init &= (1 << 14) - 1;
            let mi_w = (self.mi_cols >> sub_x) as i64;
            let max_x = mi_w * 4 - 1;
            let alloc_w = (((up_w + 127) & !127) >> sub_x) + 32;
            let mut dst = Plane::new(alloc_w.max(src.w), src.h);
            for y in 0..plane_h {
                for x in 0..upw {
                    let src_x = -(1i64 << 14) + init + x * step_x;
                    let px = src_x >> 14;
                    let sub = ((src_x & ((1 << 14) - 1)) >> 8) as usize;
                    let mut sum = 0i32;
                    for k in 0..8 {
                        let sx = (px + k as i64 - 3).clamp(0, max_x) as usize;
                        sum += i32::from(src.get(sx, y)) * i32::from(t::UPSCALE_FILTER[sub][k]);
                    }
                    dst.set(x as usize, y, round2(sum, 7).clamp(0, max_v) as u16);
                }
            }
            planes.push(dst);
        }
        Frame { planes }
    }

    // ---- Loop restoration (7.17) ----

    fn loop_restoration(&self, cur: &Frame, cdef: &Frame) -> Frame {
        let mut out = cdef.clone();
        let up_w = self.hdr.upscaled_width as usize;
        let frame_h = self.hdr.frame_height as usize;
        for plane in 0..self.num_planes {
            if self.hdr.frame_restoration_type[plane] == RESTORE_NONE {
                continue;
            }
            let (sub_x, sub_y) = self.plane_sub(plane);
            let unit_size = self.hdr.loop_restoration_size[plane] as usize;
            let plane_w = (up_w + sub_x) >> sub_x;
            let plane_h = (frame_h + sub_y) >> sub_y;
            let unit_rows = self.lr_rows[plane];
            let unit_cols = self.lr_cols[plane];
            let mut stripe = 0usize;
            loop {
                let luma_start = stripe as i32 * 64 - 8;
                let sy = (luma_start >> sub_y).max(0) as usize;
                if sy >= plane_h {
                    break;
                }
                let stripe_start = luma_start >> sub_y;
                let stripe_end = stripe_start + (64 >> sub_y) - 1;
                let ey = ((stripe_end + 1) as usize).min(plane_h);
                let unit_row = unit_rows
                    .saturating_sub(1)
                    .min(((stripe * 64) >> sub_y) / unit_size);
                for unit_col in 0..unit_cols {
                    let x0 = unit_col * unit_size;
                    let x1 = if unit_col + 1 == unit_cols {
                        plane_w
                    } else {
                        ((unit_col + 1) * unit_size).min(plane_w)
                    };
                    if x0 >= x1 {
                        continue;
                    }
                    let u = self.lr_units[plane][unit_row * unit_cols + unit_col];
                    if u.ty == RESTORE_NONE {
                        continue;
                    }
                    let (w, h) = (x1 - x0, ey - sy);
                    // Padded source window (3 samples on each side), per get_source_sample (7.17.6).
                    let ww = w + 6;
                    let mut win = vec![0i32; ww * (h + 6)];
                    let pe_x = plane_w as i32 - 1;
                    let pe_y = plane_h as i32 - 1;
                    for r in 0..h + 6 {
                        let mut yy = (sy as i32 + r as i32 - 3).min(pe_y).max(0);
                        let from_cur;
                        if yy < stripe_start {
                            yy = yy.max(stripe_start - 2);
                            from_cur = true;
                        } else if yy > stripe_end {
                            yy = yy.min(stripe_end + 2);
                            from_cur = true;
                        } else {
                            from_cur = false;
                        }
                        let src = if from_cur {
                            &cur.planes[plane]
                        } else {
                            &cdef.planes[plane]
                        };
                        for c in 0..ww {
                            let xx = (x0 as i32 + c as i32 - 3).min(pe_x).max(0);
                            win[r * ww + c] = i32::from(src.get(xx as usize, yy as usize));
                        }
                    }
                    let dst = &mut out.planes[plane];
                    if u.ty == RESTORE_WIENER {
                        self.wiener(dst, &win, ww, x0, sy, w, h, &u);
                    } else {
                        self.self_guided(dst, &win, ww, x0, sy, w, h, &u);
                    }
                }
                stripe += 1;
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn wiener(
        &self,
        dst: &mut Plane,
        win: &[i32],
        ww: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        u: &LrUnit,
    ) {
        let bd = self.bit_depth;
        let (round0, round1) = if bd == 12 { (5, 9) } else { (3, 11) };
        let coef = |c: &[i8; 3]| -> [i32; 7] {
            let mut f = [0i32; 7];
            f[3] = 128;
            for i in 0..3 {
                let v = i32::from(c[i]);
                f[i] = v;
                f[6 - i] = v;
                f[3] -= 2 * v;
            }
            f
        };
        let vf = coef(&u.wiener[0]);
        let hf = coef(&u.wiener[1]);
        let offset = 1i32 << (bd + 7 - round0 - 1);
        let limit = (1i32 << (bd + 1 + 7 - round0)) - 1;
        let mut inter = vec![0i32; (h + 6) * w];
        for r in 0..h + 6 {
            for c in 0..w {
                let mut s = 0;
                for k in 0..7 {
                    s += hf[k] * win[r * ww + c + k];
                }
                inter[r * w + c] = round2(s, round0).clamp(-offset, limit - offset);
            }
        }
        let max_v = (1i32 << bd) - 1;
        for r in 0..h {
            for c in 0..w {
                let mut s = 0;
                for k in 0..7 {
                    s += vf[k] * inter[(r + k) * w + c];
                }
                dst.set(x + c, y + r, round2(s, round1).clamp(0, max_v) as u16);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn self_guided(
        &self,
        dst: &mut Plane,
        win: &[i32],
        ww: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        u: &LrUnit,
    ) {
        let bd = self.bit_depth;
        let set = usize::from(u.sgr_set);
        let flt0 = self.box_filter(win, ww, w, h, set, 0);
        let flt1 = self.box_filter(win, ww, w, h, set, 1);
        let w0 = i64::from(u.sgr_xqd[0]);
        let w1 = i64::from(u.sgr_xqd[1]);
        let w2 = (1i64 << 7) - w0 - w1;
        let r0 = t::SGR_PARAMS[set][0];
        let r1 = t::SGR_PARAMS[set][2];
        let max_v = (1i64 << bd) - 1;
        for i in 0..h {
            for j in 0..w {
                let uu = i64::from(win[(i + 3) * ww + j + 3]) << 4;
                let mut v = w1 * uu;
                v += if r0 != 0 {
                    w0 * i64::from(flt0[i * w + j])
                } else {
                    w0 * uu
                };
                v += if r1 != 0 {
                    w2 * i64::from(flt1[i * w + j])
                } else {
                    w2 * uu
                };
                let s = (v + (1 << 10)) >> 11;
                dst.set(x + j, y + i, s.clamp(0, max_v) as u16);
            }
        }
    }

    fn box_filter(
        &self,
        win: &[i32],
        ww: usize,
        w: usize,
        h: usize,
        set: usize,
        pass: usize,
    ) -> Vec<i32> {
        let r = i32::from(t::SGR_PARAMS[set][pass * 2]);
        if r == 0 {
            return Vec::new();
        }
        let eps = i64::from(t::SGR_PARAMS[set][pass * 2 + 1]);
        let bd = self.bit_depth;
        let n = ((2 * r + 1) * (2 * r + 1)) as i64;
        let n2e = n * n * eps;
        let s = ((1i64 << 20) + n2e / 2) / n2e;
        let one_over_n = ((1i64 << 12) + n / 2) / n;
        let aw = w + 2;
        let mut aa = vec![0i32; aw * (h + 2)];
        let mut bb = vec![0i32; aw * (h + 2)];
        for i in 0..h + 2 {
            if pass == 0 && (i & 1) == 1 {
                // Only odd rows of A and B (spec index i - 1 odd) are used when pass is 0.
                continue;
            }
            for j in 0..w + 2 {
                let mut a: i64 = 0;
                let mut b: i64 = 0;
                for dy in -r..=r {
                    for dx in -r..=r {
                        let c = i64::from(
                            win[((i as i32 + 2 + dy) as usize) * ww + (j as i32 + 2 + dx) as usize],
                        );
                        a += c * c;
                        b += c;
                    }
                }
                let sh = 2 * (bd - 8);
                let a = if sh == 0 {
                    a
                } else {
                    (a + (1 << (sh - 1))) >> sh
                };
                let sh2 = bd - 8;
                let d = if sh2 == 0 {
                    b
                } else {
                    (b + (1 << (sh2 - 1))) >> sh2
                };
                let p = (a * n - d * d).max(0);
                let z = (p * s + (1 << 19)) >> 20;
                let a2 = if z >= 255 {
                    256
                } else if z == 0 {
                    1
                } else {
                    ((z << 8) + z / 2) / (z + 1)
                };
                let b2 = (256 - a2) * b * one_over_n;
                aa[i * aw + j] = a2 as i32;
                bb[i * aw + j] = ((b2 + (1 << 11)) >> 12) as i32;
            }
        }
        let mut f = vec![0i32; w * h];
        for i in 0..h {
            let shift = if pass == 0 && (i & 1) == 1 { 4 } else { 5 };
            for j in 0..w {
                let mut a: i64 = 0;
                let mut b: i64 = 0;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let weight = if pass == 0 {
                            if ((i as i32 + dy) & 1) != 0 {
                                if dx == 0 {
                                    6
                                } else {
                                    5
                                }
                            } else {
                                0
                            }
                        } else if dx == 0 || dy == 0 {
                            4
                        } else {
                            3
                        };
                        if weight != 0 {
                            let idx =
                                ((i as i32 + 1 + dy) as usize) * aw + (j as i32 + 1 + dx) as usize;
                            a += weight * i64::from(aa[idx]);
                            b += weight * i64::from(bb[idx]);
                        }
                    }
                }
                let v = a * i64::from(win[(i + 3) * ww + j + 3]) + b;
                let sh = 8 + shift - 4;
                f[i * w + j] = ((v + (1 << (sh - 1))) >> sh) as i32;
            }
        }
        f
    }
}

#[allow(clippy::too_many_arguments)]
fn sample_filter(
    p: &mut Plane,
    x: usize,
    y: usize,
    plane: usize,
    limit: i32,
    blimit: i32,
    thresh: i32,
    dx: usize,
    dy: usize,
    filter_size: usize,
    bd: u32,
) {
    let stride = p.stride;
    let step = dx + dy * stride;
    let pos = y * stride + x;
    // q_k = d[pos + k*step], p_k = d[pos - (k+1)*step]
    let q = |k: usize| i32::from(p.d[pos + k * step]);
    let pp = |k: usize| i32::from(p.d[pos - (k + 1) * step]);
    let sh = bd - 8;
    let hev = {
        let tb = thresh << sh;
        (pp(1) - pp(0)).abs() > tb || (q(1) - q(0)).abs() > tb
    };
    let filter_len = if filter_size == 4 {
        4
    } else if plane != 0 {
        6
    } else if filter_size == 8 {
        8
    } else {
        16
    };
    let limit_bd = limit << sh;
    let blimit_bd = blimit << sh;
    let mut mask = (pp(1) - pp(0)).abs() > limit_bd
        || (q(1) - q(0)).abs() > limit_bd
        || (pp(0) - q(0)).abs() * 2 + (pp(1) - q(1)).abs() / 2 > blimit_bd;
    if filter_len >= 6 {
        mask |= (pp(2) - pp(1)).abs() > limit_bd || (q(2) - q(1)).abs() > limit_bd;
    }
    if filter_len >= 8 {
        mask |= (pp(3) - pp(2)).abs() > limit_bd || (q(3) - q(2)).abs() > limit_bd;
    }
    if mask {
        return;
    }
    let one = 1i32 << sh;
    let mut flat = false;
    if filter_size >= 8 {
        let mut m = (pp(1) - pp(0)).abs() > one
            || (q(1) - q(0)).abs() > one
            || (pp(2) - pp(0)).abs() > one
            || (q(2) - q(0)).abs() > one;
        if filter_len >= 8 {
            m |= (pp(3) - pp(0)).abs() > one || (q(3) - q(0)).abs() > one;
        }
        flat = !m;
    }
    let mut flat2 = false;
    if filter_size >= 16 {
        let m = (pp(6) - pp(0)).abs() > one
            || (q(6) - q(0)).abs() > one
            || (pp(5) - pp(0)).abs() > one
            || (q(5) - q(0)).abs() > one
            || (pp(4) - pp(0)).abs() > one
            || (q(4) - q(0)).abs() > one;
        flat2 = !m;
    }
    if filter_size == 4 || !flat {
        // Narrow filter (7.14.6.3).
        let lo = -(1i32 << (bd - 1));
        let hi = (1i32 << (bd - 1)) - 1;
        let c = |v: i32| v.clamp(lo, hi);
        let off = 0x80 << sh;
        let (ps1, ps0, qs0, qs1) = (pp(1) - off, pp(0) - off, q(0) - off, q(1) - off);
        let mut filter = if hev { c(ps1 - qs1) } else { 0 };
        filter = c(filter + 3 * (qs0 - ps0));
        let filter1 = c(filter + 4) >> 3;
        let filter2 = c(filter + 3) >> 3;
        p.d[pos] = (c(qs0 - filter1) + off) as u16;
        p.d[pos - step] = (c(ps0 + filter2) + off) as u16;
        if !hev {
            let f = (filter1 + 1) >> 1;
            p.d[pos + step] = (c(qs1 - f) + off) as u16;
            p.d[pos - 2 * step] = (c(ps1 + f) + off) as u16;
        }
        return;
    }
    let log2 = if filter_size == 8 || !flat2 { 3 } else { 4 };
    let n: i32 = if log2 == 4 {
        6
    } else if plane == 0 {
        3
    } else {
        2
    };
    let n2: i32 = if log2 == 3 && plane == 0 { 0 } else { 1 };
    let mut vals = [0i32; 14];
    for k in -(n + 1)..=n {
        vals[(k + 7) as usize] =
            i32::from(p.d[(pos as isize + k as isize * step as isize) as usize]);
    }
    let mut out = [0i32; 14];
    for i in -n..n {
        let mut tsum = 0;
        for j in -n..=n {
            let pi = (i + j).clamp(-(n + 1), n);
            let tap = if j.abs() <= n2 { 2 } else { 1 };
            tsum += vals[(pi + 7) as usize] * tap;
        }
        out[(i + 7) as usize] = round2(tsum, log2);
    }
    for i in -n..n {
        p.d[(pos as isize + i as isize * step as isize) as usize] = out[(i + 7) as usize] as u16;
    }
}
