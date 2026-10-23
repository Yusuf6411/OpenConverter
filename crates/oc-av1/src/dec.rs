//! Decoder state for one frame: sample planes, per-4x4 mode info and the block-level variables
//! that the specification keeps in globals.
use crate::cdf::Cdfs;
use crate::hdr::{FrameHdr, RefHdr, SeqHdr};
use crate::sym::Sym;
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub d: Vec<u16>,
}

impl Plane {
    pub fn new(w: usize, h: usize) -> Plane {
        Plane {
            w,
            h,
            stride: w,
            d: vec![0; w * h],
        }
    }
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u16 {
        self.d[y * self.stride + x]
    }
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: u16) {
        self.d[y * self.stride + x] = v;
    }
}

#[derive(Clone, Default)]
pub struct Frame {
    pub planes: Vec<Plane>,
}

/// Everything remembered per 4x4 luma block (the specification's YModes, RefFrames, ... arrays).
#[derive(Clone, Copy, Default)]
pub struct MiInfo {
    pub mi_size: u8,
    pub y_mode: u8,
    pub uv_mode: u8,
    pub ref_frame: [i8; 2],
    pub is_inter: bool,
    pub skip: bool,
    pub skip_mode: bool,
    pub tx_size: u8,
    pub inter_tx_size: u8,
    pub segment_id: u8,
    pub palette_size: [u8; 2],
    pub delta_lf: [i8; 4],
    pub comp_group_idx: u8,
    pub compound_idx: u8,
    pub interp_filter: [u8; 2],
    pub mv: [[i16; 2]; 2],
    pub motion_mode: u8,
    pub is_inter_intra: bool,
    pub written: bool,
}

/// Data kept with a reference slot beyond the header (section 7.20).
#[derive(Clone)]
pub struct RefData {
    pub frame: Rc<Frame>,
    pub cdfs: Rc<Cdfs>,
    pub mf_ref_frames: Rc<Vec<i8>>,
    pub mf_mvs: Rc<Vec<[i16; 2]>>,
    pub segment_ids: Rc<Vec<u8>>,
    pub mi_stride: usize,
}

#[derive(Clone, Copy, Default)]
pub struct LrUnit {
    pub ty: u8,
    pub wiener: [[i8; 3]; 2],
    pub sgr_set: u8,
    pub sgr_xqd: [i8; 2],
}

pub struct Blk {
    pub mi_row: usize,
    pub mi_col: usize,
    pub mi_size: usize,
    pub has_chroma: bool,
    pub avail_u: bool,
    pub avail_l: bool,
    pub avail_u_chroma: bool,
    pub avail_l_chroma: bool,
    pub skip: bool,
    pub skip_mode: bool,
    pub segment_id: usize,
    pub lossless: bool,
    pub is_inter: bool,
    pub use_intrabc: bool,
    pub y_mode: u8,
    pub uv_mode: u8,
    pub angle_delta_y: i32,
    pub angle_delta_uv: i32,
    pub use_filter_intra: bool,
    pub filter_intra_mode: u8,
    pub cfl_alpha_u: i32,
    pub cfl_alpha_v: i32,
    pub palette_size_y: usize,
    pub palette_size_uv: usize,
    pub palette_colors_y: [u16; 8],
    pub palette_colors_u: [u16; 8],
    pub palette_colors_v: [u16; 8],
    pub color_map_y: Vec<u8>,
    pub color_map_uv: Vec<u8>,
    pub ref_frame: [i8; 2],
    pub mv: [[i32; 2]; 2],
    pub pred_mv: [[i32; 2]; 2],
    pub tx_size: usize,
    pub motion_mode: u8,
    pub compound_type: u8,
    pub interintra: bool,
    pub interintra_mode: u8,
    pub wedge_interintra: bool,
    pub wedge_index: usize,
    pub wedge_sign: usize,
    pub mask_type: usize,
    pub comp_group_idx: u8,
    pub compound_idx: u8,
    pub interp_filter: [u8; 2],
    pub ref_mv_idx: usize,
    pub num_mv_found: usize,
    pub new_mv_count: usize,
    pub ref_stack_mv: [[[i32; 2]; 2]; MAX_STACK + 2],
    pub weight_stack: [u32; MAX_STACK + 2],
    pub global_mvs: [[i32; 2]; 2],
    pub drl_ctx_stack: [usize; MAX_STACK + 2],
    pub new_mv_context: usize,
    pub ref_mv_context: usize,
    pub zero_mv_context: usize,
    pub num_samples: usize,
    pub num_samples_scanned: usize,
    pub cand_list: [[i32; 4]; 8],
    pub local_warp_params: [i32; 6],
    pub local_valid: bool,
    pub left_ref_frame: [i8; 2],
    pub above_ref_frame: [i8; 2],
    pub left_intra: bool,
    pub above_intra: bool,
    pub left_single: bool,
    pub above_single: bool,
    pub max_luma_w: usize,
    pub max_luma_h: usize,
    pub plane_tx_type: u8,
    pub is_inter_intra: bool,
    pub found_match: bool,
    pub close_matches: usize,
    pub total_matches: usize,
}

pub const MAX_STACK: usize = 8;

