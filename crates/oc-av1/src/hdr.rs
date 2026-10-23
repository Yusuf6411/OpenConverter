//! Sequence header and frame header parsing (spec sections 5.5 and 5.9).
use crate::bits::Bits;
use crate::tables;
use oc_core::{Error, Result};

pub const KEY_FRAME: u8 = 0;
pub const INTER_FRAME: u8 = 1;
pub const INTRA_ONLY_FRAME: u8 = 2;
pub const SWITCH_FRAME: u8 = 3;

pub const NUM_REF_FRAMES: usize = 8;
pub const REFS_PER_FRAME: usize = 7;
pub const PRIMARY_REF_NONE: u8 = 7;
pub const WARPEDMODEL_PREC_BITS: u32 = 16;

pub const IDENTITY: u8 = 0;
pub const TRANSLATION: u8 = 1;
pub const ROTZOOM: u8 = 2;
pub const AFFINE: u8 = 3;

pub const RESTORE_NONE: u8 = 0;
pub const RESTORE_SWITCHABLE: u8 = 3;

pub const ONLY_4X4: u8 = 0;
pub const TX_MODE_LARGEST: u8 = 1;
pub const TX_MODE_SELECT: u8 = 2;

pub const SWITCHABLE: u8 = 4;

#[derive(Clone, Default, Debug)]
pub struct SeqHdr {
    pub profile: u8,
    pub still_picture: bool,
    pub reduced_still_picture_header: bool,
    pub timing_info_present: bool,
    pub decoder_model_info_present: bool,
    pub equal_picture_interval: bool,
    pub buffer_delay_length: u32,
    pub buffer_removal_time_length: u32,
    pub frame_presentation_time_length: u32,
    pub operating_points: usize,
    pub operating_point_idc: [u32; 32],
    pub decoder_model_present_for_op: [bool; 32],
    pub operating_point: usize,
    pub frame_width_bits: u32,
    pub frame_height_bits: u32,
    pub max_frame_width: u32,
    pub max_frame_height: u32,
    pub frame_id_numbers_present: bool,
    pub delta_frame_id_length: u32,
    pub additional_frame_id_length: u32,
    pub use_128x128_superblock: bool,
    pub enable_filter_intra: bool,
    pub enable_intra_edge_filter: bool,
    pub enable_interintra_compound: bool,
    pub enable_masked_compound: bool,
    pub enable_warped_motion: bool,
    pub enable_dual_filter: bool,
    pub enable_order_hint: bool,
    pub enable_jnt_comp: bool,
    pub enable_ref_frame_mvs: bool,
    pub seq_force_screen_content_tools: u8,
    pub seq_force_integer_mv: u8,
    pub order_hint_bits: u32,
    pub enable_superres: bool,
    pub enable_cdef: bool,
    pub enable_restoration: bool,
    pub bit_depth: u32,
    pub mono_chrome: bool,
    pub num_planes: usize,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub color_range: bool,
    pub subsampling_x: u32,
    pub subsampling_y: u32,
    pub separate_uv_delta_q: bool,
    pub film_grain_params_present: bool,
}

impl SeqHdr {
    pub fn parse(b: &mut Bits) -> Result<SeqHdr> {
        let mut s = SeqHdr {
            profile: b.f(3)? as u8,
            ..Default::default()
        };
        if s.profile > 2 {
            return Err(Error::corrupt("AV1: reserved profile"));
        }
        s.still_picture = b.flag()?;
        s.reduced_still_picture_header = b.flag()?;
        if s.reduced_still_picture_header {
            s.operating_points = 1;
            b.f(5)?; // seq_level_idx[0]
        } else {
            s.timing_info_present = b.flag()?;
            if s.timing_info_present {
                b.f(32)?;
                b.f(32)?;
                s.equal_picture_interval = b.flag()?;
                if s.equal_picture_interval {
                    b.uvlc()?;
                }
                s.decoder_model_info_present = b.flag()?;
                if s.decoder_model_info_present {
                    s.buffer_delay_length = b.f(5)? + 1;
                    b.f(32)?;
                    s.buffer_removal_time_length = b.f(5)? + 1;
                    s.frame_presentation_time_length = b.f(5)? + 1;
                }
            }
            let initial_display_delay_present = b.flag()?;
            s.operating_points = b.f(5)? as usize + 1;
            for i in 0..s.operating_points {
                s.operating_point_idc[i] = b.f(12)?;
                let level = b.f(5)?;
                if level > 7 {
                    b.f(1)?;
                }
                if s.decoder_model_info_present {
                    s.decoder_model_present_for_op[i] = b.flag()?;
                    if s.decoder_model_present_for_op[i] {
                        b.f(s.buffer_delay_length)?;
                        b.f(s.buffer_delay_length)?;
                        b.f(1)?;
                    }
                }
                if initial_display_delay_present && b.flag()? {
                    b.f(4)?;
                }
            }
        }
        s.frame_width_bits = b.f(4)? + 1;
        s.frame_height_bits = b.f(4)? + 1;
        s.max_frame_width = b.f(s.frame_width_bits)? + 1;
        s.max_frame_height = b.f(s.frame_height_bits)? + 1;
        if !s.reduced_still_picture_header {
            s.frame_id_numbers_present = b.flag()?;
        }
        if s.frame_id_numbers_present {
            s.delta_frame_id_length = b.f(4)? + 2;
            s.additional_frame_id_length = b.f(3)? + 1;
        }
        s.use_128x128_superblock = b.flag()?;
        s.enable_filter_intra = b.flag()?;
        s.enable_intra_edge_filter = b.flag()?;
        s.seq_force_screen_content_tools = 2;
        s.seq_force_integer_mv = 2;
        if !s.reduced_still_picture_header {
            s.enable_interintra_compound = b.flag()?;
            s.enable_masked_compound = b.flag()?;
            s.enable_warped_motion = b.flag()?;
            s.enable_dual_filter = b.flag()?;
            s.enable_order_hint = b.flag()?;
            if s.enable_order_hint {
                s.enable_jnt_comp = b.flag()?;
                s.enable_ref_frame_mvs = b.flag()?;
            }
            if !b.flag()? {
                s.seq_force_screen_content_tools = b.f(1)? as u8;
            }
            if s.seq_force_screen_content_tools > 0 {
                if !b.flag()? {
                    s.seq_force_integer_mv = b.f(1)? as u8;
                }
            }
            if s.enable_order_hint {
                s.order_hint_bits = b.f(3)? + 1;
            }
        }
        s.enable_superres = b.flag()?;
        s.enable_cdef = b.flag()?;
        s.enable_restoration = b.flag()?;
        s.color_config(b)?;
        s.film_grain_params_present = b.flag()?;
        Ok(s)
    }

