//! Inter frame mode info syntax (sections 5.11.18 - 5.11.32) and its CDF contexts (section 8.3.2).
use crate::consts::*;
use crate::dec::*;
use crate::hdr::*;
use crate::tables as t;

fn nb4w(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_WIDE[b])
}
fn nb4h(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_HIGH[b])
}

fn ref_count_ctx(c0: usize, c1: usize) -> usize {
    if c0 < c1 {
        0
    } else if c0 == c1 {
        1
    } else {
        2
    }
}

fn check_backward(r: i8) -> bool {
    (BWDREF_FRAME..=ALTREF_FRAME).contains(&r)
}

fn is_samedir_ref_pair(r0: i8, r1: i8) -> bool {
    (r0 >= BWDREF_FRAME) == (r1 >= BWDREF_FRAME)
}

fn get_mode(y_mode: u8, ref_list: usize) -> u8 {
    if ref_list == 0 {
        if y_mode < NEAREST_NEARESTMV {
            y_mode
        } else if matches!(y_mode, NEW_NEWMV | NEW_NEARESTMV | NEW_NEARMV) {
            NEWMV
        } else if matches!(y_mode, NEAREST_NEARESTMV | NEAREST_NEWMV) {
            NEARESTMV
        } else if matches!(y_mode, NEAR_NEARMV | NEAR_NEWMV) {
            NEARMV
        } else {
            GLOBALMV
        }
    } else if matches!(y_mode, NEW_NEWMV | NEAREST_NEWMV | NEAR_NEWMV) {
        NEWMV
    } else if matches!(y_mode, NEAREST_NEARESTMV | NEW_NEARESTMV) {
        NEARESTMV
    } else if matches!(y_mode, NEAR_NEARMV | NEW_NEARMV) {
        NEARMV
    } else {
        GLOBALMV
    }
}

impl Dec {
    pub fn inter_frame_mode_info(&mut self) {
        self.b.use_intrabc = false;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let left = if self.b.avail_l {
            self.mi_at(r, c - 1).ref_frame
        } else {
            [INTRA_FRAME, NONE]
        };
        let above = if self.b.avail_u {
            self.mi_at(r - 1, c).ref_frame
        } else {
            [INTRA_FRAME, NONE]
        };
        self.b.left_ref_frame = left;
        self.b.above_ref_frame = above;
        self.b.left_intra = left[0] <= INTRA_FRAME;
        self.b.above_intra = above[0] <= INTRA_FRAME;
        self.b.left_single = left[1] <= INTRA_FRAME;
        self.b.above_single = above[1] <= INTRA_FRAME;
        self.b.skip = false;
        self.inter_segment_id(true);
        self.read_skip_mode();
        if self.b.skip_mode {
            self.b.skip = true;
        } else {
            self.read_skip();
        }
        if !self.hdr.seg_id_pre_skip {
            self.inter_segment_id(false);
        }
        self.b.lossless = self.hdr.lossless_array[self.b.segment_id];
        self.read_cdef();
        self.read_delta_qindex();
        self.read_delta_lf();
        self.read_deltas = false;
        self.read_is_inter();
        if self.b.is_inter {
            self.inter_block_mode_info();
        } else {
            self.intra_block_mode_info();
        }
    }

    fn get_segment_id(&self) -> usize {
        let bw4 = nb4w(self.b.mi_size);
        let bh4 = nb4h(self.b.mi_size);
        let x_mis = (self.mi_cols - self.b.mi_col).min(bw4);
        let y_mis = (self.mi_rows - self.b.mi_row).min(bh4);
        let mut seg = 7u8;
        for y in 0..y_mis {
            for x in 0..x_mis {
                seg = seg.min(
                    self.prev_segment_ids[(self.b.mi_row + y) * self.mi_stride + self.b.mi_col + x],
                );
            }
        }
        usize::from(seg)
    }

    fn set_seg_pred_ctx(&mut self, v: u8) {
        for i in 0..nb4w(self.b.mi_size) {
            self.above_seg_pred[self.b.mi_col + i] = v;
        }
        for i in 0..nb4h(self.b.mi_size) {
            self.left_seg_pred[self.b.mi_row + i] = v;
        }
    }

