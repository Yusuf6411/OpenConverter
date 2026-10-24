//! Tile, partition and block level syntax (sections 5.11.2 - 5.11.17, 5.11.42 - 5.11.58).
use crate::consts::*;
use crate::dec::*;
use crate::tables as t;

fn nb4w(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_WIDE[b])
}
fn nb4h(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_HIGH[b])
}

impl Dec {
    pub fn is_inside(&self, r: i64, c: i64) -> bool {
        c >= self.mi_col_start as i64
            && c < self.mi_col_end as i64
            && r >= self.mi_row_start as i64
            && r < self.mi_row_end as i64
    }

    pub fn decode_tile(&mut self) {
        // clear_above_context
        for p in 0..3 {
            self.above_level_ctx[p].iter_mut().for_each(|v| *v = 0);
            self.above_dc_ctx[p].iter_mut().for_each(|v| *v = 0);
        }
        self.above_seg_pred.iter_mut().for_each(|v| *v = 0);
        self.delta_lf = [0; 4];
        for plane in 0..self.num_planes {
            for pass in 0..2 {
                self.ref_sgr_xqd[plane][pass] = i32::from(t::SGRPROJ_XQD_MID[pass]);
                for i in 0..3 {
                    self.ref_lr_wiener[plane][pass][i] = i32::from(t::WIENER_TAPS_MID[i]);
                }
            }
        }
        let sb_size = if self.seq.use_128x128_superblock {
            BLOCK_128X128
        } else {
            BLOCK_64X64
        };
        let sb4 = nb4w(sb_size);
        let mut r = self.mi_row_start;
        while r < self.mi_row_end {
            // clear_left_context
            for p in 0..3 {
                self.left_level_ctx[p].iter_mut().for_each(|v| *v = 0);
                self.left_dc_ctx[p].iter_mut().for_each(|v| *v = 0);
            }
            self.left_seg_pred.iter_mut().for_each(|v| *v = 0);
            let mut c = self.mi_col_start;
            while c < self.mi_col_end {
                self.read_deltas = self.hdr.delta_q_present;
                self.clear_cdef(r, c);
                self.clear_block_decoded_flags(r, c, sb4);
                self.read_lr(r, c, sb_size);
                self.decode_partition(r, c, sb_size);
                c += sb4;
            }
            r += sb4;
        }
    }

    fn clear_cdef(&mut self, r: usize, c: usize) {
        let s = self.cdef_stride;
        self.cdef_idx[(r >> 4) * s + (c >> 4)] = -1;
        if self.seq.use_128x128_superblock {
            self.cdef_idx[(r >> 4) * s + (c >> 4) + 1] = -1;
            self.cdef_idx[((r >> 4) + 1) * s + (c >> 4)] = -1;
            self.cdef_idx[((r >> 4) + 1) * s + (c >> 4) + 1] = -1;
        }
    }

    fn clear_block_decoded_flags(&mut self, r: usize, c: usize, sb4: usize) {
        for plane in 0..self.num_planes {
            let sub_x = if plane > 0 { self.subx } else { 0 };
            let sub_y = if plane > 0 { self.suby } else { 0 };
            let sb_w4 = ((self.mi_col_end - c) >> sub_x) as i32;
            let sb_h4 = ((self.mi_row_end - r) >> sub_y) as i32;
            for y in -1..=((sb4 >> sub_y) as i32) {
                for x in -1..=((sb4 >> sub_x) as i32) {
                    let v = (y < 0 && x < sb_w4) || (x < 0 && y < sb_h4);
                    let i = self.bd_idx(plane, y, x);
                    self.block_decoded[i] = v;
                }
            }
            let i = self.bd_idx(plane, (sb4 >> sub_y) as i32, -1);
            self.block_decoded[i] = false;
        }
    }

    fn partition_cdf_ctx(
        &self,
        r: usize,
        c: usize,
        bsize: usize,
        avail_u: bool,
        avail_l: bool,
    ) -> (usize, usize) {
        let bsl = usize::from(t::MI_WIDTH_LOG2[bsize]);
        let above = avail_u
            && usize::from(t::MI_WIDTH_LOG2[usize::from(self.mi_at(r - 1, c).mi_size)]) < bsl;
        let left = avail_l
            && usize::from(t::MI_HEIGHT_LOG2[usize::from(self.mi_at(r, c - 1).mi_size)]) < bsl;
        (bsl, usize::from(left) * 2 + usize::from(above))
    }

