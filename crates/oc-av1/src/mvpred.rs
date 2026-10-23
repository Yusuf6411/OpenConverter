//! Motion vector prediction (sections 7.9 and 7.10).
use crate::consts::*;
use crate::dec::*;
use crate::hdr::*;
use crate::tables as t;

const REF_CAT_LEVEL: u32 = 640;
const MAX_FRAME_DISTANCE: i32 = 31;
const MAX_OFFSET_WIDTH: i32 = 8;
const MAX_OFFSET_HEIGHT: i32 = 0;
const INVALID_MV: i16 = -32768;

fn nb4w(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_WIDE[b])
}
fn nb4h(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_HIGH[b])
}

pub fn round2signed(x: i64, n: u32) -> i64 {
    if x >= 0 {
        (x + (1 << (n - 1))) >> n
    } else {
        -((-x + (1 << (n - 1))) >> n)
    }
}

fn has_newmv(mode: u8) -> bool {
    matches!(
        mode,
        NEWMV | NEW_NEWMV | NEAR_NEWMV | NEW_NEARMV | NEAREST_NEWMV | NEW_NEARESTMV
    )
}

fn get_mv_projection(mv: [i16; 2], numerator: i32, denominator: i32) -> [i32; 2] {
    let den = denominator.min(MAX_FRAME_DISTANCE);
    let num = numerator.clamp(-MAX_FRAME_DISTANCE, MAX_FRAME_DISTANCE);
    let mut out = [0; 2];
    for i in 0..2 {
        let scaled = round2signed(
            i64::from(mv[i]) * i64::from(num) * i64::from(t::DIV_MULT[den as usize]),
            14,
        ) as i32;
        out[i] = scaled.clamp(-(1 << 14) + 1, (1 << 14) - 1);
    }
    out
}

fn project(v8: i32, delta: i32, dst_sign: i32, max8: i32, max_off8: i32) -> Option<i32> {
    let base8 = (v8 >> 3) << 3;
    let offset8 = if delta >= 0 {
        delta >> 6
    } else {
        -((-delta) >> 6)
    };
    let v = v8 + dst_sign * offset8;
    if v < 0 || v >= max8 || v < base8 - max_off8 || v >= base8 + 8 + max_off8 {
        None
    } else {
        Some(v)
    }
}

impl Dec {
    // ---------------------------------------------------------------------------------------
    // 7.9 motion field estimation

    pub fn motion_field_estimation(&mut self) {
        let w8 = self.mi_cols >> 1;
        let h8 = self.mi_rows >> 1;
        self.motion_field_mvs = vec![[[INVALID_MV; 2]; 8]; w8 * h8];
        let last_idx = self.hdr.ref_frame_idx[0];
        let cur_gold = self.hdr.order_hints[GOLDEN_FRAME as usize];
        let last_alt = self.refh[last_idx].saved_order_hints[ALTREF_FRAME as usize];
        if last_alt != cur_gold {
            self.mf_project(LAST_FRAME as usize, -1);
        }
        let mut ref_stamp = MFMV_STACK_SIZE as i32 - 2;
        let dist = |s: &Dec, r: i8| {
            get_relative_dist(&s.seq, s.hdr.order_hints[r as usize], s.hdr.order_hint)
        };
        if dist(self, BWDREF_FRAME) > 0 && self.mf_project(BWDREF_FRAME as usize, 1) {
            ref_stamp -= 1;
        }
        if dist(self, ALTREF2_FRAME) > 0 && self.mf_project(ALTREF2_FRAME as usize, 1) {
            ref_stamp -= 1;
        }
        if dist(self, ALTREF_FRAME) > 0
            && ref_stamp >= 0
            && self.mf_project(ALTREF_FRAME as usize, 1)
        {
            ref_stamp -= 1;
        }
        if ref_stamp >= 0 {
            self.mf_project(LAST2_FRAME as usize, -1);
        }
    }

