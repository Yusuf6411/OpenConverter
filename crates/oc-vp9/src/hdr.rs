//! The uncompressed and compressed frame headers (spec 6.2, 6.3).
use crate::boolcoder::BoolDecoder;
use crate::frame::FrameBuf;
use crate::probs::Probs;
use crate::tables::*;
use oc_core::{Error, Result};
use std::sync::Arc;

pub const KEY_FRAME: u8 = 0;
pub const SWITCHABLE: u8 = 4;
pub const TX_MODE_SELECT: u8 = 4;
pub const SINGLE_REFERENCE: u8 = 0;
pub const COMPOUND_REFERENCE: u8 = 1;
pub const REFERENCE_MODE_SELECT: u8 = 2;
pub const SEG_LVL_ALT_Q: usize = 0;
pub const SEG_LVL_ALT_L: usize = 1;
pub const SEG_LVL_REF_FRAME: usize = 2;
pub const SEG_LVL_SKIP: usize = 3;

fn bad(msg: &str) -> Error {
    Error::corrupt(format!("VP9: {msg}"))
}

/// State that outlives a single frame and is edited by frame headers.
pub struct Persist {
    pub refs: [Option<Arc<FrameBuf>>; 8],
    pub saved: [Probs; 4],
    pub lf_ref_deltas: [i8; 4],
    pub lf_mode_deltas: [i8; 2],
    pub feature_enabled: [[bool; 4]; 8],
    pub feature_data: [[i16; 4]; 8],
    pub seg_abs: bool,
    pub seg_tree_probs: [u8; 7],
    pub seg_pred_probs: [u8; 3],
    pub bit_depth: u8,
    pub ss_x: u8,
    pub ss_y: u8,
    pub last_frame_type: u8,
    pub frame_type: u8,
    pub last_size: Option<(usize, usize)>,
    pub last_show_frame: bool,
}

impl Default for Persist {
    fn default() -> Self {
        Self {
            refs: Default::default(),
            saved: [
                Probs::default(),
                Probs::default(),
                Probs::default(),
                Probs::default(),
            ],
            lf_ref_deltas: [1, 0, -1, -1],
            lf_mode_deltas: [0, 0],
            feature_enabled: [[false; 4]; 8],
            feature_data: [[0; 4]; 8],
            seg_abs: false,
            seg_tree_probs: [255; 7],
            seg_pred_probs: [255; 3],
            bit_depth: 8,
            ss_x: 1,
            ss_y: 1,
            last_frame_type: KEY_FRAME,
            frame_type: KEY_FRAME,
            last_size: None,
            last_show_frame: false,
        }
    }
}

/// Everything a frame header says about the current frame.
#[derive(Clone, Default)]
pub struct Hdr {
    pub profile: u8,
    pub show_existing: bool,
    pub frame_to_show: usize,
    pub frame_type: u8,
    pub show_frame: bool,
    pub error_res: bool,
    pub intra_only: bool,
    pub frame_is_intra: bool,
    pub reset_frame_context: u8,
    pub bit_depth: u8,
    pub ss_x: u8,
    pub ss_y: u8,
    pub width: usize,
    pub height: usize,
    pub refresh_flags: u8,
    pub ref_idx: [usize; 3],
    pub sign_bias: [bool; 4],
    pub allow_hp: bool,
    pub interp_filter: u8,
    pub refresh_ctx: bool,
    pub parallel: bool,
    pub ctx_idx: usize,
    pub lf_level: u8,
    pub lf_sharp: u8,
    pub lf_delta_enabled: bool,
    pub lf_delta_update: bool,
    pub base_q: i32,
    pub dq_y_dc: i32,
    pub dq_uv_dc: i32,
    pub dq_uv_ac: i32,
    pub lossless: bool,
    pub seg_enabled: bool,
    pub seg_update_map: bool,
    pub seg_temporal: bool,
    pub tile_cols_log2: u32,
    pub tile_rows_log2: u32,
    pub header_size: usize,
    pub tx_mode: u8,
    pub reference_mode: u8,
    pub comp_fixed_ref: u8,
    pub comp_var_ref: [u8; 2],
    pub mi_cols: usize,
    pub mi_rows: usize,
    pub sb_cols: usize,
    pub sb_rows: usize,
    /// Bytes taken by the uncompressed header, trailing bits included.
    pub uncompressed_len: usize,
    /// Whether this frame resets the past (`setup_past_independence` ran).
    pub past_independent: bool,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u32> {
        let byte = self
            .data
            .get(self.pos >> 3)
            .ok_or_else(|| Error::truncated("VP9 frame header ended early"))?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(u32::from(b))
    }
    fn f(&mut self, n: u32) -> Result<u32> {
        let mut x = 0;
        for _ in 0..n {
            x = (x << 1) | self.bit()?;
        }
        Ok(x)
    }
    fn flag(&mut self) -> Result<bool> {
        Ok(self.bit()? == 1)
    }
    /// `s(n)`: magnitude then sign.
    fn s(&mut self, n: u32) -> Result<i32> {
        let v = self.f(n)? as i32;
        Ok(if self.flag()? { -v } else { v })
    }
}

