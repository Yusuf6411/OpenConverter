//! Tile, partition, block and residual decoding (spec 6.4, 6.5, 8.5, 8.6, 9.3).
use crate::boolcoder::BoolDecoder;
use crate::frame::FrameBuf;
use crate::hdr::*;
use crate::mi::*;
use crate::pred::*;
use crate::probs::*;
use crate::tables::*;
use crate::transform::{inverse_2d, ADST_DCT, DCT_ADST, DCT_DCT};
use oc_core::{Error, Result};
use std::sync::Arc;

const BLOCK_8X8: usize = 3;
const BLOCK_64X64: usize = 12;
const PARTITION_NONE: usize = 0;
const PARTITION_HORZ: usize = 1;
const PARTITION_VERT: usize = 2;
const PARTITION_SPLIT: usize = 3;
const ZEROMV: u8 = 12;
const NEARESTMV: u8 = 10;
const NEARMV: u8 = 11;
const NEWMV: u8 = 13;

fn bad(msg: &str) -> Error {
    Error::corrupt(format!("VP9: {msg}"))
}

/// Segmentation parameters in force for the frame.
#[derive(Clone)]
pub struct Seg {
    pub enabled: [[bool; 4]; 8],
    pub data: [[i16; 4]; 8],
    pub abs: bool,
    pub tree_probs: [u8; 7],
    pub pred_probs: [u8; 3],
}

#[derive(Default, Clone)]
struct Blk {
    mi_row: usize,
    mi_col: usize,
    size: usize,
    avail_u: bool,
    avail_l: bool,
    seg_id: usize,
    skip: bool,
    is_inter: bool,
    tx_size: usize,
    y_mode: u8,
    sub_modes: [u8; 4],
    uv_mode: u8,
    ref_frame: [i8; 2],
    interp: u8,
    block_mvs: [[Mv; 4]; 2],
    left_ref: [i8; 2],
    above_ref: [i8; 2],
    left_intra: bool,
    above_intra: bool,
    left_single: bool,
    above_single: bool,
    ref_list_mv: [Mv; 2],
    nearest: [Mv; 2],
    near: [Mv; 2],
    best: [Mv; 2],
    mode_ctx: [u8; 4],
}

pub struct FrameState<'d> {
    pub h: Hdr,
    pub probs: Probs,
    pub counts: Counts,
    pub cur: FrameBuf,
    pub refs: [Option<Arc<FrameBuf>>; 3],
    pub ref_scale: [(i64, i64); 3],
    pub mi: Vec<Mi>,
    pub prev_mi: Vec<Mi>,
    pub use_prev_mvs: bool,
    pub prev_seg: Vec<u8>,
    pub seg: Seg,
    dq: [[[i32; 2]; 2]; 8],
    above_nz: [Vec<u8>; 3],
    left_nz: [[u8; 16]; 3],
    above_part: Vec<u8>,
    left_part: [u8; 8],
    above_seg: Vec<u8>,
    left_seg: [u8; 8],
    bd: Option<BoolDecoder<'d>>,
    col_start: usize,
    col_end: usize,
    b: Blk,
    coefs: Vec<i32>,
    token_cache: Vec<u8>,
    scratch: InterScratch,
    pred_buf: [Vec<u16>; 2],
    eob_total: u32,
}

fn mv_hp_ok(mv: Mv) -> bool {
    (i32::from(mv.row).abs() >> 3) < 8 && (i32::from(mv.col).abs() >> 3) < 8
}