    fn mf_project(&mut self, src: usize, dst_sign: i32) -> bool {
        let src_idx = self.hdr.ref_frame_idx[src - 1];
        let w8 = (self.mi_cols >> 1) as i32;
        let h8 = (self.mi_rows >> 1) as i32;
        let rh = &self.refh[src_idx];
        if rh.mi_rows as usize != self.mi_rows
            || rh.mi_cols as usize != self.mi_cols
            || rh.frame_type == INTRA_ONLY_FRAME
            || rh.frame_type == KEY_FRAME
        {
            return false;
        }
        let rd = match &self.refd[src_idx] {
            Some(r) => r.clone(),
            None => return false,
        };
        let saved_hints = rh.saved_order_hints;
        for y8 in 0..h8 {
            for x8 in 0..w8 {
                let row = (2 * y8 + 1) as usize;
                let col = (2 * x8 + 1) as usize;
                let src_ref = rd.mf_ref_frames[row * rd.mi_stride + col];
                if src_ref > INTRA_FRAME {
                    let ref_to_cur = get_relative_dist(
                        &self.seq,
                        self.hdr.order_hints[src],
                        self.hdr.order_hint,
                    );
                    let ref_offset = get_relative_dist(
                        &self.seq,
                        self.hdr.order_hints[src],
                        saved_hints[src_ref as usize],
                    );
                    let pos_valid = ref_to_cur.abs() <= MAX_FRAME_DISTANCE
                        && ref_offset.abs() <= MAX_FRAME_DISTANCE
                        && ref_offset > 0;
                    if pos_valid {
                        let mv = rd.mf_mvs[row * rd.mi_stride + col];
                        let proj = get_mv_projection(mv, ref_to_cur * dst_sign, ref_offset);
                        let py = project(y8, proj[0], dst_sign, h8, MAX_OFFSET_HEIGHT);
                        let px = project(x8, proj[1], dst_sign, w8, MAX_OFFSET_WIDTH);
                        if let (Some(py), Some(px)) = (py, px) {
                            for dst in 1..=7usize {
                                let ref_to_dst = get_relative_dist(
                                    &self.seq,
                                    self.hdr.order_hint,
                                    self.hdr.order_hints[dst],
                                );
                                let p = get_mv_projection(mv, ref_to_dst, ref_offset);
                                self.motion_field_mvs[(py * w8 + px) as usize][dst] =
                                    [p[0] as i16, p[1] as i16];
                            }
                        }
                    }
                }
            }
        }
        true
    }

    // ---------------------------------------------------------------------------------------
    // 7.10 motion vector prediction

    pub fn lower_mv_precision(&self, mv: &mut [i32; 2]) {
        if self.hdr.allow_high_precision_mv {
            return;
        }
        for v in mv.iter_mut() {
            if self.hdr.force_integer_mv {
                let a = v.abs();
                let a_int = (a + 3) >> 3;
                *v = if *v > 0 { a_int << 3 } else { -(a_int << 3) };
            } else if *v & 1 != 0 {
                if *v > 0 {
                    *v -= 1;
                } else {
                    *v += 1;
                }
            }
        }
    }

    fn setup_global_mv(&self, ref_list: usize) -> [i32; 2] {
        let r = self.b.ref_frame[ref_list];
        let mut mv = [0i32; 2];
        if r != INTRA_FRAME {
            let typ = self.hdr.gm_type[r as usize];
            let gm = &self.hdr.gm_params[r as usize];
            let bw = block_width(self.b.mi_size) as i64;
            let bh = block_height(self.b.mi_size) as i64;
            if typ == IDENTITY {
            } else if typ == TRANSLATION {
                mv[0] = gm[0] >> (WARPEDMODEL_PREC_BITS - 3);
                mv[1] = gm[1] >> (WARPEDMODEL_PREC_BITS - 3);
            } else {
                let x = self.b.mi_col as i64 * 4 + bw / 2 - 1;
                let y = self.b.mi_row as i64 * 4 + bh / 2 - 1;
                let xc = (i64::from(gm[2]) - (1 << WARPEDMODEL_PREC_BITS)) * x
                    + i64::from(gm[3]) * y
                    + i64::from(gm[0]);
                let yc = i64::from(gm[4]) * x
                    + (i64::from(gm[5]) - (1 << WARPEDMODEL_PREC_BITS)) * y
                    + i64::from(gm[1]);
                if self.hdr.allow_high_precision_mv {
                    mv[0] = round2signed(yc, WARPEDMODEL_PREC_BITS - 3) as i32;
                    mv[1] = round2signed(xc, WARPEDMODEL_PREC_BITS - 3) as i32;
                } else {
                    mv[0] = round2signed(yc, WARPEDMODEL_PREC_BITS - 2) as i32 * 2;
                    mv[1] = round2signed(xc, WARPEDMODEL_PREC_BITS - 2) as i32 * 2;
                }
            }
        }
        self.lower_mv_precision(&mut mv);
        mv
    }