fn sync_code(b: &mut Bits) -> Result<()> {
    if b.f(8)? != 0x49 || b.f(8)? != 0x83 || b.f(8)? != 0x42 {
        return Err(bad("bad frame sync code"));
    }
    Ok(())
}

fn color_config(b: &mut Bits, h: &mut Hdr) -> Result<()> {
    h.bit_depth = if h.profile >= 2 {
        if b.flag()? {
            12
        } else {
            10
        }
    } else {
        8
    };
    let color_space = b.f(3)?;
    if color_space != 7 {
        b.bit()?; // color_range
        if h.profile == 1 || h.profile == 3 {
            h.ss_x = b.f(1)? as u8;
            h.ss_y = b.f(1)? as u8;
            if b.bit()? != 0 {
                return Err(bad("reserved bit is set"));
            }
        } else {
            h.ss_x = 1;
            h.ss_y = 1;
        }
    } else if h.profile == 1 || h.profile == 3 {
        h.ss_x = 0;
        h.ss_y = 0;
        if b.bit()? != 0 {
            return Err(bad("reserved bit is set"));
        }
    } else {
        return Err(bad("RGB is not allowed in profiles 0 and 2"));
    }
    Ok(())
}

fn frame_size(b: &mut Bits, h: &mut Hdr) -> Result<()> {
    h.width = b.f(16)? as usize + 1;
    h.height = b.f(16)? as usize + 1;
    Ok(())
}

fn render_size(b: &mut Bits) -> Result<()> {
    if b.flag()? {
        b.f(32)?;
    }
    Ok(())
}

fn compute_image_size(h: &mut Hdr) {
    h.mi_cols = (h.width + 7) >> 3;
    h.mi_rows = (h.height + 7) >> 3;
    h.sb_cols = (h.mi_cols + 7) >> 3;
    h.sb_rows = (h.mi_rows + 7) >> 3;
}