    fn color_config(&mut self, b: &mut Bits) -> Result<()> {
        let high = b.flag()?;
        if self.profile == 2 && high {
            self.bit_depth = if b.flag()? { 12 } else { 10 };
        } else {
            self.bit_depth = if high { 10 } else { 8 };
        }
        self.mono_chrome = if self.profile == 1 { false } else { b.flag()? };
        self.num_planes = if self.mono_chrome { 1 } else { 3 };
        if b.flag()? {
            self.color_primaries = b.f(8)? as u8;
            self.transfer_characteristics = b.f(8)? as u8;
            self.matrix_coefficients = b.f(8)? as u8;
        } else {
            self.color_primaries = 2;
            self.transfer_characteristics = 2;
            self.matrix_coefficients = 2;
        }
        if self.mono_chrome {
            self.color_range = b.flag()?;
            self.subsampling_x = 1;
            self.subsampling_y = 1;
            self.separate_uv_delta_q = false;
            return Ok(());
        }
        if self.color_primaries == 1
            && self.transfer_characteristics == 13
            && self.matrix_coefficients == 0
        {
            self.color_range = true;
            self.subsampling_x = 0;
            self.subsampling_y = 0;
        } else {
            self.color_range = b.flag()?;
            if self.profile == 0 {
                self.subsampling_x = 1;
                self.subsampling_y = 1;
            } else if self.profile == 1 {
                self.subsampling_x = 0;
                self.subsampling_y = 0;
            } else if self.bit_depth == 12 {
                self.subsampling_x = b.f(1)?;
                self.subsampling_y = if self.subsampling_x != 0 { b.f(1)? } else { 0 };
            } else {
                self.subsampling_x = 1;
                self.subsampling_y = 0;
            }
            if self.subsampling_x != 0 && self.subsampling_y != 0 {
                b.f(2)?; // chroma_sample_position
            }
        }
        self.separate_uv_delta_q = b.flag()?;
        Ok(())
    }
}

#[derive(Clone, Default, Debug)]
pub struct FilmGrain {
    pub apply_grain: bool,
    pub grain_seed: u32,
    pub update_grain: bool,
    pub num_y_points: usize,
    pub point_y_value: [u8; 16],
    pub point_y_scaling: [u8; 16],
    pub chroma_scaling_from_luma: bool,
    pub num_cb_points: usize,
    pub point_cb_value: [u8; 16],
    pub point_cb_scaling: [u8; 16],
    pub num_cr_points: usize,
    pub point_cr_value: [u8; 16],
    pub point_cr_scaling: [u8; 16],
    pub grain_scaling_minus_8: u32,
    pub ar_coeff_lag: u32,
    pub ar_coeffs_y_plus_128: [u8; 24],
    pub ar_coeffs_cb_plus_128: [u8; 25],
    pub ar_coeffs_cr_plus_128: [u8; 25],
    pub ar_coeff_shift_minus_6: u32,
    pub grain_scale_shift: u32,
    pub cb_mult: u32,
    pub cb_luma_mult: u32,
    pub cb_offset: u32,
    pub cr_mult: u32,
    pub cr_luma_mult: u32,
    pub cr_offset: u32,
    pub overlap_flag: bool,
    pub clip_to_restricted_range: bool,
}

/// Header-level state saved with every reference slot (section 7.20).
#[derive(Clone, Default)]
pub struct RefHdr {
    pub valid: bool,
    pub frame_id: u32,
    pub upscaled_width: u32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub mi_cols: u32,
    pub mi_rows: u32,
    pub frame_type: u8,
    pub subsampling_x: u32,
    pub subsampling_y: u32,
    pub bit_depth: u32,
    pub order_hint: u32,
    pub saved_order_hints: [u32; 8],
    pub gm_params: [[i32; 6]; 8],
    pub lf_ref_deltas: [i32; 8],
    pub lf_mode_deltas: [i32; 2],
    pub feature_enabled: [[bool; 8]; 8],
    pub feature_data: [[i32; 8]; 8],
    pub film_grain: FilmGrain,
    pub showable_frame: bool,
}

#[derive(Clone, Default, Debug)]
pub struct FrameHdr {
    pub temporal_id: u32,
    pub spatial_id: u32,
    pub show_existing_frame: bool,
    pub frame_to_show_map_idx: usize,
    pub frame_type: u8,
    pub frame_is_intra: bool,
    pub show_frame: bool,
    pub showable_frame: bool,
    pub error_resilient_mode: bool,
    pub disable_cdf_update: bool,
    pub allow_screen_content_tools: bool,
    pub force_integer_mv: bool,
    pub current_frame_id: u32,
    pub frame_size_override_flag: bool,
    pub order_hint: u32,
    pub primary_ref_frame: u8,
    pub refresh_frame_flags: u32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub upscaled_width: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub use_superres: bool,
    pub superres_denom: u32,
    pub mi_cols: u32,
    pub mi_rows: u32,
    pub allow_intrabc: bool,
    pub ref_frame_idx: [usize; 7],
    pub allow_high_precision_mv: bool,
    pub interpolation_filter: u8,
    pub is_motion_mode_switchable: bool,
    pub use_ref_frame_mvs: bool,
    pub order_hints: [u32; 8],
    pub ref_frame_sign_bias: [bool; 8],
    pub disable_frame_end_update_cdf: bool,
    // tiles
    pub tile_cols: usize,
    pub tile_rows: usize,
    pub tile_cols_log2: u32,
    pub tile_rows_log2: u32,
    pub mi_col_starts: Vec<u32>,
    pub mi_row_starts: Vec<u32>,
    pub context_update_tile_id: usize,
    pub tile_size_bytes: u32,
    // quantisation
    pub base_q_idx: i32,
    pub delta_q_y_dc: i32,
    pub delta_q_u_dc: i32,
    pub delta_q_u_ac: i32,
    pub delta_q_v_dc: i32,
    pub delta_q_v_ac: i32,
    pub using_qmatrix: bool,
    pub qm_y: u32,
    pub qm_u: u32,
    pub qm_v: u32,
    // segmentation
    pub segmentation_enabled: bool,
    pub segmentation_update_map: bool,
    pub segmentation_temporal_update: bool,
    pub segmentation_update_data: bool,
    pub feature_enabled: [[bool; 8]; 8],
    pub feature_data: [[i32; 8]; 8],
    pub seg_id_pre_skip: bool,
    pub last_active_seg_id: usize,
    // delta q / lf
    pub delta_q_present: bool,
    pub delta_q_res: u32,
    pub delta_lf_present: bool,
    pub delta_lf_res: u32,
    pub delta_lf_multi: bool,
    pub coded_lossless: bool,
    pub all_lossless: bool,
    pub lossless_array: [bool; 8],
    pub seg_qm_level: [[u32; 8]; 3],
    // loop filter
    pub loop_filter_level: [i32; 4],
    pub loop_filter_sharpness: i32,
    pub loop_filter_delta_enabled: bool,
    pub loop_filter_ref_deltas: [i32; 8],
    pub loop_filter_mode_deltas: [i32; 2],
    // cdef
    pub cdef_damping: i32,
    pub cdef_bits: u32,
    pub cdef_y_pri_strength: [i32; 8],
    pub cdef_y_sec_strength: [i32; 8],
    pub cdef_uv_pri_strength: [i32; 8],
    pub cdef_uv_sec_strength: [i32; 8],
    // loop restoration
    pub frame_restoration_type: [u8; 3],
    pub loop_restoration_size: [u32; 3],
    pub uses_lr: bool,
    pub tx_mode: u8,
    pub reference_select: bool,
    pub skip_mode_present: bool,
    pub skip_mode_frame: [usize; 2],
    pub allow_warped_motion: bool,
    pub reduced_tx_set: bool,
    pub gm_type: [u8; 8],
    pub gm_params: [[i32; 6]; 8],
    pub prev_gm_params: [[i32; 6]; 8],
    pub film_grain: FilmGrain,
    /// Bits consumed by the header, for OBU_FRAME.
    pub header_bits: usize,
}