impl<'d> FrameState<'d> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        h: Hdr,
        probs: Probs,
        refs: [Option<Arc<FrameBuf>>; 3],
        prev_mi: Vec<Mi>,
        use_prev_mvs: bool,
        prev_seg: Vec<u8>,
        seg: Seg,
    ) -> Result<Self> {
        let cur = FrameBuf::new(h.width, h.height, h.ss_x, h.ss_y, h.bit_depth);
        let mut ref_scale = [(1i64 << 14, 1i64 << 14); 3];
        if !h.frame_is_intra {
            for i in 0..3 {
                let r = refs[i]
                    .as_ref()
                    .ok_or_else(|| bad("missing reference frame"))?;
                if r.bit_depth != h.bit_depth || r.ss_x != h.ss_x || r.ss_y != h.ss_y {
                    return Err(bad("reference frame has a different format"));
                }
                if 2 * h.width < r.w
                    || 2 * h.height < r.h
                    || h.width > 16 * r.w
                    || h.height > 16 * r.h
                {
                    // Unusable as a reference; blocks that use it are rejected.
                    ref_scale[i] = (0, 0);
                } else {
                    ref_scale[i] = (
                        ((r.w as i64) << 14) / h.width as i64,
                        ((r.h as i64) << 14) / h.height as i64,
                    );
                }
            }
        }
        let n = h.mi_rows * h.mi_cols;
        let sbc = h.sb_cols * 16 + 32;
        let mut dq = [[[0i32; 2]; 2]; 8];
        let bdi = usize::from((h.bit_depth - 8) >> 1);
        for (s, d) in dq.iter_mut().enumerate() {
            let mut q = h.base_q;
            if h.seg_enabled && seg.enabled[s][SEG_LVL_ALT_Q] {
                let v = i32::from(seg.data[s][SEG_LVL_ALT_Q]);
                q = (if seg.abs { v } else { h.base_q + v }).clamp(0, 255);
            }
            let dcq = |b: i32| DC_QLOOKUP[bdi][b.clamp(0, 255) as usize];
            let acq = |b: i32| AC_QLOOKUP[bdi][b.clamp(0, 255) as usize];
            d[0] = [dcq(q + h.dq_y_dc), acq(q)];
            d[1] = [dcq(q + h.dq_uv_dc), acq(q + h.dq_uv_ac)];
        }
        Ok(Self {
            probs,
            counts: Counts::default(),
            cur,
            refs,
            ref_scale,
            mi: vec![Mi::default(); n],
            prev_mi,
            use_prev_mvs,
            prev_seg,
            seg,
            dq,
            above_nz: [vec![0; sbc], vec![0; sbc], vec![0; sbc]],
            left_nz: [[0; 16]; 3],
            above_part: vec![0; h.sb_cols * 8 + 8],
            left_part: [0; 8],
            above_seg: vec![0; h.sb_cols * 8 + 8],
            left_seg: [0; 8],
            bd: None,
            col_start: 0,
            col_end: 0,
            b: Blk::default(),
            coefs: vec![0; 1024],
            token_cache: vec![0; 1024],
            scratch: InterScratch::default(),
            pred_buf: [vec![0; 64 * 64], vec![0; 64 * 64]],
            eob_total: 0,
            h,
        })
    }

    // ---- bit reading helpers ----

    #[inline]
    fn rd(&mut self, p: u8) -> bool {
        match self.bd.as_mut() {
            Some(b) => b.read(p),
            None => false,
        }
    }

    fn tree(&mut self, tree: &[i8], probs: &[u8]) -> usize {
        let mut n = 0usize;
        loop {
            let bit = usize::from(self.rd(probs[n >> 1]));
            let next = tree[n + bit];
            if next <= 0 {
                return (-next) as usize;
            }
            n = next as usize;
        }
    }

    // ---- tiles and partitions ----

    pub fn decode_tiles(&mut self, data: &'d [u8]) -> Result<()> {
        let tile_cols = 1usize << self.h.tile_cols_log2;
        let tile_rows = 1usize << self.h.tile_rows_log2;
        let mut pos = 0usize;
        for tile_row in 0..tile_rows {
            for tile_col in 0..tile_cols {
                let last = tile_row == tile_rows - 1 && tile_col == tile_cols - 1;
                let size = if last {
                    data.len() - pos
                } else {
                    let s = data
                        .get(pos..pos + 4)
                        .ok_or_else(|| Error::truncated("VP9 tile size is missing"))?;
                    pos += 4;
                    u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize
                };
                let chunk = data
                    .get(pos..pos + size)
                    .ok_or_else(|| Error::truncated("VP9 tile is truncated"))?;
                pos += size;
                let row_start = tile_offset(tile_row, self.h.mi_rows, self.h.tile_rows_log2);
                let row_end = tile_offset(tile_row + 1, self.h.mi_rows, self.h.tile_rows_log2);
                self.col_start = tile_offset(tile_col, self.h.mi_cols, self.h.tile_cols_log2);
                self.col_end = tile_offset(tile_col + 1, self.h.mi_cols, self.h.tile_cols_log2);
                self.bd = Some(BoolDecoder::new(chunk).ok_or_else(|| bad("bad tile data"))?);
                let mut r = row_start;
                while r < row_end {
                    self.left_nz = [[0; 16]; 3];
                    self.left_part = [0; 8];
                    self.left_seg = [0; 8];
                    let mut c = self.col_start;
                    while c < self.col_end {
                        self.decode_partition(r, c, BLOCK_64X64)?;
                        c += 8;
                    }
                    r += 8;
                }
            }
        }
        Ok(())
    }

    fn decode_partition(&mut self, r: usize, c: usize, bsize: usize) -> Result<()> {
        if r >= self.h.mi_rows || c >= self.h.mi_cols {
            return Ok(());
        }
        let num8x8 = usize::from(NUM_8X8_BLOCKS_WIDE_LOOKUP[bsize]);
        let half = num8x8 >> 1;
        let has_rows = (r + half) < self.h.mi_rows;
        let has_cols = (c + half) < self.h.mi_cols;
        let bsl = usize::from(MI_WIDTH_LOG2_LOOKUP[bsize]);
        let boffset = 3 - bsl;
        let (mut above, mut left) = (0u8, 0u8);
        for i in 0..num8x8 {
            above |= self.above_part[c + i];
            left |= self.left_part[(r + i) & 7];
        }
        let above = usize::from(above & (1 << boffset) != 0);
        let left = usize::from(left & (1 << boffset) != 0);
        let ctx = bsl * 4 + left * 2 + above;
        let probs = if self.h.frame_is_intra {
            KF_PARTITION_PROBS[ctx]
        } else {
            self.probs.partition[ctx]
        };
        let partition = if has_rows && has_cols {
            self.tree(&PARTITION_TREE, &probs)
        } else if has_cols {
            if self.rd(probs[1]) {
                PARTITION_SPLIT
            } else {
                PARTITION_HORZ
            }
        } else if has_rows {
            if self.rd(probs[2]) {
                PARTITION_SPLIT
            } else {
                PARTITION_VERT
            }
        } else {
            PARTITION_SPLIT
        };
        self.counts.partition[ctx][partition] += 1;
        let subsize = usize::from(SUBSIZE_LOOKUP[partition][bsize]);
        if subsize < BLOCK_8X8 || partition == PARTITION_NONE {
            self.decode_block(r, c, subsize)?;
        } else if partition == PARTITION_HORZ {
            self.decode_block(r, c, subsize)?;
            if has_rows {
                self.decode_block(r + half, c, subsize)?;
            }
        } else if partition == PARTITION_VERT {
            self.decode_block(r, c, subsize)?;
            if has_cols {
                self.decode_block(r, c + half, subsize)?;
            }
        } else {
            self.decode_partition(r, c, subsize)?;
            self.decode_partition(r, c + half, subsize)?;
            self.decode_partition(r + half, c, subsize)?;
            self.decode_partition(r + half, c + half, subsize)?;
        }
        if bsize == BLOCK_8X8 || partition != PARTITION_SPLIT {
            for i in 0..num8x8 {
                self.above_part[c + i] = 15 >> B_WIDTH_LOG2_LOOKUP[subsize];
                self.left_part[(r + i) & 7] = 15 >> B_HEIGHT_LOG2_LOOKUP[subsize];
            }
        }
        Ok(())
    }

    fn mi_at(&self, r: usize, c: usize) -> &Mi {
        &self.mi[r * self.h.mi_cols + c]
    }

    fn decode_block(&mut self, r: usize, c: usize, subsize: usize) -> Result<()> {
        self.b = Blk {
            mi_row: r,
            mi_col: c,
            size: subsize,
            avail_u: r > 0,
            avail_l: c > self.col_start,
            ..Blk::default()
        };
        if self.h.frame_is_intra {
            self.intra_frame_mode_info();
        } else {
            self.inter_frame_mode_info()?;
        }
        self.eob_total = 0;
        self.residual()?;
        if self.b.is_inter && subsize >= BLOCK_8X8 && self.eob_total == 0 {
            self.b.skip = true;
        }
        let b = &self.b;
        let mi = Mi {
            size: subsize as u8,
            skip: b.skip,
            tx_size: b.tx_size as u8,
            y_mode: b.y_mode,
            sub_modes: b.sub_modes,
            seg_id: b.seg_id as u8,
            ref_frame: b.ref_frame,
            interp: b.interp,
            mv: if b.is_inter {
                b.block_mvs
            } else {
                [[Mv::default(); 4]; 2]
            },
        };
        let bh = usize::from(NUM_8X8_BLOCKS_HIGH_LOOKUP[subsize]);
        let bw = usize::from(NUM_8X8_BLOCKS_WIDE_LOOKUP[subsize]);
        let cols = self.h.mi_cols;
        for y in 0..bh {
            if r + y >= self.h.mi_rows {
                break;
            }
            for x in 0..bw {
                if c + x >= cols {
                    break;
                }
                self.mi[(r + y) * cols + c + x] = mi;
            }
        }
        Ok(())
    }

    // ---- mode info ----

    fn seg_feature_active(&self, f: usize) -> bool {
        self.h.seg_enabled && self.seg.enabled[self.b.seg_id][f]
    }

    fn read_skip(&mut self) {
        if self.seg_feature_active(SEG_LVL_SKIP) {
            self.b.skip = true;
        } else {
            let mut ctx = 0;
            if self.b.avail_u {
                ctx += usize::from(self.mi_at(self.b.mi_row - 1, self.b.mi_col).skip);
            }
            if self.b.avail_l {
                ctx += usize::from(self.mi_at(self.b.mi_row, self.b.mi_col - 1).skip);
            }
            let s = self.rd(self.probs.skip[ctx]);
            self.counts.skip[ctx][usize::from(s)] += 1;
            self.b.skip = s;
        }
    }

    fn read_tx_size(&mut self, allow_select: bool) {
        let max_tx = usize::from(MAX_TXSIZE_LOOKUP[self.b.size]);
        if allow_select && self.h.tx_mode == TX_MODE_SELECT && self.b.size >= BLOCK_8X8 {
            let mut above = max_tx;
            let mut left = max_tx;
            if self.b.avail_u {
                let m = self.mi_at(self.b.mi_row - 1, self.b.mi_col);
                if !m.skip {
                    above = usize::from(m.tx_size);
                }
            }
            if self.b.avail_l {
                let m = self.mi_at(self.b.mi_row, self.b.mi_col - 1);
                if !m.skip {
                    left = usize::from(m.tx_size);
                }
            }
            if !self.b.avail_l {
                left = above;
            }
            if !self.b.avail_u {
                above = left;
            }
            let ctx = usize::from(above + left > max_tx);
            let p = self.probs.tx[max_tx][ctx];
            let t = match max_tx {
                3 => self.tree(&TX_SIZE_32_TREE, &p),
                2 => self.tree(&TX_SIZE_16_TREE, &p),
                _ => self.tree(&TX_SIZE_8_TREE, &p),
            };
            self.counts.tx_size[max_tx][ctx][t] += 1;
            self.b.tx_size = t;
        } else {
            self.b.tx_size = max_tx.min(usize::from(
                TX_MODE_TO_BIGGEST_TX_SIZE[usize::from(self.h.tx_mode)],
            ));
        }
    }

    fn intra_frame_mode_info(&mut self) {
        // intra_segment_id
        self.b.seg_id = if self.h.seg_enabled && self.h.seg_update_map {
            let p = self.seg.tree_probs;
            self.tree(&SEGMENT_TREE, &p)
        } else {
            0
        };
        self.read_skip();
        self.read_tx_size(true);
        self.b.ref_frame = [INTRA_FRAME, NONE];
        self.b.is_inter = false;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        if self.b.size >= BLOCK_8X8 {
            let above = if self.b.avail_u {
                self.mi_at(r - 1, c).sub_modes[2]
            } else {
                DC_PRED
            };
            let left = if self.b.avail_l {
                self.mi_at(r, c - 1).sub_modes[1]
            } else {
                DC_PRED
            };
            let p = KF_Y_MODE_PROBS[usize::from(above)][usize::from(left)];
            let m = self.tree(&INTRA_MODE_TREE, &p) as u8;
            self.b.y_mode = m;
            self.b.sub_modes = [m; 4];
        } else {
            let w4 = usize::from(NUM_4X4_BLOCKS_WIDE_LOOKUP[self.b.size]);
            let h4 = usize::from(NUM_4X4_BLOCKS_HIGH_LOOKUP[self.b.size]);
            let mut m = 0u8;
            let mut idy = 0;
            while idy < 2 {
                let mut idx = 0;
                while idx < 2 {
                    let above = if idy > 0 {
                        self.b.sub_modes[idx]
                    } else if self.b.avail_u {
                        self.mi_at(r - 1, c).sub_modes[2 + idx]
                    } else {
                        DC_PRED
                    };
                    let left = if idx > 0 {
                        self.b.sub_modes[idy * 2]
                    } else if self.b.avail_l {
                        self.mi_at(r, c - 1).sub_modes[1 + idy * 2]
                    } else {
                        DC_PRED
                    };
                    let p = KF_Y_MODE_PROBS[usize::from(above)][usize::from(left)];
                    m = self.tree(&INTRA_MODE_TREE, &p) as u8;
                    for y2 in 0..h4 {
                        for x2 in 0..w4 {
                            self.b.sub_modes[(idy + y2) * 2 + idx + x2] = m;
                        }
                    }
                    idx += w4;
                }
                idy += h4;
            }
            self.b.y_mode = m;
        }
        let p = KF_UV_MODE_PROBS[usize::from(self.b.y_mode)];
        self.b.uv_mode = self.tree(&INTRA_MODE_TREE, &p) as u8;
    }

    fn inter_frame_mode_info(&mut self) -> Result<()> {
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let (au, al) = (self.b.avail_u, self.b.avail_l);
        self.b.left_ref = if al {
            self.mi_at(r, c - 1).ref_frame
        } else {
            [INTRA_FRAME, NONE]
        };
        self.b.above_ref = if au {
            self.mi_at(r - 1, c).ref_frame
        } else {
            [INTRA_FRAME, NONE]
        };
        self.b.left_intra = self.b.left_ref[0] <= INTRA_FRAME;
        self.b.above_intra = self.b.above_ref[0] <= INTRA_FRAME;
        self.b.left_single = self.b.left_ref[1] <= NONE;
        self.b.above_single = self.b.above_ref[1] <= NONE;
        self.inter_segment_id();
        self.read_skip();
        // read_is_inter
        self.b.is_inter = if self.seg_feature_active(SEG_LVL_REF_FRAME) {
            self.seg.data[self.b.seg_id][SEG_LVL_REF_FRAME] != 0
        } else {
            let ctx = if au && al {
                if self.b.left_intra && self.b.above_intra {
                    3
                } else {
                    usize::from(self.b.left_intra || self.b.above_intra)
                }
            } else if au || al {
                2 * usize::from(if au {
                    self.b.above_intra
                } else {
                    self.b.left_intra
                })
            } else {
                0
            };
            let v = self.rd(self.probs.is_inter[ctx]);
            self.counts.is_inter[ctx][usize::from(v)] += 1;
            v
        };
        self.read_tx_size(!self.b.skip || !self.b.is_inter);
        if self.b.is_inter {
            self.inter_block_mode_info()
        } else {
            self.intra_block_mode_info();
            Ok(())
        }
    }

    fn get_segment_id(&self) -> usize {
        let b = &self.b;
        let bw = usize::from(NUM_8X8_BLOCKS_WIDE_LOOKUP[b.size]);
        let bh = usize::from(NUM_8X8_BLOCKS_HIGH_LOOKUP[b.size]);
        let xmis = (self.h.mi_cols - b.mi_col).min(bw);
        let ymis = (self.h.mi_rows - b.mi_row).min(bh);
        let mut seg = 7u8;
        for y in 0..ymis {
            for x in 0..xmis {
                seg = seg.min(self.prev_seg[(b.mi_row + y) * self.h.mi_cols + b.mi_col + x]);
            }
        }
        usize::from(seg)
    }

    fn inter_segment_id(&mut self) {
        if !self.h.seg_enabled {
            self.b.seg_id = 0;
            return;
        }
        let predicted = self.get_segment_id();
        if !self.h.seg_update_map {
            self.b.seg_id = predicted;
            return;
        }
        if self.h.seg_temporal {
            let ctx = usize::from(self.left_seg[self.b.mi_row & 7] + self.above_seg[self.b.mi_col]);
            let p = self.seg.pred_probs[ctx];
            let pred_flag = self.rd(p);
            if pred_flag {
                self.b.seg_id = predicted;
            } else {
                let p = self.seg.tree_probs;
                self.b.seg_id = self.tree(&SEGMENT_TREE, &p);
            }
            let bw = usize::from(NUM_8X8_BLOCKS_WIDE_LOOKUP[self.b.size]);
            let bh = usize::from(NUM_8X8_BLOCKS_HIGH_LOOKUP[self.b.size]);
            for i in 0..bw {
                self.above_seg[self.b.mi_col + i] = u8::from(pred_flag);
            }
            for i in 0..bh {
                self.left_seg[(self.b.mi_row + i) & 7] = u8::from(pred_flag);
            }
        } else {
            let p = self.seg.tree_probs;
            self.b.seg_id = self.tree(&SEGMENT_TREE, &p);
        }
    }

    fn intra_block_mode_info(&mut self) {
        self.b.ref_frame = [INTRA_FRAME, NONE];
        if self.b.size >= BLOCK_8X8 {
            let ctx = usize::from(SIZE_GROUP_LOOKUP[self.b.size]);
            let p = self.probs.y_mode[ctx];
            let m = self.tree(&INTRA_MODE_TREE, &p);
            self.counts.intra_mode[ctx][m] += 1;
            self.b.y_mode = m as u8;
            self.b.sub_modes = [m as u8; 4];
        } else {
            let w4 = usize::from(NUM_4X4_BLOCKS_WIDE_LOOKUP[self.b.size]);
            let h4 = usize::from(NUM_4X4_BLOCKS_HIGH_LOOKUP[self.b.size]);
            let mut m = 0usize;
            let mut idy = 0;
            while idy < 2 {
                let mut idx = 0;
                while idx < 2 {
                    let p = self.probs.y_mode[0];
                    m = self.tree(&INTRA_MODE_TREE, &p);
                    self.counts.intra_mode[0][m] += 1;
                    for y2 in 0..h4 {
                        for x2 in 0..w4 {
                            self.b.sub_modes[(idy + y2) * 2 + idx + x2] = m as u8;
                        }
                    }
                    idx += w4;
                }
                idy += h4;
            }
            self.b.y_mode = m as u8;
        }
        let ctx = usize::from(self.b.y_mode);
        let p = self.probs.uv_mode[ctx];
        let m = self.tree(&INTRA_MODE_TREE, &p);
        self.counts.uv_mode[ctx][m] += 1;
        self.b.uv_mode = m as u8;
    }

    fn inter_block_mode_info(&mut self) -> Result<()> {
        self.read_ref_frames();
        for j in 0..2 {
            if self.b.ref_frame[j] > INTRA_FRAME {
                self.find_mv_refs(self.b.ref_frame[j], -1);
                self.find_best_ref_mvs(j);
            }
        }
        let is_compound = self.b.ref_frame[1] > INTRA_FRAME;
        let size = self.b.size;
        if self.seg_feature_active(SEG_LVL_SKIP) {
            self.b.y_mode = ZEROMV;
            if size < BLOCK_8X8 {
                return Err(bad("segment skip on a block smaller than 8x8"));
            }
        } else if size >= BLOCK_8X8 {
            let m = self.read_inter_mode();
            self.b.y_mode = NEARESTMV + m;
        }
        // interp_filter
        if self.h.interp_filter == SWITCHABLE {
            let (r, c) = (self.b.mi_row, self.b.mi_col);
            let left = if self.b.avail_l && self.b.left_ref[0] > INTRA_FRAME {
                self.mi_at(r, c - 1).interp
            } else {
                3
            };
            let above = if self.b.avail_u && self.b.above_ref[0] > INTRA_FRAME {
                self.mi_at(r - 1, c).interp
            } else {
                3
            };
            let ctx = if left == above {
                left
            } else if left == 3 && above != 3 {
                above
            } else if left != 3 && above == 3 {
                left
            } else {
                3
            } as usize;
            let p = self.probs.interp_filter[ctx];
            let f = self.tree(&INTERP_FILTER_TREE, &p);
            self.counts.interp_filter[ctx][f] += 1;
            self.b.interp = f as u8;
        } else {
            self.b.interp = self.h.interp_filter;
        }
        let nref = 1 + usize::from(is_compound);
        if size < BLOCK_8X8 {
            let w4 = usize::from(NUM_4X4_BLOCKS_WIDE_LOOKUP[size]);
            let h4 = usize::from(NUM_4X4_BLOCKS_HIGH_LOOKUP[size]);
            let mut idy = 0;
            while idy < 2 {
                let mut idx = 0;
                while idx < 2 {
                    let m = self.read_inter_mode();
                    self.b.y_mode = NEARESTMV + m;
                    if self.b.y_mode == NEARESTMV || self.b.y_mode == NEARMV {
                        for j in 0..nref {
                            self.append_sub8x8_mvs(idy * 2 + idx, j);
                        }
                    }
                    let mv = self.assign_mv(is_compound);
                    for y2 in 0..h4 {
                        for x2 in 0..w4 {
                            let block = (idy + y2) * 2 + idx + x2;
                            for rl in 0..nref {
                                self.b.block_mvs[rl][block] = mv[rl];
                            }
                        }
                    }
                    idx += w4;
                }
                idy += h4;
            }
        } else {
            let mv = self.assign_mv(is_compound);
            for rl in 0..nref {
                for block in 0..4 {
                    self.b.block_mvs[rl][block] = mv[rl];
                }
            }
        }
        Ok(())
    }

    fn read_inter_mode(&mut self) -> u8 {
        let ctx = usize::from(self.b.mode_ctx[self.b.ref_frame[0] as usize]);
        let p = self.probs.inter_mode[ctx];
        let m = self.tree(&INTER_MODE_TREE, &p);
        self.counts.inter_mode[ctx][m] += 1;
        m as u8
    }

    fn assign_mv(&mut self, is_compound: bool) -> [Mv; 2] {
        let mut mv = [Mv::default(); 2];
        for i in 0..1 + usize::from(is_compound) {
            mv[i] = match self.b.y_mode {
                NEWMV => self.read_mv(i),
                NEARESTMV => self.b.nearest[i],
                NEARMV => self.b.near[i],
                _ => Mv::default(),
            };
        }
        mv
    }

    fn read_mv(&mut self, r: usize) -> Mv {
        let best = self.b.best[r];
        let use_hp = self.h.allow_hp && mv_hp_ok(best);
        let joint = self.tree(&MV_JOINT_TREE, &self.probs.mv_joint.clone());
        self.counts.mv_joint[joint] += 1;
        let mut diff = [0i32; 2];
        if joint == 2 || joint == 3 {
            diff[0] = self.read_mv_component(0, use_hp);
        }
        if joint == 1 || joint == 3 {
            diff[1] = self.read_mv_component(1, use_hp);
        }
        Mv {
            row: (i32::from(best.row) + diff[0]) as i16,
            col: (i32::from(best.col) + diff[1]) as i16,
        }
    }

    #[allow(clippy::needless_late_init)]
    fn read_mv_component(&mut self, comp: usize, use_hp: bool) -> i32 {
        let sign = self.rd(self.probs.mv_sign[comp]);
        self.counts.mv_sign[comp][usize::from(sign)] += 1;
        let class = self.tree(&MV_CLASS_TREE, &self.probs.mv_class[comp].clone());
        self.counts.mv_class[comp][class] += 1;
        let mag;
        if class == 0 {
            let bit = usize::from(self.rd(self.probs.mv_class0_bit[comp]));
            self.counts.mv_class0_bit[comp][bit] += 1;
            let fr = self.tree(&MV_FR_TREE, &self.probs.mv_class0_fr[comp][bit].clone());
            self.counts.mv_class0_fr[comp][bit][fr] += 1;
            let hp = if use_hp {
                usize::from(self.rd(self.probs.mv_class0_hp[comp]))
            } else {
                1
            };
            self.counts.mv_class0_hp[comp][hp] += 1;
            mag = (((bit << 3) | (fr << 1) | hp) + 1) as i32;
        } else {
            let mut d = 0usize;
            for i in 0..class {
                let bit = usize::from(self.rd(self.probs.mv_bits[comp][i]));
                self.counts.mv_bits[comp][i][bit] += 1;
                d |= bit << i;
            }
            let fr = self.tree(&MV_FR_TREE, &self.probs.mv_fr[comp].clone());
            self.counts.mv_fr[comp][fr] += 1;
            let hp = if use_hp {
                usize::from(self.rd(self.probs.mv_hp[comp]))
            } else {
                1
            };
            self.counts.mv_hp[comp][hp] += 1;
            mag = ((2usize << (class + 2)) + ((d << 3) | (fr << 1) | hp) + 1) as i32;
        }
        if sign {
            -mag
        } else {
            mag
        }
    }

    // ---- reference frame selection ----

    fn read_ref_frames(&mut self) {
        if self.seg_feature_active(SEG_LVL_REF_FRAME) {
            self.b.ref_frame = [self.seg.data[self.b.seg_id][SEG_LVL_REF_FRAME] as i8, NONE];
            return;
        }
        let comp_mode = if self.h.reference_mode == REFERENCE_MODE_SELECT {
            let ctx = self.comp_mode_ctx();
            let v = self.rd(self.probs.comp_mode[ctx]);
            self.counts.comp_mode[ctx][usize::from(v)] += 1;
            u8::from(v)
        } else {
            self.h.reference_mode
        };
        if comp_mode == COMPOUND_REFERENCE {
            let idx = usize::from(self.h.sign_bias[usize::from(self.h.comp_fixed_ref)]);
            let ctx = self.comp_ref_ctx();
            let v = self.rd(self.probs.comp_ref[ctx]);
            self.counts.comp_ref[ctx][usize::from(v)] += 1;
            self.b.ref_frame[idx] = self.h.comp_fixed_ref as i8;
            self.b.ref_frame[1 - idx] = self.h.comp_var_ref[usize::from(v)] as i8;
        } else {
            let ctx = self.single_ref_p1_ctx();
            let p1 = self.rd(self.probs.single_ref[ctx][0]);
            self.counts.single_ref[ctx][0][usize::from(p1)] += 1;
            if p1 {
                let ctx = self.single_ref_p2_ctx();
                let p2 = self.rd(self.probs.single_ref[ctx][1]);
                self.counts.single_ref[ctx][1][usize::from(p2)] += 1;
                self.b.ref_frame[0] = if p2 { ALTREF_FRAME } else { GOLDEN_FRAME };
            } else {
                self.b.ref_frame[0] = LAST_FRAME;
            }
            self.b.ref_frame[1] = NONE;
        }
    }

    fn comp_mode_ctx(&self) -> usize {
        let b = &self.b;
        let fixed = self.h.comp_fixed_ref as i8;
        if b.avail_u && b.avail_l {
            if b.above_single && b.left_single {
                usize::from((b.above_ref[0] == fixed) ^ (b.left_ref[0] == fixed))
            } else if b.above_single {
                2 + usize::from(b.above_ref[0] == fixed || b.above_intra)
            } else if b.left_single {
                2 + usize::from(b.left_ref[0] == fixed || b.left_intra)
            } else {
                4
            }
        } else if b.avail_u {
            if b.above_single {
                usize::from(b.above_ref[0] == fixed)
            } else {
                3
            }
        } else if b.avail_l {
            if b.left_single {
                usize::from(b.left_ref[0] == fixed)
            } else {
                3
            }
        } else {
            1
        }
    }

    fn comp_ref_ctx(&self) -> usize {
        let b = &self.b;
        let fix_ref_idx = usize::from(self.h.sign_bias[usize::from(self.h.comp_fixed_ref)]);
        let var_ref_idx = 1 - fix_ref_idx;
        let fixed = self.h.comp_fixed_ref as i8;
        let var = [self.h.comp_var_ref[0] as i8, self.h.comp_var_ref[1] as i8];
        if b.avail_u && b.avail_l {
            if b.above_intra && b.left_intra {
                2
            } else if b.left_intra {
                if b.above_single {
                    1 + 2 * usize::from(b.above_ref[0] != var[1])
                } else {
                    1 + 2 * usize::from(b.above_ref[var_ref_idx] != var[1])
                }
            } else if b.above_intra {
                if b.left_single {
                    1 + 2 * usize::from(b.left_ref[0] != var[1])
                } else {
                    1 + 2 * usize::from(b.left_ref[var_ref_idx] != var[1])
                }
            } else {
                let vrfa = if b.above_single {
                    b.above_ref[0]
                } else {
                    b.above_ref[var_ref_idx]
                };
                let vrfl = if b.left_single {
                    b.left_ref[0]
                } else {
                    b.left_ref[var_ref_idx]
                };
                if vrfa == vrfl && var[1] == vrfa {
                    0
                } else if b.left_single && b.above_single {
                    if (vrfa == fixed && vrfl == var[0]) || (vrfl == fixed && vrfa == var[0]) {
                        4
                    } else if vrfa == vrfl {
                        3
                    } else {
                        1
                    }
                } else if b.left_single || b.above_single {
                    let vrfc = if b.left_single { vrfa } else { vrfl };
                    let rfs = if b.above_single { vrfa } else { vrfl };
                    if vrfc == var[1] && rfs != var[1] {
                        1
                    } else if rfs == var[1] && vrfc != var[1] {
                        2
                    } else {
                        4
                    }
                } else if vrfa == vrfl {
                    4
                } else {
                    2
                }
            }
        } else if b.avail_u {
            if b.above_intra {
                2
            } else if b.above_single {
                3 * usize::from(b.above_ref[0] != var[1])
            } else {
                4 * usize::from(b.above_ref[var_ref_idx] != var[1])
            }
        } else if b.avail_l {
            if b.left_intra {
                2
            } else if b.left_single {
                3 * usize::from(b.left_ref[0] != var[1])
            } else {
                4 * usize::from(b.left_ref[var_ref_idx] != var[1])
            }
        } else {
            2
        }
    }

    fn single_ref_p1_ctx(&self) -> usize {
        let b = &self.b;
        let last = LAST_FRAME;
        let has_last = |r: [i8; 2]| r[0] == last || r[1] == last;
        if b.avail_u && b.avail_l {
            if b.above_intra && b.left_intra {
                2
            } else if b.left_intra {
                if b.above_single {
                    4 * usize::from(b.above_ref[0] == last)
                } else {
                    1 + usize::from(has_last(b.above_ref))
                }
            } else if b.above_intra {
                if b.left_single {
                    4 * usize::from(b.left_ref[0] == last)
                } else {
                    1 + usize::from(has_last(b.left_ref))
                }
            } else if b.above_single && b.left_single {
                2 * usize::from(b.above_ref[0] == last) + 2 * usize::from(b.left_ref[0] == last)
            } else if !b.above_single && !b.left_single {
                1 + usize::from(has_last(b.above_ref) || has_last(b.left_ref))
            } else {
                let rfs = if b.above_single {
                    b.above_ref[0]
                } else {
                    b.left_ref[0]
                };
                let crf = if b.above_single {
                    b.left_ref
                } else {
                    b.above_ref
                };
                if rfs == last {
                    3 + usize::from(has_last(crf))
                } else {
                    usize::from(has_last(crf))
                }
            }
        } else if b.avail_u {
            if b.above_intra {
                2
            } else if b.above_single {
                4 * usize::from(b.above_ref[0] == last)
            } else {
                1 + usize::from(has_last(b.above_ref))
            }
        } else if b.avail_l {
            if b.left_intra {
                2
            } else if b.left_single {
                4 * usize::from(b.left_ref[0] == last)
            } else {
                1 + usize::from(has_last(b.left_ref))
            }
        } else {
            2
        }
    }

    fn single_ref_p2_ctx(&self) -> usize {
        let b = &self.b;
        let (last, golden, alt) = (LAST_FRAME, GOLDEN_FRAME, ALTREF_FRAME);
        let has_golden = |r: [i8; 2]| r[0] == golden || r[1] == golden;
        if b.avail_u && b.avail_l {
            if b.above_intra && b.left_intra {
                2
            } else if b.left_intra {
                if b.above_single {
                    if b.above_ref[0] == last {
                        3
                    } else {
                        4 * usize::from(b.above_ref[0] == golden)
                    }
                } else {
                    1 + 2 * usize::from(has_golden(b.above_ref))
                }
            } else if b.above_intra {
                if b.left_single {
                    if b.left_ref[0] == last {
                        3
                    } else {
                        4 * usize::from(b.left_ref[0] == golden)
                    }
                } else {
                    1 + 2 * usize::from(has_golden(b.left_ref))
                }
            } else if b.above_single && b.left_single {
                if b.above_ref[0] == last && b.left_ref[0] == last {
                    3
                } else if b.above_ref[0] == last {
                    4 * usize::from(b.left_ref[0] == golden)
                } else if b.left_ref[0] == last {
                    4 * usize::from(b.above_ref[0] == golden)
                } else {
                    2 * usize::from(b.above_ref[0] == golden)
                        + 2 * usize::from(b.left_ref[0] == golden)
                }
            } else if !b.above_single && !b.left_single {
                if b.above_ref[0] == b.left_ref[0] && b.above_ref[1] == b.left_ref[1] {
                    3 * usize::from(has_golden(b.above_ref))
                } else {
                    2
                }
            } else {
                let rfs = if b.above_single {
                    b.above_ref[0]
                } else {
                    b.left_ref[0]
                };
                let crf = if b.above_single {
                    b.left_ref
                } else {
                    b.above_ref
                };
                if rfs == golden {
                    3 + usize::from(has_golden(crf))
                } else if rfs == alt {
                    usize::from(has_golden(crf))
                } else {
                    1 + 2 * usize::from(has_golden(crf))
                }
            }
        } else if b.avail_u {
            if b.above_intra || (b.above_ref[0] == last && b.above_single) {
                2
            } else if b.above_single {
                4 * usize::from(b.above_ref[0] == golden)
            } else {
                3 * usize::from(has_golden(b.above_ref))
            }
        } else if b.avail_l {
            if b.left_intra || (b.left_ref[0] == last && b.left_single) {
                2
            } else if b.left_single {
                4 * usize::from(b.left_ref[0] == golden)
            } else {
                3 * usize::from(has_golden(b.left_ref))
            }
        } else {
            2
        }
    }

    // ---- motion vector prediction ----

    fn is_inside(&self, r: i32, c: i32) -> bool {
        r >= 0
            && (r as usize) < self.h.mi_rows
            && c >= self.col_start as i32
            && (c as usize) < self.col_end
    }

    fn clamp_mv_row(&self, v: i32, border: i32) -> i32 {
        let bh = i32::from(NUM_8X8_BLOCKS_HIGH_LOOKUP[self.b.size]);
        let top = -((self.b.mi_row as i32 * 8) * 8);
        let bottom = ((self.h.mi_rows as i32 - bh - self.b.mi_row as i32) * 8) * 8;
        v.clamp(top - border, bottom + border)
    }

    fn clamp_mv_col(&self, v: i32, border: i32) -> i32 {
        let bw = i32::from(NUM_8X8_BLOCKS_WIDE_LOOKUP[self.b.size]);
        let left = -((self.b.mi_col as i32 * 8) * 8);
        let right = ((self.h.mi_cols as i32 - bw - self.b.mi_col as i32) * 8) * 8;
        v.clamp(left - border, right + border)
    }

    fn find_mv_refs(&mut self, ref_frame: i8, block: i32) {
        let cols = self.h.mi_cols;
        let (mi_row, mi_col) = (self.b.mi_row as i32, self.b.mi_col as i32);
        let search = &MV_REF_BLOCKS[self.b.size];
        let mut list = [Mv::default(); 2];
        let mut count = 0usize;
        let mut different_ref_found = false;
        let mut context_counter = 0usize;
        let add = |list: &mut [Mv; 2], count: &mut usize, mv: Mv| {
            if *count >= 2 {
                return;
            }
            if *count > 0 && mv == list[0] {
                return;
            }
            list[*count] = mv;
            *count += 1;
        };
        for (i, s) in search.iter().enumerate().take(2) {
            let (cr, cc) = (mi_row + s[0], mi_col + s[1]);
            if self.is_inside(cr, cc) {
                let cand = &self.mi[cr as usize * cols + cc as usize];
                different_ref_found = true;
                context_counter += usize::from(MODE_2_COUNTER[usize::from(cand.y_mode)]);
                for j in 0..2 {
                    if cand.ref_frame[j] == ref_frame {
                        let idx = if block >= 0 {
                            IDX_N_COLUMN_TO_SUBBLOCK[block as usize][usize::from(s[1] == 0)]
                        } else {
                            3
                        };
                        add(&mut list, &mut count, cand.mv[j][idx]);
                        break;
                    }
                }
            }
            let _ = i;
        }
        for s in search.iter().skip(2) {
            let (cr, cc) = (mi_row + s[0], mi_col + s[1]);
            if self.is_inside(cr, cc) {
                let cand = &self.mi[cr as usize * cols + cc as usize];
                different_ref_found = true;
                for j in 0..2 {
                    if cand.ref_frame[j] == ref_frame {
                        add(&mut list, &mut count, cand.mv[j][3]);
                        break;
                    }
                }
            }
        }
        let here = (mi_row as usize) * cols + mi_col as usize;
        if self.use_prev_mvs {
            let cand = &self.prev_mi[here];
            for j in 0..2 {
                if cand.ref_frame[j] == ref_frame {
                    add(&mut list, &mut count, cand.mv[j][3]);
                    break;
                }
            }
        }
        let sign_bias = self.h.sign_bias;
        let diff_add = |cand: &Mi, list: &mut [Mv; 2], count: &mut usize| {
            let mvs = [cand.mv[0][3], cand.mv[1][3]];
            let frames = cand.ref_frame;
            let mvs_same = mvs[0] == mvs[1];
            let scale = |k: usize| -> Mv {
                let mut mv = mvs[k];
                if sign_bias[frames[k] as usize] != sign_bias[ref_frame as usize] {
                    mv = Mv {
                        row: mv.row.wrapping_neg(),
                        col: mv.col.wrapping_neg(),
                    };
                }
                mv
            };
            if frames[0] > INTRA_FRAME && frames[0] != ref_frame {
                add(list, count, scale(0));
            }
            if frames[1] > INTRA_FRAME && frames[1] != ref_frame && !mvs_same {
                add(list, count, scale(1));
            }
        };
        if different_ref_found {
            for s in search.iter() {
                let (cr, cc) = (mi_row + s[0], mi_col + s[1]);
                if self.is_inside(cr, cc) {
                    let cand = self.mi[cr as usize * cols + cc as usize];
                    diff_add(&cand, &mut list, &mut count);
                }
            }
        }
        if self.use_prev_mvs {
            let cand = self.prev_mi[here];
            diff_add(&cand, &mut list, &mut count);
        }
        self.b.mode_ctx[ref_frame as usize] = COUNTER_TO_CONTEXT[context_counter];
        for l in list.iter_mut() {
            l.row = self.clamp_mv_row(i32::from(l.row), 128) as i16;
            l.col = self.clamp_mv_col(i32::from(l.col), 128) as i16;
        }
        self.b.ref_list_mv = list;
    }

    fn find_best_ref_mvs(&mut self, ref_list: usize) {
        let mut list = self.b.ref_list_mv;
        for l in list.iter_mut() {
            let mut dr = i32::from(l.row);
            let mut dc = i32::from(l.col);
            if !self.h.allow_hp || !mv_hp_ok(*l) {
                if dr & 1 != 0 {
                    dr += if dr > 0 { -1 } else { 1 };
                }
                if dc & 1 != 0 {
                    dc += if dc > 0 { -1 } else { 1 };
                }
            }
            l.row = self.clamp_mv_row(dr, (160 - 4) << 3) as i16;
            l.col = self.clamp_mv_col(dc, (160 - 4) << 3) as i16;
        }
        self.b.ref_list_mv = list;
        self.b.nearest[ref_list] = list[0];
        self.b.near[ref_list] = list[1];
        self.b.best[ref_list] = list[0];
    }

    fn append_sub8x8_mvs(&mut self, block: usize, ref_list: usize) {
        self.find_mv_refs(self.b.ref_frame[ref_list], block as i32);
        let rl = self.b.ref_list_mv;
        let bm = self.b.block_mvs[ref_list];
        let mut out = [Mv::default(); 2];
        let mut dst = 0usize;
        if block == 0 {
            out = rl;
            dst = 2;
        } else if block <= 2 {
            out[dst] = bm[0];
            dst += 1;
        } else {
            out[dst] = bm[2];
            dst += 1;
            let mut idx = 1i32;
            while idx >= 0 && dst < 2 {
                if bm[idx as usize] != out[0] {
                    out[dst] = bm[idx as usize];
                    dst += 1;
                }
                idx -= 1;
            }
        }
        let mut n = 0;
        while n < 2 && dst < 2 {
            if rl[n] != out[0] {
                out[dst] = rl[n];
                dst += 1;
            }
            n += 1;
        }
        if dst < 2 {
            out[dst] = Mv::default();
        }
        self.b.nearest[ref_list] = out[0];
        self.b.near[ref_list] = out[1];
    }

    // ---- residual, prediction, reconstruction ----

    fn uv_tx_size(&self) -> usize {
        if self.b.size < BLOCK_8X8 {
            return 0;
        }
        let ps = SS_SIZE_LOOKUP[self.b.size][usize::from(self.h.ss_x)][usize::from(self.h.ss_y)];
        let max = MAX_TXSIZE_LOOKUP.get(usize::from(ps)).copied().unwrap_or(0);
        self.b.tx_size.min(usize::from(max))
    }

    fn residual(&mut self) -> Result<()> {
        let bsize = self.b.size.max(BLOCK_8X8);
        for plane in 0..3 {
            let tx_sz = if plane > 0 {
                self.uv_tx_size()
            } else {
                self.b.tx_size
            };
            let step = 1usize << tx_sz;
            let (sub_x, sub_y) = if plane > 0 {
                (usize::from(self.h.ss_x), usize::from(self.h.ss_y))
            } else {
                (0, 0)
            };
            let plane_sz = SS_SIZE_LOOKUP[bsize][sub_x][sub_y];
            if usize::from(plane_sz) >= NUM_4X4_BLOCKS_WIDE_LOOKUP.len() {
                return Err(bad("block size is not valid for this chroma format"));
            }
            let plane_sz = usize::from(plane_sz);
            let num4x4w = usize::from(NUM_4X4_BLOCKS_WIDE_LOOKUP[plane_sz]);
            let num4x4h = usize::from(NUM_4X4_BLOCKS_HIGH_LOOKUP[plane_sz]);
            let base_x = (self.b.mi_col * 8) >> sub_x;
            let base_y = (self.b.mi_row * 8) >> sub_y;
            if self.b.is_inter {
                if self.b.size < BLOCK_8X8 {
                    for y in 0..num4x4h {
                        for x in 0..num4x4w {
                            self.predict_inter(
                                plane,
                                base_x + 4 * x,
                                base_y + 4 * y,
                                4,
                                4,
                                y * num4x4w + x,
                            )?;
                        }
                    }
                } else {
                    self.predict_inter(plane, base_x, base_y, num4x4w * 4, num4x4h * 4, 0)?;
                }
            }
            let max_x = (self.h.mi_cols * 8) >> sub_x;
            let max_y = (self.h.mi_rows * 8) >> sub_y;
            let mut block_idx = 0usize;
            let mut y = 0;
            while y < num4x4h {
                let mut x = 0;
                while x < num4x4w {
                    let start_x = base_x + 4 * x;
                    let start_y = base_y + 4 * y;
                    let mut nonzero = false;
                    if start_x < max_x && start_y < max_y {
                        if !self.b.is_inter {
                            let mode = if plane > 0 {
                                self.b.uv_mode
                            } else if self.b.size >= BLOCK_8X8 {
                                self.b.y_mode
                            } else {
                                self.b.sub_modes[block_idx]
                            };
                            let edge = IntraEdge {
                                plane,
                                x: start_x,
                                y: start_y,
                                have_left: self.b.avail_l || x > 0,
                                have_above: self.b.avail_u || y > 0,
                                not_on_right: x + step < num4x4w,
                                tx_size: tx_sz,
                                max_x: max_x - 1,
                                max_y: max_y - 1,
                            };
                            predict_intra(&mut self.cur, &edge, mode);
                        }
                        if !self.b.skip {
                            let (nz, eob, tx_type) =
                                self.tokens(plane, start_x, start_y, tx_sz, block_idx);
                            nonzero = nz;
                            if eob > 0 {
                                self.reconstruct(plane, start_x, start_y, tx_sz, tx_type);
                            }
                        }
                    }
                    let nz = u8::from(nonzero);
                    for i in 0..step {
                        self.above_nz[plane][(start_x >> 2) + i] = nz;
                        self.left_nz[plane][((start_y >> 2) + i) & 15] = nz;
                    }
                    block_idx += 1;
                    x += step;
                }
                y += step;
            }
        }
        Ok(())
    }

    fn tx_type(&self, plane: usize, tx_sz: usize, block_idx: usize) -> u8 {
        if plane > 0 || tx_sz == 3 {
            DCT_DCT
        } else if tx_sz == 0 {
            if self.h.lossless || self.b.is_inter {
                DCT_DCT
            } else {
                let m = if self.b.size < BLOCK_8X8 {
                    self.b.sub_modes[block_idx]
                } else {
                    self.b.y_mode
                };
                MODE2TXFM_MAP[usize::from(m)]
            }
        } else {
            MODE2TXFM_MAP[usize::from(self.b.y_mode)]
        }
    }

    /// Returns (nonzero, end of block position, transform type).
    fn tokens(
        &mut self,
        plane: usize,
        start_x: usize,
        start_y: usize,
        tx_sz: usize,
        block_idx: usize,
    ) -> (bool, usize, u8) {
        let seg_eob = 16usize << (tx_sz << 1);
        let tx_type = self.tx_type(plane, tx_sz, block_idx);
        let scan: &[u16] = match (tx_sz, tx_type) {
            (0, ADST_DCT) => &ROW_SCAN_4X4,
            (0, DCT_ADST) => &COL_SCAN_4X4,
            (0, _) => &DEFAULT_SCAN_4X4,
            (1, ADST_DCT) => &ROW_SCAN_8X8,
            (1, DCT_ADST) => &COL_SCAN_8X8,
            (1, _) => &DEFAULT_SCAN_8X8,
            (2, ADST_DCT) => &ROW_SCAN_16X16,
            (2, DCT_ADST) => &COL_SCAN_16X16,
            (2, _) => &DEFAULT_SCAN_16X16,
            _ => &DEFAULT_SCAN_32X32,
        };
        let pt = usize::from(plane > 0);
        let rt = usize::from(self.b.is_inter);
        let (sx, sy) = if plane > 0 {
            (usize::from(self.h.ss_x), usize::from(self.h.ss_y))
        } else {
            (0, 0)
        };
        let max_x4 = (2 * self.h.mi_cols) >> sx;
        let max_y4 = (2 * self.h.mi_rows) >> sy;
        let numpts = 1usize << tx_sz;
        let x4 = start_x >> 2;
        let y4 = start_y >> 2;
        let mut above = 0u8;
        let mut left = 0u8;
        for i in 0..numpts {
            if x4 + i < max_x4 {
                above |= self.above_nz[plane][x4 + i];
            }
            if y4 + i < max_y4 {
                left |= self.left_nz[plane][(y4 + i) & 15];
            }
        }
        let mut ctx = usize::from(above + left);
        let dq = self.dq[self.b.seg_id][pt];
        let denom = if tx_sz == 3 { 2 } else { 1 };
        let n = 4usize << tx_sz;
        let log_n = tx_sz + 2;
        let mut check_eob = true;
        let mut c = 0usize;
        while c < seg_eob {
            let pos = usize::from(scan[c]);
            let band = if tx_sz == 0 {
                usize::from(COEFBAND_4X4[c])
            } else {
                usize::from(COEFBAND_8X8PLUS[c])
            };
            if c > 0 {
                let i = pos >> log_n;
                let j = pos & (n - 1);
                let (n0, n1) = if i > 0 && j > 0 {
                    let a = (i - 1) * n + j;
                    let a2 = i * n + j - 1;
                    match tx_type {
                        DCT_ADST => (a, a),
                        ADST_DCT => (a2, a2),
                        _ => (a, a2),
                    }
                } else if i > 0 {
                    ((i - 1) * n + j, (i - 1) * n + j)
                } else {
                    (j - 1, j - 1)
                };
                ctx = (1 + usize::from(self.token_cache[n0]) + usize::from(self.token_cache[n1]))
                    >> 1;
            }
            let p = self.probs.coef[tx_sz][pt][rt][band][ctx];
            if check_eob {
                let more = self.rd(p[0]);
                self.counts.more_coefs[tx_sz][pt][rt][band][ctx][usize::from(more)] += 1;
                if !more {
                    break;
                }
            }
            let token = if !self.rd(p[1]) {
                0
            } else if !self.rd(p[2]) {
                1
            } else {
                let x = usize::from((p[2] - 1) / 2);
                let mut row = PARETO_TABLE[x];
                if p[2] & 1 == 0 {
                    let next = PARETO_TABLE[x + 1];
                    for k in 0..8 {
                        row[k] = ((u16::from(row[k]) + u16::from(next[k])) >> 1) as u8;
                    }
                }
                let mut nidx = 4usize;
                loop {
                    let bit = usize::from(self.rd(row[(nidx >> 1) - 2]));
                    let next = TOKEN_TREE[nidx + bit];
                    if next <= 0 {
                        break (-next) as usize;
                    }
                    nidx = next as usize;
                }
            };
            self.counts.token[tx_sz][pt][rt][band][ctx][token.min(2)] += 1;
            self.token_cache[pos] = ENERGY_CLASS[token];
            if token == 0 {
                check_eob = false;
            } else {
                let coef = self.read_coef(token);
                let sign = self.rd(128);
                let q = if pos == 0 { dq[0] } else { dq[1] };
                let v = (i64::from(coef) * i64::from(q) / denom) as i32;
                self.coefs[pos] = if sign { -v } else { v };
                check_eob = true;
            }
            c += 1;
        }
        let nonzero = c > 0;
        if nonzero {
            self.eob_total += 1;
        }
        (nonzero, c, tx_type)
    }

    fn read_coef(&mut self, token: usize) -> i32 {
        let cat = EXTRA_BITS[token][0] as usize;
        let num_extra = EXTRA_BITS[token][1] as usize;
        let mut coef = EXTRA_BITS[token][2];
        if token == 10 {
            let bdp = u32::from(self.h.bit_depth);
            for e in 0..bdp - 8 {
                let hb = i32::from(self.rd(255));
                coef += hb << (5 + bdp - e);
            }
        }
        for e in 0..num_extra {
            let bit = i32::from(self.rd(CAT_PROBS[cat][e]));
            coef += bit << (num_extra - 1 - e);
        }
        coef
    }

    fn reconstruct(&mut self, plane: usize, x: usize, y: usize, tx_sz: usize, tx_type: u8) {
        let n = tx_sz as u32 + 2;
        let n0 = 1usize << n;
        let block = &mut self.coefs[..n0 * n0];
        inverse_2d(block, n, tx_type, self.h.lossless);
        let max = (1i32 << self.h.bit_depth) - 1;
        let stride = self.cur.stride[plane];
        let buf = &mut self.cur.planes[plane];
        for i in 0..n0 {
            let row = (y + i) * stride + x;
            for j in 0..n0 {
                let v = i32::from(buf[row + j]) + block[i * n0 + j];
                buf[row + j] = v.clamp(0, max) as u16;
                block[i * n0 + j] = 0;
            }
        }
    }

    fn predict_inter(
        &mut self,
        plane: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        block_idx: usize,
    ) -> Result<()> {
        let is_compound = self.b.ref_frame[1] > INTRA_FRAME;
        let (sx, sy) = if plane > 0 {
            (i32::from(self.h.ss_x), i32::from(self.h.ss_y))
        } else {
            (0, 0)
        };
        for rl in 0..1 + usize::from(is_compound) {
            // motion vector selection
            let bm = &self.b.block_mvs[rl];
            let mv: [i32; 2] = if plane == 0 || self.b.size >= BLOCK_8X8 || (sx == 0 && sy == 0) {
                [i32::from(bm[block_idx].row), i32::from(bm[block_idx].col)]
            } else if sx == 0 && sy == 1 {
                let q2 = |a: i16, b: i16| {
                    let v = i32::from(a) + i32::from(b);
                    (if v < 0 { v - 1 } else { v + 1 }) / 2
                };
                [
                    q2(bm[block_idx].row, bm[block_idx + 2].row),
                    q2(bm[block_idx].col, bm[block_idx + 2].col),
                ]
            } else if sx == 1 && sy == 0 {
                let q2 = |a: i16, b: i16| {
                    let v = i32::from(a) + i32::from(b);
                    (if v < 0 { v - 1 } else { v + 1 }) / 2
                };
                [
                    q2(bm[block_idx].row, bm[block_idx + 1].row),
                    q2(bm[block_idx].col, bm[block_idx + 1].col),
                ]
            } else {
                let q4 = |k: fn(&Mv) -> i16| {
                    let v: i32 = bm.iter().map(|m| i32::from(k(m))).sum();
                    (if v < 0 { v - 2 } else { v + 2 }) / 4
                };
                [q4(|m| m.row), q4(|m| m.col)]
            };
            // clamping
            let bh = i32::from(NUM_8X8_BLOCKS_HIGH_LOOKUP[self.b.size]);
            let bw = i32::from(NUM_8X8_BLOCKS_WIDE_LOOKUP[self.b.size]);
            let (mi_row, mi_col) = (self.b.mi_row as i32, self.b.mi_col as i32);
            let to_top = -((mi_row * 8) * 16) >> sy;
            let to_bottom = (((self.h.mi_rows as i32 - bh - mi_row) * 8) * 16) >> sy;
            let to_left = -((mi_col * 8) * 16) >> sx;
            let to_right = (((self.h.mi_cols as i32 - bw - mi_col) * 8) * 16) >> sx;
            let spel_left = (4 + ((bw * 8) >> sx)) << 4;
            let spel_right = spel_left - 16;
            let spel_top = (4 + ((bh * 8) >> sy)) << 4;
            let spel_bottom = spel_top - 16;
            let cmv = [
                ((2 * mv[0]) >> sy).clamp(to_top - spel_top, to_bottom + spel_bottom),
                ((2 * mv[1]) >> sx).clamp(to_left - spel_left, to_right + spel_right),
            ];
            // scaling
            let ref_slot = (self.b.ref_frame[rl] - 1) as usize;
            let (xs, ys) = self.ref_scale[ref_slot];
            if xs == 0 || ys == 0 {
                return Err(bad("reference frame has an unusable size"));
            }
            let rf = self.refs[ref_slot]
                .clone()
                .ok_or_else(|| bad("missing reference frame"))?;
            let base_x = ((x as i64 * xs) >> 14) as i32;
            let base_y = ((y as i64 * ys) >> 14) as i32;
            let luma_x = if plane > 0 {
                (x as i64) << sx
            } else {
                x as i64
            };
            let luma_y = if plane > 0 {
                (y as i64) << sy
            } else {
                y as i64
            };
            let frac_x = (((16 * luma_x * xs) >> 14) & 15) as i32;
            let frac_y = (((16 * luma_y * ys) >> 14) & 15) as i32;
            let dx = ((i64::from(cmv[1]) * xs) >> 14) as i32 + frac_x;
            let dy = ((i64::from(cmv[0]) * ys) >> 14) as i32 + frac_y;
            let step_x = ((16 * xs) >> 14) as i32;
            let step_y = ((16 * ys) >> 14) as i32;
            let start_x = (base_x << 4) + dx;
            let start_y = (base_y << 4) + dy;
            let mut out = std::mem::take(&mut self.pred_buf[rl]);
            predict_inter_block(
                &rf,
                plane,
                start_x,
                start_y,
                step_x,
                step_y,
                w,
                h,
                usize::from(self.b.interp),
                &mut out,
                &mut self.scratch,
            );
            self.pred_buf[rl] = out;
        }
        let stride = self.cur.stride[plane];
        let buf = &mut self.cur.planes[plane];
        for i in 0..h {
            let row = (y + i) * stride + x;
            for j in 0..w {
                buf[row + j] = if is_compound {
                    ((u32::from(self.pred_buf[0][i * w + j])
                        + u32::from(self.pred_buf[1][i * w + j])
                        + 1)
                        >> 1) as u16
                } else {
                    self.pred_buf[0][i * w + j]
                };
            }
        }
        Ok(())
    }
}

fn tile_offset(num: usize, mis: usize, log2: u32) -> usize {
    let sbs = (mis + 7) >> 3;
    let offset = ((num * sbs) >> log2) << 3;
    offset.min(mis)
}