    fn decode_partition(&mut self, r: usize, c: usize, bsize: usize) {
        if r >= self.mi_rows || c >= self.mi_cols {
            return;
        }
        let avail_u = self.is_inside(r as i64 - 1, c as i64);
        let avail_l = self.is_inside(r as i64, c as i64 - 1);
        let num4x4 = nb4w(bsize);
        let half = num4x4 >> 1;
        let quarter = half >> 1;
        let has_rows = (r + half) < self.mi_rows;
        let has_cols = (c + half) < self.mi_cols;
        let partition;
        if bsize < BLOCK_8X8 {
            partition = PARTITION_NONE;
        } else if has_rows && has_cols {
            let (bsl, ctx) = self.partition_cdf_ctx(r, c, bsize, avail_u, avail_l);
            partition = match bsl {
                1 => self.sym.symbol(&mut self.cdf.partition_w8[ctx]),
                2 => self.sym.symbol(&mut self.cdf.partition_w16[ctx]),
                3 => self.sym.symbol(&mut self.cdf.partition_w32[ctx]),
                4 => self.sym.symbol(&mut self.cdf.partition_w64[ctx]),
                _ => self.sym.symbol(&mut self.cdf.partition_w128[ctx]),
            };
        } else if has_cols || has_rows {
            let (bsl, ctx) = self.partition_cdf_ctx(r, c, bsize, avail_u, avail_l);
            let pc: Vec<u16> = match bsl {
                2 => self.cdf.partition_w16[ctx].to_vec(),
                3 => self.cdf.partition_w32[ctx].to_vec(),
                4 => self.cdf.partition_w64[ctx].to_vec(),
                _ => self.cdf.partition_w128[ctx].to_vec(),
            };
            let p = |i: usize| i32::from(pc[i]) - if i > 0 { i32::from(pc[i - 1]) } else { 0 };
            let mut psum;
            if has_cols {
                psum = p(PARTITION_VERT)
                    + p(PARTITION_SPLIT)
                    + p(PARTITION_HORZ_A)
                    + p(PARTITION_VERT_A)
                    + p(PARTITION_VERT_B);
                if bsize != BLOCK_128X128 {
                    psum += p(PARTITION_VERT_4);
                }
            } else {
                psum = p(PARTITION_HORZ)
                    + p(PARTITION_SPLIT)
                    + p(PARTITION_HORZ_A)
                    + p(PARTITION_HORZ_B)
                    + p(PARTITION_VERT_A);
                if bsize != BLOCK_128X128 {
                    psum += p(PARTITION_HORZ_4);
                }
            }
            let mut cdf = [(32768 - psum) as u16, 32768u16 as u16, 0];
            cdf[1] = 32768u32 as u16;
            let s = self.sym.symbol(&mut cdf);
            partition = if has_cols {
                if s != 0 {
                    PARTITION_SPLIT
                } else {
                    PARTITION_HORZ
                }
            } else if s != 0 {
                PARTITION_SPLIT
            } else {
                PARTITION_VERT
            };
        } else {
            partition = PARTITION_SPLIT;
        }
        let sub_size = usize::from(t::PARTITION_SUBSIZE[partition][bsize]);
        let split_size = usize::from(t::PARTITION_SUBSIZE[PARTITION_SPLIT][bsize]);
        match partition {
            PARTITION_NONE => self.decode_block(r, c, sub_size),
            PARTITION_HORZ => {
                self.decode_block(r, c, sub_size);
                if has_rows {
                    self.decode_block(r + half, c, sub_size);
                }
            }
            PARTITION_VERT => {
                self.decode_block(r, c, sub_size);
                if has_cols {
                    self.decode_block(r, c + half, sub_size);
                }
            }
            PARTITION_SPLIT => {
                self.decode_partition(r, c, sub_size);
                self.decode_partition(r, c + half, sub_size);
                self.decode_partition(r + half, c, sub_size);
                self.decode_partition(r + half, c + half, sub_size);
            }
            PARTITION_HORZ_A => {
                self.decode_block(r, c, split_size);
                self.decode_block(r, c + half, split_size);
                self.decode_block(r + half, c, sub_size);
            }
            PARTITION_HORZ_B => {
                self.decode_block(r, c, sub_size);
                self.decode_block(r + half, c, split_size);
                self.decode_block(r + half, c + half, split_size);
            }
            PARTITION_VERT_A => {
                self.decode_block(r, c, split_size);
                self.decode_block(r + half, c, split_size);
                self.decode_block(r, c + half, sub_size);
            }
            PARTITION_VERT_B => {
                self.decode_block(r, c, sub_size);
                self.decode_block(r, c + half, split_size);
                self.decode_block(r + half, c + half, split_size);
            }
            PARTITION_HORZ_4 => {
                self.decode_block(r, c, sub_size);
                self.decode_block(r + quarter, c, sub_size);
                self.decode_block(r + quarter * 2, c, sub_size);
                if r + quarter * 3 < self.mi_rows {
                    self.decode_block(r + quarter * 3, c, sub_size);
                }
            }
            _ => {
                self.decode_block(r, c, sub_size);
                self.decode_block(r, c + quarter, sub_size);
                self.decode_block(r, c + quarter * 2, sub_size);
                if c + quarter * 3 < self.mi_cols {
                    self.decode_block(r, c + quarter * 3, sub_size);
                }
            }
        }
    }

    fn decode_block(&mut self, r: usize, c: usize, sub_size: usize) {
        let bw4 = nb4w(sub_size);
        let bh4 = nb4h(sub_size);
        {
            let b = &mut self.b;
            b.mi_row = r;
            b.mi_col = c;
            b.mi_size = sub_size;
        }
        let has_chroma = if bh4 == 1 && self.suby != 0 && (r & 1) == 0 {
            false
        } else if bw4 == 1 && self.subx != 0 && (c & 1) == 0 {
            false
        } else {
            self.num_planes > 1
        };
        let avail_u = self.is_inside(r as i64 - 1, c as i64);
        let avail_l = self.is_inside(r as i64, c as i64 - 1);
        let mut avail_u_chroma = avail_u;
        let mut avail_l_chroma = avail_l;
        if has_chroma {
            if self.suby != 0 && bh4 == 1 {
                avail_u_chroma = self.is_inside(r as i64 - 2, c as i64);
            }
            if self.subx != 0 && bw4 == 1 {
                avail_l_chroma = self.is_inside(r as i64, c as i64 - 2);
            }
        } else {
            avail_u_chroma = false;
            avail_l_chroma = false;
        }
        {
            let b = &mut self.b;
            b.has_chroma = has_chroma;
            b.avail_u = avail_u;
            b.avail_l = avail_l;
            b.avail_u_chroma = avail_u_chroma;
            b.avail_l_chroma = avail_l_chroma;
            // defaults for syntax elements that are not always present
            b.use_intrabc = false;
            b.interintra = false;
            b.motion_mode = SIMPLE;
            b.compound_type = COMPOUND_AVERAGE;
            b.use_filter_intra = false;
            b.angle_delta_y = 0;
            b.angle_delta_uv = 0;
            b.palette_size_y = 0;
            b.palette_size_uv = 0;
            b.interp_filter = [0; 2];
            b.mv = [[0; 2]; 2];
            b.comp_group_idx = 0;
            b.compound_idx = 0;
            b.is_inter_intra = false;
            b.uv_mode = DC_PRED;
        }
        if self.hdr.frame_is_intra {
            self.intra_frame_mode_info();
        } else {
            self.inter_frame_mode_info();
        }
        self.palette_tokens();
        self.read_block_tx_size();
        if self.b.skip {
            self.reset_block_context(bw4, bh4);
        }
        let is_compound = self.b.ref_frame[1] > INTRA_FRAME;
        let stride = self.mi_stride;
        for y in 0..bh4 {
            for x in 0..bw4 {
                let m = &mut self.mi[(r + y) * stride + c + x];
                m.written = true;
                m.y_mode = self.b.y_mode;
                if self.b.ref_frame[0] == INTRA_FRAME && has_chroma {
                    m.uv_mode = self.b.uv_mode;
                }
                m.ref_frame = self.b.ref_frame;
                if self.b.is_inter {
                    if !self.b.use_intrabc {
                        m.comp_group_idx = self.b.comp_group_idx;
                        m.compound_idx = self.b.compound_idx;
                    }
                    m.interp_filter = self.b.interp_filter;
                    for rl in 0..(1 + usize::from(is_compound)) {
                        m.mv[rl] = [self.b.mv[rl][0] as i16, self.b.mv[rl][1] as i16];
                    }
                }
                m.motion_mode = self.b.motion_mode;
                m.is_inter_intra = self.b.is_inter && self.b.ref_frame[1] == INTRA_FRAME;
            }
        }
        self.compute_prediction();
        self.residual();
        for y in 0..bh4 {
            for x in 0..bw4 {
                let idx = (r + y) * stride + c + x;
                let m = &mut self.mi[idx];
                m.is_inter = self.b.is_inter;
                m.skip_mode = self.b.skip_mode;
                m.skip = self.b.skip;
                m.tx_size = self.b.tx_size as u8;
                m.mi_size = self.b.mi_size as u8;
                m.segment_id = self.b.segment_id as u8;
                m.palette_size = [self.b.palette_size_y as u8, self.b.palette_size_uv as u8];
                for i in 0..4 {
                    m.delta_lf[i] = self.delta_lf[i] as i8;
                }
                let pc = &mut self.pal_colors[idx];
                for i in 0..self.b.palette_size_y {
                    pc[0][i] = self.b.palette_colors_y[i];
                }
                for i in 0..self.b.palette_size_uv {
                    pc[1][i] = self.b.palette_colors_u[i];
                }
            }
        }
    }