fn default_gm() -> [[i32; 6]; 8] {
    let mut g = [[0; 6]; 8];
    for r in g.iter_mut() {
        for (i, v) in r.iter_mut().enumerate() {
            *v = if i % 3 == 2 {
                1 << WARPEDMODEL_PREC_BITS
            } else {
                0
            };
        }
    }
    g
}

fn tile_log2(blk: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blk << k) < target {
        k += 1;
    }
    k
}

pub fn get_relative_dist(seq: &SeqHdr, a: u32, b: u32) -> i32 {
    if !seq.enable_order_hint {
        return 0;
    }
    let diff = a as i32 - b as i32;
    let m = 1i32 << (seq.order_hint_bits - 1);
    (diff & (m - 1)) - (diff & m)
}

impl FrameHdr {
    /// `refs` is updated for the parts of the header that change reference state directly.
    pub fn parse(
        b: &mut Bits,
        seq: &SeqHdr,
        refs: &mut [RefHdr; 8],
        temporal_id: u32,
        spatial_id: u32,
    ) -> Result<FrameHdr> {
        let mut h = FrameHdr {
            temporal_id,
            spatial_id,
            ..Default::default()
        };
        let id_len = if seq.frame_id_numbers_present {
            seq.additional_frame_id_length + seq.delta_frame_id_length + 1
        } else {
            0
        };
        let all_frames = 0xffu32;
        if seq.reduced_still_picture_header {
            h.frame_type = KEY_FRAME;
            h.frame_is_intra = true;
            h.show_frame = true;
        } else {
            h.show_existing_frame = b.flag()?;
            if h.show_existing_frame {
                h.frame_to_show_map_idx = b.f(3)? as usize;
                if seq.decoder_model_info_present && !seq.equal_picture_interval {
                    b.f(seq.frame_presentation_time_length)?;
                }
                if seq.frame_id_numbers_present {
                    b.f(id_len)?;
                }
                let r = &refs[h.frame_to_show_map_idx];
                h.frame_type = r.frame_type;
                h.refresh_frame_flags = if h.frame_type == KEY_FRAME {
                    all_frames
                } else {
                    0
                };
                if seq.film_grain_params_present {
                    h.film_grain = r.film_grain.clone();
                }
                h.header_bits = b.position();
                return Ok(h);
            }
            h.frame_type = b.f(2)? as u8;
            h.frame_is_intra = h.frame_type == INTRA_ONLY_FRAME || h.frame_type == KEY_FRAME;
            h.show_frame = b.flag()?;
            if h.show_frame && seq.decoder_model_info_present && !seq.equal_picture_interval {
                b.f(seq.frame_presentation_time_length)?;
            }
            if h.show_frame {
                h.showable_frame = h.frame_type != KEY_FRAME;
            } else {
                h.showable_frame = b.flag()?;
            }
            if h.frame_type == SWITCH_FRAME || (h.frame_type == KEY_FRAME && h.show_frame) {
                h.error_resilient_mode = true;
            } else {
                h.error_resilient_mode = b.flag()?;
            }
        }
        if h.frame_type == KEY_FRAME && h.show_frame {
            for r in refs.iter_mut() {
                r.valid = false;
                r.order_hint = 0;
            }
            for i in 0..REFS_PER_FRAME {
                h.order_hints[1 + i] = 0;
            }
        }
        h.disable_cdf_update = b.flag()?;
        h.allow_screen_content_tools = if seq.seq_force_screen_content_tools == 2 {
            b.flag()?
        } else {
            seq.seq_force_screen_content_tools != 0
        };
        if h.allow_screen_content_tools {
            h.force_integer_mv = if seq.seq_force_integer_mv == 2 {
                b.flag()?
            } else {
                seq.seq_force_integer_mv != 0
            };
        }
        if h.frame_is_intra {
            h.force_integer_mv = true;
        }
        if seq.frame_id_numbers_present {
            h.current_frame_id = b.f(id_len)?;
            let diff_len = seq.delta_frame_id_length;
            for r in refs.iter_mut() {
                if h.current_frame_id > (1 << diff_len) {
                    if r.frame_id > h.current_frame_id
                        || r.frame_id < h.current_frame_id - (1 << diff_len)
                    {
                        r.valid = false;
                    }
                } else if r.frame_id > h.current_frame_id
                    && r.frame_id < (1 << id_len) + h.current_frame_id - (1 << diff_len)
                {
                    r.valid = false;
                }
            }
        }
        h.frame_size_override_flag = if h.frame_type == SWITCH_FRAME {
            true
        } else if seq.reduced_still_picture_header {
            false
        } else {
            b.flag()?
        };
        h.order_hint = b.f(seq.order_hint_bits)?;
        h.primary_ref_frame = if h.frame_is_intra || h.error_resilient_mode {
            PRIMARY_REF_NONE
        } else {
            b.f(3)? as u8
        };
        if seq.decoder_model_info_present {
            if b.flag()? {
                for op in 0..seq.operating_points {
                    if seq.decoder_model_present_for_op[op] {
                        let idc = seq.operating_point_idc[op];
                        let in_t = (idc >> temporal_id) & 1;
                        let in_s = (idc >> (spatial_id + 8)) & 1;
                        if idc == 0 || (in_t != 0 && in_s != 0) {
                            b.f(seq.buffer_removal_time_length)?;
                        }
                    }
                }
            }
        }
        h.refresh_frame_flags =
            if h.frame_type == SWITCH_FRAME || (h.frame_type == KEY_FRAME && h.show_frame) {
                all_frames
            } else {
                b.f(8)?
            };
        if (!h.frame_is_intra || h.refresh_frame_flags != all_frames)
            && h.error_resilient_mode
            && seq.enable_order_hint
        {
            for r in refs.iter_mut() {
                let roh = b.f(seq.order_hint_bits)?;
                if roh != r.order_hint {
                    r.valid = false;
                    r.order_hint = roh;
                }
            }
        }
        if h.frame_is_intra {
            h.frame_size(b, seq)?;
            h.render_size(b);
            if h.allow_screen_content_tools && h.upscaled_width == h.frame_width {
                h.allow_intrabc = b.flag()?;
            }
        } else {
            let mut short = false;
            if seq.enable_order_hint {
                short = b.flag()?;
                if short {
                    let last = b.f(3)? as usize;
                    let gold = b.f(3)? as usize;
                    h.set_frame_refs(seq, refs, last, gold);
                }
            }
            for i in 0..REFS_PER_FRAME {
                if !short {
                    h.ref_frame_idx[i] = b.f(3)? as usize;
                }
                if seq.frame_id_numbers_present {
                    b.f(seq.delta_frame_id_length)?;
                }
            }
            if h.ref_frame_idx
                .iter()
                .any(|&i| !refs[i].valid && refs[i].upscaled_width == 0)
            {
                return Err(Error::corrupt(
                    "AV1: frame refers to an empty reference slot",
                ));
            }
            if h.frame_size_override_flag && !h.error_resilient_mode {
                h.frame_size_with_refs(b, seq, refs)?;
            } else {
                h.frame_size(b, seq)?;
                h.render_size(b);
            }
            h.allow_high_precision_mv = if h.force_integer_mv { false } else { b.flag()? };
            h.interpolation_filter = if b.flag()? { SWITCHABLE } else { b.f(2)? as u8 };
            h.is_motion_mode_switchable = b.flag()?;
            h.use_ref_frame_mvs = if h.error_resilient_mode || !seq.enable_ref_frame_mvs {
                false
            } else {
                b.flag()?
            };
            for i in 0..REFS_PER_FRAME {
                let rf = 1 + i;
                let hint = refs[h.ref_frame_idx[i]].order_hint;
                h.order_hints[rf] = hint;
                h.ref_frame_sign_bias[rf] =
                    seq.enable_order_hint && get_relative_dist(seq, hint, h.order_hint) > 0;
            }
        }
        h.disable_frame_end_update_cdf = if seq.reduced_still_picture_header || h.disable_cdf_update
        {
            true
        } else {
            b.flag()?
        };
        // setup_past_independence / load_previous
        h.prev_gm_params = default_gm();
        h.loop_filter_delta_enabled = true;
        h.loop_filter_ref_deltas = [1, 0, 0, 0, -1, 0, -1, -1];
        h.loop_filter_mode_deltas = [0; 2];
        if h.primary_ref_frame != PRIMARY_REF_NONE {
            let prev = &refs[h.ref_frame_idx[h.primary_ref_frame as usize]];
            h.prev_gm_params = prev.gm_params;
            h.loop_filter_ref_deltas = prev.lf_ref_deltas;
            h.loop_filter_mode_deltas = prev.lf_mode_deltas;
            h.feature_enabled = prev.feature_enabled;
            h.feature_data = prev.feature_data;
        }
        h.gm_params = default_gm();
        h.tile_info(b, seq)?;
        h.quantization_params(b, seq)?;
        h.segmentation_params(b)?;
        // delta_q_params / delta_lf_params
        if h.base_q_idx > 0 {
            h.delta_q_present = b.flag()?;
        }
        if h.delta_q_present {
            h.delta_q_res = b.f(2)?;
            if !h.allow_intrabc {
                h.delta_lf_present = b.flag()?;
            }
            if h.delta_lf_present {
                h.delta_lf_res = b.f(2)?;
                h.delta_lf_multi = b.flag()?;
            }
        }
        h.coded_lossless = true;
        for seg in 0..8 {
            let qindex = h.get_qindex(true, seg, 0);
            let lossless = qindex == 0
                && h.delta_q_y_dc == 0
                && h.delta_q_u_ac == 0
                && h.delta_q_u_dc == 0
                && h.delta_q_v_ac == 0
                && h.delta_q_v_dc == 0;
            h.lossless_array[seg] = lossless;
            if !lossless {
                h.coded_lossless = false;
            }
            if h.using_qmatrix {
                if lossless {
                    for p in 0..3 {
                        h.seg_qm_level[p][seg] = 15;
                    }
                } else {
                    h.seg_qm_level[0][seg] = h.qm_y;
                    h.seg_qm_level[1][seg] = h.qm_u;
                    h.seg_qm_level[2][seg] = h.qm_v;
                }
            }
        }
        h.all_lossless = h.coded_lossless && h.frame_width == h.upscaled_width;
        h.loop_filter_params(b, seq)?;
        h.cdef_params(b, seq)?;
        h.lr_params(b, seq)?;
        // read_tx_mode
        h.tx_mode = if h.coded_lossless {
            ONLY_4X4
        } else if b.flag()? {
            TX_MODE_SELECT
        } else {
            TX_MODE_LARGEST
        };
        h.reference_select = if h.frame_is_intra { false } else { b.flag()? };
        h.skip_mode_params(b, seq, refs)?;
        h.allow_warped_motion =
            if h.frame_is_intra || h.error_resilient_mode || !seq.enable_warped_motion {
                false
            } else {
                b.flag()?
            };
        h.reduced_tx_set = b.flag()?;
        h.global_motion_params(b)?;
        h.film_grain_params(b, seq, refs)?;
        h.header_bits = b.position();
        Ok(h)
    }

