//! Per-8x8 mode info kept for the whole frame.

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mv {
    pub row: i16,
    pub col: i16,
}

pub const NONE: i8 = -1;
pub const INTRA_FRAME: i8 = 0;
pub const LAST_FRAME: i8 = 1;
pub const GOLDEN_FRAME: i8 = 2;
pub const ALTREF_FRAME: i8 = 3;

#[derive(Clone, Copy)]
pub struct Mi {
    pub size: u8,
    pub skip: bool,
    pub tx_size: u8,
    pub y_mode: u8,
    pub sub_modes: [u8; 4],
    pub seg_id: u8,
    pub ref_frame: [i8; 2],
    pub interp: u8,
    /// `[ref list][sub block]`; the block's own vector is entry 3.
    pub mv: [[Mv; 4]; 2],
}

impl Default for Mi {
    fn default() -> Self {
        Self {
            size: 0,
            skip: false,
            tx_size: 0,
            y_mode: 0,
            sub_modes: [0; 4],
            seg_id: 0,
            ref_frame: [INTRA_FRAME, NONE],
            interp: 0,
            mv: [[Mv::default(); 4]; 2],
        }
    }
}