/// Parse the uncompressed header of `data` and apply `setup_past_independence` to `p`.
pub fn parse_uncompressed(data: &[u8], p: &mut Persist) -> Result<Hdr> {
    let mut b = Bits { data, pos: 0 };
    let mut h = Hdr::default();
    if b.f(2)? != 2 {
        return Err(bad("bad frame marker"));
    }
    let lo = b.f(1)?;
    let hi = b.f(1)?;
    h.profile = ((hi << 1) | lo) as u8;
    if h.profile == 3 && b.bit()? != 0 {
        return Err(bad("reserved bit is set"));
    }
    h.show_existing = b.flag()?;
    if h.show_existing {
        h.frame_to_show = b.f(3)? as usize;
        return Ok(h);
    }
    p.last_frame_type = p.frame_type;
    h.frame_type = b.f(1)? as u8;
    p.frame_type = h.frame_type;
    h.show_frame = b.flag()?;
    h.error_res = b.flag()?;
    h.bit_depth = p.bit_depth;
    h.ss_x = p.ss_x;
    h.ss_y = p.ss_y;
    if h.frame_type == KEY_FRAME {
        sync_code(&mut b)?;
        color_config(&mut b, &mut h)?;
        frame_size(&mut b, &mut h)?;
        render_size(&mut b)?;
        h.refresh_flags = 0xff;
        h.frame_is_intra = true;
    } else {
        h.intra_only = if h.show_frame { false } else { b.flag()? };
        h.frame_is_intra = h.intra_only;
        h.reset_frame_context = if h.error_res { 0 } else { b.f(2)? as u8 };
        if h.intra_only {
            sync_code(&mut b)?;
            if h.profile > 0 {
                color_config(&mut b, &mut h)?;
            } else {
                h.bit_depth = 8;
                h.ss_x = 1;
                h.ss_y = 1;
            }
            h.refresh_flags = b.f(8)? as u8;
            frame_size(&mut b, &mut h)?;
            render_size(&mut b)?;
        } else {
            h.refresh_flags = b.f(8)? as u8;
            for i in 0..3 {
                h.ref_idx[i] = b.f(3)? as usize;
                h.sign_bias[1 + i] = b.flag()?;
            }
            let mut found = false;
            for i in 0..3 {
                if b.flag()? {
                    let r = p.refs[h.ref_idx[i]]
                        .as_ref()
                        .ok_or_else(|| bad("missing reference frame"))?;
                    h.width = r.w;
                    h.height = r.h;
                    found = true;
                    break;
                }
            }
            if !found {
                frame_size(&mut b, &mut h)?;
            }
            render_size(&mut b)?;
            h.allow_hp = b.flag()?;
            h.interp_filter = if b.flag()? {
                SWITCHABLE
            } else {
                [1, 0, 2, 3][b.f(2)? as usize]
            };
        }
    }
    compute_image_size(&mut h);
    p.bit_depth = h.bit_depth;
    p.ss_x = h.ss_x;
    p.ss_y = h.ss_y;
    if h.width == 0 || h.height == 0 {
        return Err(bad("empty frame"));
    }
    if h.error_res {
        h.refresh_ctx = false;
        h.parallel = true;
    } else {
        h.refresh_ctx = b.flag()?;
        h.parallel = b.flag()?;
    }
    h.ctx_idx = b.f(2)? as usize;
    if h.frame_is_intra || h.error_res {
        h.past_independent = true;
        p.feature_enabled = [[false; 4]; 8];
        p.feature_data = [[0; 4]; 8];
        p.seg_abs = false;
        p.lf_ref_deltas = [1, 0, -1, -1];
        p.lf_mode_deltas = [0, 0];
        let defaults = Probs::default();
        if h.frame_type == KEY_FRAME || h.error_res || h.reset_frame_context == 3 {
            for s in &mut p.saved {
                *s = defaults.clone();
            }
        } else if h.reset_frame_context == 2 {
            p.saved[h.ctx_idx] = defaults.clone();
        }
        h.ctx_idx = 0;
    }

    // loop_filter_params
    h.lf_level = b.f(6)? as u8;
    h.lf_sharp = b.f(3)? as u8;
    h.lf_delta_enabled = b.flag()?;
    if h.lf_delta_enabled {
        h.lf_delta_update = b.flag()?;
        if h.lf_delta_update {
            for i in 0..4 {
                if b.flag()? {
                    p.lf_ref_deltas[i] = b.s(6)? as i8;
                }
            }
            for i in 0..2 {
                if b.flag()? {
                    p.lf_mode_deltas[i] = b.s(6)? as i8;
                }
            }
        }
    }

    // quantization_params
    h.base_q = b.f(8)? as i32;
    let delta = |b: &mut Bits| -> Result<i32> {
        if b.flag()? {
            b.s(4)
        } else {
            Ok(0)
        }
    };
    h.dq_y_dc = delta(&mut b)?;
    h.dq_uv_dc = delta(&mut b)?;
    h.dq_uv_ac = delta(&mut b)?;
    h.lossless = h.base_q == 0 && h.dq_y_dc == 0 && h.dq_uv_dc == 0 && h.dq_uv_ac == 0;

    // segmentation_params
    h.seg_enabled = b.flag()?;
    if h.seg_enabled {
        h.seg_update_map = b.flag()?;
        if h.seg_update_map {
            let prob = |b: &mut Bits| -> Result<u8> {
                if b.flag()? {
                    Ok(b.f(8)? as u8)
                } else {
                    Ok(255)
                }
            };
            for i in 0..7 {
                p.seg_tree_probs[i] = prob(&mut b)?;
            }
            h.seg_temporal = b.flag()?;
            for i in 0..3 {
                p.seg_pred_probs[i] = if h.seg_temporal { prob(&mut b)? } else { 255 };
            }
        }
        if b.flag()? {
            p.seg_abs = b.flag()?;
            const BITS: [u32; 4] = [8, 6, 2, 0];
            const SIGNED: [bool; 4] = [true, true, false, false];
            for i in 0..8 {
                for j in 0..4 {
                    let mut v = 0i32;
                    let en = b.flag()?;
                    p.feature_enabled[i][j] = en;
                    if en {
                        v = b.f(BITS[j])? as i32;
                        if SIGNED[j] && b.flag()? {
                            v = -v;
                        }
                    }
                    p.feature_data[i][j] = v as i16;
                }
            }
        }
    }

    // tile_info
    let mut min_log2 = 0;
    while (64usize << min_log2) < h.sb_cols {
        min_log2 += 1;
    }
    let mut max_log2 = 1;
    while (h.sb_cols >> max_log2) >= 4 {
        max_log2 += 1;
    }
    max_log2 -= 1;
    h.tile_cols_log2 = min_log2;
    while h.tile_cols_log2 < max_log2 {
        if b.flag()? {
            h.tile_cols_log2 += 1;
        } else {
            break;
        }
    }
    h.tile_rows_log2 = b.f(1)?;
    if h.tile_rows_log2 == 1 {
        h.tile_rows_log2 += b.f(1)?;
    }
    h.header_size = b.f(16)? as usize;
    h.uncompressed_len = b.pos.div_ceil(8);
    if h.uncompressed_len > data.len() {
        return Err(Error::truncated("VP9 frame header ended early"));
    }
    Ok(h)
}