    pub fn get_qindex(&self, ignore_delta_q: bool, segment_id: usize, current_q_index: i32) -> i32 {
        if self.seg_feature_active_idx(segment_id, 0) {
            let data = self.feature_data[segment_id][0];
            let mut q = self.base_q_idx + data;
            if !ignore_delta_q && self.delta_q_present {
                q = current_q_index + data;
            }
            return q.clamp(0, 255);
        }
        if !ignore_delta_q && self.delta_q_present {
            return current_q_index;
        }
        self.base_q_idx
    }

    pub fn seg_feature_active_idx(&self, segment_id: usize, feature: usize) -> bool {
        self.segmentation_enabled && self.feature_enabled[segment_id][feature]
    }

    fn superres_params(&mut self, b: &mut Bits, seq: &SeqHdr) -> Result<()> {
        let use_superres = if seq.enable_superres {
            b.flag()?
        } else {
            false
        };
        self.use_superres = use_superres;
        self.superres_denom = if use_superres { b.f(3)? + 9 } else { 8 };
        self.upscaled_width = self.frame_width;
        self.frame_width =
            (self.upscaled_width * 8 + self.superres_denom / 2) / self.superres_denom;
        Ok(())
    }

    fn compute_image_size(&mut self) {
        self.mi_cols = 2 * ((self.frame_width + 7) >> 3);
        self.mi_rows = 2 * ((self.frame_height + 7) >> 3);
    }

