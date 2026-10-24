//! Symbolic constants from section 3 and the semantics of section 6 of the AV1 specification.
#![allow(dead_code)]

pub const BLOCK_4X4: usize = 0;
pub const BLOCK_4X8: usize = 1;
pub const BLOCK_8X4: usize = 2;
pub const BLOCK_8X8: usize = 3;
pub const BLOCK_8X16: usize = 4;
pub const BLOCK_16X8: usize = 5;
pub const BLOCK_16X16: usize = 6;
pub const BLOCK_16X32: usize = 7;
pub const BLOCK_32X16: usize = 8;
pub const BLOCK_32X32: usize = 9;
pub const BLOCK_32X64: usize = 10;
pub const BLOCK_64X32: usize = 11;
pub const BLOCK_64X64: usize = 12;
pub const BLOCK_64X128: usize = 13;
pub const BLOCK_128X64: usize = 14;
pub const BLOCK_128X128: usize = 15;
pub const BLOCK_4X16: usize = 16;
pub const BLOCK_16X4: usize = 17;
pub const BLOCK_8X32: usize = 18;
pub const BLOCK_32X8: usize = 19;
pub const BLOCK_16X64: usize = 20;
pub const BLOCK_64X16: usize = 21;
pub const BLOCK_INVALID: usize = 22;

pub const PARTITION_NONE: usize = 0;
pub const PARTITION_HORZ: usize = 1;
pub const PARTITION_VERT: usize = 2;
pub const PARTITION_SPLIT: usize = 3;
pub const PARTITION_HORZ_A: usize = 4;
pub const PARTITION_HORZ_B: usize = 5;
pub const PARTITION_VERT_A: usize = 6;
pub const PARTITION_VERT_B: usize = 7;
pub const PARTITION_HORZ_4: usize = 8;
pub const PARTITION_VERT_4: usize = 9;

pub const TX_4X4: usize = 0;
pub const TX_8X8: usize = 1;
pub const TX_16X16: usize = 2;
pub const TX_32X32: usize = 3;
pub const TX_64X64: usize = 4;
pub const TX_4X8: usize = 5;
pub const TX_8X4: usize = 6;
pub const TX_8X16: usize = 7;
pub const TX_16X8: usize = 8;
pub const TX_16X32: usize = 9;
pub const TX_32X16: usize = 10;
pub const TX_32X64: usize = 11;
pub const TX_64X32: usize = 12;
pub const TX_4X16: usize = 13;
pub const TX_16X4: usize = 14;
pub const TX_8X32: usize = 15;
pub const TX_32X8: usize = 16;
pub const TX_16X64: usize = 17;
pub const TX_64X16: usize = 18;

pub const DCT_DCT: u8 = 0;
pub const ADST_DCT: u8 = 1;
pub const DCT_ADST: u8 = 2;
pub const ADST_ADST: u8 = 3;
pub const FLIPADST_DCT: u8 = 4;
pub const DCT_FLIPADST: u8 = 5;
pub const FLIPADST_FLIPADST: u8 = 6;
pub const ADST_FLIPADST: u8 = 7;
pub const FLIPADST_ADST: u8 = 8;
pub const IDTX: u8 = 9;
pub const V_DCT: u8 = 10;
pub const H_DCT: u8 = 11;
pub const V_ADST: u8 = 12;
pub const H_ADST: u8 = 13;
pub const V_FLIPADST: u8 = 14;
pub const H_FLIPADST: u8 = 15;

pub const TX_CLASS_2D: usize = 0;
pub const TX_CLASS_HORIZ: usize = 1;
pub const TX_CLASS_VERT: usize = 2;

pub const TX_SET_DCTONLY: usize = 0;
pub const TX_SET_INTRA_1: usize = 1;
pub const TX_SET_INTRA_2: usize = 2;
pub const TX_SET_INTER_1: usize = 1;
pub const TX_SET_INTER_2: usize = 2;
pub const TX_SET_INTER_3: usize = 3;