fn diff_update_prob(bd: &mut BoolDecoder, prob: &mut u8) -> Result<()> {
    if bd.read(252) {
        let delta = decode_term_subexp(bd) as usize;
        if delta >= INV_MAP_TABLE.len() {
            return Err(bad("probability update is out of range"));
        }
        *prob = inv_remap_prob(delta, *prob);
    }
    Ok(())
}

fn decode_term_subexp(bd: &mut BoolDecoder) -> u32 {
    if bd.literal(1) == 0 {
        return bd.literal(4);
    }
    if bd.literal(1) == 0 {
        return bd.literal(4) + 16;
    }
    if bd.literal(1) == 0 {
        return bd.literal(5) + 32;
    }
    let v = bd.literal(7);
    if v < 65 {
        return v + 64;
    }
    let bit = bd.literal(1);
    (v << 1) - 1 + bit
}

fn inv_recenter_nonneg(v: i32, m: i32) -> i32 {
    if v > 2 * m {
        v
    } else if v & 1 == 1 {
        m - ((v + 1) >> 1)
    } else {
        m + (v >> 1)
    }
}

fn inv_remap_prob(delta: usize, prob: u8) -> u8 {
    let v = i32::from(INV_MAP_TABLE[delta]);
    let m = i32::from(prob) - 1;
    let r = if (m << 1) <= 255 {
        1 + inv_recenter_nonneg(v, m)
    } else {
        255 - inv_recenter_nonneg(v, 255 - 1 - m)
    };
    r.clamp(1, 255) as u8
}

fn update_mv_prob(bd: &mut BoolDecoder, prob: &mut u8) {
    if bd.read(252) {
        *prob = ((bd.literal(7) << 1) | 1) as u8;
    }
}