    fn frame_size(&mut self, b: &mut Bits, seq: &SeqHdr) -> Result<()> {
        if self.frame_size_override_flag {
            self.frame_width = b.f(seq.frame_width_bits)? + 1;
            self.frame_height = b.f(seq.frame_height_bits)? + 1;
        } else {
            self.frame_width = seq.max_frame_width;
            self.frame_height = seq.max_frame_height;
        }
        self.superres_params(b, seq)?;
        self.compute_image_size();
        Ok(())
    }

    fn render_size(&mut self, b: &mut Bits) {
        if b.flag().unwrap_or(false) {
            self.render_width = b.f(16).unwrap_or(0) + 1;
            self.render_height = b.f(16).unwrap_or(0) + 1;
        } else {
            self.render_width = self.upscaled_width;
            self.render_height = self.frame_height;
        }
    }

    fn frame_size_with_refs(
        &mut self,
        b: &mut Bits,
        seq: &SeqHdr,
        refs: &[RefHdr; 8],
    ) -> Result<()> {
        let mut found = false;
        for i in 0..REFS_PER_FRAME {
            if b.flag()? {
                let r = &refs[self.ref_frame_idx[i]];
                self.upscaled_width = r.upscaled_width;
                self.frame_width = self.upscaled_width;
                self.frame_height = r.frame_height;
                self.render_width = r.render_width;
                self.render_height = r.render_height;
                found = true;
                break;
            }
        }
        if !found {
            self.frame_size(b, seq)?;
            self.render_size(b);
        } else {
            self.superres_params(b, seq)?;
            self.compute_image_size();
        }
        Ok(())
    }

    fn set_frame_refs(&mut self, seq: &SeqHdr, refs: &[RefHdr; 8], last: usize, gold: usize) {
        let mut idx = [-1i32; REFS_PER_FRAME];
        idx[0] = last as i32;
        idx[3] = gold as i32;
        let mut used = [false; 8];
        used[last] = true;
        used[gold] = true;
        let cur = 1i32 << (seq.order_hint_bits - 1);
        let mut shifted = [0i32; 8];
        for i in 0..8 {
            shifted[i] = cur + get_relative_dist(seq, refs[i].order_hint, self.order_hint);
        }
        let find_latest_backward = |used: &[bool; 8]| {
            let mut r = -1i32;
            let mut latest = 0;
            for i in 0..8 {
                let hint = shifted[i];
                if !used[i] && hint >= cur && (r < 0 || hint >= latest) {
                    r = i as i32;
                    latest = hint;
                }
            }
            r
        };
        let find_earliest_backward = |used: &[bool; 8]| {
            let mut r = -1i32;
            let mut earliest = 0;
            for i in 0..8 {
                let hint = shifted[i];
                if !used[i] && hint >= cur && (r < 0 || hint < earliest) {
                    r = i as i32;
                    earliest = hint;
                }
            }
            r
        };
        let find_latest_forward = |used: &[bool; 8]| {
            let mut r = -1i32;
            let mut latest = 0;
            for i in 0..8 {
                let hint = shifted[i];
                if !used[i] && hint < cur && (r < 0 || hint >= latest) {
                    r = i as i32;
                    latest = hint;
                }
            }
            r
        };
        let r = find_latest_backward(&used);
        if r >= 0 {
            idx[6] = r;
            used[r as usize] = true;
        }
        let r = find_earliest_backward(&used);
        if r >= 0 {
            idx[4] = r;
            used[r as usize] = true;
        }
        let r = find_earliest_backward(&used);
        if r >= 0 {
            idx[5] = r;
            used[r as usize] = true;
        }
        for &rf in &tables::REF_FRAME_LIST {
            let k = rf as usize - 1;
            if idx[k] < 0 {
                let r = find_latest_forward(&used);
                if r >= 0 {
                    idx[k] = r;
                    used[r as usize] = true;
                }
            }
        }
        let mut r = -1i32;
        let mut earliest = 0;
        for i in 0..8 {
            let hint = shifted[i];
            if r < 0 || hint < earliest {
                r = i as i32;
                earliest = hint;
            }
        }
        for i in 0..REFS_PER_FRAME {
            if idx[i] < 0 {
                idx[i] = r;
            }
            self.ref_frame_idx[i] = idx[i] as usize;
        }
    }

