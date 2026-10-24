//! Intra, palette and chroma-from-luma prediction (sections 7.11.2, 7.11.4, 7.11.5).
use crate::consts::*;
use crate::dec::*;
use crate::tables as t;

const OFF: usize = 16;
const EDGE_LEN: usize = OFF + 2 * 160;

fn sm_weights(log2: usize) -> &'static [u8] {
    match log2 {
        2 => &t::SM_WEIGHTS_TX_4X4,
        3 => &t::SM_WEIGHTS_TX_8X8,
        4 => &t::SM_WEIGHTS_TX_16X16,
        5 => &t::SM_WEIGHTS_TX_32X32,
        _ => &t::SM_WEIGHTS_TX_64X64,
    }
}

fn edge_strength(w: usize, h: usize, filter_type: bool, delta: i32) -> usize {
    let d = delta.abs();
    let blk = w + h;
    let mut s = 0;
    if !filter_type {
        if blk <= 8 {
            if d >= 56 {
                s = 1;
            }
        } else if blk <= 16 {
            if d >= 40 {
                s = 1;
            }
        } else if blk <= 24 {
            if d >= 8 {
                s = 1;
            }
            if d >= 16 {
                s = 2;
            }
            if d >= 32 {
                s = 3;
            }
        } else if blk <= 32 {
            s = 1;
            if d >= 4 {
                s = 2;
            }
            if d >= 32 {
                s = 3;
            }
        } else {
            s = 3;
        }
    } else if blk <= 8 {
        if d >= 40 {
            s = 1;
        }
        if d >= 64 {
            s = 2;
        }
    } else if blk <= 16 {
        if d >= 20 {
            s = 1;
        }
        if d >= 48 {
            s = 2;
        }
    } else if blk <= 24 {
        if d >= 4 {
            s = 3;
        }
    } else {
        s = 3;
    }
    s
}

fn use_upsample(w: usize, h: usize, filter_type: bool, delta: i32) -> bool {
    let d = delta.abs();
    if d <= 0 || d >= 40 {
        false
    } else if !filter_type {
        w + h <= 16
    } else {
        w + h <= 8
    }
}

fn edge_filter(buf: &mut [i32; EDGE_LEN], sz: usize, strength: usize) {
    if strength == 0 {
        return;
    }
    let mut edge = [0i32; 160];
    for i in 0..sz {
        edge[i] = buf[OFF + i - 1];
    }
    for i in 1..sz {
        let mut s = 0;
        for j in 0..5 {
            let k = (i as i32 - 2 + j as i32).clamp(0, sz as i32 - 1) as usize;
            s += i32::from(t::INTRA_EDGE_KERNEL[strength - 1][j]) * edge[k];
        }
        buf[OFF + i - 1] = (s + 8) >> 4;
    }
}

fn edge_upsample(buf: &mut [i32; EDGE_LEN], num_px: usize, maxv: i32) {
    let mut dup = [0i32; 40];
    dup[0] = buf[OFF - 1];
    for i in -1..num_px as i32 {
        dup[(i + 2) as usize] = buf[(OFF as i32 + i) as usize];
    }
    dup[num_px + 2] = buf[OFF + num_px - 1];
    buf[OFF - 2] = dup[0];
    for i in 0..num_px {
        let s = -dup[i] + 9 * dup[i + 1] + 9 * dup[i + 2] - dup[i + 3];
        let s = ((s + 8) >> 4).clamp(0, maxv);
        buf[OFF + 2 * i - 1] = s;
        buf[OFF + 2 * i] = dup[i + 2];
    }
}

impl Dec {
    fn is_smooth(&self, row: usize, col: usize, plane: usize) -> bool {
        let m = self.mi_at(row, col);
        let mode = if plane == 0 {
            m.y_mode
        } else {
            if m.ref_frame[0] > INTRA_FRAME {
                return false;
            }
            m.uv_mode
        };
        matches!(mode, SMOOTH_PRED | SMOOTH_V_PRED | SMOOTH_H_PRED)
    }