    pub fn find_mv_stack(&mut self, is_compound: bool) {
        let bw4 = nb4w(self.b.mi_size);
        let bh4 = nb4h(self.b.mi_size);
        self.b.num_mv_found = 0;
        self.b.new_mv_count = 0;
        self.b.global_mvs[0] = self.setup_global_mv(0);
        if is_compound {
            self.b.global_mvs[1] = self.setup_global_mv(1);
        }
        self.b.found_match = false;
        self.scan_row(-1, is_compound);
        let mut found_above = self.b.found_match;
        self.b.found_match = false;
        self.scan_col(-1, is_compound);
        let mut found_left = self.b.found_match;
        self.b.found_match = false;
        if bw4.max(bh4) <= 16 {
            self.scan_point(-1, bw4 as i32, is_compound);
        }
        if self.b.found_match {
            found_above = true;
        }
        self.b.close_matches = usize::from(found_above) + usize::from(found_left);
        let num_nearest = self.b.num_mv_found;
        let num_new = self.b.new_mv_count;
        for i in 0..num_nearest {
            self.b.weight_stack[i] += REF_CAT_LEVEL;
        }
        self.b.zero_mv_context = 0;
        if self.hdr.use_ref_frame_mvs {
            self.temporal_scan(is_compound);
        }
        self.scan_point(-1, -1, is_compound);
        if self.b.found_match {
            found_above = true;
        }
        self.b.found_match = false;
        self.scan_row(-3, is_compound);
        if self.b.found_match {
            found_above = true;
        }
        self.b.found_match = false;
        self.scan_col(-3, is_compound);
        if self.b.found_match {
            found_left = true;
        }
        self.b.found_match = false;
        if bh4 > 1 {
            self.scan_row(-5, is_compound);
        }
        if self.b.found_match {
            found_above = true;
        }
        self.b.found_match = false;
        if bw4 > 1 {
            self.scan_col(-5, is_compound);
        }
        if self.b.found_match {
            found_left = true;
        }
        self.b.total_matches = usize::from(found_above) + usize::from(found_left);
        self.sort_stack(0, num_nearest, is_compound);
        self.sort_stack(num_nearest, self.b.num_mv_found, is_compound);
        if self.b.num_mv_found < 2 {
            self.extra_search(is_compound);
        }
        self.context_and_clamping(is_compound, num_new);
    }

    fn scan_row(&mut self, delta_row: i32, is_compound: bool) {
        let bw4 = nb4w(self.b.mi_size);
        let end4 = bw4.min(self.mi_cols - self.b.mi_col).min(16);
        let mut delta_row = delta_row;
        let mut delta_col = 0i32;
        let use_step16 = bw4 >= 16;
        if delta_row.abs() > 1 {
            delta_row += (self.b.mi_row & 1) as i32;
            delta_col = 1 - (self.b.mi_col & 1) as i32;
        }
        let mut i = 0usize;
        while i < end4 {
            let mv_row = self.b.mi_row as i32 + delta_row;
            let mv_col = self.b.mi_col as i32 + delta_col + i as i32;
            if !self.is_inside(i64::from(mv_row), i64::from(mv_col)) {
                break;
            }
            let mut len = bw4.min(nb4w(usize::from(
                self.mi_at(mv_row as usize, mv_col as usize).mi_size,
            )));
            if delta_row.abs() > 1 {
                len = len.max(2);
            }
            if use_step16 {
                len = len.max(4);
            }
            self.add_ref_mv_candidate(
                mv_row as usize,
                mv_col as usize,
                is_compound,
                (len * 2) as u32,
            );
            i += len;
        }
    }

    fn scan_col(&mut self, delta_col: i32, is_compound: bool) {
        let bh4 = nb4h(self.b.mi_size);
        let end4 = bh4.min(self.mi_rows - self.b.mi_row).min(16);
        let mut delta_row = 0i32;
        let mut delta_col = delta_col;
        let use_step16 = bh4 >= 16;
        if delta_col.abs() > 1 {
            delta_row = 1 - (self.b.mi_row & 1) as i32;
            delta_col += (self.b.mi_col & 1) as i32;
        }
        let mut i = 0usize;
        while i < end4 {
            let mv_row = self.b.mi_row as i32 + delta_row + i as i32;
            let mv_col = self.b.mi_col as i32 + delta_col;
            if !self.is_inside(i64::from(mv_row), i64::from(mv_col)) {
                break;
            }
            let mut len = bh4.min(nb4h(usize::from(
                self.mi_at(mv_row as usize, mv_col as usize).mi_size,
            )));
            if delta_col.abs() > 1 {
                len = len.max(2);
            }
            if use_step16 {
                len = len.max(4);
            }
            self.add_ref_mv_candidate(
                mv_row as usize,
                mv_col as usize,
                is_compound,
                (len * 2) as u32,
            );
            i += len;
        }
    }

