//! Prediction dispatch, residual and coefficient syntax (sections 5.11.33 - 5.11.41, 5.11.47 - 5.11.48).
use crate::block::find_tx_size;
use crate::consts::*;
use crate::dec::*;
use crate::tables as t;

fn nb4w(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_WIDE[b])
}
fn nb4h(b: usize) -> usize {
    usize::from(t::NUM_4X4_BLOCKS_HIGH[b])
}

pub fn get_tx_class(tx_type: u8) -> usize {
    match tx_type {
        V_DCT | V_ADST | V_FLIPADST => TX_CLASS_VERT,
        H_DCT | H_ADST | H_FLIPADST => TX_CLASS_HORIZ,
        _ => TX_CLASS_2D,
    }
}

impl Dec {
    pub fn get_plane_residual_size(&self, subsize: usize, plane: usize) -> usize {
        let sx = if plane > 0 { self.subx } else { 0 };
        let sy = if plane > 0 { self.suby } else { 0 };
        usize::from(t::SUBSAMPLED_SIZE[subsize][sx][sy])
    }

    pub fn compute_prediction(&mut self) {
        let sb_mask = if self.seq.use_128x128_superblock {
            31
        } else {
            15
        };
        let sub_block_mi_row = self.b.mi_row & sb_mask;
        let sub_block_mi_col = self.b.mi_col & sb_mask;
        let planes = 1 + 2 * usize::from(self.b.has_chroma);
        for plane in 0..planes {
            let plane_sz = self.get_plane_residual_size(self.b.mi_size, plane);
            let num4x4w = nb4w(plane_sz);
            let num4x4h = nb4h(plane_sz);
            let log2w = 2 + usize::from(t::MI_WIDTH_LOG2[plane_sz]);
            let log2h = 2 + usize::from(t::MI_HEIGHT_LOG2[plane_sz]);
            let sub_x = if plane > 0 { self.subx } else { 0 };
            let sub_y = if plane > 0 { self.suby } else { 0 };
            let base_x = (self.b.mi_col >> sub_x) * 4;
            let base_y = (self.b.mi_row >> sub_y) * 4;
            let mut cand_row = (self.b.mi_row >> sub_y) << sub_y;
            let mut cand_col = (self.b.mi_col >> sub_x) << sub_x;
            self.b.is_inter_intra = self.b.is_inter && self.b.ref_frame[1] == INTRA_FRAME;
            if self.b.is_inter_intra {
                let mode = match self.b.interintra_mode {
                    II_DC_PRED => DC_PRED,
                    II_V_PRED => V_PRED,
                    II_H_PRED => H_PRED,
                    _ => SMOOTH_PRED,
                };
                let a = self.block_decoded[self.bd_idx(
                    plane,
                    (sub_block_mi_row >> sub_y) as i32 - 1,
                    ((sub_block_mi_col >> sub_x) + num4x4w) as i32,
                )];
                let l = self.block_decoded[self.bd_idx(
                    plane,
                    ((sub_block_mi_row >> sub_y) + num4x4h) as i32,
                    (sub_block_mi_col >> sub_x) as i32 - 1,
                )];
                let (have_l, have_u) = if plane == 0 {
                    (self.b.avail_l, self.b.avail_u)
                } else {
                    (self.b.avail_l_chroma, self.b.avail_u_chroma)
                };
                self.predict_intra(
                    plane, base_x, base_y, have_l, have_u, a, l, mode, log2w, log2h,
                );
            }
            if self.b.is_inter {
                let mut pred_w = block_width(self.b.mi_size) >> sub_x;
                let mut pred_h = block_height(self.b.mi_size) >> sub_y;
                let mut some_use_intra = false;
                for r in 0..(num4x4h << sub_y) {
                    for c in 0..(num4x4w << sub_x) {
                        if self.mi_at(cand_row + r, cand_col + c).ref_frame[0] == INTRA_FRAME {
                            some_use_intra = true;
                        }
                    }
                }
                if some_use_intra {
                    pred_w = num4x4w * 4;
                    pred_h = num4x4h * 4;
                    cand_row = self.b.mi_row;
                    cand_col = self.b.mi_col;
                }
                let mut r = 0;
                let mut y = 0;
                while y < num4x4h * 4 {
                    let mut c = 0;
                    let mut x = 0;
                    while x < num4x4w * 4 {
                        self.predict_inter(
                            plane,
                            base_x + x,
                            base_y + y,
                            pred_w,
                            pred_h,
                            cand_row + r,
                            cand_col + c,
                        );
                        x += pred_w;
                        c += 1;
                    }
                    y += pred_h;
                    r += 1;
                }
            }
        }
    }

