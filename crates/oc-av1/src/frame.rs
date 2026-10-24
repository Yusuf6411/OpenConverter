//! Per-frame state set-up, tile-group decoding and the reference update process (sections 5.11.1, 7.4, 7.20, 7.21).
use crate::cdf::Cdfs;
use crate::consts::*;
use crate::dec::*;
use crate::hdr::*;
use crate::sym::Sym;
use oc_core::{Error, Result};
use std::rc::Rc;

fn align(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

fn count_units_in_frame(unit_size: usize, frame_size: usize) -> usize {
    ((frame_size + (unit_size >> 1)) / unit_size).max(1)
}

impl Dec {
    pub fn new_frame(
        seq: &SeqHdr,
        hdr: FrameHdr,
        refh: &[RefHdr; 8],
        refd: &[Option<RefData>; 8],
    ) -> Result<Dec> {
        let mi_cols = hdr.mi_cols as usize;
        let mi_rows = hdr.mi_rows as usize;
        let mi_stride = align(mi_cols, 32);
        let mi_rows_alloc = align(mi_rows, 32);
        let num_planes = seq.num_planes;
        let subx = seq.subsampling_x as usize;
        let suby = seq.subsampling_y as usize;
        let mi_total = mi_stride * mi_rows_alloc;
        let (cdf,) = if hdr.primary_ref_frame == PRIMARY_REF_NONE {
            (Cdfs::new(hdr.base_q_idx),)
        } else {
            let idx = hdr.ref_frame_idx[hdr.primary_ref_frame as usize];
            let r = refd[idx]
                .as_ref()
                .ok_or_else(|| Error::corrupt("AV1: primary reference frame missing"))?;
            let mut c = (*r.cdfs).clone();
            c.clear_counts();
            (c,)
        };
        let mut planes = Vec::new();
        for p in 0..num_planes {
            let (sx, sy) = if p > 0 { (subx, suby) } else { (0, 0) };
            let w = align(mi_cols * 4, 128) >> sx;
            let h = align(mi_rows * 4, 128) >> sy;
            planes.push(Plane::new(w + 32, h + 32));
        }
        let mut prev_segment_ids = vec![0u8; mi_total];
        if hdr.primary_ref_frame != PRIMARY_REF_NONE && hdr.segmentation_enabled {
            let idx = hdr.ref_frame_idx[hdr.primary_ref_frame as usize];
            if let (Some(r), rh) = (&refd[idx], &refh[idx]) {
                if rh.mi_cols as usize == mi_cols && rh.mi_rows as usize == mi_rows {
                    for row in 0..mi_rows {
                        for col in 0..mi_cols {
                            prev_segment_ids[row * mi_stride + col] =
                                r.segment_ids[row * r.mi_stride + col];
                        }
                    }
                }
            }
        }
        let cdef_stride = (mi_cols >> 4) + 3;
        let cdef_rows = (mi_rows >> 4) + 3;
        let mut lr_units: [Vec<LrUnit>; 3] = Default::default();
        let mut lr_cols = [0usize; 3];
        let mut lr_rows = [0usize; 3];
        if hdr.uses_lr {
            for p in 0..num_planes {
                if hdr.frame_restoration_type[p] != RESTORE_NONE {
                    let (sx, sy) = if p > 0 { (subx, suby) } else { (0, 0) };
                    let unit_size = hdr.loop_restoration_size[p] as usize;
                    lr_rows[p] =
                        count_units_in_frame(unit_size, (hdr.frame_height as usize + sy) >> sy);
                    lr_cols[p] =
                        count_units_in_frame(unit_size, (hdr.upscaled_width as usize + sx) >> sx);
                    lr_units[p] = vec![LrUnit::default(); lr_rows[p] * lr_cols[p]];
                }
            }
        }
        let ctx_len = mi_stride + 32;
        let left_len = mi_rows_alloc + 32;
        let mut refh_c: [RefHdr; 8] = Default::default();
        refh_c.clone_from_slice(refh);
        let d = Dec {
            seq: seq.clone(),
            frame_cdf: cdf.clone(),
            cdf,
            saved_cdf: None,
            sym: Sym::new(&[], true),
            b: Blk::default(),
            hdr,
            refh: refh_c,
            refd: refd.clone(),
            cur: Frame { planes },
            mi_rows,
            mi_cols,
            mi_stride,
            mi_rows_alloc,
            num_planes,
            subx,
            suby,
            bit_depth: seq.bit_depth,
            mi_row_start: 0,
            mi_row_end: 0,
            mi_col_start: 0,
            mi_col_end: 0,
            current_q_index: 0,
            read_deltas: false,
            delta_lf: [0; 4],
            ref_sgr_xqd: [[0; 2]; 3],
            ref_lr_wiener: [[[0; 3]; 2]; 3],
            mi: vec![MiInfo::default(); mi_total],
            pal_colors: vec![[[0; 8]; 2]; mi_total],
            tx_types: vec![0; mi_total],
            lf_tx_sizes: [vec![0; mi_total], vec![0; mi_total], vec![0; mi_total]],
            prev_segment_ids,
            cdef_idx: vec![-1; cdef_stride * cdef_rows],
            cdef_stride,
            lr_units,
            lr_cols,
            lr_rows,
            above_level_ctx: [vec![0; ctx_len], vec![0; ctx_len], vec![0; ctx_len]],
            above_dc_ctx: [vec![0; ctx_len], vec![0; ctx_len], vec![0; ctx_len]],
            above_seg_pred: vec![0; ctx_len],
            left_level_ctx: [vec![0; left_len], vec![0; left_len], vec![0; left_len]],
            left_dc_ctx: [vec![0; left_len], vec![0; left_len], vec![0; left_len]],
            left_seg_pred: vec![0; left_len],
            block_decoded: vec![false; 3 * 35 * 35],
            quant: vec![0; 1024],
            dequant: Vec::new(),
            mf_ref_frames: Vec::new(),
            mf_mvs: Vec::new(),
            motion_field_mvs: Vec::new(),
            motion_field_valid: Vec::new(),
            scratch: Vec::new(),
            mask: vec![0; 128 * 128],
        };
        let mut d = d;
        if d.hdr.use_ref_frame_mvs {
            d.motion_field_estimation();
        }
        Ok(d)
    }

    /// Decodes the tiles of one tile group OBU. Returns true when the last tile of the frame was decoded.
    pub fn tile_group(&mut self, data: &[u8]) -> Result<bool> {
        let num_tiles = self.hdr.tile_cols * self.hdr.tile_rows;
        let mut b = crate::bits::Bits::new(data);
        let mut tile_start_and_end_present = false;
        if num_tiles > 1 {
            tile_start_and_end_present = b.flag()?;
        }
        let (tg_start, tg_end) = if num_tiles == 1 || !tile_start_and_end_present {
            (0, num_tiles - 1)
        } else {
            let bits = self.hdr.tile_cols_log2 + self.hdr.tile_rows_log2;
            (b.f(bits)? as usize, b.f(bits)? as usize)
        };
        b.byte_align();
        let mut pos = b.position() / 8;
        if tg_end >= num_tiles || tg_start > tg_end {
            return Err(Error::corrupt("AV1: bad tile group range"));
        }
        for tile_num in tg_start..=tg_end {
            let tile_row = tile_num / self.hdr.tile_cols;
            let tile_col = tile_num % self.hdr.tile_cols;
            let last = tile_num == tg_end;
            let tile_size;
            if last {
                tile_size = data.len().saturating_sub(pos);
            } else {
                let n = self.hdr.tile_size_bytes as usize;
                if pos + n > data.len() {
                    return Err(Error::truncated("AV1: tile size field"));
                }
                let mut v = 0usize;
                for i in 0..n {
                    v |= usize::from(data[pos + i]) << (8 * i);
                }
                pos += n;
                tile_size = v + 1;
            }
            if pos + tile_size > data.len() {
                return Err(Error::truncated("AV1: tile data"));
            }
            self.mi_row_start = self.hdr.mi_row_starts[tile_row] as usize;
            self.mi_row_end = self.hdr.mi_row_starts[tile_row + 1] as usize;
            self.mi_col_start = self.hdr.mi_col_starts[tile_col] as usize;
            self.mi_col_end = self.hdr.mi_col_starts[tile_col + 1] as usize;
            self.current_q_index = self.hdr.base_q_idx;
            self.sym = Sym::new(&data[pos..pos + tile_size], self.hdr.disable_cdf_update);
            self.cdf = self.frame_cdf.clone();
            self.decode_tile();
            if !self.hdr.disable_frame_end_update_cdf && tile_num == self.hdr.context_update_tile_id
            {
                self.saved_cdf = Some(self.cdf.clone());
            }
            pos += tile_size;
        }
        Ok(tg_end == num_tiles - 1)
    }

    /// decode_frame_wrapup for a normally decoded frame: applies the filters and stores motion vectors.
    pub fn wrapup(&mut self) {
        if !self.hdr.disable_frame_end_update_cdf {
            if let Some(s) = self.saved_cdf.take() {
                self.frame_cdf = s;
            }
        }
        if self.hdr.loop_filter_level[0] != 0 || self.hdr.loop_filter_level[1] != 0 {
            self.loop_filter_frame();
        }
        self.post_filters();
        self.store_motion_vectors();
        if self.hdr.segmentation_enabled && !self.hdr.segmentation_update_map {
            for row in 0..self.mi_rows {
                for col in 0..self.mi_cols {
                    let i = row * self.mi_stride + col;
                    self.mi[i].segment_id = self.prev_segment_ids[i];
                }
            }
        }
    }

    fn store_motion_vectors(&mut self) {
        let n = self.mi.len();
        self.mf_ref_frames = vec![NONE; n];
        self.mf_mvs = vec![[0, 0]; n];
        for row in 0..self.mi_rows {
            for col in 0..self.mi_cols {
                let idx = row * self.mi_stride + col;
                let m = self.mi[idx];
                for list in 0..2 {
                    let r = m.ref_frame[list];
                    if r > INTRA_FRAME {
                        let dist = get_relative_dist(
                            &self.seq,
                            self.hdr.order_hints[r as usize],
                            self.hdr.order_hint,
                        );
                        if dist < 0 {
                            let mv = m.mv[list];
                            if i32::from(mv[0]).abs() <= REFMVS_LIMIT
                                && i32::from(mv[1]).abs() <= REFMVS_LIMIT
                            {
                                self.mf_ref_frames[idx] = r;
                                self.mf_mvs[idx] = mv;
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn ref_hdr(&self) -> RefHdr {
        let h = &self.hdr;
        RefHdr {
            valid: true,
            frame_id: h.current_frame_id,
            upscaled_width: h.upscaled_width,
            frame_width: h.frame_width,
            frame_height: h.frame_height,
            render_width: h.render_width,
            render_height: h.render_height,
            mi_cols: h.mi_cols,
            mi_rows: h.mi_rows,
            frame_type: h.frame_type,
            subsampling_x: self.seq.subsampling_x,
            subsampling_y: self.seq.subsampling_y,
            bit_depth: self.seq.bit_depth,
            order_hint: h.order_hint,
            saved_order_hints: h.order_hints,
            gm_params: h.gm_params,
            lf_ref_deltas: h.loop_filter_ref_deltas,
            lf_mode_deltas: h.loop_filter_mode_deltas,
            feature_enabled: h.feature_enabled,
            feature_data: h.feature_data,
            film_grain: h.film_grain.clone(),
            showable_frame: h.showable_frame,
        }
    }

    pub fn ref_data(&self, frame: Rc<Frame>) -> RefData {
        let mut seg = vec![0u8; self.mi.len()];
        for (i, m) in self.mi.iter().enumerate() {
            seg[i] = m.segment_id;
        }
        RefData {
            frame,
            cdfs: Rc::new(self.frame_cdf.clone()),
            mf_ref_frames: Rc::new(self.mf_ref_frames.clone()),
            mf_mvs: Rc::new(self.mf_mvs.clone()),
            segment_ids: Rc::new(seg),
            mi_stride: self.mi_stride,
        }
    }
}