    fn scan_point(&mut self, delta_row: i32, delta_col: i32, is_compound: bool) {
        let mv_row = self.b.mi_row as i32 + delta_row;
        let mv_col = self.b.mi_col as i32 + delta_col;
        if self.is_inside(i64::from(mv_row), i64::from(mv_col))
            && self.mi_at(mv_row as usize, mv_col as usize).written
        {
            self.add_ref_mv_candidate(mv_row as usize, mv_col as usize, is_compound, 4);
        }
    }

    fn temporal_scan(&mut self, is_compound: bool) {
        let bw4 = nb4w(self.b.mi_size) as i32;
        let bh4 = nb4h(self.b.mi_size) as i32;
        let step_w4 = if bw4 >= 16 { 4 } else { 2 };
        let step_h4 = if bh4 >= 16 { 4 } else { 2 };
        let mut dr = 0;
        while dr < bh4.min(16) {
            let mut dc = 0;
            while dc < bw4.min(16) {
                self.add_tpl_ref_mv(dr, dc, is_compound);
                dc += step_w4;
            }
            dr += step_h4;
        }
        let allow_ext = (2..16).contains(&bh4) && (2..16).contains(&bw4);
        if allow_ext {
            let pos = [[bh4, -2], [bh4, bw4], [bh4 - 2, bw4]];
            for p in pos {
                let row = (self.b.mi_row & 15) as i32 + p[0];
                let col = (self.b.mi_col & 15) as i32 + p[1];
                if (0..16).contains(&row) && (0..16).contains(&col) {
                    self.add_tpl_ref_mv(p[0], p[1], is_compound);
                }
            }
        }
    }

    fn add_tpl_ref_mv(&mut self, delta_row: i32, delta_col: i32, is_compound: bool) {
        let mv_row = (self.b.mi_row as i32 + delta_row) | 1;
        let mv_col = (self.b.mi_col as i32 + delta_col) | 1;
        if !self.is_inside(i64::from(mv_row), i64::from(mv_col)) {
            return;
        }
        let x8 = (mv_col >> 1) as usize;
        let y8 = (mv_row >> 1) as usize;
        let w8 = self.mi_cols >> 1;
        let at_origin = delta_row == 0 && delta_col == 0;
        if at_origin {
            self.b.zero_mv_context = 1;
        }
        let field = self.motion_field_mvs[y8 * w8 + x8];
        let to32 = |m: [i16; 2]| [i32::from(m[0]), i32::from(m[1])];
        if !is_compound {
            let c = field[self.b.ref_frame[0] as usize];
            if c[0] == INVALID_MV {
                return;
            }
            let mut cand = to32(c);
            self.lower_mv_precision(&mut cand);
            if at_origin {
                let g = self.b.global_mvs[0];
                self.b.zero_mv_context =
                    usize::from((cand[0] - g[0]).abs() >= 16 || (cand[1] - g[1]).abs() >= 16);
            }
            let n = self.b.num_mv_found;
            let mut idx = 0;
            while idx < n {
                if cand == self.b.ref_stack_mv[idx][0] {
                    break;
                }
                idx += 1;
            }
            if idx < n {
                self.b.weight_stack[idx] += 2;
            } else if n < MAX_REF_MV_STACK_SIZE {
                self.b.ref_stack_mv[n][0] = cand;
                self.b.weight_stack[n] = 2;
                self.b.num_mv_found += 1;
            }
        } else {
            let c0 = field[self.b.ref_frame[0] as usize];
            if c0[0] == INVALID_MV {
                return;
            }
            let c1 = field[self.b.ref_frame[1] as usize];
            if c1[0] == INVALID_MV {
                return;
            }
            let mut cand0 = to32(c0);
            let mut cand1 = to32(c1);
            self.lower_mv_precision(&mut cand0);
            self.lower_mv_precision(&mut cand1);
            if at_origin {
                let g = self.b.global_mvs;
                self.b.zero_mv_context = usize::from(
                    (cand0[0] - g[0][0]).abs() >= 16
                        || (cand0[1] - g[0][1]).abs() >= 16
                        || (cand1[0] - g[1][0]).abs() >= 16
                        || (cand1[1] - g[1][1]).abs() >= 16,
                );
            }
            let n = self.b.num_mv_found;
            let mut idx = 0;
            while idx < n {
                if cand0 == self.b.ref_stack_mv[idx][0] && cand1 == self.b.ref_stack_mv[idx][1] {
                    break;
                }
                idx += 1;
            }
            if idx < n {
                self.b.weight_stack[idx] += 2;
            } else if n < MAX_REF_MV_STACK_SIZE {
                self.b.ref_stack_mv[n][0] = cand0;
                self.b.ref_stack_mv[n][1] = cand1;
                self.b.weight_stack[n] = 2;
                self.b.num_mv_found += 1;
            }
        }
    }