    pub fn residual(&mut self) {
        let sb_mask = if self.seq.use_128x128_superblock {
            31
        } else {
            15
        };
        let _ = sb_mask;
        let mi_size = self.b.mi_size;
        let width_chunks = (block_width(mi_size) >> 6).max(1);
        let height_chunks = (block_height(mi_size) >> 6).max(1);
        let mi_size_chunk = if width_chunks > 1 || height_chunks > 1 {
            BLOCK_64X64
        } else {
            mi_size
        };
        for chunk_y in 0..height_chunks {
            for chunk_x in 0..width_chunks {
                let mi_row_chunk = self.b.mi_row + (chunk_y << 4);
                let mi_col_chunk = self.b.mi_col + (chunk_x << 4);
                let planes = 1 + 2 * usize::from(self.b.has_chroma);
                for plane in 0..planes {
                    let tx_sz = if self.b.lossless {
                        TX_4X4
                    } else {
                        self.get_tx_size(plane, self.b.tx_size)
                    };
                    let step_x = usize::from(t::TX_WIDTH[tx_sz]) >> 2;
                    let step_y = usize::from(t::TX_HEIGHT[tx_sz]) >> 2;
                    let plane_sz = self.get_plane_residual_size(mi_size_chunk, plane);
                    let num4x4w = nb4w(plane_sz);
                    let num4x4h = nb4h(plane_sz);
                    let sub_x = if plane > 0 { self.subx } else { 0 };
                    let sub_y = if plane > 0 { self.suby } else { 0 };
                    let base_x = (mi_col_chunk >> sub_x) * 4;
                    let base_y = (mi_row_chunk >> sub_y) * 4;
                    if self.b.is_inter && !self.b.lossless && plane == 0 {
                        self.transform_tree(base_x, base_y, num4x4w * 4, num4x4h * 4);
                    } else {
                        let base_x_block = (self.b.mi_col >> sub_x) * 4;
                        let base_y_block = (self.b.mi_row >> sub_y) * 4;
                        let mut y = 0;
                        while y < num4x4h {
                            let mut x = 0;
                            while x < num4x4w {
                                self.transform_block(
                                    plane,
                                    base_x_block,
                                    base_y_block,
                                    tx_sz,
                                    x + ((chunk_x << 4) >> sub_x),
                                    y + ((chunk_y << 4) >> sub_y),
                                );
                                x += step_x;
                            }
                            y += step_y;
                        }
                    }
                }
            }
        }
    }

    fn get_tx_size(&self, plane: usize, tx_sz: usize) -> usize {
        if plane == 0 {
            return tx_sz;
        }
        let uv_tx =
            usize::from(t::MAX_TX_SIZE_RECT[self.get_plane_residual_size(self.b.mi_size, plane)]);
        if t::TX_WIDTH[uv_tx] == 64 || t::TX_HEIGHT[uv_tx] == 64 {
            if t::TX_WIDTH[uv_tx] == 16 {
                return TX_16X32;
            }
            if t::TX_HEIGHT[uv_tx] == 16 {
                return TX_32X16;
            }
            return TX_32X32;
        }
        uv_tx
    }