impl Default for Blk {
    fn default() -> Blk {
        Blk {
            mi_row: 0,
            mi_col: 0,
            mi_size: 0,
            has_chroma: false,
            avail_u: false,
            avail_l: false,
            avail_u_chroma: false,
            avail_l_chroma: false,
            skip: false,
            skip_mode: false,
            segment_id: 0,
            lossless: false,
            is_inter: false,
            use_intrabc: false,
            y_mode: 0,
            uv_mode: 0,
            angle_delta_y: 0,
            angle_delta_uv: 0,
            use_filter_intra: false,
            filter_intra_mode: 0,
            cfl_alpha_u: 0,
            cfl_alpha_v: 0,
            palette_size_y: 0,
            palette_size_uv: 0,
            palette_colors_y: [0; 8],
            palette_colors_u: [0; 8],
            palette_colors_v: [0; 8],
            color_map_y: vec![0; 64 * 64],
            color_map_uv: vec![0; 64 * 64],
            ref_frame: [0, -1],
            mv: [[0; 2]; 2],
            pred_mv: [[0; 2]; 2],
            tx_size: 0,
            motion_mode: 0,
            compound_type: 0,
            interintra: false,
            interintra_mode: 0,
            wedge_interintra: false,
            wedge_index: 0,
            wedge_sign: 0,
            mask_type: 0,
            comp_group_idx: 0,
            compound_idx: 0,
            interp_filter: [0; 2],
            ref_mv_idx: 0,
            num_mv_found: 0,
            new_mv_count: 0,
            ref_stack_mv: [[[0; 2]; 2]; MAX_STACK + 2],
            weight_stack: [0; MAX_STACK + 2],
            global_mvs: [[0; 2]; 2],
            drl_ctx_stack: [0; MAX_STACK + 2],
            new_mv_context: 0,
            ref_mv_context: 0,
            zero_mv_context: 0,
            num_samples: 0,
            num_samples_scanned: 0,
            cand_list: [[0; 4]; 8],
            local_warp_params: [0; 6],
            local_valid: false,
            left_ref_frame: [0; 2],
            above_ref_frame: [0; 2],
            left_intra: false,
            above_intra: false,
            left_single: false,
            above_single: false,
            max_luma_w: 0,
            max_luma_h: 0,
            plane_tx_type: 0,
            is_inter_intra: false,
            found_match: false,
            close_matches: 0,
            total_matches: 0,
        }
    }
}

pub struct Dec {
    pub seq: SeqHdr,
    pub hdr: FrameHdr,
    pub refh: [RefHdr; 8],
    pub refd: [Option<RefData>; 8],
    pub cur: Frame,
    pub cdf: Cdfs,
    pub frame_cdf: Cdfs,
    pub saved_cdf: Option<Cdfs>,
    pub sym: Sym,
    pub b: Blk,
    // geometry
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub mi_stride: usize,
    pub mi_rows_alloc: usize,
    pub num_planes: usize,
    pub subx: usize,
    pub suby: usize,
    pub bit_depth: u32,
    // tile
    pub mi_row_start: usize,
    pub mi_row_end: usize,
    pub mi_col_start: usize,
    pub mi_col_end: usize,
    pub current_q_index: i32,
    pub read_deltas: bool,
    pub delta_lf: [i32; 4],
    pub ref_sgr_xqd: [[i32; 2]; 3],
    pub ref_lr_wiener: [[[i32; 3]; 2]; 3],
    // per-4x4 arrays
    pub mi: Vec<MiInfo>,
    pub pal_colors: Vec<[[u16; 8]; 2]>,
    pub tx_types: Vec<u8>,
    pub lf_tx_sizes: [Vec<u8>; 3],
    pub prev_segment_ids: Vec<u8>,
    pub cdef_idx: Vec<i8>,
    pub cdef_stride: usize,
    pub lr_units: [Vec<LrUnit>; 3],
    pub lr_cols: [usize; 3],
    pub lr_rows: [usize; 3],
    // contexts
    pub above_level_ctx: [Vec<u8>; 3],
    pub above_dc_ctx: [Vec<u8>; 3],
    pub above_seg_pred: Vec<u8>,
    pub left_level_ctx: [Vec<u8>; 3],
    pub left_dc_ctx: [Vec<u8>; 3],
    pub left_seg_pred: Vec<u8>,
    pub block_decoded: Vec<bool>, // [3][35][35] with a 1 offset
    // coefficients
    pub quant: Vec<i32>,
    pub dequant: Vec<i32>,
    // motion field
    pub mf_ref_frames: Vec<i8>,
    pub mf_mvs: Vec<[i16; 2]>,
    pub motion_field_mvs: Vec<[[i16; 2]; 8]>,
    pub motion_field_valid: Vec<[bool; 8]>,
    pub scratch: Vec<i32>,
    pub mask: Vec<u8>,
}

impl Dec {
    #[inline]
    pub fn mi_at(&self, row: usize, col: usize) -> &MiInfo {
        &self.mi[row * self.mi_stride + col]
    }
    #[inline]
    pub fn mi_idx(&self, row: usize, col: usize) -> usize {
        row * self.mi_stride + col
    }
    #[inline]
    pub fn bd_idx(&self, plane: usize, y: i32, x: i32) -> usize {
        (plane * 35 + (y + 1) as usize) * 35 + (x + 1) as usize
    }
}