    fn tile_info(&mut self, b: &mut Bits, seq: &SeqHdr) -> Result<()> {
        let sb128 = seq.use_128x128_superblock;
        let sb_cols = if sb128 {
            (self.mi_cols + 31) >> 5
        } else {
            (self.mi_cols + 15) >> 4
        };
        let sb_rows = if sb128 {
            (self.mi_rows + 31) >> 5
        } else {
            (self.mi_rows + 15) >> 4
        };
        let sb_shift = if sb128 { 5 } else { 4 };
        let sb_size = sb_shift + 2;
        let max_tile_width_sb = 4096 >> sb_size;
        let mut max_tile_area_sb = (4096 * 2304) >> (2 * sb_size);
        let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
        let max_log2_tile_cols = tile_log2(1, sb_cols.min(64));
        let max_log2_tile_rows = tile_log2(1, sb_rows.min(64));
        let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
        self.mi_col_starts.clear();
        self.mi_row_starts.clear();
        if b.flag()? {
            self.tile_cols_log2 = min_log2_tile_cols;
            while self.tile_cols_log2 < max_log2_tile_cols {
                if b.flag()? {
                    self.tile_cols_log2 += 1;
                } else {
                    break;
                }
            }
            let tile_width_sb = (sb_cols + (1 << self.tile_cols_log2) - 1) >> self.tile_cols_log2;
            let mut start = 0;
            while start < sb_cols {
                self.mi_col_starts.push(start << sb_shift);
                start += tile_width_sb;
            }
            self.tile_cols = self.mi_col_starts.len();
            self.mi_col_starts.push(self.mi_cols);
            let min_log2_tile_rows = min_log2_tiles.saturating_sub(self.tile_cols_log2);
            self.tile_rows_log2 = min_log2_tile_rows;
            while self.tile_rows_log2 < max_log2_tile_rows {
                if b.flag()? {
                    self.tile_rows_log2 += 1;
                } else {
                    break;
                }
            }
            let tile_height_sb = (sb_rows + (1 << self.tile_rows_log2) - 1) >> self.tile_rows_log2;
            let mut start = 0;
            while start < sb_rows {
                self.mi_row_starts.push(start << sb_shift);
                start += tile_height_sb;
            }
            self.tile_rows = self.mi_row_starts.len();
            self.mi_row_starts.push(self.mi_rows);
        } else {
            let mut widest = 0;
            let mut start = 0;
            while start < sb_cols {
                self.mi_col_starts.push(start << sb_shift);
                let max_width = (sb_cols - start).min(max_tile_width_sb);
                let size = b.ns(max_width)? + 1;
                widest = widest.max(size);
                start += size;
            }
            self.tile_cols = self.mi_col_starts.len();
            self.mi_col_starts.push(self.mi_cols);
            self.tile_cols_log2 = tile_log2(1, self.tile_cols as u32);
            max_tile_area_sb = if min_log2_tiles > 0 {
                (sb_rows * sb_cols) >> (min_log2_tiles + 1)
            } else {
                sb_rows * sb_cols
            };
            let max_tile_height_sb = (max_tile_area_sb / widest).max(1);
            let mut start = 0;
            while start < sb_rows {
                self.mi_row_starts.push(start << sb_shift);
                let max_height = (sb_rows - start).min(max_tile_height_sb);
                let size = b.ns(max_height)? + 1;
                start += size;
            }
            self.tile_rows = self.mi_row_starts.len();
            self.mi_row_starts.push(self.mi_rows);
            self.tile_rows_log2 = tile_log2(1, self.tile_rows as u32);
        }
        if self.tile_cols > 64 || self.tile_rows > 64 {
            return Err(Error::corrupt("AV1: too many tiles"));
        }
        if self.tile_cols_log2 > 0 || self.tile_rows_log2 > 0 {
            self.context_update_tile_id = b.f(self.tile_rows_log2 + self.tile_cols_log2)? as usize;
            self.tile_size_bytes = b.f(2)? + 1;
        }
        if self.context_update_tile_id >= self.tile_cols * self.tile_rows {
            return Err(Error::corrupt("AV1: bad context_update_tile_id"));
        }
        Ok(())
    }

    fn read_delta_q(b: &mut Bits) -> Result<i32> {
        if b.flag()? {
            b.su(7)
        } else {
            Ok(0)
        }
    }

    fn quantization_params(&mut self, b: &mut Bits, seq: &SeqHdr) -> Result<()> {
        self.base_q_idx = b.f(8)? as i32;
        self.delta_q_y_dc = Self::read_delta_q(b)?;
        if seq.num_planes > 1 {
            let diff_uv_delta = if seq.separate_uv_delta_q {
                b.flag()?
            } else {
                false
            };
            self.delta_q_u_dc = Self::read_delta_q(b)?;
            self.delta_q_u_ac = Self::read_delta_q(b)?;
            if diff_uv_delta {
                self.delta_q_v_dc = Self::read_delta_q(b)?;
                self.delta_q_v_ac = Self::read_delta_q(b)?;
            } else {
                self.delta_q_v_dc = self.delta_q_u_dc;
                self.delta_q_v_ac = self.delta_q_u_ac;
            }
        }
        self.using_qmatrix = b.flag()?;
        if self.using_qmatrix {
            self.qm_y = b.f(4)?;
            self.qm_u = b.f(4)?;
            self.qm_v = if seq.separate_uv_delta_q {
                b.f(4)?
            } else {
                self.qm_u
            };
        }
        Ok(())
    }

    fn segmentation_params(&mut self, b: &mut Bits) -> Result<()> {
        self.segmentation_enabled = b.flag()?;
        if self.segmentation_enabled {
            if self.primary_ref_frame == PRIMARY_REF_NONE {
                self.segmentation_update_map = true;
                self.segmentation_temporal_update = false;
                self.segmentation_update_data = true;
            } else {
                self.segmentation_update_map = b.flag()?;
                if self.segmentation_update_map {
                    self.segmentation_temporal_update = b.flag()?;
                }
                self.segmentation_update_data = b.flag()?;
            }
            if self.segmentation_update_data {
                for i in 0..8 {
                    for j in 0..8 {
                        let enabled = b.flag()?;
                        self.feature_enabled[i][j] = enabled;
                        let mut clipped = 0;
                        if enabled {
                            let bits = u32::from(tables::SEGMENTATION_FEATURE_BITS[j]);
                            let limit = i32::from(tables::SEGMENTATION_FEATURE_MAX[j]);
                            if tables::SEGMENTATION_FEATURE_SIGNED[j] == 1 {
                                let v = b.su(1 + bits)?;
                                clipped = v.clamp(-limit, limit);
                            } else {
                                let v = b.f(bits)? as i32;
                                clipped = v.clamp(0, limit);
                            }
                        }
                        self.feature_data[i][j] = clipped;
                    }
                }
            }
        } else {
            self.feature_enabled = [[false; 8]; 8];
            self.feature_data = [[0; 8]; 8];
        }
        self.seg_id_pre_skip = false;
        self.last_active_seg_id = 0;
        for i in 0..8 {
            for j in 0..8 {
                if self.feature_enabled[i][j] {
                    self.last_active_seg_id = i;
                    if j >= 5 {
                        self.seg_id_pre_skip = true;
                    }
                }
            }
        }
        Ok(())
    }