    fn add_ref_mv_candidate(
        &mut self,
        mv_row: usize,
        mv_col: usize,
        is_compound: bool,
        weight: u32,
    ) {
        let m = *self.mi_at(mv_row, mv_col);
        if !m.is_inter {
            return;
        }
        if !is_compound {
            for cand_list in 0..2 {
                if m.ref_frame[cand_list] == self.b.ref_frame[0] {
                    self.search_stack(&m, cand_list, weight);
                }
            }
        } else if m.ref_frame[0] == self.b.ref_frame[0] && m.ref_frame[1] == self.b.ref_frame[1] {
            self.compound_search_stack(&m, weight);
        }
    }

    fn search_stack(&mut self, m: &MiInfo, cand_list: usize, weight: u32) {
        let cand_mode = m.y_mode;
        let cand_size = usize::from(m.mi_size);
        let large = block_width(cand_size).min(block_height(cand_size)) >= 8;
        let mut cand_mv = if (cand_mode == GLOBALMV || cand_mode == GLOBAL_GLOBALMV)
            && self.hdr.gm_type[self.b.ref_frame[0] as usize] > TRANSLATION
            && large
        {
            self.b.global_mvs[0]
        } else {
            [i32::from(m.mv[cand_list][0]), i32::from(m.mv[cand_list][1])]
        };
        self.lower_mv_precision(&mut cand_mv);
        if has_newmv(cand_mode) {
            self.b.new_mv_count += 1;
        }
        self.b.found_match = true;
        let n = self.b.num_mv_found;
        let mut idx = 0;
        while idx < n {
            if cand_mv == self.b.ref_stack_mv[idx][0] {
                break;
            }
            idx += 1;
        }
        if idx < n {
            self.b.weight_stack[idx] += weight;
        } else if n < MAX_REF_MV_STACK_SIZE {
            self.b.ref_stack_mv[n][0] = cand_mv;
            self.b.weight_stack[n] = weight;
            self.b.num_mv_found += 1;
        }
    }

    fn compound_search_stack(&mut self, m: &MiInfo, weight: u32) {
        let mut cand = [
            [i32::from(m.mv[0][0]), i32::from(m.mv[0][1])],
            [i32::from(m.mv[1][0]), i32::from(m.mv[1][1])],
        ];
        let cand_mode = m.y_mode;
        if cand_mode == GLOBAL_GLOBALMV {
            for rl in 0..2 {
                if self.hdr.gm_type[self.b.ref_frame[rl] as usize] > TRANSLATION {
                    cand[rl] = self.b.global_mvs[rl];
                }
            }
        }
        for c in cand.iter_mut() {
            self.lower_mv_precision(c);
        }
        self.b.found_match = true;
        let n = self.b.num_mv_found;
        let mut idx = 0;
        while idx < n {
            if cand[0] == self.b.ref_stack_mv[idx][0] && cand[1] == self.b.ref_stack_mv[idx][1] {
                break;
            }
            idx += 1;
        }
        if idx < n {
            self.b.weight_stack[idx] += weight;
        } else if n < MAX_REF_MV_STACK_SIZE {
            self.b.ref_stack_mv[n][0] = cand[0];
            self.b.ref_stack_mv[n][1] = cand[1];
            self.b.weight_stack[n] = weight;
            self.b.num_mv_found += 1;
        }
        if has_newmv(cand_mode) {
            self.b.new_mv_count += 1;
        }
    }