    fn transform_tree(&mut self, start_x: usize, start_y: usize, w: usize, h: usize) {
        let max_x = self.mi_cols * 4;
        let max_y = self.mi_rows * 4;
        if start_x >= max_x || start_y >= max_y {
            return;
        }
        let row = start_y >> 2;
        let col = start_x >> 2;
        let luma_tx = usize::from(self.mi_at(row, col).inter_tx_size);
        let luma_w = usize::from(t::TX_WIDTH[luma_tx]);
        let luma_h = usize::from(t::TX_HEIGHT[luma_tx]);
        if w <= luma_w && h <= luma_h {
            let tx_sz = find_tx_size(w, h);
            self.transform_block(0, start_x, start_y, tx_sz, 0, 0);
        } else if w > h {
            self.transform_tree(start_x, start_y, w / 2, h);
            self.transform_tree(start_x + w / 2, start_y, w / 2, h);
        } else if w < h {
            self.transform_tree(start_x, start_y, w, h / 2);
            self.transform_tree(start_x, start_y + h / 2, w, h / 2);
        } else {
            self.transform_tree(start_x, start_y, w / 2, h / 2);
            self.transform_tree(start_x + w / 2, start_y, w / 2, h / 2);
            self.transform_tree(start_x, start_y + h / 2, w / 2, h / 2);
            self.transform_tree(start_x + w / 2, start_y + h / 2, w / 2, h / 2);
        }
    }

    fn transform_block(
        &mut self,
        plane: usize,
        base_x: usize,
        base_y: usize,
        tx_sz: usize,
        x: usize,
        y: usize,
    ) {
        let start_x = base_x + 4 * x;
        let start_y = base_y + 4 * y;
        let sub_x = if plane > 0 { self.subx } else { 0 };
        let sub_y = if plane > 0 { self.suby } else { 0 };
        let row = (start_y << sub_y) >> 2;
        let col = (start_x << sub_x) >> 2;
        let sb_mask = if self.seq.use_128x128_superblock {
            31
        } else {
            15
        };
        let sub_block_mi_row = row & sb_mask;
        let sub_block_mi_col = col & sb_mask;
        let step_x = usize::from(t::TX_WIDTH[tx_sz]) >> 2;
        let step_y = usize::from(t::TX_HEIGHT[tx_sz]) >> 2;
        let max_x = (self.mi_cols * 4) >> sub_x;
        let max_y = (self.mi_rows * 4) >> sub_y;
        if start_x >= max_x || start_y >= max_y {
            return;
        }
        if !self.b.is_inter {
            if (plane == 0 && self.b.palette_size_y > 0)
                || (plane != 0 && self.b.palette_size_uv > 0)
            {
                self.predict_palette(plane, start_x, start_y, x, y, tx_sz);
            } else {
                let is_cfl = plane > 0 && self.b.uv_mode == UV_CFL_PRED;
                let mode = if plane == 0 {
                    self.b.y_mode
                } else if is_cfl {
                    DC_PRED
                } else {
                    self.b.uv_mode
                };
                let log2w = usize::from(t::TX_WIDTH_LOG2[tx_sz]);
                let log2h = usize::from(t::TX_HEIGHT_LOG2[tx_sz]);
                let have_l = (if plane == 0 {
                    self.b.avail_l
                } else {
                    self.b.avail_l_chroma
                }) || x > 0;
                let have_u = (if plane == 0 {
                    self.b.avail_u
                } else {
                    self.b.avail_u_chroma
                }) || y > 0;
                let a = self.block_decoded[self.bd_idx(
                    plane,
                    (sub_block_mi_row >> sub_y) as i32 - 1,
                    ((sub_block_mi_col >> sub_x) + step_x) as i32,
                )];
                let l = self.block_decoded[self.bd_idx(
                    plane,
                    ((sub_block_mi_row >> sub_y) + step_y) as i32,
                    (sub_block_mi_col >> sub_x) as i32 - 1,
                )];
                self.predict_intra(
                    plane, start_x, start_y, have_l, have_u, a, l, mode, log2w, log2h,
                );
                if is_cfl {
                    self.predict_chroma_from_luma(plane, start_x, start_y, tx_sz);
                }
            }
            if plane == 0 {
                self.b.max_luma_w = start_x + step_x * 4;
                self.b.max_luma_h = start_y + step_y * 4;
            }
        }
        if !self.b.skip {
            let eob = self.coeffs(plane, start_x, start_y, tx_sz);
            if eob > 0 {
                self.reconstruct(plane, start_x, start_y, tx_sz);
            }
        }
        for i in 0..step_y {
            for j in 0..step_x {
                let idx = ((row >> sub_y) + i) * self.mi_stride + (col >> sub_x) + j;
                self.lf_tx_sizes[plane][idx] = tx_sz as u8;
                let bi = self.bd_idx(
                    plane,
                    ((sub_block_mi_row >> sub_y) + i) as i32,
                    ((sub_block_mi_col >> sub_x) + j) as i32,
                );
                self.block_decoded[bi] = true;
            }
        }
    }