    fn loop_filter_params(&mut self, b: &mut Bits, seq: &SeqHdr) -> Result<()> {
        if self.coded_lossless || self.allow_intrabc {
            self.loop_filter_level[0] = 0;
            self.loop_filter_level[1] = 0;
            self.loop_filter_ref_deltas = [1, 0, 0, 0, -1, 0, -1, -1];
            self.loop_filter_mode_deltas = [0; 2];
            return Ok(());
        }
        self.loop_filter_level[0] = b.f(6)? as i32;
        self.loop_filter_level[1] = b.f(6)? as i32;
        if seq.num_planes > 1 && (self.loop_filter_level[0] != 0 || self.loop_filter_level[1] != 0)
        {
            self.loop_filter_level[2] = b.f(6)? as i32;
            self.loop_filter_level[3] = b.f(6)? as i32;
        }
        self.loop_filter_sharpness = b.f(3)? as i32;
        self.loop_filter_delta_enabled = b.flag()?;
        if self.loop_filter_delta_enabled && b.flag()? {
            for i in 0..8 {
                if b.flag()? {
                    self.loop_filter_ref_deltas[i] = b.su(7)?;
                }
            }
            for i in 0..2 {
                if b.flag()? {
                    self.loop_filter_mode_deltas[i] = b.su(7)?;
                }
            }
        }
        Ok(())
    }

    fn cdef_params(&mut self, b: &mut Bits, seq: &SeqHdr) -> Result<()> {
        if self.coded_lossless || self.allow_intrabc || !seq.enable_cdef {
            self.cdef_bits = 0;
            self.cdef_damping = 3;
            return Ok(());
        }
        self.cdef_damping = b.f(2)? as i32 + 3;
        self.cdef_bits = b.f(2)?;
        for i in 0..(1usize << self.cdef_bits) {
            self.cdef_y_pri_strength[i] = b.f(4)? as i32;
            self.cdef_y_sec_strength[i] = b.f(2)? as i32;
            if self.cdef_y_sec_strength[i] == 3 {
                self.cdef_y_sec_strength[i] += 1;
            }
            if seq.num_planes > 1 {
                self.cdef_uv_pri_strength[i] = b.f(4)? as i32;
                self.cdef_uv_sec_strength[i] = b.f(2)? as i32;
                if self.cdef_uv_sec_strength[i] == 3 {
                    self.cdef_uv_sec_strength[i] += 1;
                }
            }
        }
        Ok(())
    }

    fn lr_params(&mut self, b: &mut Bits, seq: &SeqHdr) -> Result<()> {
        if self.all_lossless || self.allow_intrabc || !seq.enable_restoration {
            self.frame_restoration_type = [RESTORE_NONE; 3];
            self.uses_lr = false;
            return Ok(());
        }
        let mut uses_chroma_lr = false;
        for i in 0..seq.num_planes {
            let t = tables::REMAP_LR_TYPE[b.f(2)? as usize];
            self.frame_restoration_type[i] = t;
            if t != RESTORE_NONE {
                self.uses_lr = true;
                if i > 0 {
                    uses_chroma_lr = true;
                }
            }
        }
        if self.uses_lr {
            let mut shift;
            if seq.use_128x128_superblock {
                shift = b.f(1)?;
                shift += 1;
            } else {
                shift = b.f(1)?;
                if shift != 0 {
                    shift += b.f(1)?;
                }
            }
            self.loop_restoration_size[0] = 256 >> (2 - shift);
            let uv_shift = if seq.subsampling_x != 0 && seq.subsampling_y != 0 && uses_chroma_lr {
                b.f(1)?
            } else {
                0
            };
            self.loop_restoration_size[1] = self.loop_restoration_size[0] >> uv_shift;
            self.loop_restoration_size[2] = self.loop_restoration_size[0] >> uv_shift;
        }
        Ok(())
    }

    fn skip_mode_params(&mut self, b: &mut Bits, seq: &SeqHdr, refs: &[RefHdr; 8]) -> Result<()> {
        let mut allowed = false;
        if !(self.frame_is_intra || !self.reference_select || !seq.enable_order_hint) {
            let mut forward_idx = -1i32;
            let mut backward_idx = -1i32;
            let mut forward_hint = 0;
            let mut backward_hint = 0;
            for i in 0..REFS_PER_FRAME {
                let ref_hint = refs[self.ref_frame_idx[i]].order_hint;
                if get_relative_dist(seq, ref_hint, self.order_hint) < 0 {
                    if forward_idx < 0 || get_relative_dist(seq, ref_hint, forward_hint) > 0 {
                        forward_idx = i as i32;
                        forward_hint = ref_hint;
                    }
                } else if get_relative_dist(seq, ref_hint, self.order_hint) > 0
                    && (backward_idx < 0 || get_relative_dist(seq, ref_hint, backward_hint) < 0)
                {
                    backward_idx = i as i32;
                    backward_hint = ref_hint;
                }
            }
            if forward_idx < 0 {
                allowed = false;
            } else if backward_idx >= 0 {
                allowed = true;
                self.skip_mode_frame[0] = 1 + forward_idx.min(backward_idx) as usize;
                self.skip_mode_frame[1] = 1 + forward_idx.max(backward_idx) as usize;
            } else {
                let mut second = -1i32;
                let mut second_hint = 0;
                for i in 0..REFS_PER_FRAME {
                    let ref_hint = refs[self.ref_frame_idx[i]].order_hint;
                    if get_relative_dist(seq, ref_hint, forward_hint) < 0
                        && (second < 0 || get_relative_dist(seq, ref_hint, second_hint) > 0)
                    {
                        second = i as i32;
                        second_hint = ref_hint;
                    }
                }
                if second >= 0 {
                    allowed = true;
                    self.skip_mode_frame[0] = 1 + forward_idx.min(second) as usize;
                    self.skip_mode_frame[1] = 1 + forward_idx.max(second) as usize;
                }
            }
        }
        self.skip_mode_present = if allowed { b.flag()? } else { false };
        Ok(())
    }

    fn global_motion_params(&mut self, b: &mut Bits) -> Result<()> {
        self.gm_type = [IDENTITY; 8];
        self.gm_params = default_gm();
        if self.frame_is_intra {
            return Ok(());
        }
        for rf in 1..=7usize {
            let ty = if b.flag()? {
                if b.flag()? {
                    ROTZOOM
                } else if b.flag()? {
                    TRANSLATION
                } else {
                    AFFINE
                }
            } else {
                IDENTITY
            };
            self.gm_type[rf] = ty;
            if ty >= ROTZOOM {
                self.read_global_param(b, ty, rf, 2)?;
                self.read_global_param(b, ty, rf, 3)?;
                if ty == AFFINE {
                    self.read_global_param(b, ty, rf, 4)?;
                    self.read_global_param(b, ty, rf, 5)?;
                } else {
                    self.gm_params[rf][4] = -self.gm_params[rf][3];
                    self.gm_params[rf][5] = self.gm_params[rf][2];
                }
            }
            if ty >= TRANSLATION {
                self.read_global_param(b, ty, rf, 0)?;
                self.read_global_param(b, ty, rf, 1)?;
            }
        }
        Ok(())
    }

