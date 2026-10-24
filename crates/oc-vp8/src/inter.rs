//! Inter prediction shared by the VP8 decoder and encoder: motion vector types and entropy
//! tables, the neighbour search that picks mode contexts, and sub-pixel motion compensation.
//!
//! Motion vectors are stored in eighth-sample units (the bitstream codes quarter samples and
//! the decoder doubles them), so `mv >> 3` is the whole-sample part and `mv & 7` indexes the
//! filter. Prediction reads past the frame edges as if the edge samples repeated forever,
//! which is what the reference decoder's border extension amounts to.
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use crate::recon::Planes;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mv {
    pub row: i32,
    pub col: i32,
}

impl Mv {
    pub const ZERO: Mv = Mv { row: 0, col: 0 };
    pub fn is_zero(self) -> bool {
        self.row == 0 && self.col == 0
    }
}

/// Reference frame slots.
pub const INTRA_FRAME: u8 = 0;
pub const LAST_FRAME: u8 = 1;
pub const GOLDEN_FRAME: u8 = 2;
pub const ALTREF_FRAME: u8 = 3;

/// Macroblock modes beyond the intra ones in `predict.rs` (DC..H are 0..=3).
pub const M_BPRED: u8 = 10;
pub const M_NEAREST: u8 = 11;
pub const M_NEAR: u8 = 12;
pub const M_ZERO: u8 = 13;
pub const M_NEW: u8 = 14;
pub const M_SPLIT: u8 = 15;

/// What the neighbour search and the loop filter need to know of a coded macroblock.
#[derive(Clone, Copy, Default)]
pub struct MbInfo {
    pub ref_frame: u8,
    pub mode: u8,
    pub mv: Mv,
    /// Per-block vectors; all equal to `mv` unless the mode is `M_SPLIT`.
    pub bmv: [Mv; 16],
}

pub const MV_PROB_COUNT: usize = 19;
pub type MvProbs = [[u8; MV_PROB_COUNT]; 2];

pub const DEFAULT_MV_PROBS: MvProbs = [
    [
        162, 128, 225, 146, 172, 147, 214, 39, 156, 128, 129, 132, 75, 145, 178, 206, 239, 254, 254,
    ],
    [
        164, 128, 204, 170, 119, 235, 140, 230, 228, 128, 130, 130, 74, 148, 180, 203, 236, 254,
        254,
    ],
];

pub const MV_UPDATE_PROBS: MvProbs = [
    [
        237, 246, 253, 253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 250, 250, 252, 254,
        254,
    ],
    [
        231, 243, 245, 253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 251, 251, 254, 254,
        254,
    ],
];

pub const DEFAULT_YMODE_PROBS: [u8; 4] = [112, 86, 140, 37];
pub const DEFAULT_UVMODE_PROBS: [u8; 3] = [162, 101, 204];
/// Sub-block intra modes in inter frames have no context.
pub const INTER_BMODE_PROBS: [u8; 9] = [120, 90, 79, 133, 87, 85, 80, 111, 151];

/// Probabilities of the macroblock-level motion modes, indexed by neighbour count and node.
pub const MODE_CONTEXTS: [[u8; 4]; 6] = [
    [7, 1, 1, 143],
    [14, 18, 14, 107],
    [135, 64, 57, 68],
    [60, 56, 128, 65],
    [159, 134, 128, 34],
    [234, 188, 128, 28],
];

pub const SPLIT_PROBS: [u8; 3] = [110, 111, 150];
pub const SUB_MV_REF_PROBS: [[u8; 3]; 5] = [
    [147, 136, 18],
    [106, 145, 1],
    [179, 121, 1],
    [223, 1, 34],
    [208, 1, 1],
];

/// Block-to-partition maps of the four split shapes, and each shape's partition count.
pub const SPLIT_LAYOUT: [[u8; 16]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];
pub const SPLIT_COUNT: [usize; 4] = [2, 2, 4, 16];

/// Context for the sub-block vector mode from the vectors on its left and above.
pub fn sub_mv_context(left: Mv, above: Mv) -> usize {
    let lez = left.is_zero();
    let aez = above.is_zero();
    let lea = left == above;
    if lea && lez {
        4
    } else if lea {
        3
    } else if aez {
        2
    } else if lez {
        1
    } else {
        0
    }
}

/// Clamp window for a macroblock's predicted vectors: the macroblock may point at most 16
/// samples beyond the frame.
#[derive(Clone, Copy)]
pub struct Bounds {
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
}

impl Bounds {
    pub fn new(mb_x: usize, mb_y: usize, mb_w: usize, mb_h: usize) -> Self {
        let (x, y) = (mb_x as i32, mb_y as i32);
        Self {
            left: -((x + 1) << 7),
            right: (mb_w as i32 - x) << 7,
            top: -((y + 1) << 7),
            bottom: (mb_h as i32 - y) << 7,
        }
    }
    pub fn clamp(&self, mv: Mv) -> Mv {
        Mv {
            row: mv.row.clamp(self.top, self.bottom),
            col: mv.col.clamp(self.left, self.right),
        }
    }
}