    fn inter_segment_id(&mut self, pre_skip: bool) {
        if self.hdr.segmentation_enabled {
            let predicted = self.get_segment_id();
            if self.hdr.segmentation_update_map {
                if pre_skip && !self.hdr.seg_id_pre_skip {
                    self.b.segment_id = 0;
                    return;
                }
                if !pre_skip {
                    if self.b.skip {
                        self.set_seg_pred_ctx(0);
                        self.read_segment_id();
                        return;
                    }
                }
                if self.hdr.segmentation_temporal_update {
                    let ctx = usize::from(self.left_seg_pred[self.b.mi_row])
                        + usize::from(self.above_seg_pred[self.b.mi_col]);
                    let pred_flag = self.sym.symbol(&mut self.cdf.segment_id_predicted[ctx]) != 0;
                    if pred_flag {
                        self.b.segment_id = predicted;
                    } else {
                        self.read_segment_id();
                    }
                    self.set_seg_pred_ctx(u8::from(pred_flag));
                } else {
                    self.read_segment_id();
                }
            } else {
                self.b.segment_id = predicted;
            }
        } else {
            self.b.segment_id = 0;
        }
    }

    fn read_skip_mode(&mut self) {
        if self.seg_feature_active(SEG_LVL_SKIP)
            || self.seg_feature_active(SEG_LVL_REF_FRAME)
            || self.seg_feature_active(SEG_LVL_GLOBALMV)
            || !self.hdr.skip_mode_present
            || block_width(self.b.mi_size) < 8
            || block_height(self.b.mi_size) < 8
        {
            self.b.skip_mode = false;
        } else {
            let mut ctx = 0;
            if self.b.avail_u {
                ctx += usize::from(self.mi_at(self.b.mi_row - 1, self.b.mi_col).skip_mode);
            }
            if self.b.avail_l {
                ctx += usize::from(self.mi_at(self.b.mi_row, self.b.mi_col - 1).skip_mode);
            }
            self.b.skip_mode = self.sym.symbol(&mut self.cdf.skip_mode[ctx]) != 0;
        }
    }

    fn read_is_inter(&mut self) {
        if self.b.skip_mode {
            self.b.is_inter = true;
        } else if self.seg_feature_active(SEG_LVL_REF_FRAME) {
            self.b.is_inter = self.hdr.feature_data[self.b.segment_id][SEG_LVL_REF_FRAME]
                != i32::from(INTRA_FRAME);
        } else if self.seg_feature_active(SEG_LVL_GLOBALMV) {
            self.b.is_inter = true;
        } else {
            let b = &self.b;
            let ctx = if b.avail_u && b.avail_l {
                if b.left_intra && b.above_intra {
                    3
                } else {
                    usize::from(b.left_intra || b.above_intra)
                }
            } else if b.avail_u || b.avail_l {
                2 * usize::from(if b.avail_u {
                    b.above_intra
                } else {
                    b.left_intra
                })
            } else {
                0
            };
            self.b.is_inter = self.sym.symbol(&mut self.cdf.is_inter[ctx]) != 0;
        }
    }