    // -----------------------------------------------------------------------------------------

    fn get_tx_set(&self, tx_sz: usize) -> usize {
        let sqr = usize::from(t::TX_SIZE_SQR[tx_sz]);
        let sqr_up = usize::from(t::TX_SIZE_SQR_UP[tx_sz]);
        if sqr_up > TX_32X32 {
            return TX_SET_DCTONLY;
        }
        if self.b.is_inter {
            if self.hdr.reduced_tx_set || sqr_up == TX_32X32 {
                TX_SET_INTER_3
            } else if sqr == TX_16X16 {
                TX_SET_INTER_2
            } else {
                TX_SET_INTER_1
            }
        } else if sqr_up == TX_32X32 {
            TX_SET_DCTONLY
        } else if self.hdr.reduced_tx_set || sqr == TX_16X16 {
            TX_SET_INTRA_2
        } else {
            TX_SET_INTRA_1
        }
    }

    fn is_tx_type_in_set(&self, tx_set: usize, tx_type: u8) -> bool {
        if self.b.is_inter {
            t::TX_TYPE_IN_SET_INTER[tx_set][usize::from(tx_type)] != 0
        } else {
            t::TX_TYPE_IN_SET_INTRA[tx_set][usize::from(tx_type)] != 0
        }
    }

    fn compute_tx_type(&self, plane: usize, tx_sz: usize, block_x: usize, block_y: usize) -> u8 {
        let sqr_up = usize::from(t::TX_SIZE_SQR_UP[tx_sz]);
        if self.b.lossless || sqr_up > TX_32X32 {
            return DCT_DCT;
        }
        let tx_set = self.get_tx_set(tx_sz);
        if plane == 0 {
            return self.tx_types[block_y * self.mi_stride + block_x];
        }
        if self.b.is_inter {
            let x4 = self.b.mi_col.max(block_x << self.subx);
            let y4 = self.b.mi_row.max(block_y << self.suby);
            let tx_type = self.tx_types[y4 * self.mi_stride + x4];
            if !self.is_tx_type_in_set(tx_set, tx_type) {
                return DCT_DCT;
            }
            return tx_type;
        }
        let tx_type = t::MODE_TO_TXFM[usize::from(self.b.uv_mode)];
        if !self.is_tx_type_in_set(tx_set, tx_type) {
            return DCT_DCT;
        }
        tx_type
    }