/// Parse the compressed header into `probs`, filling in `tx_mode` and the reference mode of `h`.
pub fn parse_compressed(data: &[u8], h: &mut Hdr, probs: &mut Probs) -> Result<()> {
    let mut bd = BoolDecoder::new(data).ok_or_else(|| bad("bad compressed header"))?;
    let bd = &mut bd;
    // read_tx_mode
    if h.lossless {
        h.tx_mode = 0;
    } else {
        h.tx_mode = bd.literal(2) as u8;
        if h.tx_mode == 3 {
            h.tx_mode += bd.literal(1) as u8;
        }
    }
    if h.tx_mode == TX_MODE_SELECT {
        for i in 0..2 {
            diff_update_prob(bd, &mut probs.tx[1][i][0])?;
        }
        for i in 0..2 {
            for j in 0..2 {
                diff_update_prob(bd, &mut probs.tx[2][i][j])?;
            }
        }
        for i in 0..2 {
            for j in 0..3 {
                diff_update_prob(bd, &mut probs.tx[3][i][j])?;
            }
        }
    }
    // read_coef_probs
    let max_tx = TX_MODE_TO_BIGGEST_TX_SIZE[usize::from(h.tx_mode)] as usize;
    for tx in 0..=max_tx {
        if bd.literal(1) == 1 {
            for i in 0..2 {
                for j in 0..2 {
                    for k in 0..6 {
                        let max_l = if k == 0 { 3 } else { 6 };
                        for l in 0..max_l {
                            for m in 0..3 {
                                diff_update_prob(bd, &mut probs.coef[tx][i][j][k][l][m])?;
                            }
                        }
                    }
                }
            }
        }
    }
    for i in 0..3 {
        diff_update_prob(bd, &mut probs.skip[i])?;
    }
    if !h.frame_is_intra {
        for i in 0..7 {
            for j in 0..3 {
                diff_update_prob(bd, &mut probs.inter_mode[i][j])?;
            }
        }
        if h.interp_filter == SWITCHABLE {
            for j in 0..4 {
                for i in 0..2 {
                    diff_update_prob(bd, &mut probs.interp_filter[j][i])?;
                }
            }
        }
        for i in 0..4 {
            diff_update_prob(bd, &mut probs.is_inter[i])?;
        }
        // frame_reference_mode
        let allowed = (1..3).any(|i| h.sign_bias[i + 1] != h.sign_bias[1]);
        h.reference_mode = SINGLE_REFERENCE;
        if allowed && bd.literal(1) == 1 {
            h.reference_mode = if bd.literal(1) == 0 {
                COMPOUND_REFERENCE
            } else {
                REFERENCE_MODE_SELECT
            };
        }
        // setup_compound_reference_mode
        if h.sign_bias[1] == h.sign_bias[2] {
            h.comp_fixed_ref = 3;
            h.comp_var_ref = [1, 2];
        } else if h.sign_bias[1] == h.sign_bias[3] {
            h.comp_fixed_ref = 2;
            h.comp_var_ref = [1, 3];
        } else {
            h.comp_fixed_ref = 1;
            h.comp_var_ref = [2, 3];
        }
        if h.reference_mode == REFERENCE_MODE_SELECT {
            for i in 0..5 {
                diff_update_prob(bd, &mut probs.comp_mode[i])?;
            }
        }
        if h.reference_mode != COMPOUND_REFERENCE {
            for i in 0..5 {
                diff_update_prob(bd, &mut probs.single_ref[i][0])?;
                diff_update_prob(bd, &mut probs.single_ref[i][1])?;
            }
        }
        if h.reference_mode != SINGLE_REFERENCE {
            for i in 0..5 {
                diff_update_prob(bd, &mut probs.comp_ref[i])?;
            }
        }
        for i in 0..4 {
            for j in 0..9 {
                diff_update_prob(bd, &mut probs.y_mode[i][j])?;
            }
        }
        for i in 0..16 {
            for j in 0..3 {
                diff_update_prob(bd, &mut probs.partition[i][j])?;
            }
        }
        // mv_probs
        for j in 0..3 {
            update_mv_prob(bd, &mut probs.mv_joint[j]);
        }
        for i in 0..2 {
            update_mv_prob(bd, &mut probs.mv_sign[i]);
            for j in 0..10 {
                update_mv_prob(bd, &mut probs.mv_class[i][j]);
            }
            update_mv_prob(bd, &mut probs.mv_class0_bit[i]);
            for j in 0..10 {
                update_mv_prob(bd, &mut probs.mv_bits[i][j]);
            }
        }
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..3 {
                    update_mv_prob(bd, &mut probs.mv_class0_fr[i][j][k]);
                }
            }
            for k in 0..3 {
                update_mv_prob(bd, &mut probs.mv_fr[i][k]);
            }
        }
        if h.allow_hp {
            for i in 0..2 {
                update_mv_prob(bd, &mut probs.mv_class0_hp[i]);
                update_mv_prob(bd, &mut probs.mv_hp[i]);
            }
        }
    }
    Ok(())
}