    fn intra_block_mode_info(&mut self) {
        self.b.ref_frame = [INTRA_FRAME, NONE];
        let ctx = usize::from(t::SIZE_GROUP[self.b.mi_size]);
        self.b.y_mode = self.sym.symbol(&mut self.cdf.y_mode[ctx]) as u8;
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

    fn has_nearmv(&self) -> bool {
        matches!(
            self.b.y_mode,
            NEARMV | NEAR_NEARMV | NEAR_NEWMV | NEW_NEARMV
        )
    }

    fn inter_block_mode_info(&mut self) {
        self.b.palette_size_y = 0;
        self.b.palette_size_uv = 0;
        self.read_ref_frames();
        let is_compound = self.b.ref_frame[1] > INTRA_FRAME;
        self.find_mv_stack(is_compound);
        if self.b.skip_mode {
            self.b.y_mode = NEAREST_NEARESTMV;
        } else if self.seg_feature_active(SEG_LVL_SKIP) || self.seg_feature_active(SEG_LVL_GLOBALMV)
        {
            self.b.y_mode = GLOBALMV;
        } else if is_compound {
            let ctx = usize::from(
                t::COMPOUND_MODE_CTX_MAP[self.b.ref_mv_context >> 1][self.b.new_mv_context.min(4)],
            );
            let m = self.sym.symbol(&mut self.cdf.compound_mode[ctx]);
            self.b.y_mode = NEAREST_NEARESTMV + m as u8;
        } else {
            let new_mv = self.sym.symbol(&mut self.cdf.new_mv[self.b.new_mv_context]);
            if new_mv == 0 {
                self.b.y_mode = NEWMV;
            } else {
                let zero_mv = self
                    .sym
                    .symbol(&mut self.cdf.zero_mv[self.b.zero_mv_context]);
                if zero_mv == 0 {
                    self.b.y_mode = GLOBALMV;
                } else {
                    let ref_mv = self.sym.symbol(&mut self.cdf.ref_mv[self.b.ref_mv_context]);
                    self.b.y_mode = if ref_mv == 0 { NEARESTMV } else { NEARMV };
                }
            }
        }
        self.b.ref_mv_idx = 0;
        if self.b.y_mode == NEWMV || self.b.y_mode == NEW_NEWMV {
            for idx in 0..2 {
                if self.b.num_mv_found > idx + 1 {
                    let drl = self
                        .sym
                        .symbol(&mut self.cdf.drl_mode[self.b.drl_ctx_stack[idx]]);
                    if drl == 0 {
                        self.b.ref_mv_idx = idx;
                        break;
                    }
                    self.b.ref_mv_idx = idx + 1;
                }
            }
        } else if self.has_nearmv() {
            self.b.ref_mv_idx = 1;
            for idx in 1..3 {
                if self.b.num_mv_found > idx + 1 {
                    let drl = self
                        .sym
                        .symbol(&mut self.cdf.drl_mode[self.b.drl_ctx_stack[idx]]);
                    if drl == 0 {
                        self.b.ref_mv_idx = idx;
                        break;
                    }
                    self.b.ref_mv_idx = idx + 1;
                }
            }
        }
        self.assign_mv(is_compound);
        self.read_interintra_mode(is_compound);
        self.read_motion_mode(is_compound);
        self.read_compound_type(is_compound);
        if self.hdr.interpolation_filter == SWITCHABLE {
            let dirs = if self.seq.enable_dual_filter { 2 } else { 1 };
            for dir in 0..dirs {
                if self.needs_interp_filter() {
                    let ctx = self.interp_filter_ctx(dir);
                    self.b.interp_filter[dir] =
                        self.sym.symbol(&mut self.cdf.interp_filter[ctx]) as u8;
                } else {
                    self.b.interp_filter[dir] = EIGHTTAP;
                }
            }
            if !self.seq.enable_dual_filter {
                self.b.interp_filter[1] = self.b.interp_filter[0];
            }
        } else {
            self.b.interp_filter = [self.hdr.interpolation_filter; 2];
        }
    }

    fn needs_interp_filter(&self) -> bool {
        let large = block_width(self.b.mi_size).min(block_height(self.b.mi_size)) >= 8;
        if self.b.skip_mode || self.b.motion_mode == LOCALWARP {
            false
        } else if large && self.b.y_mode == GLOBALMV {
            self.hdr.gm_type[self.b.ref_frame[0] as usize] == TRANSLATION
        } else if large && self.b.y_mode == GLOBAL_GLOBALMV {
            self.hdr.gm_type[self.b.ref_frame[0] as usize] == TRANSLATION
                || self.hdr.gm_type[self.b.ref_frame[1] as usize] == TRANSLATION
        } else {
            true
        }
    }

    fn interp_filter_ctx(&self, dir: usize) -> usize {
        let mut ctx = ((dir & 1) * 2 + usize::from(self.b.ref_frame[1] > INTRA_FRAME)) * 4;
        let mut left_type = 3usize;
        let mut above_type = 3usize;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        if self.b.avail_l {
            let m = self.mi_at(r, c - 1);
            if m.ref_frame[0] == self.b.ref_frame[0] || m.ref_frame[1] == self.b.ref_frame[0] {
                left_type = usize::from(m.interp_filter[dir]);
            }
        }
        if self.b.avail_u {
            let m = self.mi_at(r - 1, c);
            if m.ref_frame[0] == self.b.ref_frame[0] || m.ref_frame[1] == self.b.ref_frame[0] {
                above_type = usize::from(m.interp_filter[dir]);
            }
        }
        if left_type == above_type {
            ctx += left_type;
        } else if left_type == 3 {
            ctx += above_type;
        } else if above_type == 3 {
            ctx += left_type;
        } else {
            ctx += 3;
        }
        ctx
    }

    fn count_refs(&self, frame_type: i8) -> usize {
        let b = &self.b;
        let mut c = 0;
        if b.avail_u {
            c += usize::from(b.above_ref_frame[0] == frame_type)
                + usize::from(b.above_ref_frame[1] == frame_type);
        }
        if b.avail_l {
            c += usize::from(b.left_ref_frame[0] == frame_type)
                + usize::from(b.left_ref_frame[1] == frame_type);
        }
        c
    }

    fn ctx_comp_ref(&self) -> usize {
        ref_count_ctx(
            self.count_refs(LAST_FRAME) + self.count_refs(LAST2_FRAME),
            self.count_refs(LAST3_FRAME) + self.count_refs(GOLDEN_FRAME),
        )
    }
    fn ctx_comp_ref_p1(&self) -> usize {
        ref_count_ctx(self.count_refs(LAST_FRAME), self.count_refs(LAST2_FRAME))
    }
    fn ctx_comp_ref_p2(&self) -> usize {
        ref_count_ctx(self.count_refs(LAST3_FRAME), self.count_refs(GOLDEN_FRAME))
    }
    fn ctx_comp_bwdref(&self) -> usize {
        ref_count_ctx(
            self.count_refs(BWDREF_FRAME) + self.count_refs(ALTREF2_FRAME),
            self.count_refs(ALTREF_FRAME),
        )
    }
    fn ctx_comp_bwdref_p1(&self) -> usize {
        ref_count_ctx(
            self.count_refs(BWDREF_FRAME),
            self.count_refs(ALTREF2_FRAME),
        )
    }
    fn ctx_single_ref_p1(&self) -> usize {
        let fwd = self.count_refs(LAST_FRAME)
            + self.count_refs(LAST2_FRAME)
            + self.count_refs(LAST3_FRAME)
            + self.count_refs(GOLDEN_FRAME);
        let bwd = self.count_refs(BWDREF_FRAME)
            + self.count_refs(ALTREF2_FRAME)
            + self.count_refs(ALTREF_FRAME);
        ref_count_ctx(fwd, bwd)
    }

    fn ctx_comp_mode(&self) -> usize {
        let b = &self.b;
        let (a0, l0) = (b.above_ref_frame[0], b.left_ref_frame[0]);
        if b.avail_u && b.avail_l {
            if b.above_single && b.left_single {
                usize::from(check_backward(a0) ^ check_backward(l0))
            } else if b.above_single {
                2 + usize::from(check_backward(a0) || b.above_intra)
            } else if b.left_single {
                2 + usize::from(check_backward(l0) || b.left_intra)
            } else {
                4
            }
        } else if b.avail_u {
            if b.above_single {
                usize::from(check_backward(a0))
            } else {
                3
            }
        } else if b.avail_l {
            if b.left_single {
                usize::from(check_backward(l0))
            } else {
                3
            }
        } else {
            1
        }
    }

    fn ctx_comp_ref_type(&self) -> usize {
        let b = &self.b;
        let (above0, above1) = (b.above_ref_frame[0], b.above_ref_frame[1]);
        let (left0, left1) = (b.left_ref_frame[0], b.left_ref_frame[1]);
        let above_comp_inter = b.avail_u && !b.above_intra && !b.above_single;
        let left_comp_inter = b.avail_l && !b.left_intra && !b.left_single;
        let above_uni_comp = above_comp_inter && is_samedir_ref_pair(above0, above1);
        let left_uni_comp = left_comp_inter && is_samedir_ref_pair(left0, left1);
        if b.avail_u && !b.above_intra && b.avail_l && !b.left_intra {
            let samedir = usize::from(is_samedir_ref_pair(above0, left0));
            if !above_comp_inter && !left_comp_inter {
                1 + 2 * samedir
            } else if !above_comp_inter {
                if !left_uni_comp {
                    1
                } else {
                    3 + samedir
                }
            } else if !left_comp_inter {
                if !above_uni_comp {
                    1
                } else {
                    3 + samedir
                }
            } else if !above_uni_comp && !left_uni_comp {
                0
            } else if !above_uni_comp || !left_uni_comp {
                2
            } else {
                3 + usize::from((above0 == BWDREF_FRAME) == (left0 == BWDREF_FRAME))
            }
        } else if b.avail_u && b.avail_l {
            if above_comp_inter {
                1 + 2 * usize::from(above_uni_comp)
            } else if left_comp_inter {
                1 + 2 * usize::from(left_uni_comp)
            } else {
                2
            }
        } else if above_comp_inter {
            4 * usize::from(above_uni_comp)
        } else if left_comp_inter {
            4 * usize::from(left_uni_comp)
        } else {
            2
        }
    }

    fn read_ref_frames(&mut self) {
        if self.b.skip_mode {
            self.b.ref_frame = [
                self.hdr.skip_mode_frame[0] as i8,
                self.hdr.skip_mode_frame[1] as i8,
            ];
        } else if self.seg_feature_active(SEG_LVL_REF_FRAME) {
            self.b.ref_frame = [
                self.hdr.feature_data[self.b.segment_id][SEG_LVL_REF_FRAME] as i8,
                NONE,
            ];
        } else if self.seg_feature_active(SEG_LVL_SKIP) || self.seg_feature_active(SEG_LVL_GLOBALMV)
        {
            self.b.ref_frame = [LAST_FRAME, NONE];
        } else {
            let bw4 = nb4w(self.b.mi_size);
            let bh4 = nb4h(self.b.mi_size);
            let comp_mode = if self.hdr.reference_select && bw4.min(bh4) >= 2 {
                let ctx = self.ctx_comp_mode();
                self.sym.symbol(&mut self.cdf.comp_mode[ctx])
            } else {
                0
            };
            if comp_mode == 1 {
                let ctx = self.ctx_comp_ref_type();
                let comp_ref_type = self.sym.symbol(&mut self.cdf.comp_ref_type[ctx]);
                if comp_ref_type == 0 {
                    let ctx = self.ctx_single_ref_p1();
                    let uni = self.sym.symbol(&mut self.cdf.uni_comp_ref[ctx][0]);
                    if uni != 0 {
                        self.b.ref_frame = [BWDREF_FRAME, ALTREF_FRAME];
                    } else {
                        let ctx = ref_count_ctx(
                            self.count_refs(LAST2_FRAME),
                            self.count_refs(LAST3_FRAME) + self.count_refs(GOLDEN_FRAME),
                        );
                        let p1 = self.sym.symbol(&mut self.cdf.uni_comp_ref[ctx][1]);
                        if p1 != 0 {
                            let ctx = self.ctx_comp_ref_p2();
                            let p2 = self.sym.symbol(&mut self.cdf.uni_comp_ref[ctx][2]);
                            self.b.ref_frame = if p2 != 0 {
                                [LAST_FRAME, GOLDEN_FRAME]
                            } else {
                                [LAST_FRAME, LAST3_FRAME]
                            };
                        } else {
                            self.b.ref_frame = [LAST_FRAME, LAST2_FRAME];
                        }
                    }
                } else {
                    let ctx = self.ctx_comp_ref();
                    let comp_ref = self.sym.symbol(&mut self.cdf.comp_ref[ctx][0]);
                    let r0;
                    if comp_ref == 0 {
                        let ctx = self.ctx_comp_ref_p1();
                        let p1 = self.sym.symbol(&mut self.cdf.comp_ref[ctx][1]);
                        r0 = if p1 != 0 { LAST2_FRAME } else { LAST_FRAME };
                    } else {
                        let ctx = self.ctx_comp_ref_p2();
                        let p2 = self.sym.symbol(&mut self.cdf.comp_ref[ctx][2]);
                        r0 = if p2 != 0 { GOLDEN_FRAME } else { LAST3_FRAME };
                    }
                    let ctx = self.ctx_comp_bwdref();
                    let bwd = self.sym.symbol(&mut self.cdf.comp_bwd_ref[ctx][0]);
                    let r1;
                    if bwd == 0 {
                        let ctx = self.ctx_comp_bwdref_p1();
                        let p1 = self.sym.symbol(&mut self.cdf.comp_bwd_ref[ctx][1]);
                        r1 = if p1 != 0 { ALTREF2_FRAME } else { BWDREF_FRAME };
                    } else {
                        r1 = ALTREF_FRAME;
                    }
                    self.b.ref_frame = [r0, r1];
                }
            } else {
                let ctx = self.ctx_single_ref_p1();
                let p1 = self.sym.symbol(&mut self.cdf.single_ref[ctx][0]);
                let r0;
                if p1 != 0 {
                    let ctx = self.ctx_comp_bwdref();
                    let p2 = self.sym.symbol(&mut self.cdf.single_ref[ctx][1]);
                    if p2 == 0 {
                        let ctx = self.ctx_comp_bwdref_p1();
                        let p6 = self.sym.symbol(&mut self.cdf.single_ref[ctx][5]);
                        r0 = if p6 != 0 { ALTREF2_FRAME } else { BWDREF_FRAME };
                    } else {
                        r0 = ALTREF_FRAME;
                    }
                } else {
                    let ctx = self.ctx_comp_ref();
                    let p3 = self.sym.symbol(&mut self.cdf.single_ref[ctx][2]);
                    if p3 != 0 {
                        let ctx = self.ctx_comp_ref_p2();
                        let p5 = self.sym.symbol(&mut self.cdf.single_ref[ctx][4]);
                        r0 = if p5 != 0 { GOLDEN_FRAME } else { LAST3_FRAME };
                    } else {
                        let ctx = self.ctx_comp_ref_p1();
                        let p4 = self.sym.symbol(&mut self.cdf.single_ref[ctx][3]);
                        r0 = if p4 != 0 { LAST2_FRAME } else { LAST_FRAME };
                    }
                }
                self.b.ref_frame = [r0, NONE];
            }
        }
    }

    pub fn assign_mv(&mut self, is_compound: bool) {
        for i in 0..1 + usize::from(is_compound) {
            let comp_mode = if self.b.use_intrabc {
                NEWMV
            } else {
                get_mode(self.b.y_mode, i)
            };
            if self.b.use_intrabc {
                self.b.pred_mv[0] = self.b.ref_stack_mv[0][0];
                if self.b.pred_mv[0] == [0, 0] {
                    self.b.pred_mv[0] = self.b.ref_stack_mv[1][0];
                }
                if self.b.pred_mv[0] == [0, 0] {
                    let sb_size = if self.seq.use_128x128_superblock {
                        BLOCK_128X128
                    } else {
                        BLOCK_64X64
                    };
                    let sb_size4 = nb4h(sb_size) as i32;
                    if (self.b.mi_row as i32) - sb_size4 < self.mi_row_start as i32 {
                        self.b.pred_mv[0][0] = 0;
                        self.b.pred_mv[0][1] = -(sb_size4 * 4 + 256) * 8;
                    } else {
                        self.b.pred_mv[0][0] = -(sb_size4 * 4 * 8);
                        self.b.pred_mv[0][1] = 0;
                    }
                }
            } else if comp_mode == GLOBALMV {
                self.b.pred_mv[i] = self.b.global_mvs[i];
            } else {
                let mut pos = if comp_mode == NEARESTMV {
                    0
                } else {
                    self.b.ref_mv_idx
                };
                if comp_mode == NEWMV && self.b.num_mv_found <= 1 {
                    pos = 0;
                }
                self.b.pred_mv[i] = self.b.ref_stack_mv[pos][i];
            }
            if comp_mode == NEWMV {
                self.read_mv(i);
            } else {
                self.b.mv[i] = self.b.pred_mv[i];
            }
        }
    }

    fn read_mv(&mut self, r: usize) {
        let mut diff = [0i32; 2];
        let ctx = usize::from(self.b.use_intrabc);
        let joint = self.sym.symbol(&mut self.cdf.mv_joint[ctx]);
        if joint == 2 || joint == 3 {
            diff[0] = self.read_mv_component(ctx, 0);
        }
        if joint == 1 || joint == 3 {
            diff[1] = self.read_mv_component(ctx, 1);
        }
        self.b.mv[r] = [
            self.b.pred_mv[r][0] + diff[0],
            self.b.pred_mv[r][1] + diff[1],
        ];
    }

    fn read_mv_component(&mut self, ctx: usize, comp: usize) -> i32 {
        let sign = self.sym.symbol(&mut self.cdf.mv_sign[ctx][comp]);
        let class = self.sym.symbol(&mut self.cdf.mv_class[ctx][comp]);
        let mag;
        if class == 0 {
            let c0bit = self.sym.symbol(&mut self.cdf.mv_class0_bit[ctx][comp]);
            let fr = if self.hdr.force_integer_mv {
                3
            } else {
                self.sym
                    .symbol(&mut self.cdf.mv_class0_fr[ctx][comp][c0bit])
            };
            let hp = if self.hdr.allow_high_precision_mv {
                self.sym.symbol(&mut self.cdf.mv_class0_hp[ctx][comp])
            } else {
                1
            };
            mag = (((c0bit << 3) | (fr << 1) | hp) + 1) as i32;
        } else {
            let mut d = 0usize;
            for i in 0..class {
                let bit = self.sym.symbol(&mut self.cdf.mv_bit[ctx][comp][i]);
                d |= bit << i;
            }
            let mut m = 2usize << (class + 2);
            let fr = if self.hdr.force_integer_mv {
                3
            } else {
                self.sym.symbol(&mut self.cdf.mv_fr[ctx][comp])
            };
            let hp = if self.hdr.allow_high_precision_mv {
                self.sym.symbol(&mut self.cdf.mv_hp[ctx][comp])
            } else {
                1
            };
            m += ((d << 3) | (fr << 1) | hp) + 1;
            mag = m as i32;
        }
        if sign != 0 {
            -mag
        } else {
            mag
        }
    }

    pub fn is_scaled(&self, ref_frame: i8) -> bool {
        let ref_idx = self.hdr.ref_frame_idx[(ref_frame - LAST_FRAME) as usize];
        let rh = &self.refh[ref_idx];
        let fw = self.hdr.frame_width;
        let fh = self.hdr.frame_height;
        let x_scale = ((rh.upscaled_width << 14) + fw / 2) / fw;
        let y_scale = ((rh.frame_height << 14) + fh / 2) / fh;
        x_scale != 1 << 14 || y_scale != 1 << 14
    }

    fn read_motion_mode(&mut self, is_compound: bool) {
        self.b.motion_mode = SIMPLE;
        if self.b.skip_mode || !self.hdr.is_motion_mode_switchable {
            return;
        }
        if block_width(self.b.mi_size).min(block_height(self.b.mi_size)) < 8 {
            return;
        }
        if !self.hdr.force_integer_mv
            && (self.b.y_mode == GLOBALMV || self.b.y_mode == GLOBAL_GLOBALMV)
            && self.hdr.gm_type[self.b.ref_frame[0] as usize] > TRANSLATION
        {
            return;
        }
        if is_compound || self.b.ref_frame[1] == INTRA_FRAME || !self.has_overlappable_candidates()
        {
            return;
        }
        self.find_warp_samples();
        if self.hdr.force_integer_mv
            || self.b.num_samples == 0
            || !self.hdr.allow_warped_motion
            || self.is_scaled(self.b.ref_frame[0])
        {
            let obmc = self.sym.symbol(&mut self.cdf.use_obmc[self.b.mi_size]);
            self.b.motion_mode = if obmc != 0 { OBMC } else { SIMPLE };
        } else {
            self.b.motion_mode = self.sym.symbol(&mut self.cdf.motion_mode[self.b.mi_size]) as u8;
        }
    }

    fn read_interintra_mode(&mut self, is_compound: bool) {
        if !self.b.skip_mode
            && self.seq.enable_interintra_compound
            && !is_compound
            && self.b.mi_size >= BLOCK_8X8
            && self.b.mi_size <= BLOCK_32X32
        {
            let ctx = usize::from(t::SIZE_GROUP[self.b.mi_size]) - 1;
            self.b.interintra = self.sym.symbol(&mut self.cdf.inter_intra[ctx]) != 0;
            if self.b.interintra {
                self.b.interintra_mode = self.sym.symbol(&mut self.cdf.inter_intra_mode[ctx]) as u8;
                self.b.ref_frame[1] = INTRA_FRAME;
                self.b.angle_delta_y = 0;
                self.b.angle_delta_uv = 0;
                self.b.use_filter_intra = false;
                self.b.wedge_interintra = self
                    .sym
                    .symbol(&mut self.cdf.wedge_inter_intra[self.b.mi_size])
                    != 0;
                if self.b.wedge_interintra {
                    self.b.wedge_index = self.sym.symbol(&mut self.cdf.wedge_index[self.b.mi_size]);
                    self.b.wedge_sign = 0;
                }
            }
        } else {
            self.b.interintra = false;
        }
    }

    fn read_compound_type(&mut self, is_compound: bool) {
        self.b.comp_group_idx = 0;
        self.b.compound_idx = 1;
        if self.b.skip_mode {
            self.b.compound_type = COMPOUND_AVERAGE;
            return;
        }
        if is_compound {
            let n = t::WEDGE_BITS[self.b.mi_size];
            if self.seq.enable_masked_compound {
                let b = &self.b;
                let mut ctx = 0usize;
                if b.avail_u {
                    if !b.above_single {
                        ctx += usize::from(self.mi_at(b.mi_row - 1, b.mi_col).comp_group_idx);
                    } else if b.above_ref_frame[0] == ALTREF_FRAME {
                        ctx += 3;
                    }
                }
                if b.avail_l {
                    if !b.left_single {
                        ctx += usize::from(self.mi_at(b.mi_row, b.mi_col - 1).comp_group_idx);
                    } else if b.left_ref_frame[0] == ALTREF_FRAME {
                        ctx += 3;
                    }
                }
                let ctx = ctx.min(5);
                self.b.comp_group_idx = self.sym.symbol(&mut self.cdf.comp_group_idx[ctx]) as u8;
            }
            if self.b.comp_group_idx == 0 {
                if self.seq.enable_jnt_comp {
                    let b = &self.b;
                    let fwd = get_relative_dist(
                        &self.seq,
                        self.hdr.order_hints[b.ref_frame[0] as usize],
                        self.hdr.order_hint,
                    )
                    .abs();
                    let bck = get_relative_dist(
                        &self.seq,
                        self.hdr.order_hints[b.ref_frame[1] as usize],
                        self.hdr.order_hint,
                    )
                    .abs();
                    let mut ctx = if fwd == bck { 3 } else { 0 };
                    if b.avail_u {
                        if !b.above_single {
                            ctx += usize::from(self.mi_at(b.mi_row - 1, b.mi_col).compound_idx);
                        } else if b.above_ref_frame[0] == ALTREF_FRAME {
                            ctx += 1;
                        }
                    }
                    if b.avail_l {
                        if !b.left_single {
                            ctx += usize::from(self.mi_at(b.mi_row, b.mi_col - 1).compound_idx);
                        } else if b.left_ref_frame[0] == ALTREF_FRAME {
                            ctx += 1;
                        }
                    }
                    self.b.compound_idx = self.sym.symbol(&mut self.cdf.compound_idx[ctx]) as u8;
                    self.b.compound_type = if self.b.compound_idx != 0 {
                        COMPOUND_AVERAGE
                    } else {
                        COMPOUND_DISTANCE
                    };
                } else {
                    self.b.compound_type = COMPOUND_AVERAGE;
                }
            } else if n == 0 {
                self.b.compound_type = COMPOUND_DIFFWTD;
            } else {
                self.b.compound_type =
                    self.sym.symbol(&mut self.cdf.compound_type[self.b.mi_size]) as u8;
            }
            if self.b.compound_type == COMPOUND_WEDGE {
                self.b.wedge_index = self.sym.symbol(&mut self.cdf.wedge_index[self.b.mi_size]);
                self.b.wedge_sign = self.sym.literal(1) as usize;
            } else if self.b.compound_type == COMPOUND_DIFFWTD {
                self.b.mask_type = self.sym.literal(1) as usize;
            }
        } else if self.b.interintra {
            self.b.compound_type = if self.b.wedge_interintra {
                COMPOUND_WEDGE
            } else {
                COMPOUND_INTRA
            };
        } else {
            self.b.compound_type = COMPOUND_AVERAGE;
        }
    }
}