    fn transform_type(&mut self, x4: usize, y4: usize, tx_sz: usize) {
        let set = self.get_tx_set(tx_sz);
        let qidx = if self.hdr.segmentation_enabled {
            self.hdr
                .get_qindex(true, self.b.segment_id, self.current_q_index)
        } else {
            self.hdr.base_q_idx
        };
        let sqr = usize::from(t::TX_SIZE_SQR[tx_sz]);
        let tx_type;
        if set > 0 && qidx > 0 {
            if self.b.is_inter {
                tx_type = match set {
                    TX_SET_INTER_1 => {
                        let s = self.sym.symbol(&mut self.cdf.inter_tx_type_set1[sqr]);
                        t::TX_TYPE_INTER_INV_SET1[s]
                    }
                    TX_SET_INTER_2 => {
                        let s = self.sym.symbol(&mut self.cdf.inter_tx_type_set2);
                        t::TX_TYPE_INTER_INV_SET2[s]
                    }
                    _ => {
                        let s = self.sym.symbol(&mut self.cdf.inter_tx_type_set3[sqr]);
                        t::TX_TYPE_INTER_INV_SET3[s]
                    }
                };
            } else {
                let intra_dir = if self.b.use_filter_intra {
                    usize::from(
                        t::FILTER_INTRA_MODE_TO_INTRA_DIR[usize::from(self.b.filter_intra_mode)],
                    )
                } else {
                    usize::from(self.b.y_mode)
                };
                tx_type = if set == TX_SET_INTRA_1 {
                    let s = self
                        .sym
                        .symbol(&mut self.cdf.intra_tx_type_set1[sqr][intra_dir]);
                    t::TX_TYPE_INTRA_INV_SET1[s]
                } else {
                    let s = self
                        .sym
                        .symbol(&mut self.cdf.intra_tx_type_set2[sqr][intra_dir]);
                    t::TX_TYPE_INTRA_INV_SET2[s]
                };
            }
        } else {
            tx_type = DCT_DCT;
        }
        for i in 0..(usize::from(t::TX_WIDTH[tx_sz]) >> 2) {
            for j in 0..(usize::from(t::TX_HEIGHT[tx_sz]) >> 2) {
                self.tx_types[(y4 + j) * self.mi_stride + x4 + i] = tx_type;
            }
        }
    }