    fn sort_stack(&mut self, start: usize, end: usize, is_compound: bool) {
        let mut end = end;
        while end > start {
            let mut new_end = start;
            for idx in start + 1..end {
                if self.b.weight_stack[idx - 1] < self.b.weight_stack[idx] {
                    self.b.weight_stack.swap(idx - 1, idx);
                    for list in 0..1 + usize::from(is_compound) {
                        let tmp = self.b.ref_stack_mv[idx - 1][list];
                        self.b.ref_stack_mv[idx - 1][list] = self.b.ref_stack_mv[idx][list];
                        self.b.ref_stack_mv[idx][list] = tmp;
                    }
                    new_end = idx;
                }
            }
            end = new_end;
        }
    }

    fn extra_search(&mut self, is_compound: bool) {
        let mut ref_id_count = [0usize; 2];
        let mut ref_diff_count = [0usize; 2];
        let mut ref_id_mvs = [[[0i32; 2]; 2]; 2];
        let mut ref_diff_mvs = [[[0i32; 2]; 2]; 2];
        let mut w4 = nb4w(self.b.mi_size).min(16);
        let mut h4 = nb4h(self.b.mi_size).min(16);
        w4 = w4.min(self.mi_cols - self.b.mi_col);
        h4 = h4.min(self.mi_rows - self.b.mi_row);
        let num4x4 = w4.min(h4);
        for pass in 0..2 {
            let mut idx = 0usize;
            while idx < num4x4 && self.b.num_mv_found < 2 {
                let (mv_row, mv_col) = if pass == 0 {
                    (self.b.mi_row as i64 - 1, (self.b.mi_col + idx) as i64)
                } else {
                    ((self.b.mi_row + idx) as i64, self.b.mi_col as i64 - 1)
                };
                if !self.is_inside(mv_row, mv_col) {
                    break;
                }
                let m = *self.mi_at(mv_row as usize, mv_col as usize);
                // add_extra_mv_candidate
                if is_compound {
                    for cand_list in 0..2 {
                        let cand_ref = m.ref_frame[cand_list];
                        if cand_ref > INTRA_FRAME {
                            for list in 0..2 {
                                let mut cand_mv =
                                    [i32::from(m.mv[cand_list][0]), i32::from(m.mv[cand_list][1])];
                                if cand_ref == self.b.ref_frame[list] && ref_id_count[list] < 2 {
                                    ref_id_mvs[list][ref_id_count[list]] = cand_mv;
                                    ref_id_count[list] += 1;
                                } else if ref_diff_count[list] < 2 {
                                    if self.hdr.ref_frame_sign_bias[cand_ref as usize]
                                        != self.hdr.ref_frame_sign_bias
                                            [self.b.ref_frame[list] as usize]
                                    {
                                        cand_mv[0] *= -1;
                                        cand_mv[1] *= -1;
                                    }
                                    ref_diff_mvs[list][ref_diff_count[list]] = cand_mv;
                                    ref_diff_count[list] += 1;
                                }
                            }
                        }
                    }
                } else {
                    for cand_list in 0..2 {
                        let cand_ref = m.ref_frame[cand_list];
                        if cand_ref > INTRA_FRAME {
                            let mut cand_mv =
                                [i32::from(m.mv[cand_list][0]), i32::from(m.mv[cand_list][1])];
                            if self.hdr.ref_frame_sign_bias[cand_ref as usize]
                                != self.hdr.ref_frame_sign_bias[self.b.ref_frame[0] as usize]
                            {
                                cand_mv[0] *= -1;
                                cand_mv[1] *= -1;
                            }
                            let n = self.b.num_mv_found;
                            let mut i = 0;
                            while i < n {
                                if cand_mv == self.b.ref_stack_mv[i][0] {
                                    break;
                                }
                                i += 1;
                            }
                            if i == n {
                                self.b.ref_stack_mv[i][0] = cand_mv;
                                self.b.weight_stack[i] = 2;
                                self.b.num_mv_found += 1;
                            }
                        }
                    }
                }
                if pass == 0 {
                    idx += nb4w(usize::from(m.mi_size));
                } else {
                    idx += nb4h(usize::from(m.mi_size));
                }
            }
        }
        if is_compound {
            let mut combined = [[[0i32; 2]; 2]; 2];
            for list in 0..2 {
                let mut comp_count = 0;
                for idx in 0..ref_id_count[list] {
                    combined[comp_count][list] = ref_id_mvs[list][idx];
                    comp_count += 1;
                }
                let mut idx = 0;
                while idx < ref_diff_count[list] && comp_count < 2 {
                    combined[comp_count][list] = ref_diff_mvs[list][idx];
                    comp_count += 1;
                    idx += 1;
                }
                while comp_count < 2 {
                    combined[comp_count][list] = self.b.global_mvs[list];
                    comp_count += 1;
                }
            }
            if self.b.num_mv_found == 1 {
                let n = self.b.num_mv_found;
                if combined[0][0] == self.b.ref_stack_mv[0][0]
                    && combined[0][1] == self.b.ref_stack_mv[0][1]
                {
                    self.b.ref_stack_mv[n] = combined[1];
                } else {
                    self.b.ref_stack_mv[n] = combined[0];
                }
                self.b.weight_stack[n] = 2;
                self.b.num_mv_found += 1;
            } else {
                for idx in 0..2 {
                    let n = self.b.num_mv_found;
                    self.b.ref_stack_mv[n] = combined[idx];
                    self.b.weight_stack[n] = 2;
                    self.b.num_mv_found += 1;
                }
            }
        } else {
            for idx in self.b.num_mv_found..2 {
                self.b.ref_stack_mv[idx][0] = self.b.global_mvs[0];
            }
        }
    }