pub const DC_PRED: u8 = 0;
pub const V_PRED: u8 = 1;
pub const H_PRED: u8 = 2;
pub const D45_PRED: u8 = 3;
pub const D135_PRED: u8 = 4;
pub const D113_PRED: u8 = 5;
pub const D157_PRED: u8 = 6;
pub const D203_PRED: u8 = 7;
pub const D67_PRED: u8 = 8;
pub const SMOOTH_PRED: u8 = 9;
pub const SMOOTH_V_PRED: u8 = 10;
pub const SMOOTH_H_PRED: u8 = 11;
pub const PAETH_PRED: u8 = 12;
pub const UV_CFL_PRED: u8 = 13;
pub const NEARESTMV: u8 = 14;
pub const NEARMV: u8 = 15;
pub const GLOBALMV: u8 = 16;
pub const NEWMV: u8 = 17;
pub const NEAREST_NEARESTMV: u8 = 18;
pub const NEAR_NEARMV: u8 = 19;
pub const NEAREST_NEWMV: u8 = 20;
pub const NEW_NEARESTMV: u8 = 21;
pub const NEAR_NEWMV: u8 = 22;
pub const NEW_NEARMV: u8 = 23;
pub const GLOBAL_GLOBALMV: u8 = 24;
pub const NEW_NEWMV: u8 = 25;

pub const NONE: i8 = -1;
pub const INTRA_FRAME: i8 = 0;
pub const LAST_FRAME: i8 = 1;
pub const LAST2_FRAME: i8 = 2;
pub const LAST3_FRAME: i8 = 3;
pub const GOLDEN_FRAME: i8 = 4;
pub const BWDREF_FRAME: i8 = 5;
pub const ALTREF2_FRAME: i8 = 6;
pub const ALTREF_FRAME: i8 = 7;

pub const SIMPLE: u8 = 0;
pub const OBMC: u8 = 1;
pub const LOCALWARP: u8 = 2;

pub const COMPOUND_WEDGE: u8 = 0;
pub const COMPOUND_DIFFWTD: u8 = 1;
pub const COMPOUND_AVERAGE: u8 = 2;
pub const COMPOUND_INTRA: u8 = 3;
pub const COMPOUND_DISTANCE: u8 = 4;

pub const EIGHTTAP: u8 = 0;
pub const EIGHTTAP_SMOOTH: u8 = 1;
pub const EIGHTTAP_SHARP: u8 = 2;
pub const BILINEAR: u8 = 3;

pub const SEG_LVL_ALT_Q: usize = 0;
pub const SEG_LVL_ALT_LF_Y_V: usize = 1;
pub const SEG_LVL_REF_FRAME: usize = 5;
pub const SEG_LVL_SKIP: usize = 6;
pub const SEG_LVL_GLOBALMV: usize = 7;

pub const RESTORE_WIENER: u8 = 1;
pub const RESTORE_SGRPROJ: u8 = 2;

pub const SIG_COEF_CONTEXTS: usize = 42;
pub const SIG_COEF_CONTEXTS_EOB: usize = 4;
pub const NUM_BASE_LEVELS: i32 = 2;
pub const COEFF_BASE_RANGE: i32 = 12;
pub const BR_CDF_SIZE: i32 = 4;
pub const MAX_LOOP_FILTER: i32 = 63;
pub const DELTA_Q_SMALL: usize = 3;
pub const DELTA_LF_SMALL: usize = 3;
pub const MAX_ANGLE_DELTA: i32 = 3;
pub const ANGLE_STEP: i32 = 3;
pub const MV_BORDER: i32 = 128;
pub const MAX_VARTX_DEPTH: usize = 2;
pub const MAX_REF_MV_STACK_SIZE: usize = 8;
pub const MFMV_STACK_SIZE: usize = 3;
pub const REFMVS_LIMIT: i32 = (1 << 12) - 1;
pub const II_DC_PRED: u8 = 0;
pub const II_V_PRED: u8 = 1;
pub const II_H_PRED: u8 = 2;
pub const II_SMOOTH_PRED: u8 = 3;

pub fn is_directional_mode(mode: u8) -> bool {
    (V_PRED..=D67_PRED).contains(&mode)
}

pub fn block_width(bsize: usize) -> usize {
    4 * usize::from(crate::tables::NUM_4X4_BLOCKS_WIDE[bsize])
}

pub fn block_height(bsize: usize) -> usize {
    4 * usize::from(crate::tables::NUM_4X4_BLOCKS_HIGH[bsize])
}