    fn get_scan(&self, tx_sz: usize) -> &'static [u16] {
        if tx_sz == TX_16X64 {
            return &t::DEFAULT_SCAN_16X32;
        }
        if tx_sz == TX_64X16 {
            return &t::DEFAULT_SCAN_32X16;
        }
        if usize::from(t::TX_SIZE_SQR_UP[tx_sz]) == TX_64X64 {
            return &t::DEFAULT_SCAN_32X32;
        }
        let ptt = self.b.plane_tx_type;
        if ptt == IDTX {
            return default_scan(tx_sz);
        }
        let prefer_row = ptt == V_DCT || ptt == V_ADST || ptt == V_FLIPADST;
        let prefer_col = ptt == H_DCT || ptt == H_ADST || ptt == H_FLIPADST;
        if prefer_row {
            match tx_sz {
                TX_4X4 => &t::MROW_SCAN_4X4,
                TX_4X8 => &t::MROW_SCAN_4X8,
                TX_8X4 => &t::MROW_SCAN_8X4,
                TX_8X8 => &t::MROW_SCAN_8X8,
                TX_8X16 => &t::MROW_SCAN_8X16,
                TX_16X8 => &t::MROW_SCAN_16X8,
                TX_16X16 => &t::MROW_SCAN_16X16,
                TX_4X16 => &t::MROW_SCAN_4X16,
                _ => &t::MROW_SCAN_16X4,
            }
        } else if prefer_col {
            match tx_sz {
                TX_4X4 => &t::MCOL_SCAN_4X4,
                TX_4X8 => &t::MCOL_SCAN_4X8,
                TX_8X4 => &t::MCOL_SCAN_8X4,
                TX_8X8 => &t::MCOL_SCAN_8X8,
                TX_8X16 => &t::MCOL_SCAN_8X16,
                TX_16X8 => &t::MCOL_SCAN_16X8,
                TX_16X16 => &t::MCOL_SCAN_16X16,
                TX_4X16 => &t::MCOL_SCAN_4X16,
                _ => &t::MCOL_SCAN_16X4,
            }
        } else {
            default_scan(tx_sz)
        }
    }

    fn coeffs(&mut self, plane: usize, start_x: usize, start_y: usize, tx_sz: usize) -> usize {
        let x4 = start_x >> 2;
        let y4 = start_y >> 2;
        let w4 = usize::from(t::TX_WIDTH[tx_sz]) >> 2;
        let h4 = usize::from(t::TX_HEIGHT[tx_sz]) >> 2;
        let tx_sz_ctx =
            (usize::from(t::TX_SIZE_SQR[tx_sz]) + usize::from(t::TX_SIZE_SQR_UP[tx_sz]) + 1) >> 1;
        let ptype = usize::from(plane > 0);
        let seg_eob = if tx_sz == TX_16X64 || tx_sz == TX_64X16 {
            512
        } else {
            1024.min(usize::from(t::TX_WIDTH[tx_sz]) * usize::from(t::TX_HEIGHT[tx_sz]))
        };
        for c in 0..seg_eob {
            self.quant[c] = 0;
        }
        let mut eob = 0usize;
        let mut cul_level = 0u32;
        let mut dc_category = 0u8;

        // all_zero context
        let mut max_x4 = self.mi_cols;
        let mut max_y4 = self.mi_rows;
        if plane > 0 {
            max_x4 >>= self.subx;
            max_y4 >>= self.suby;
        }
        let w = usize::from(t::TX_WIDTH[tx_sz]);
        let h = usize::from(t::TX_HEIGHT[tx_sz]);
        let ctx;
        {
            let bsize = self.get_plane_residual_size(self.b.mi_size, plane);
            let bw = block_width(bsize);
            let bh = block_height(bsize);
            if plane == 0 {
                let mut top = 0u32;
                let mut left = 0u32;
                for k in 0..w4 {
                    if x4 + k < max_x4 {
                        top = top.max(u32::from(self.above_level_ctx[plane][x4 + k]));
                    }
                }
                for k in 0..h4 {
                    if y4 + k < max_y4 {
                        left = left.max(u32::from(self.left_level_ctx[plane][y4 + k]));
                    }
                }
                top = top.min(255);
                left = left.min(255);
                ctx = if bw == w && bh == h {
                    0
                } else if top == 0 && left == 0 {
                    1
                } else if top == 0 || left == 0 {
                    2 + usize::from(top.max(left) > 3)
                } else if top.max(left) <= 3 {
                    4
                } else if top.min(left) <= 3 {
                    5
                } else {
                    6
                };
            } else {
                let mut above = 0u8;
                let mut left = 0u8;
                for i in 0..w4 {
                    if x4 + i < max_x4 {
                        above |= self.above_level_ctx[plane][x4 + i];
                        above |= self.above_dc_ctx[plane][x4 + i];
                    }
                }
                for i in 0..h4 {
                    if y4 + i < max_y4 {
                        left |= self.left_level_ctx[plane][y4 + i];
                        left |= self.left_dc_ctx[plane][y4 + i];
                    }
                }
                let mut c = usize::from(above != 0) + usize::from(left != 0);
                c += 7;
                if bw * bh > w * h {
                    c += 3;
                }
                ctx = c;
            }
        }
        let all_zero = self.sym.symbol(&mut self.cdf.txb_skip[tx_sz_ctx][ctx]) != 0;
        if all_zero {
            if plane == 0 {
                for i in 0..w4 {
                    for j in 0..h4 {
                        self.tx_types[(y4 + j) * self.mi_stride + x4 + i] = DCT_DCT;
                    }
                }
            }
        } else {
            if plane == 0 {
                self.transform_type(x4, y4, tx_sz);
            }
            self.b.plane_tx_type = self.compute_tx_type(plane, tx_sz, x4, y4);
            let tx_class = get_tx_class(self.b.plane_tx_type);
            let scan = self.get_scan(tx_sz);
            let eob_multisize = usize::from(t::TX_WIDTH_LOG2[tx_sz]).min(5)
                + usize::from(t::TX_HEIGHT_LOG2[tx_sz]).min(5)
                - 4;
            let ectx = usize::from(tx_class != TX_CLASS_2D);
            let eob_pt = 1 + match eob_multisize {
                0 => self.sym.symbol(&mut self.cdf.eob_pt_16[ptype][ectx]),
                1 => self.sym.symbol(&mut self.cdf.eob_pt_32[ptype][ectx]),
                2 => self.sym.symbol(&mut self.cdf.eob_pt_64[ptype][ectx]),
                3 => self.sym.symbol(&mut self.cdf.eob_pt_128[ptype][ectx]),
                4 => self.sym.symbol(&mut self.cdf.eob_pt_256[ptype][ectx]),
                5 => self.sym.symbol(&mut self.cdf.eob_pt_512[ptype]),
                _ => self.sym.symbol(&mut self.cdf.eob_pt_1024[ptype]),
            };
            eob = if eob_pt < 2 {
                eob_pt
            } else {
                (1 << (eob_pt - 2)) + 1
            };
            let eob_shift = eob_pt as i32 - 3;
            if eob_shift >= 0 {
                let extra = self
                    .sym
                    .symbol(&mut self.cdf.eob_extra[tx_sz_ctx][ptype][eob_pt - 3]);
                if extra != 0 {
                    eob += 1 << eob_shift;
                }
                for i in 1..(eob_pt.saturating_sub(2)) {
                    let shift = eob_pt.saturating_sub(2) - 1 - i;
                    if self.sym.literal(1) != 0 {
                        eob += 1 << shift;
                    }
                }
            }
            // geometry of the coded area
            let adj = usize::from(t::ADJUSTED_TX_SIZE[tx_sz]);
            let bwl = usize::from(t::TX_WIDTH_LOG2[adj]);
            let width = 1usize << bwl;
            let height = usize::from(t::TX_HEIGHT[adj]);
            for c in (0..eob).rev() {
                let pos = usize::from(scan[c]);
                let row = pos >> bwl;
                let col = pos - (row << bwl);
                let mut level;
                if c == eob - 1 {
                    let ctx = if c == 0 {
                        0
                    } else if c <= (height << bwl) / 8 {
                        1
                    } else if c <= (height << bwl) / 4 {
                        2
                    } else {
                        3
                    };
                    level = self
                        .sym
                        .symbol(&mut self.cdf.coeff_base_eob[tx_sz_ctx][ptype][ctx])
                        as i32
                        + 1;
                } else {
                    // get_coeff_base_ctx
                    let mut mag = 0i32;
                    for idx in 0..5 {
                        let ref_row = row + usize::from(t::SIG_REF_DIFF_OFFSET[tx_class][idx][0]);
                        let ref_col = col + usize::from(t::SIG_REF_DIFF_OFFSET[tx_class][idx][1]);
                        if ref_row < height && ref_col < width {
                            mag += self.quant[(ref_row << bwl) + ref_col].abs().min(3);
                        }
                    }
                    let c0 = ((mag + 1) >> 1).min(4) as usize;
                    let ctx = if tx_class == TX_CLASS_2D {
                        if row == 0 && col == 0 {
                            0
                        } else {
                            c0 + usize::from(
                                t::COEFF_BASE_CTX_OFFSET[tx_sz][row.min(4)][col.min(4)],
                            )
                        }
                    } else {
                        let idx = if tx_class == TX_CLASS_VERT { row } else { col };
                        c0 + usize::from(t::COEFF_BASE_POS_CTX_OFFSET[idx.min(2)])
                    };
                    level = self
                        .sym
                        .symbol(&mut self.cdf.coeff_base[tx_sz_ctx][ptype][ctx])
                        as i32;
                }
                if level > NUM_BASE_LEVELS {
                    // coeff_br context
                    let mut mag = 0i32;
                    for idx in 0..3 {
                        let ref_row =
                            row + usize::from(t::MAG_REF_OFFSET_WITH_TX_CLASS[tx_class][idx][0]);
                        let ref_col =
                            col + usize::from(t::MAG_REF_OFFSET_WITH_TX_CLASS[tx_class][idx][1]);
                        if ref_row < height && ref_col < width {
                            mag += self.quant[ref_row * width + ref_col]
                                .min(COEFF_BASE_RANGE + NUM_BASE_LEVELS + 1);
                        }
                    }
                    let mag = ((mag + 1) >> 1).min(6) as usize;
                    let ctx = if pos == 0 {
                        mag
                    } else if tx_class == 0 {
                        if row < 2 && col < 2 {
                            mag + 7
                        } else {
                            mag + 14
                        }
                    } else if tx_class == 1 {
                        if col == 0 {
                            mag + 7
                        } else {
                            mag + 14
                        }
                    } else if row == 0 {
                        mag + 7
                    } else {
                        mag + 14
                    };
                    let brc = tx_sz_ctx.min(TX_32X32);
                    for _ in 0..(COEFF_BASE_RANGE / (BR_CDF_SIZE - 1)) {
                        let br = self.sym.symbol(&mut self.cdf.coeff_br[brc][ptype][ctx]) as i32;
                        level += br;
                        if br < BR_CDF_SIZE - 1 {
                            break;
                        }
                    }
                }
                self.quant[pos] = level;
            }
            for c in 0..eob {
                let pos = usize::from(scan[c]);
                let mut sign = false;
                if self.quant[pos] != 0 {
                    if c == 0 {
                        // dc_sign context
                        let mut dc_sign = 0i32;
                        for k in 0..w4 {
                            if x4 + k < max_x4 {
                                match self.above_dc_ctx[plane][x4 + k] {
                                    1 => dc_sign -= 1,
                                    2 => dc_sign += 1,
                                    _ => {}
                                }
                            }
                        }
                        for k in 0..h4 {
                            if y4 + k < max_y4 {
                                match self.left_dc_ctx[plane][y4 + k] {
                                    1 => dc_sign -= 1,
                                    2 => dc_sign += 1,
                                    _ => {}
                                }
                            }
                        }
                        let ctx = if dc_sign < 0 {
                            1
                        } else if dc_sign > 0 {
                            2
                        } else {
                            0
                        };
                        sign = self.sym.symbol(&mut self.cdf.dc_sign[ptype][ctx]) != 0;
                    } else {
                        sign = self.sym.literal(1) != 0;
                    }
                }
                if self.quant[pos] > NUM_BASE_LEVELS + COEFF_BASE_RANGE {
                    let mut length = 0;
                    loop {
                        length += 1;
                        if self.sym.literal(1) != 0 {
                            break;
                        }
                        if length >= 32 {
                            break;
                        }
                    }
                    let mut x: u32 = 1;
                    for _ in 0..(length - 1) {
                        x = (x << 1) | self.sym.literal(1);
                    }
                    self.quant[pos] = (x as i32).wrapping_add(COEFF_BASE_RANGE + NUM_BASE_LEVELS);
                }
                if pos == 0 && self.quant[pos] > 0 {
                    dc_category = if sign { 1 } else { 2 };
                }
                self.quant[pos] &= 0xFFFFF;
                cul_level += self.quant[pos] as u32;
                if sign {
                    self.quant[pos] = -self.quant[pos];
                }
            }
            cul_level = cul_level.min(63);
        }
        for i in 0..w4 {
            self.above_level_ctx[plane][x4 + i] = cul_level as u8;
            self.above_dc_ctx[plane][x4 + i] = dc_category;
        }
        for i in 0..h4 {
            self.left_level_ctx[plane][y4 + i] = cul_level as u8;
            self.left_dc_ctx[plane][y4 + i] = dc_category;
        }
        eob
    }
}

fn default_scan(tx_sz: usize) -> &'static [u16] {
    match tx_sz {
        TX_4X4 => &t::DEFAULT_SCAN_4X4,
        TX_4X8 => &t::DEFAULT_SCAN_4X8,
        TX_8X4 => &t::DEFAULT_SCAN_8X4,
        TX_8X8 => &t::DEFAULT_SCAN_8X8,
        TX_8X16 => &t::DEFAULT_SCAN_8X16,
        TX_16X8 => &t::DEFAULT_SCAN_16X8,
        TX_16X16 => &t::DEFAULT_SCAN_16X16,
        TX_16X32 => &t::DEFAULT_SCAN_16X32,
        TX_32X16 => &t::DEFAULT_SCAN_32X16,
        TX_4X16 => &t::DEFAULT_SCAN_4X16,
        TX_16X4 => &t::DEFAULT_SCAN_16X4,
        TX_8X32 => &t::DEFAULT_SCAN_8X32,
        TX_32X8 => &t::DEFAULT_SCAN_32X8,
        _ => &t::DEFAULT_SCAN_32X32,
    }
}