    fn clamp_mv_row(&self, mvec: i32, border: i32) -> i32 {
        let bh4 = nb4h(self.b.mi_size) as i32;
        let to_top = -((self.b.mi_row as i32 * 4) * 8);
        let to_bottom = ((self.mi_rows as i32 - bh4 - self.b.mi_row as i32) * 4) * 8;
        mvec.clamp(to_top - border, to_bottom + border)
    }

    fn clamp_mv_col(&self, mvec: i32, border: i32) -> i32 {
        let bw4 = nb4w(self.b.mi_size) as i32;
        let to_left = -((self.b.mi_col as i32 * 4) * 8);
        let to_right = ((self.mi_cols as i32 - bw4 - self.b.mi_col as i32) * 4) * 8;
        mvec.clamp(to_left - border, to_right + border)
    }

    fn context_and_clamping(&mut self, is_compound: bool, num_new: usize) {
        let bw = block_width(self.b.mi_size) as i32;
        let bh = block_height(self.b.mi_size) as i32;
        let n = self.b.num_mv_found;
        for idx in 0..n {
            let mut z = 0;
            if idx + 1 < n {
                let w0 = self.b.weight_stack[idx];
                let w1 = self.b.weight_stack[idx + 1];
                if w0 >= REF_CAT_LEVEL {
                    if w1 < REF_CAT_LEVEL {
                        z = 1;
                    }
                } else {
                    z = 2;
                }
            }
            self.b.drl_ctx_stack[idx] = z;
        }
        for list in 0..1 + usize::from(is_compound) {
            for idx in 0..n {
                let mv = self.b.ref_stack_mv[idx][list];
                self.b.ref_stack_mv[idx][list] = [
                    self.clamp_mv_row(mv[0], MV_BORDER + bh * 8),
                    self.clamp_mv_col(mv[1], MV_BORDER + bw * 8),
                ];
            }
        }
        let close = self.b.close_matches;
        let total = self.b.total_matches;
        if close == 0 {
            self.b.new_mv_context = total.min(1);
            self.b.ref_mv_context = total;
        } else if close == 1 {
            self.b.new_mv_context = 3 - num_new.min(1);
            self.b.ref_mv_context = 2 + total;
        } else {
            self.b.new_mv_context = 5 - num_new.min(1);
            self.b.ref_mv_context = 5;
        }
    }

    pub fn has_overlappable_candidates(&self) -> bool {
        if self.b.avail_u {
            let w4 = nb4w(self.b.mi_size);
            let mut x4 = self.b.mi_col;
            while x4 < self.mi_cols.min(self.b.mi_col + w4) {
                if self.mi_at(self.b.mi_row - 1, x4 | 1).ref_frame[0] > INTRA_FRAME {
                    return true;
                }
                x4 += 2;
            }
        }
        if self.b.avail_l {
            let h4 = nb4h(self.b.mi_size);
            let mut y4 = self.b.mi_row;
            while y4 < self.mi_rows.min(self.b.mi_row + h4) {
                if self.mi_at(y4 | 1, self.b.mi_col - 1).ref_frame[0] > INTRA_FRAME {
                    return true;
                }
                y4 += 2;
            }
        }
        false
    }