    fn reset_block_context(&mut self, bw4: usize, bh4: usize) {
        let planes = 1 + 2 * usize::from(self.b.has_chroma);
        for plane in 0..planes {
            let sub_x = if plane > 0 { self.subx } else { 0 };
            let sub_y = if plane > 0 { self.suby } else { 0 };
            for i in (self.b.mi_col >> sub_x)..((self.b.mi_col + bw4) >> sub_x) {
                self.above_level_ctx[plane][i] = 0;
                self.above_dc_ctx[plane][i] = 0;
            }
            for i in (self.b.mi_row >> sub_y)..((self.b.mi_row + bh4) >> sub_y) {
                self.left_level_ctx[plane][i] = 0;
                self.left_dc_ctx[plane][i] = 0;
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Intra frame mode info

    fn intra_frame_mode_info(&mut self) {
        self.b.skip = false;
        if self.hdr.seg_id_pre_skip {
            self.intra_segment_id();
        }
        self.b.skip_mode = false;
        self.read_skip();
        if !self.hdr.seg_id_pre_skip {
            self.intra_segment_id();
        }
        self.read_cdef();
        self.read_delta_qindex();
        self.read_delta_lf();
        self.read_deltas = false;
        self.b.ref_frame = [INTRA_FRAME, NONE];
        self.b.use_intrabc = if self.hdr.allow_intrabc {
            self.sym.symbol(&mut self.cdf.intrabc) != 0
        } else {
            false
        };
        if self.b.use_intrabc {
            self.b.is_inter = true;
            self.b.y_mode = DC_PRED;
            self.b.uv_mode = DC_PRED;
            self.b.motion_mode = SIMPLE;
            self.b.compound_type = COMPOUND_AVERAGE;
            self.b.palette_size_y = 0;
            self.b.palette_size_uv = 0;
            self.b.interp_filter = [BILINEAR, BILINEAR];
            self.find_mv_stack(false);
            self.assign_mv(false);
        } else {
            self.b.is_inter = false;
            let above_mode = if self.b.avail_u {
                self.mi_at(self.b.mi_row - 1, self.b.mi_col).y_mode
            } else {
                DC_PRED
            };
            let left_mode = if self.b.avail_l {
                self.mi_at(self.b.mi_row, self.b.mi_col - 1).y_mode
            } else {
                DC_PRED
            };
            let a = usize::from(t::INTRA_MODE_CONTEXT[usize::from(above_mode)]);
            let l = usize::from(t::INTRA_MODE_CONTEXT[usize::from(left_mode)]);
            self.b.y_mode = self.sym.symbol(&mut self.cdf.intra_frame_y_mode[a][l]) as u8;
            self.intra_angle_info_y();
            if self.b.has_chroma {
                self.read_uv_mode();
                if self.b.uv_mode == UV_CFL_PRED {
                    self.read_cfl_alphas();
                }
                self.intra_angle_info_uv();
            }
            self.b.palette_size_y = 0;
            self.b.palette_size_uv = 0;
            if self.b.mi_size >= BLOCK_8X8
                && block_width(self.b.mi_size) <= 64
                && block_height(self.b.mi_size) <= 64
                && self.hdr.allow_screen_content_tools
            {
                self.palette_mode_info();
            }
            self.filter_intra_mode_info();
        }
    }

    pub fn read_uv_mode(&mut self) {
        let b = &self.b;
        let cfl_allowed = if b.lossless
            && usize::from(t::SUBSAMPLED_SIZE[b.mi_size][self.subx][self.suby]) == BLOCK_4X4
        {
            true
        } else {
            !b.lossless && block_width(b.mi_size).max(block_height(b.mi_size)) <= 32
        };
        let y = usize::from(b.y_mode);
        self.b.uv_mode = if cfl_allowed {
            self.sym.symbol(&mut self.cdf.uv_mode_cfl_allowed[y]) as u8
        } else {
            self.sym.symbol(&mut self.cdf.uv_mode_cfl_not_allowed[y]) as u8
        };
    }

    pub fn intra_angle_info_y(&mut self) {
        self.b.angle_delta_y = 0;
        if self.b.mi_size >= BLOCK_8X8 && is_directional_mode(self.b.y_mode) {
            let v = self
                .sym
                .symbol(&mut self.cdf.angle_delta[usize::from(self.b.y_mode - V_PRED)]);
            self.b.angle_delta_y = v as i32 - MAX_ANGLE_DELTA;
        }
    }

    pub fn intra_angle_info_uv(&mut self) {
        self.b.angle_delta_uv = 0;
        if self.b.mi_size >= BLOCK_8X8 && is_directional_mode(self.b.uv_mode) {
            let v = self
                .sym
                .symbol(&mut self.cdf.angle_delta[usize::from(self.b.uv_mode - V_PRED)]);
            self.b.angle_delta_uv = v as i32 - MAX_ANGLE_DELTA;
        }
    }

    pub fn read_cfl_alphas(&mut self) {
        let signs = self.sym.symbol(&mut self.cdf.cfl_sign);
        let sign_u = (signs + 1) / 3;
        let sign_v = (signs + 1) % 3;
        if sign_u != 0 {
            let ctx = (sign_u - 1) * 3 + sign_v;
            let a = self.sym.symbol(&mut self.cdf.cfl_alpha[ctx]) as i32;
            self.b.cfl_alpha_u = if sign_u == 1 { -(1 + a) } else { 1 + a };
        } else {
            self.b.cfl_alpha_u = 0;
        }
        if sign_v != 0 {
            let ctx = (sign_v - 1) * 3 + sign_u;
            let a = self.sym.symbol(&mut self.cdf.cfl_alpha[ctx]) as i32;
            self.b.cfl_alpha_v = if sign_v == 1 { -(1 + a) } else { 1 + a };
        } else {
            self.b.cfl_alpha_v = 0;
        }
    }

    pub fn filter_intra_mode_info(&mut self) {
        self.b.use_filter_intra = false;
        if self.seq.enable_filter_intra
            && self.b.y_mode == DC_PRED
            && self.b.palette_size_y == 0
            && block_width(self.b.mi_size).max(block_height(self.b.mi_size)) <= 32
        {
            self.b.use_filter_intra =
                self.sym.symbol(&mut self.cdf.filter_intra[self.b.mi_size]) != 0;
            if self.b.use_filter_intra {
                self.b.filter_intra_mode = self.sym.symbol(&mut self.cdf.filter_intra_mode) as u8;
            }
        }
    }

    pub fn seg_feature_active(&self, feature: usize) -> bool {
        self.hdr.seg_feature_active_idx(self.b.segment_id, feature)
    }

    fn intra_segment_id(&mut self) {
        if self.hdr.segmentation_enabled {
            self.read_segment_id();
        } else {
            self.b.segment_id = 0;
        }
        self.b.lossless = self.hdr.lossless_array[self.b.segment_id];
    }

    pub fn read_segment_id(&mut self) {
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let prev_ul: i32 = if self.b.avail_u && self.b.avail_l {
            i32::from(self.mi_at(r - 1, c - 1).segment_id)
        } else {
            -1
        };
        let prev_u: i32 = if self.b.avail_u {
            i32::from(self.mi_at(r - 1, c).segment_id)
        } else {
            -1
        };
        let prev_l: i32 = if self.b.avail_l {
            i32::from(self.mi_at(r, c - 1).segment_id)
        } else {
            -1
        };
        let pred = if prev_u == -1 {
            if prev_l == -1 {
                0
            } else {
                prev_l
            }
        } else if prev_l == -1 {
            prev_u
        } else if prev_ul == prev_u {
            prev_u
        } else {
            prev_l
        };
        if self.b.skip {
            self.b.segment_id = pred as usize;
        } else {
            let ctx = if prev_ul < 0 {
                0
            } else if prev_ul == prev_u && prev_ul == prev_l {
                2
            } else if prev_ul == prev_u || prev_ul == prev_l || prev_u == prev_l {
                1
            } else {
                0
            };
            let v = self.sym.symbol(&mut self.cdf.segment_id[ctx]) as i32;
            let max = self.hdr.last_active_seg_id as i32 + 1;
            let s = neg_deinterleave(v, pred, max).clamp(0, self.hdr.last_active_seg_id as i32);
            self.b.segment_id = s as usize;
        }
    }

    pub fn read_skip(&mut self) {
        if self.hdr.seg_id_pre_skip && self.seg_feature_active(SEG_LVL_SKIP) {
            self.b.skip = true;
        } else {
            let mut ctx = 0;
            if self.b.avail_u {
                ctx += usize::from(self.mi_at(self.b.mi_row - 1, self.b.mi_col).skip);
            }
            if self.b.avail_l {
                ctx += usize::from(self.mi_at(self.b.mi_row, self.b.mi_col - 1).skip);
            }
            self.b.skip = self.sym.symbol(&mut self.cdf.skip[ctx]) != 0;
        }
    }

    pub fn read_cdef(&mut self) {
        if self.b.skip || self.hdr.coded_lossless || !self.seq.enable_cdef || self.hdr.allow_intrabc
        {
            return;
        }
        let r = self.b.mi_row & !15;
        let c = self.b.mi_col & !15;
        let s = self.cdef_stride;
        if self.cdef_idx[(r >> 4) * s + (c >> 4)] == -1 {
            let v = self.sym.literal(self.hdr.cdef_bits) as i8;
            let w4 = nb4w(self.b.mi_size);
            let h4 = nb4h(self.b.mi_size);
            let mut i = r;
            while i < r + h4 {
                let mut j = c;
                while j < c + w4 {
                    self.cdef_idx[(i >> 4) * s + (j >> 4)] = v;
                    j += 16;
                }
                i += 16;
            }
        }
    }

    pub fn read_delta_qindex(&mut self) {
        let sb_size = if self.seq.use_128x128_superblock {
            BLOCK_128X128
        } else {
            BLOCK_64X64
        };
        if self.b.mi_size == sb_size && self.b.skip {
            return;
        }
        if self.read_deltas {
            let mut abs = self.sym.symbol(&mut self.cdf.delta_q);
            if abs == DELTA_Q_SMALL {
                let rem_bits = self.sym.literal(3) + 1;
                let abs_bits = self.sym.literal(rem_bits);
                abs = (abs_bits + (1 << rem_bits) + 1) as usize;
            }
            if abs != 0 {
                let sign = self.sym.literal(1);
                let reduced = if sign != 0 { -(abs as i32) } else { abs as i32 };
                self.current_q_index =
                    (self.current_q_index + (reduced << self.hdr.delta_q_res)).clamp(1, 255);
            }
        }
    }

    pub fn read_delta_lf(&mut self) {
        let sb_size = if self.seq.use_128x128_superblock {
            BLOCK_128X128
        } else {
            BLOCK_64X64
        };
        if self.b.mi_size == sb_size && self.b.skip {
            return;
        }
        if self.read_deltas && self.hdr.delta_lf_present {
            let mut frame_lf_count = 1;
            if self.hdr.delta_lf_multi {
                frame_lf_count = if self.num_planes > 1 { 4 } else { 2 };
            }
            for i in 0..frame_lf_count {
                let mut abs = if self.hdr.delta_lf_multi {
                    self.sym.symbol(&mut self.cdf.delta_lf_multi[i])
                } else {
                    self.sym.symbol(&mut self.cdf.delta_lf)
                };
                if abs == DELTA_LF_SMALL {
                    let n = self.sym.literal(3) + 1;
                    let abs_bits = self.sym.literal(n);
                    abs = (abs_bits + (1 << n) + 1) as usize;
                }
                if abs != 0 {
                    let sign = self.sym.literal(1);
                    let reduced = if sign != 0 { -(abs as i32) } else { abs as i32 };
                    self.delta_lf[i] = (self.delta_lf[i] + (reduced << self.hdr.delta_lf_res))
                        .clamp(-MAX_LOOP_FILTER, MAX_LOOP_FILTER);
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Transform size

    fn get_above_tx_width(&self, row: usize, col: usize) -> usize {
        if row == self.b.mi_row {
            if !self.b.avail_u {
                return 64;
            }
            let m = self.mi_at(row - 1, col);
            if m.skip && m.is_inter {
                return block_width(usize::from(m.mi_size));
            }
        }
        usize::from(t::TX_WIDTH[usize::from(self.mi_at(row - 1, col).inter_tx_size)])
    }

    fn get_left_tx_height(&self, row: usize, col: usize) -> usize {
        if col == self.b.mi_col {
            if !self.b.avail_l {
                return 64;
            }
            let m = self.mi_at(row, col - 1);
            if m.skip && m.is_inter {
                return block_height(usize::from(m.mi_size));
            }
        }
        usize::from(t::TX_HEIGHT[usize::from(self.mi_at(row, col - 1).inter_tx_size)])
    }

    fn read_tx_size(&mut self, allow_select: bool) {
        if self.b.lossless {
            self.b.tx_size = TX_4X4;
            return;
        }
        let mi_size = self.b.mi_size;
        let max_rect = usize::from(t::MAX_TX_SIZE_RECT[mi_size]);
        let max_depth = usize::from(t::MAX_TX_DEPTH[mi_size]);
        self.b.tx_size = max_rect;
        if mi_size > BLOCK_4X4 && allow_select && self.hdr.tx_mode == crate::hdr::TX_MODE_SELECT {
            let (r, c) = (self.b.mi_row, self.b.mi_col);
            let max_w = usize::from(t::TX_WIDTH[max_rect]);
            let max_h = usize::from(t::TX_HEIGHT[max_rect]);
            let above_w = if self.b.avail_u {
                let m = self.mi_at(r - 1, c);
                if m.is_inter {
                    block_width(usize::from(m.mi_size))
                } else {
                    self.get_above_tx_width(r, c)
                }
            } else {
                0
            };
            let left_h = if self.b.avail_l {
                let m = self.mi_at(r, c - 1);
                if m.is_inter {
                    block_height(usize::from(m.mi_size))
                } else {
                    self.get_left_tx_height(r, c)
                }
            } else {
                0
            };
            let ctx = usize::from(above_w >= max_w) + usize::from(left_h >= max_h);
            let depth = match max_depth {
                4 => self.sym.symbol(&mut self.cdf.tx_64x64[ctx]),
                3 => self.sym.symbol(&mut self.cdf.tx_32x32[ctx]),
                2 => self.sym.symbol(&mut self.cdf.tx_16x16[ctx]),
                _ => self.sym.symbol(&mut self.cdf.tx_8x8[ctx]),
            };
            for _ in 0..depth {
                self.b.tx_size = usize::from(t::SPLIT_TX_SIZE[self.b.tx_size]);
            }
        }
    }

    fn read_block_tx_size(&mut self) {
        let bw4 = nb4w(self.b.mi_size);
        let bh4 = nb4h(self.b.mi_size);
        let (r0, c0) = (self.b.mi_row, self.b.mi_col);
        if self.hdr.tx_mode == crate::hdr::TX_MODE_SELECT
            && self.b.mi_size > BLOCK_4X4
            && self.b.is_inter
            && !self.b.skip
            && !self.b.lossless
        {
            let max_tx = usize::from(t::MAX_TX_SIZE_RECT[self.b.mi_size]);
            let tx_w4 = usize::from(t::TX_WIDTH[max_tx]) / 4;
            let tx_h4 = usize::from(t::TX_HEIGHT[max_tx]) / 4;
            let mut row = r0;
            while row < r0 + bh4 {
                let mut col = c0;
                while col < c0 + bw4 {
                    self.read_var_tx_size(row, col, max_tx, 0);
                    col += tx_w4;
                }
                row += tx_h4;
            }
        } else {
            self.read_tx_size(!self.b.skip || !self.b.is_inter);
            let ts = self.b.tx_size as u8;
            for row in r0..r0 + bh4 {
                for col in c0..c0 + bw4 {
                    let i = self.mi_idx(row, col);
                    self.mi[i].inter_tx_size = ts;
                }
            }
        }
    }

    fn read_var_tx_size(&mut self, row: usize, col: usize, tx_sz: usize, depth: usize) {
        if row >= self.mi_rows || col >= self.mi_cols {
            return;
        }
        let split = if tx_sz == TX_4X4 || depth == MAX_VARTX_DEPTH {
            false
        } else {
            let above = self.get_above_tx_width(row, col) < usize::from(t::TX_WIDTH[tx_sz]);
            let left = self.get_left_tx_height(row, col) < usize::from(t::TX_HEIGHT[tx_sz]);
            let size = 64.min(block_width(self.b.mi_size).max(block_height(self.b.mi_size)));
            let max_tx = find_tx_size(size, size);
            let sqr_up = usize::from(t::TX_SIZE_SQR_UP[tx_sz]);
            let ctx = usize::from(sqr_up != max_tx) * 3
                + (4 - max_tx) * 6
                + usize::from(above)
                + usize::from(left);
            self.sym.symbol(&mut self.cdf.txfm_split[ctx]) != 0
        };
        let w4 = usize::from(t::TX_WIDTH[tx_sz]) / 4;
        let h4 = usize::from(t::TX_HEIGHT[tx_sz]) / 4;
        if split {
            let sub = usize::from(t::SPLIT_TX_SIZE[tx_sz]);
            let step_w = usize::from(t::TX_WIDTH[sub]) / 4;
            let step_h = usize::from(t::TX_HEIGHT[sub]) / 4;
            let mut i = 0;
            while i < h4 {
                let mut j = 0;
                while j < w4 {
                    self.read_var_tx_size(row + i, col + j, sub, depth + 1);
                    j += step_w;
                }
                i += step_h;
            }
        } else {
            for i in 0..h4 {
                for j in 0..w4 {
                    let idx = self.mi_idx(row + i, col + j);
                    self.mi[idx].inter_tx_size = tx_sz as u8;
                }
            }
            self.b.tx_size = tx_sz;
        }
    }

    // ---------------------------------------------------------------------------------------
    // Palette

    fn get_palette_cache(&self, plane: usize, cache: &mut [u16; 16]) -> usize {
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let mut above_n = 0;
        if (r * 4) % 64 != 0 {
            above_n = usize::from(self.mi_at(r - 1, c).palette_size[plane]);
        }
        let mut left_n = 0;
        if self.b.avail_l {
            left_n = usize::from(self.mi_at(r, c - 1).palette_size[plane]);
        }
        let above_colors = if above_n > 0 {
            self.pal_colors[self.mi_idx(r - 1, c)][plane]
        } else {
            [0; 8]
        };
        let left_colors = if left_n > 0 {
            self.pal_colors[self.mi_idx(r, c - 1)][plane]
        } else {
            [0; 8]
        };
        let (mut ai, mut li, mut n) = (0, 0, 0);
        while ai < above_n && li < left_n {
            let above_c = above_colors[ai];
            let left_c = left_colors[li];
            if left_c < above_c {
                if n == 0 || left_c != cache[n - 1] {
                    cache[n] = left_c;
                    n += 1;
                }
                li += 1;
            } else {
                if n == 0 || above_c != cache[n - 1] {
                    cache[n] = above_c;
                    n += 1;
                }
                ai += 1;
                if left_c == above_c {
                    li += 1;
                }
            }
        }
        while ai < above_n {
            let v = above_colors[ai];
            ai += 1;
            if n == 0 || v != cache[n - 1] {
                cache[n] = v;
                n += 1;
            }
        }
        while li < left_n {
            let v = left_colors[li];
            li += 1;
            if n == 0 || v != cache[n - 1] {
                cache[n] = v;
                n += 1;
            }
        }
        n
    }

    pub fn palette_mode_info(&mut self) {
        let mi_size = self.b.mi_size;
        let bsize_ctx =
            usize::from(t::MI_WIDTH_LOG2[mi_size]) + usize::from(t::MI_HEIGHT_LOG2[mi_size]) - 2;
        let bd = self.bit_depth;
        let max_val = (1i32 << bd) - 1;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        if self.b.y_mode == DC_PRED {
            let mut ctx = 0;
            if self.b.avail_u && self.mi_at(r - 1, c).palette_size[0] > 0 {
                ctx += 1;
            }
            if self.b.avail_l && self.mi_at(r, c - 1).palette_size[0] > 0 {
                ctx += 1;
            }
            let has = self
                .sym
                .symbol(&mut self.cdf.palette_y_mode[bsize_ctx][ctx])
                != 0;
            if has {
                let size = self.sym.symbol(&mut self.cdf.palette_y_size[bsize_ctx]) + 2;
                self.b.palette_size_y = size;
                let mut cache = [0u16; 16];
                let cache_n = self.get_palette_cache(0, &mut cache);
                let mut idx = 0;
                let mut i = 0;
                while i < cache_n && idx < size {
                    if self.sym.literal(1) != 0 {
                        self.b.palette_colors_y[idx] = cache[i];
                        idx += 1;
                    }
                    i += 1;
                }
                if idx < size {
                    self.b.palette_colors_y[idx] = self.sym.literal(bd) as u16;
                    idx += 1;
                }
                let mut palette_bits = 0;
                if idx < size {
                    let min_bits = bd - 3;
                    palette_bits = min_bits + self.sym.literal(2);
                }
                while idx < size {
                    let delta = self.sym.literal(palette_bits) as i32 + 1;
                    let v = (i32::from(self.b.palette_colors_y[idx - 1]) + delta).clamp(0, max_val);
                    self.b.palette_colors_y[idx] = v as u16;
                    let range = (1i32 << bd) - v - 1;
                    palette_bits = palette_bits.min(ceil_log2(range));
                    idx += 1;
                }
                self.b.palette_colors_y[..size].sort_unstable();
            }
        }
        if self.b.has_chroma && self.b.uv_mode == DC_PRED {
            let ctx = usize::from(self.b.palette_size_y > 0);
            let has = self.sym.symbol(&mut self.cdf.palette_uv_mode[ctx]) != 0;
            if has {
                let size = self.sym.symbol(&mut self.cdf.palette_uv_size[bsize_ctx]) + 2;
                self.b.palette_size_uv = size;
                let mut cache = [0u16; 16];
                let cache_n = self.get_palette_cache(1, &mut cache);
                let mut idx = 0;
                let mut i = 0;
                while i < cache_n && idx < size {
                    if self.sym.literal(1) != 0 {
                        self.b.palette_colors_u[idx] = cache[i];
                        idx += 1;
                    }
                    i += 1;
                }
                if idx < size {
                    self.b.palette_colors_u[idx] = self.sym.literal(bd) as u16;
                    idx += 1;
                }
                let mut palette_bits = 0;
                if idx < size {
                    let min_bits = bd - 3;
                    palette_bits = min_bits + self.sym.literal(2);
                }
                while idx < size {
                    let delta = self.sym.literal(palette_bits) as i32;
                    let v = (i32::from(self.b.palette_colors_u[idx - 1]) + delta).clamp(0, max_val);
                    self.b.palette_colors_u[idx] = v as u16;
                    let range = (1i32 << bd) - v;
                    palette_bits = palette_bits.min(ceil_log2(range));
                    idx += 1;
                }
                self.b.palette_colors_u[..size].sort_unstable();
                if self.sym.literal(1) != 0 {
                    let min_bits = bd - 4;
                    let max_v = 1i32 << bd;
                    let palette_bits = min_bits + self.sym.literal(2);
                    self.b.palette_colors_v[0] = self.sym.literal(bd) as u16;
                    for idx in 1..size {
                        let mut delta = self.sym.literal(palette_bits) as i32;
                        if delta != 0 && self.sym.literal(1) != 0 {
                            delta = -delta;
                        }
                        let mut val = i32::from(self.b.palette_colors_v[idx - 1]) + delta;
                        if val < 0 {
                            val += max_v;
                        }
                        if val >= max_v {
                            val -= max_v;
                        }
                        self.b.palette_colors_v[idx] = val.clamp(0, max_val) as u16;
                    }
                } else {
                    for idx in 0..size {
                        self.b.palette_colors_v[idx] = self.sym.literal(bd) as u16;
                    }
                }
            }
        }
    }

    fn palette_tokens(&mut self) {
        let mi_size = self.b.mi_size;
        let mut block_h = block_height(mi_size);
        let mut block_w = block_width(mi_size);
        let mut on_h = block_h.min((self.mi_rows - self.b.mi_row) * 4);
        let mut on_w = block_w.min((self.mi_cols - self.b.mi_col) * 4);
        if self.b.palette_size_y > 0 {
            let n = self.b.palette_size_y;
            let mut map = std::mem::take(&mut self.b.color_map_y);
            self.read_color_map(&mut map, n, block_w, block_h, on_w, on_h, false);
            self.b.color_map_y = map;
        }
        if self.b.palette_size_uv > 0 {
            block_h >>= self.suby;
            block_w >>= self.subx;
            on_h >>= self.suby;
            on_w >>= self.subx;
            if block_w < 4 {
                block_w += 2;
                on_w += 2;
            }
            if block_h < 4 {
                block_h += 2;
                on_h += 2;
            }
            let n = self.b.palette_size_uv;
            let mut map = std::mem::take(&mut self.b.color_map_uv);
            self.read_color_map(&mut map, n, block_w, block_h, on_w, on_h, true);
            self.b.color_map_uv = map;
        }
    }

    fn read_color_map(
        &mut self,
        map: &mut [u8],
        n: usize,
        block_w: usize,
        block_h: usize,
        on_w: usize,
        on_h: usize,
        uv: bool,
    ) {
        const S: usize = 64;
        map[0] = self.sym.ns(n as u32) as u8;
        for i in 1..(on_h + on_w - 1) {
            let hi = i.min(on_w - 1);
            let lo = (i + 1).saturating_sub(on_h);
            let mut j = hi as i64;
            while j >= lo as i64 {
                let ju = j as usize;
                let (r, c) = (i - ju, ju);
                // get_palette_color_context
                let mut scores = [0i32; 8];
                let mut order = [0u8, 1, 2, 3, 4, 5, 6, 7];
                if c > 0 {
                    scores[usize::from(map[r * S + c - 1])] += 2;
                }
                if r > 0 && c > 0 {
                    scores[usize::from(map[(r - 1) * S + c - 1])] += 1;
                }
                if r > 0 {
                    scores[usize::from(map[(r - 1) * S + c])] += 2;
                }
                for k in 0..3 {
                    let mut max_score = scores[k];
                    let mut max_idx = k;
                    for l in (k + 1)..n {
                        if scores[l] > max_score {
                            max_score = scores[l];
                            max_idx = l;
                        }
                    }
                    if max_idx != k {
                        let ms = scores[max_idx];
                        let mo = order[max_idx];
                        let mut l = max_idx;
                        while l > k {
                            scores[l] = scores[l - 1];
                            order[l] = order[l - 1];
                            l -= 1;
                        }
                        scores[k] = ms;
                        order[k] = mo;
                    }
                }
                let mut hash = 0i32;
                for k in 0..3 {
                    hash += scores[k] * i32::from(t::PALETTE_COLOR_HASH_MULTIPLIERS[k]);
                }
                let ctx = t::PALETTE_COLOR_CONTEXT[hash as usize] as usize;
                let idx = if !uv {
                    match n {
                        2 => self.sym.symbol(&mut self.cdf.palette_size_2_y_color[ctx]),
                        3 => self.sym.symbol(&mut self.cdf.palette_size_3_y_color[ctx]),
                        4 => self.sym.symbol(&mut self.cdf.palette_size_4_y_color[ctx]),
                        5 => self.sym.symbol(&mut self.cdf.palette_size_5_y_color[ctx]),
                        6 => self.sym.symbol(&mut self.cdf.palette_size_6_y_color[ctx]),
                        7 => self.sym.symbol(&mut self.cdf.palette_size_7_y_color[ctx]),
                        _ => self.sym.symbol(&mut self.cdf.palette_size_8_y_color[ctx]),
                    }
                } else {
                    match n {
                        2 => self.sym.symbol(&mut self.cdf.palette_size_2_uv_color[ctx]),
                        3 => self.sym.symbol(&mut self.cdf.palette_size_3_uv_color[ctx]),
                        4 => self.sym.symbol(&mut self.cdf.palette_size_4_uv_color[ctx]),
                        5 => self.sym.symbol(&mut self.cdf.palette_size_5_uv_color[ctx]),
                        6 => self.sym.symbol(&mut self.cdf.palette_size_6_uv_color[ctx]),
                        7 => self.sym.symbol(&mut self.cdf.palette_size_7_uv_color[ctx]),
                        _ => self.sym.symbol(&mut self.cdf.palette_size_8_uv_color[ctx]),
                    }
                };
                map[r * S + c] = order[idx];
                j -= 1;
            }
        }
        for i in 0..on_h {
            for j in on_w..block_w {
                map[i * S + j] = map[i * S + on_w - 1];
            }
        }
        for i in on_h..block_h {
            for j in 0..block_w {
                map[i * S + j] = map[(on_h - 1) * S + j];
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Loop restoration units

    fn read_lr(&mut self, r: usize, c: usize, bsize: usize) {
        if self.hdr.allow_intrabc {
            return;
        }
        let w = nb4w(bsize);
        let h = nb4h(bsize);
        for plane in 0..self.num_planes {
            if self.hdr.frame_restoration_type[plane] == crate::hdr::RESTORE_NONE {
                continue;
            }
            let sub_x = if plane == 0 { 0 } else { self.subx };
            let sub_y = if plane == 0 { 0 } else { self.suby };
            let unit_size = self.hdr.loop_restoration_size[plane] as usize;
            let unit_rows = self.lr_rows[plane];
            let unit_cols = self.lr_cols[plane];
            let unit_row_start = (r * (4 >> sub_y) + unit_size - 1) / unit_size;
            let unit_row_end = unit_rows.min(((r + h) * (4 >> sub_y) + unit_size - 1) / unit_size);
            let (numerator, denominator) = if self.hdr.use_superres {
                (
                    (4 >> sub_x) * self.hdr.superres_denom as usize,
                    unit_size * 8,
                )
            } else {
                (4 >> sub_x, unit_size)
            };
            let unit_col_start = (c * numerator + denominator - 1) / denominator;
            let unit_col_end = unit_cols.min(((c + w) * numerator + denominator - 1) / denominator);
            for unit_row in unit_row_start..unit_row_end {
                for unit_col in unit_col_start..unit_col_end {
                    self.read_lr_unit(plane, unit_row, unit_col);
                }
            }
        }
    }

    fn read_lr_unit(&mut self, plane: usize, unit_row: usize, unit_col: usize) {
        use crate::hdr::RESTORE_NONE;
        let ft = self.hdr.frame_restoration_type[plane];
        let restoration_type = if ft == RESTORE_WIENER {
            if self.sym.symbol(&mut self.cdf.use_wiener) != 0 {
                RESTORE_WIENER
            } else {
                RESTORE_NONE
            }
        } else if ft == RESTORE_SGRPROJ {
            if self.sym.symbol(&mut self.cdf.use_sgrproj) != 0 {
                RESTORE_SGRPROJ
            } else {
                RESTORE_NONE
            }
        } else {
            self.sym.symbol(&mut self.cdf.restoration_type) as u8
        };
        let mut unit = LrUnit {
            ty: restoration_type,
            ..Default::default()
        };
        if restoration_type == RESTORE_WIENER {
            for pass in 0..2 {
                let first = if plane > 0 {
                    unit.wiener[pass][0] = 0;
                    1
                } else {
                    0
                };
                for j in first..3 {
                    let min = i32::from(t::WIENER_TAPS_MIN[j]);
                    let max = i32::from(t::WIENER_TAPS_MAX[j]);
                    let k = u32::from(t::WIENER_TAPS_K[j]);
                    let v = self.decode_signed_subexp_with_ref_bool(
                        min,
                        max + 1,
                        k,
                        self.ref_lr_wiener[plane][pass][j],
                    );
                    unit.wiener[pass][j] = v as i8;
                    self.ref_lr_wiener[plane][pass][j] = v;
                }
            }
        } else if restoration_type == RESTORE_SGRPROJ {
            let set = self.sym.literal(4) as usize;
            unit.sgr_set = set as u8;
            for i in 0..2 {
                let radius = t::SGR_PARAMS[set][i * 2];
                let min = i32::from(t::SGRPROJ_XQD_MIN[i]);
                let max = i32::from(t::SGRPROJ_XQD_MAX[i]);
                let v;
                if radius != 0 {
                    v = self.decode_signed_subexp_with_ref_bool(
                        min,
                        max + 1,
                        4,
                        self.ref_sgr_xqd[plane][i],
                    );
                } else if i == 1 {
                    v = ((1 << 7) - self.ref_sgr_xqd[plane][0]).clamp(min, max);
                } else {
                    v = 0;
                }
                unit.sgr_xqd[i] = v as i8;
                self.ref_sgr_xqd[plane][i] = v;
            }
        }
        let idx = unit_row * self.lr_cols[plane] + unit_col;
        self.lr_units[plane][idx] = unit;
    }

    fn decode_signed_subexp_with_ref_bool(&mut self, low: i32, high: i32, k: u32, r: i32) -> i32 {
        let x = self.decode_unsigned_subexp_with_ref_bool(high - low, k, r - low);
        x + low
    }

    fn decode_unsigned_subexp_with_ref_bool(&mut self, mx: i32, k: u32, r: i32) -> i32 {
        let v = self.decode_subexp_bool(mx, k);
        if (r << 1) <= mx {
            inverse_recenter(r, v)
        } else {
            mx - 1 - inverse_recenter(mx - 1 - r, v)
        }
    }

    fn decode_subexp_bool(&mut self, num_syms: i32, k: u32) -> i32 {
        let mut i = 0u32;
        let mut mk = 0i32;
        loop {
            let b2 = if i != 0 { k + i - 1 } else { k };
            let a = 1i32 << b2;
            if num_syms <= mk + 3 * a {
                return self.sym.ns((num_syms - mk) as u32) as i32 + mk;
            }
            if self.sym.literal(1) != 0 {
                i += 1;
                mk += a;
            } else {
                return self.sym.literal(b2) as i32 + mk;
            }
        }
    }
}

pub fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 != 0 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

pub fn find_tx_size(w: usize, h: usize) -> usize {
    for tx in 0..19 {
        if usize::from(t::TX_WIDTH[tx]) == w && usize::from(t::TX_HEIGHT[tx]) == h {
            return tx;
        }
    }
    19
}

pub fn ceil_log2(x: i32) -> u32 {
    if x < 2 {
        return 0;
    }
    let mut i = 1;
    let mut p = 2;
    while p < x {
        i += 1;
        p <<= 1;
    }
    i
}

fn neg_deinterleave(diff: i32, r: i32, max: i32) -> i32 {
    if r == 0 {
        return diff;
    }
    if r >= max - 1 {
        return max - diff - 1;
    }
    if 2 * r < max {
        if diff <= 2 * r {
            return if diff & 1 != 0 {
                r + ((diff + 1) >> 1)
            } else {
                r - (diff >> 1)
            };
        }
        diff
    } else {
        if diff <= 2 * (max - r - 1) {
            return if diff & 1 != 0 {
                r + ((diff + 1) >> 1)
            } else {
                r - (diff >> 1)
            };
        }
        max - (diff + 1)
    }
}