    fn get_filter_type(&self, plane: usize) -> bool {
        let mut above = false;
        let mut left = false;
        if if plane == 0 {
            self.b.avail_u
        } else {
            self.b.avail_u_chroma
        } {
            let mut r = self.b.mi_row - 1;
            let mut c = self.b.mi_col;
            if plane > 0 {
                if self.subx != 0 && (self.b.mi_col & 1) == 0 {
                    c += 1;
                }
                if self.suby != 0 && (self.b.mi_row & 1) == 1 {
                    r -= 1;
                }
            }
            above = self.is_smooth(r, c, plane);
        }
        if if plane == 0 {
            self.b.avail_l
        } else {
            self.b.avail_l_chroma
        } {
            let mut r = self.b.mi_row;
            let mut c = self.b.mi_col - 1;
            if plane > 0 {
                if self.subx != 0 && (self.b.mi_col & 1) == 1 {
                    c -= 1;
                }
                if self.suby != 0 && (self.b.mi_row & 1) == 0 {
                    r += 1;
                }
            }
            left = self.is_smooth(r, c, plane);
        }
        above || left
    }

    #[allow(clippy::too_many_arguments)]
    pub fn predict_intra(
        &mut self,
        plane: usize,
        x: usize,
        y: usize,
        have_left: bool,
        have_above: bool,
        have_above_rt: bool,
        have_below_lft: bool,
        mode: u8,
        log2w: usize,
        log2h: usize,
    ) {
        let w = 1usize << log2w;
        let h = 1usize << log2h;
        let bd = self.bit_depth;
        let maxv = (1i32 << bd) - 1;
        let (max_x, max_y) = if plane > 0 {
            (
                ((self.mi_cols * 4) >> self.subx) - 1,
                ((self.mi_rows * 4) >> self.suby) - 1,
            )
        } else {
            (self.mi_cols * 4 - 1, self.mi_rows * 4 - 1)
        };
        let mut above = [0i32; EDGE_LEN];
        let mut left = [0i32; EDGE_LEN];
        {
            let pl = &self.cur.planes[plane];
            let px = |xx: usize, yy: usize| i32::from(pl.d[yy * pl.stride + xx]);
            let n = w + h;
            if !have_above && have_left {
                let v = px(x - 1, y);
                for i in 0..n {
                    above[OFF + i] = v;
                }
            } else if !have_above {
                for i in 0..n {
                    above[OFF + i] = (1 << (bd - 1)) - 1;
                }
            } else {
                let limit = max_x.min(x + (if have_above_rt { 2 * w } else { w }) - 1);
                for i in 0..n {
                    above[OFF + i] = px(limit.min(x + i), y - 1);
                }
            }
            if !have_left && have_above {
                let v = px(x, y - 1);
                for i in 0..n {
                    left[OFF + i] = v;
                }
            } else if !have_left {
                for i in 0..n {
                    left[OFF + i] = (1 << (bd - 1)) + 1;
                }
            } else {
                let limit = max_y.min(y + (if have_below_lft { 2 * h } else { h }) - 1);
                for i in 0..n {
                    left[OFF + i] = px(x - 1, limit.min(y + i));
                }
            }
            let corner = if have_above && have_left {
                px(x - 1, y - 1)
            } else if have_above {
                px(x, y - 1)
            } else if have_left {
                px(x - 1, y)
            } else {
                1 << (bd - 1)
            };
            above[OFF - 1] = corner;
            left[OFF - 1] = corner;
        }
        let mut pred = vec![0i32; w * h];
        let a = |i: i32| above[(OFF as i32 + i) as usize];
        let l = |i: i32| left[(OFF as i32 + i) as usize];
        if plane == 0 && self.b.use_filter_intra {
            let w4 = w >> 2;
            let h2 = h >> 1;
            let fm = usize::from(self.b.filter_intra_mode);
            for i2 in 0..h2 {
                for j4 in 0..w4 {
                    let mut p = [0i32; 7];
                    for (i, pv) in p.iter_mut().enumerate() {
                        *pv = if i < 5 {
                            if i2 == 0 {
                                a(((j4 << 2) + i) as i32 - 1)
                            } else if j4 == 0 && i == 0 {
                                l(((i2 << 1) as i32) - 1)
                            } else {
                                pred[((i2 << 1) - 1) * w + (j4 << 2) + i - 1]
                            }
                        } else if j4 == 0 {
                            l(((i2 << 1) + i - 5) as i32)
                        } else {
                            pred[((i2 << 1) + i - 5) * w + (j4 << 2) - 1]
                        };
                    }
                    for i1 in 0..2 {
                        for j1 in 0..4 {
                            let mut pr = 0i32;
                            for (i, pv) in p.iter().enumerate() {
                                pr += i32::from(t::INTRA_FILTER_TAPS[fm][(i1 << 2) + j1][i]) * pv;
                            }
                            let v = if pr >= 0 {
                                (pr + 8) >> 4
                            } else {
                                -((-pr + 8) >> 4)
                            };
                            pred[((i2 << 1) + i1) * w + (j4 << 2) + j1] = v.clamp(0, maxv);
                        }
                    }
                }
            }
        } else if is_directional_mode(mode) {
            let angle_delta = if plane == 0 {
                self.b.angle_delta_y
            } else {
                self.b.angle_delta_uv
            };
            let p_angle = i32::from(t::MODE_TO_ANGLE[usize::from(mode)]) + angle_delta * ANGLE_STEP;
            let mut up_a = false;
            let mut up_l = false;
            if self.seq.enable_intra_edge_filter {
                let mut filter_type = false;
                if p_angle != 90 && p_angle != 180 {
                    if p_angle > 90 && p_angle < 180 && w + h >= 24 {
                        let s = l(0) * 5 + a(-1) * 6 + a(0) * 5;
                        let v = (s + 8) >> 4;
                        left[OFF - 1] = v;
                        above[OFF - 1] = v;
                    }
                    filter_type = self.get_filter_type(plane);
                    if have_above {
                        let strength = edge_strength(w, h, filter_type, p_angle - 90);
                        let num_px = w.min(max_x - x + 1) + (if p_angle < 90 { h } else { 0 }) + 1;
                        edge_filter(&mut above, num_px, strength);
                    }
                    if have_left {
                        let strength = edge_strength(w, h, filter_type, p_angle - 180);
                        let num_px = h.min(max_y - y + 1) + (if p_angle > 180 { w } else { 0 }) + 1;
                        edge_filter(&mut left, num_px, strength);
                    }
                }
                up_a = use_upsample(w, h, filter_type, p_angle - 90);
                if up_a {
                    let num_px = w + if p_angle < 90 { h } else { 0 };
                    edge_upsample(&mut above, num_px, maxv);
                }
                up_l = use_upsample(w, h, filter_type, p_angle - 180);
                if up_l {
                    let num_px = h + if p_angle > 180 { w } else { 0 };
                    edge_upsample(&mut left, num_px, maxv);
                }
            }
            let a = |i: i32| above[(OFF as i32 + i) as usize];
            let l = |i: i32| left[(OFF as i32 + i) as usize];
            let ua = i32::from(up_a);
            let ul = i32::from(up_l);
            let dx = if p_angle < 90 {
                i32::from(t::DR_INTRA_DERIVATIVE[p_angle as usize])
            } else if p_angle > 90 && p_angle < 180 {
                i32::from(t::DR_INTRA_DERIVATIVE[(180 - p_angle) as usize])
            } else {
                0
            };
            let dy = if p_angle > 90 && p_angle < 180 {
                i32::from(t::DR_INTRA_DERIVATIVE[(p_angle - 90) as usize])
            } else if p_angle > 180 {
                i32::from(t::DR_INTRA_DERIVATIVE[(270 - p_angle) as usize])
            } else {
                0
            };
            for i in 0..h as i32 {
                for j in 0..w as i32 {
                    let v = if p_angle < 90 {
                        let idx = (i + 1) * dx;
                        let base = (idx >> (6 - ua)) + (j << ua);
                        let shift = ((idx << ua) >> 1) & 0x1F;
                        let max_base = ((w + h - 1) as i32) << ua;
                        if base < max_base {
                            (a(base) * (32 - shift) + a(base + 1) * shift + 16) >> 5
                        } else {
                            a(max_base)
                        }
                    } else if p_angle > 90 && p_angle < 180 {
                        let idx = (j << 6) - (i + 1) * dx;
                        let base = idx >> (6 - ua);
                        if base >= -(1 << ua) {
                            let shift = ((idx << ua) >> 1) & 0x1F;
                            (a(base) * (32 - shift) + a(base + 1) * shift + 16) >> 5
                        } else {
                            let idx = (i << 6) - (j + 1) * dy;
                            let base = idx >> (6 - ul);
                            let shift = ((idx << ul) >> 1) & 0x1F;
                            (l(base) * (32 - shift) + l(base + 1) * shift + 16) >> 5
                        }
                    } else if p_angle > 180 {
                        let idx = (j + 1) * dy;
                        let base = (idx >> (6 - ul)) + (i << ul);
                        let shift = ((idx << ul) >> 1) & 0x1F;
                        (l(base) * (32 - shift) + l(base + 1) * shift + 16) >> 5
                    } else if p_angle == 90 {
                        a(j)
                    } else {
                        l(i)
                    };
                    pred[i as usize * w + j as usize] = v;
                }
            }
        } else if matches!(mode, SMOOTH_PRED | SMOOTH_V_PRED | SMOOTH_H_PRED) {
            let wx = sm_weights(log2w);
            let wy = sm_weights(log2h);
            for i in 0..h {
                for j in 0..w {
                    let v = match mode {
                        SMOOTH_PRED => {
                            let s = i32::from(wy[i]) * a(j as i32)
                                + (256 - i32::from(wy[i])) * l(h as i32 - 1)
                                + i32::from(wx[j]) * l(i as i32)
                                + (256 - i32::from(wx[j])) * a(w as i32 - 1);
                            (s + 256) >> 9
                        }
                        SMOOTH_V_PRED => {
                            let s = i32::from(wy[i]) * a(j as i32)
                                + (256 - i32::from(wy[i])) * l(h as i32 - 1);
                            (s + 128) >> 8
                        }
                        _ => {
                            let s = i32::from(wx[j]) * l(i as i32)
                                + (256 - i32::from(wx[j])) * a(w as i32 - 1);
                            (s + 128) >> 8
                        }
                    };
                    pred[i * w + j] = v;
                }
            }
        } else if mode == DC_PRED {
            let v = if have_left && have_above {
                let mut sum = 0i32;
                for k in 0..h as i32 {
                    sum += l(k);
                }
                for k in 0..w as i32 {
                    sum += a(k);
                }
                sum += ((w + h) >> 1) as i32;
                sum / (w + h) as i32
            } else if have_left {
                let mut sum = 0i32;
                for k in 0..h as i32 {
                    sum += l(k);
                }
                ((sum + (h >> 1) as i32) >> log2h).clamp(0, maxv)
            } else if have_above {
                let mut sum = 0i32;
                for k in 0..w as i32 {
                    sum += a(k);
                }
                ((sum + (w >> 1) as i32) >> log2w).clamp(0, maxv)
            } else {
                1 << (bd - 1)
            };
            for p in pred.iter_mut() {
                *p = v;
            }
        } else {
            // PAETH_PRED
            for i in 0..h {
                for j in 0..w {
                    let base = a(j as i32) + l(i as i32) - a(-1);
                    let p_left = (base - l(i as i32)).abs();
                    let p_top = (base - a(j as i32)).abs();
                    let p_tl = (base - a(-1)).abs();
                    pred[i * w + j] = if p_left <= p_top && p_left <= p_tl {
                        l(i as i32)
                    } else if p_top <= p_tl {
                        a(j as i32)
                    } else {
                        a(-1)
                    };
                }
            }
        }
        let pl = &mut self.cur.planes[plane];
        for i in 0..h {
            for j in 0..w {
                let (px, py) = (x + j, y + i);
                if px < pl.stride && py < pl.h {
                    pl.d[py * pl.stride + px] = pred[i * w + j] as u16;
                }
            }
        }
    }