    pub fn find_warp_samples(&mut self) {
        self.b.num_samples = 0;
        self.b.num_samples_scanned = 0;
        let w4 = nb4w(self.b.mi_size);
        let h4 = nb4h(self.b.mi_size);
        let (mi_row, mi_col) = (self.b.mi_row, self.b.mi_col);
        let mut do_top_left = true;
        let mut do_top_right = true;
        if self.b.avail_u {
            let src_size = usize::from(self.mi_at(mi_row - 1, mi_col).mi_size);
            let src_w = nb4w(src_size);
            if w4 <= src_w {
                let col_offset = -((mi_col & (src_w - 1)) as i32);
                if col_offset < 0 {
                    do_top_left = false;
                }
                if col_offset + src_w as i32 > w4 as i32 {
                    do_top_right = false;
                }
                self.add_sample(-1, 0);
            } else {
                let mut i = 0usize;
                while i < w4.min(self.mi_cols - mi_col) {
                    let src_size = usize::from(self.mi_at(mi_row - 1, mi_col + i).mi_size);
                    let src_w = nb4w(src_size);
                    let step = w4.min(src_w);
                    self.add_sample(-1, i as i32);
                    i += step;
                }
            }
        }
        if self.b.avail_l {
            let src_size = usize::from(self.mi_at(mi_row, mi_col - 1).mi_size);
            let src_h = nb4h(src_size);
            if h4 <= src_h {
                let row_offset = -((mi_row & (src_h - 1)) as i32);
                if row_offset < 0 {
                    do_top_left = false;
                }
                self.add_sample(0, -1);
            } else {
                let mut i = 0usize;
                while i < h4.min(self.mi_rows - mi_row) {
                    let src_size = usize::from(self.mi_at(mi_row + i, mi_col - 1).mi_size);
                    let src_h = nb4h(src_size);
                    let step = h4.min(src_h);
                    self.add_sample(i as i32, -1);
                    i += step;
                }
            }
        }
        if do_top_left {
            self.add_sample(-1, -1);
        }
        if do_top_right && w4.max(h4) <= 16 {
            self.add_sample(-1, w4 as i32);
        }
        if self.b.num_samples == 0 && self.b.num_samples_scanned > 0 {
            self.b.num_samples = 1;
        }
    }

    fn add_sample(&mut self, delta_row: i32, delta_col: i32) {
        if self.b.num_samples_scanned >= 8 {
            return;
        }
        let mv_row = self.b.mi_row as i32 + delta_row;
        let mv_col = self.b.mi_col as i32 + delta_col;
        if !self.is_inside(i64::from(mv_row), i64::from(mv_col)) {
            return;
        }
        let m = *self.mi_at(mv_row as usize, mv_col as usize);
        if !m.written {
            return;
        }
        if m.ref_frame[0] != self.b.ref_frame[0] || m.ref_frame[1] != NONE {
            return;
        }
        let cand_sz = usize::from(m.mi_size);
        let cand_w4 = nb4w(cand_sz) as i32;
        let cand_h4 = nb4h(cand_sz) as i32;
        let cand_row = mv_row & !(cand_h4 - 1);
        let cand_col = mv_col & !(cand_w4 - 1);
        let mid_y = cand_row * 4 + cand_h4 * 2 - 1;
        let mid_x = cand_col * 4 + cand_w4 * 2 - 1;
        let threshold =
            (block_width(self.b.mi_size).max(block_height(self.b.mi_size)) as i32).clamp(16, 112);
        let cm = self.mi_at(cand_row as usize, cand_col as usize).mv[0];
        let mv_diff_row = (i32::from(cm[0]) - self.b.mv[0][0]).abs();
        let mv_diff_col = (i32::from(cm[1]) - self.b.mv[0][1]).abs();
        let valid = mv_diff_row + mv_diff_col <= threshold;
        let cand = [
            mid_y * 8,
            mid_x * 8,
            mid_y * 8 + i32::from(cm[0]),
            mid_x * 8 + i32::from(cm[1]),
        ];
        self.b.num_samples_scanned += 1;
        if !valid && self.b.num_samples_scanned > 1 {
            return;
        }
        self.b.cand_list[self.b.num_samples] = cand;
        if valid {
            self.b.num_samples += 1;
        }
    }
}