/// Result of the neighbour search: candidate vectors and the counts that pick the
/// probabilities of the mode tree.
pub struct Near {
    pub best: Mv,
    pub nearest: Mv,
    pub near: Mv,
    pub probs: [u8; 4],
    pub cnt: [usize; 4],
}

/// `mi` is the mode-info grid with a one-entry border above and to the left (stride
/// `mb_w + 1`), `idx` the current macroblock's position in it.
pub fn find_near(
    mi: &[MbInfo],
    idx: usize,
    stride: usize,
    ref_frame: u8,
    sign_bias: &[bool; 4],
    bounds: &Bounds,
) -> Near {
    let above = mi[idx - stride];
    let left = mi[idx - 1];
    let aboveleft = mi[idx - stride - 1];
    let mut mvs = [Mv::ZERO; 4];
    let mut cnt = [0usize; 4];
    let mut n = 0usize;
    let biased = |m: &MbInfo| -> Mv {
        if sign_bias[usize::from(m.ref_frame)] != sign_bias[usize::from(ref_frame)] {
            Mv {
                row: -m.mv.row,
                col: -m.mv.col,
            }
        } else {
            m.mv
        }
    };
    if above.ref_frame != INTRA_FRAME {
        if !above.mv.is_zero() {
            n += 1;
            mvs[n] = biased(&above);
        }
        cnt[n] += 2;
    }
    if left.ref_frame != INTRA_FRAME {
        if !left.mv.is_zero() {
            let this = biased(&left);
            if this != mvs[n] {
                n += 1;
                mvs[n] = this;
            }
            cnt[n] += 2;
        } else {
            cnt[0] += 2;
        }
    }
    if aboveleft.ref_frame != INTRA_FRAME {
        if !aboveleft.mv.is_zero() {
            let this = biased(&aboveleft);
            if this != mvs[n] {
                n += 1;
                mvs[n] = this;
            }
            cnt[n] += 1;
        } else {
            cnt[0] += 1;
        }
    }
    // Three distinct vectors where the last equals the first.
    if cnt[3] > 0 && mvs[n] == mvs[1] {
        cnt[1] += 1;
    }
    cnt[3] = (usize::from(above.mode == M_SPLIT) + usize::from(left.mode == M_SPLIT)) * 2
        + usize::from(aboveleft.mode == M_SPLIT);
    if cnt[2] > cnt[1] {
        cnt.swap(1, 2);
        mvs.swap(1, 2);
    }
    if cnt[1] >= cnt[0] {
        mvs[0] = mvs[1];
    }
    let probs = [
        MODE_CONTEXTS[cnt[0]][0],
        MODE_CONTEXTS[cnt[1]][1],
        MODE_CONTEXTS[cnt[2]][2],
        MODE_CONTEXTS[cnt[3]][3],
    ];
    Near {
        best: bounds.clamp(mvs[0]),
        nearest: bounds.clamp(mvs[1]),
        near: bounds.clamp(mvs[2]),
        probs,
        cnt,
    }
}

const SIXTAP: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, -6, 123, 12, -1, 0],
    [2, -11, 108, 36, -8, 1],
    [0, -9, 93, 50, -6, 0],
    [3, -16, 77, 77, -16, 3],
    [0, -6, 50, 93, -9, 0],
    [1, -8, 36, 108, -11, 2],
    [0, -1, 12, 123, -6, 0],
];

/// A reference plane: `data[(y + 1) * stride + x + 1]` is sample (x, y).
#[derive(Clone, Copy)]
pub struct RefPlane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub w: i32,
    pub h: i32,
}

impl<'a> RefPlane<'a> {
    pub fn luma(p: &'a Planes) -> Self {
        Self {
            data: &p.y,
            stride: p.ys,
            w: (p.mb_w * 16) as i32,
            h: (p.mb_h * 16) as i32,
        }
    }
    pub fn chroma(p: &'a Planes, ch: usize) -> Self {
        Self {
            data: if ch == 0 { &p.u } else { &p.v },
            stride: p.uvs,
            w: (p.mb_w * 8) as i32,
            h: (p.mb_h * 8) as i32,
        }
    }
}

fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Predicts the `bw` x `bh` block whose top-left sample is (`x`, `y`) displaced by `mv`.
pub fn mc_block(
    r: &RefPlane,
    x: i32,
    y: i32,
    mv: Mv,
    bw: usize,
    bh: usize,
    bilinear: bool,
    out: &mut [u8],
    out_stride: usize,
) {
    let ix = x + (mv.col >> 3);
    let iy = y + (mv.row >> 3);
    let fx = (mv.col & 7) as usize;
    let fy = (mv.row & 7) as usize;
    // The source samples the filters can touch: two before and three after in each direction.
    const PW: usize = 21;
    let (pw, ph) = (bw + 5, bh + 5);
    let mut patch = [0u8; PW * PW];
    let (x0, y0) = (ix - 2, iy - 2);
    let x_inside = x0 >= 0 && x0 + pw as i32 <= r.w;
    for j in 0..ph {
        let yy = (y0 + j as i32).clamp(0, r.h - 1) as usize;
        let row = &r.data[(yy + 1) * r.stride + 1..(yy + 1) * r.stride + 1 + r.w as usize];
        let dst = &mut patch[j * PW..j * PW + pw];
        if x_inside {
            dst.copy_from_slice(&row[x0 as usize..x0 as usize + pw]);
        } else {
            for (i, d) in dst.iter_mut().enumerate() {
                *d = row[(x0 + i as i32).clamp(0, r.w - 1) as usize];
            }
        }
    }
    let at = |i: usize, j: usize| i32::from(patch[j * PW + i]);
    if fx == 0 && fy == 0 {
        for j in 0..bh {
            for i in 0..bw {
                out[j * out_stride + i] = patch[(j + 2) * PW + i + 2];
            }
        }
        return;
    }
    if bilinear {
        let (hx0, hx1) = (128 - 16 * fx as i32, 16 * fx as i32);
        let (vy0, vy1) = (128 - 16 * fy as i32, 16 * fy as i32);
        let mut mid = [0i32; 17 * 16];
        for j in 0..=bh {
            for i in 0..bw {
                mid[j * 16 + i] = (at(i + 2, j + 2) * hx0 + at(i + 3, j + 2) * hx1 + 64) >> 7;
            }
        }
        for j in 0..bh {
            for i in 0..bw {
                out[j * out_stride + i] =
                    clip((mid[j * 16 + i] * vy0 + mid[(j + 1) * 16 + i] * vy1 + 64) >> 7);
            }
        }
        return;
    }
    // Six-tap: horizontal pass over bh + 5 rows, then vertical.
    let hf = &SIXTAP[fx];
    let vf = &SIXTAP[fy];
    let mut mid = [0u8; 21 * 16];
    for j in 0..bh + 5 {
        for i in 0..bw {
            mid[j * 16 + i] = if fx == 0 {
                patch[j * PW + i + 2]
            } else {
                let mut s = 64;
                for k in 0..6 {
                    s += hf[k] * at(i + k, j);
                }
                clip(s >> 7)
            };
        }
    }
    for j in 0..bh {
        for i in 0..bw {
            out[j * out_stride + i] = if fy == 0 {
                mid[(j + 2) * 16 + i]
            } else {
                let mut s = 64;
                for k in 0..6 {
                    s += vf[k] * i32::from(mid[(j + k) * 16 + i]);
                }
                clip(s >> 7)
            };
        }
    }
}

/// Builds the 16x16 luma and two 8x8 chroma predictions of an inter macroblock.
pub fn predict_mb(
    refp: &Planes,
    mb_x: usize,
    mb_y: usize,
    info: &MbInfo,
    bilinear: bool,
    full_pixel: bool,
) -> ([u8; 256], [u8; 64], [u8; 64]) {
    let mut luma = [0u8; 256];
    let mut cb = [0u8; 64];
    let mut cr = [0u8; 64];
    let ly = RefPlane::luma(refp);
    let (lx0, ly0) = ((mb_x * 16) as i32, (mb_y * 16) as i32);
    let (cx0, cy0) = ((mb_x * 8) as i32, (mb_y * 8) as i32);
    let mask = |v: i32| if full_pixel { v & !7 } else { v };
    if info.mode != M_SPLIT {
        mc_block(&ly, lx0, ly0, info.mv, 16, 16, bilinear, &mut luma, 16);
        let half = |v: i32| (v + if v < 0 { -1 } else { 1 }) / 2;
        let uv = Mv {
            row: mask(half(info.mv.row)),
            col: mask(half(info.mv.col)),
        };
        for (ch, out) in [(0, &mut cb), (1, &mut cr)] {
            let c = RefPlane::chroma(refp, ch);
            mc_block(&c, cx0, cy0, uv, 8, 8, bilinear, out, 8);
        }
        return (luma, cb, cr);
    }
    for b in 0..16 {
        let (bx, by) = (b % 4, b / 4);
        mc_block(
            &ly,
            lx0 + 4 * bx as i32,
            ly0 + 4 * by as i32,
            info.bmv[b],
            4,
            4,
            bilinear,
            &mut luma[by * 64 + bx * 4..],
            16,
        );
    }
    for by in 0..2 {
        for bx in 0..2 {
            let k = by * 8 + bx * 2;
            let sum = |f: fn(&Mv) -> i32| {
                let t = f(&info.bmv[k])
                    + f(&info.bmv[k + 1])
                    + f(&info.bmv[k + 4])
                    + f(&info.bmv[k + 5]);
                mask((t + if t < 0 { -4 } else { 4 }) / 8)
            };
            let uv = Mv {
                row: sum(|m| m.row),
                col: sum(|m| m.col),
            };
            for (ch, out) in [(0, &mut cb), (1, &mut cr)] {
                let c = RefPlane::chroma(refp, ch);
                mc_block(
                    &c,
                    cx0 + 4 * bx as i32,
                    cy0 + 4 * by as i32,
                    uv,
                    4,
                    4,
                    bilinear,
                    &mut out[by * 32 + bx * 4..],
                    8,
                );
            }
        }
    }
    (luma, cb, cr)
}