    pub fn predict_palette(
        &mut self,
        plane: usize,
        start_x: usize,
        start_y: usize,
        x: usize,
        y: usize,
        tx_sz: usize,
    ) {
        let w = usize::from(t::TX_WIDTH[tx_sz]);
        let h = usize::from(t::TX_HEIGHT[tx_sz]);
        let palette = match plane {
            0 => self.b.palette_colors_y,
            1 => self.b.palette_colors_u,
            _ => self.b.palette_colors_v,
        };
        let pl = &mut self.cur.planes[plane];
        let map = if plane == 0 {
            &self.b.color_map_y
        } else {
            &self.b.color_map_uv
        };
        for i in 0..h {
            for j in 0..w {
                let (px, py) = (start_x + j, start_y + i);
                if px < pl.stride && py < pl.h {
                    pl.d[py * pl.stride + px] =
                        palette[usize::from(map[(y * 4 + i) * 64 + x * 4 + j])];
                }
            }
        }
    }

    pub fn predict_chroma_from_luma(
        &mut self,
        plane: usize,
        start_x: usize,
        start_y: usize,
        tx_sz: usize,
    ) {
        let w = usize::from(t::TX_WIDTH[tx_sz]);
        let h = usize::from(t::TX_HEIGHT[tx_sz]);
        let sub_x = self.subx;
        let sub_y = self.suby;
        let alpha = if plane == 1 {
            self.b.cfl_alpha_u
        } else {
            self.b.cfl_alpha_v
        };
        let mut lbuf = vec![0i32; w * h];
        let mut luma_avg = 0i32;
        {
            let luma = &self.cur.planes[0];
            for i in 0..h {
                let luma_y = ((start_y + i) << sub_y).min(self.b.max_luma_h - (1 << sub_y));
                for j in 0..w {
                    let luma_x = ((start_x + j) << sub_x).min(self.b.max_luma_w - (1 << sub_x));
                    let mut tt = 0i32;
                    for dy in 0..=sub_y {
                        for dx in 0..=sub_x {
                            tt += i32::from(luma.d[(luma_y + dy) * luma.stride + luma_x + dx]);
                        }
                    }
                    let v = tt << (3 - sub_x - sub_y);
                    lbuf[i * w + j] = v;
                    luma_avg += v;
                }
            }
        }
        let sh = usize::from(t::TX_WIDTH_LOG2[tx_sz]) + usize::from(t::TX_HEIGHT_LOG2[tx_sz]);
        luma_avg = (luma_avg + (1 << (sh - 1))) >> sh;
        let maxv = (1i32 << self.bit_depth) - 1;
        let pl = &mut self.cur.planes[plane];
        for i in 0..h {
            for j in 0..w {
                let idx = (start_y + i) * pl.stride + start_x + j;
                let dc = i32::from(pl.d[idx]);
                let m = alpha * (lbuf[i * w + j] - luma_avg);
                let scaled = if m >= 0 {
                    (m + 32) >> 6
                } else {
                    -((-m + 32) >> 6)
                };
                pl.d[idx] = (dc + scaled).clamp(0, maxv) as u16;
            }
        }
    }
}