    fn read_global_param(&mut self, b: &mut Bits, ty: u8, rf: usize, idx: usize) -> Result<()> {
        let mut abs_bits = 12i32;
        let mut prec_bits = 15i32;
        if idx < 2 {
            if ty == TRANSLATION {
                let hp = i32::from(!self.allow_high_precision_mv);
                abs_bits = 9 - hp;
                prec_bits = 3 - hp;
            } else {
                abs_bits = 12;
                prec_bits = 6;
            }
        }
        let prec_diff = WARPEDMODEL_PREC_BITS as i32 - prec_bits;
        let round = if idx % 3 == 2 {
            1i32 << WARPEDMODEL_PREC_BITS
        } else {
            0
        };
        let sub = if idx % 3 == 2 { 1i32 << prec_bits } else { 0 };
        let mx = 1i32 << abs_bits;
        let r = (self.prev_gm_params[rf][idx] >> prec_diff) - sub;
        let v = decode_signed_subexp_with_ref(b, -mx, mx + 1, r)?;
        self.gm_params[rf][idx] = (v << prec_diff) + round;
        Ok(())
    }

    fn film_grain_params(&mut self, b: &mut Bits, seq: &SeqHdr, refs: &[RefHdr; 8]) -> Result<()> {
        self.film_grain = FilmGrain::default();
        if !seq.film_grain_params_present || (!self.show_frame && !self.showable_frame) {
            return Ok(());
        }
        let mut g = FilmGrain {
            apply_grain: b.flag()?,
            ..Default::default()
        };
        if !g.apply_grain {
            return Ok(());
        }
        g.grain_seed = b.f(16)?;
        g.update_grain = if self.frame_type == INTER_FRAME {
            b.flag()?
        } else {
            true
        };
        if !g.update_grain {
            let idx = b.f(3)? as usize;
            let seed = g.grain_seed;
            g = refs[idx].film_grain.clone();
            g.grain_seed = seed;
            self.film_grain = g;
            return Ok(());
        }
        g.num_y_points = b.f(4)? as usize;
        if g.num_y_points > 14 {
            return Err(Error::corrupt("AV1: too many film grain points"));
        }
        for i in 0..g.num_y_points {
            g.point_y_value[i] = b.f(8)? as u8;
            g.point_y_scaling[i] = b.f(8)? as u8;
        }
        g.chroma_scaling_from_luma = if seq.mono_chrome { false } else { b.flag()? };
        if seq.mono_chrome
            || g.chroma_scaling_from_luma
            || (seq.subsampling_x == 1 && seq.subsampling_y == 1 && g.num_y_points == 0)
        {
            g.num_cb_points = 0;
            g.num_cr_points = 0;
        } else {
            g.num_cb_points = b.f(4)? as usize;
            if g.num_cb_points > 10 {
                return Err(Error::corrupt("AV1: too many film grain points"));
            }
            for i in 0..g.num_cb_points {
                g.point_cb_value[i] = b.f(8)? as u8;
                g.point_cb_scaling[i] = b.f(8)? as u8;
            }
            g.num_cr_points = b.f(4)? as usize;
            if g.num_cr_points > 10 {
                return Err(Error::corrupt("AV1: too many film grain points"));
            }
            for i in 0..g.num_cr_points {
                g.point_cr_value[i] = b.f(8)? as u8;
                g.point_cr_scaling[i] = b.f(8)? as u8;
            }
        }
        g.grain_scaling_minus_8 = b.f(2)?;
        g.ar_coeff_lag = b.f(2)?;
        let num_pos_luma = (2 * g.ar_coeff_lag * (g.ar_coeff_lag + 1)) as usize;
        let num_pos_chroma;
        if g.num_y_points > 0 {
            num_pos_chroma = num_pos_luma + 1;
            for i in 0..num_pos_luma {
                g.ar_coeffs_y_plus_128[i] = b.f(8)? as u8;
            }
        } else {
            num_pos_chroma = num_pos_luma;
        }
        if g.chroma_scaling_from_luma || g.num_cb_points > 0 {
            for i in 0..num_pos_chroma {
                g.ar_coeffs_cb_plus_128[i] = b.f(8)? as u8;
            }
        }
        if g.chroma_scaling_from_luma || g.num_cr_points > 0 {
            for i in 0..num_pos_chroma {
                g.ar_coeffs_cr_plus_128[i] = b.f(8)? as u8;
            }
        }
        g.ar_coeff_shift_minus_6 = b.f(2)?;
        g.grain_scale_shift = b.f(2)?;
        if g.num_cb_points > 0 {
            g.cb_mult = b.f(8)?;
            g.cb_luma_mult = b.f(8)?;
            g.cb_offset = b.f(9)?;
        }
        if g.num_cr_points > 0 {
            g.cr_mult = b.f(8)?;
            g.cr_luma_mult = b.f(8)?;
            g.cr_offset = b.f(9)?;
        }
        g.overlap_flag = b.flag()?;
        g.clip_to_restricted_range = b.flag()?;
        self.film_grain = g;
        Ok(())
    }
}

fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 != 0 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

fn decode_subexp(b: &mut Bits, num_syms: i32) -> Result<i32> {
    let mut i = 0;
    let mut mk = 0;
    let k = 3;
    loop {
        let b2 = if i != 0 { k + i - 1 } else { k };
        let a = 1 << b2;
        if num_syms <= mk + 3 * a {
            return Ok(b.ns((num_syms - mk) as u32)? as i32 + mk);
        }
        if b.flag()? {
            i += 1;
            mk += a;
        } else {
            return Ok(b.f(b2 as u32)? as i32 + mk);
        }
    }
}

fn decode_unsigned_subexp_with_ref(b: &mut Bits, mx: i32, r: i32) -> Result<i32> {
    let v = decode_subexp(b, mx)?;
    Ok(if (r << 1) <= mx {
        inverse_recenter(r, v)
    } else {
        mx - 1 - inverse_recenter(mx - 1 - r, v)
    })
}

fn decode_signed_subexp_with_ref(b: &mut Bits, low: i32, high: i32, r: i32) -> Result<i32> {
    Ok(decode_unsigned_subexp_with_ref(b, high - low, r - low)? + low)
}
