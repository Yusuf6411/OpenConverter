//! VP8 encoder: key frames, and inter frames predicted from the previous frame.
//!
//! Every macroblock is coded with the intra or inter mode that minimises distortion plus a
//! rate term estimated from the very probabilities that end up in the stream. The encoder
//! reconstructs with the decoder's own prediction and transform code, so what it predicts from
//! is exactly what a decoder will have.
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use crate::bool::BoolWriter;
use crate::dec::Frame;
use crate::filter::{filter_mb, inter_strengths, FilterInfo, PlaneMut};
use crate::inter::{
    MbInfo, Mv, MvProbs, DEFAULT_MV_PROBS, DEFAULT_UVMODE_PROBS, DEFAULT_YMODE_PROBS,
    INTER_BMODE_PROBS, MV_PROB_COUNT, MV_UPDATE_PROBS, M_NEAR, M_NEAREST, M_NEW, M_SPLIT,
};
use crate::predict::{predict_4x4, predict_block, DC_PRED, H_PRED, V_PRED};
use crate::recon::Planes;
use crate::tables::{AC_TABLE, BANDS, COEFF_PROBS, COEFF_UPDATE_PROBS, DC_TABLE, ZIGZAG};
use crate::tokens::{
    bmode_cost, bmode_paths, cost_table, put_bmode, walk_i16, walk_i4, walk_uv, Cost, Nz, Probs,
    Sink, Stats, Writer,
};
use crate::transform::{fdct, fwht, idct_add, iwht};
use oc_core::{Error, ErrorCode, Progress, Result};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Params {
    /// 0 (smallest) to 100 (best quality).
    pub quality: f32,
    /// 0 (fastest) to 6 (best compression).
    pub effort: u8,
    /// Loop filter level 0..=63; `None` picks the level that reproduces the source best.
    pub filter_level: Option<u8>,
    /// Loop filter sharpness 0..=7.
    pub filter_sharpness: u8,
    /// Number of token partitions, as a power of two exponent (0..=3).
    pub partitions_log2: u8,
    /// Quantiser index 0..=127 that replaces the one derived from `quality`.
    pub qindex: Option<u8>,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            quality: 75.0,
            effort: 4,
            filter_level: None,
            filter_sharpness: 0,
            partitions_log2: 0,
            qindex: None,
        }
    }
}

const QFIX: u32 = 17;

/// Quantiser for one kind of block: steps for DC and AC, with the reciprocal and the
/// rounding offset (a dead zone: values just under half a step round down).
#[derive(Clone, Copy, Default)]
pub(crate) struct Matrix {
    pub q: [i32; 2],
    iq: [u32; 2],
    bias: [u32; 2],
}

impl Matrix {
    fn new(dc: i32, ac: i32, bias: (u32, u32)) -> Self {
        let iq = |q: i32| (1u32 << QFIX) / q as u32;
        Self {
            q: [dc, ac],
            iq: [iq(dc), iq(ac)],
            bias: [bias.0 << (QFIX - 8), bias.1 << (QFIX - 8)],
        }
    }

    /// Quantises `coeffs` (natural order) from scan position `first`: levels in zigzag order,
    /// dequantised values in natural order. Returns whether any level is non-zero.
    pub(crate) fn quantize(
        &self,
        coeffs: &[i16; 16],
        first: usize,
        levels: &mut [i16; 16],
        deq: &mut [i16; 16],
    ) -> bool {
        let mut any = false;
        for n in first..16 {
            let j = usize::from(ZIGZAG[n]);
            let k = usize::from(j > 0);
            let c = i32::from(coeffs[j]);
            let level = ((c.unsigned_abs() * self.iq[k] + self.bias[k]) >> QFIX).min(2047) as i32;
            if level != 0 {
                let s = if c < 0 { -1 } else { 1 };
                levels[n] = (s * level) as i16;
                deq[j] = (s * level * self.q[k]) as i16;
                any = true;
            }
        }
        any
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SegQ {
    pub index: i32,
    pub y1: Matrix,
    pub y2: Matrix,
    pub uv: Matrix,
    pub lam_y: u64,
    pub lam_uv: u64,
}

/// Rate-distortion trade-off: squared error per bit is about this fraction of the squared
/// step (in percent, times 256 because rates are counted in 1/256 bit).
const LAMBDA_PERCENT: u64 = 500;

pub(crate) fn seg_q(index: i32) -> SegQ {
    let clip = |v: i32, m: i32| v.clamp(0, m) as usize;
    let dc = i32::from(DC_TABLE[clip(index, 127)]);
    let ac = i32::from(AC_TABLE[clip(index, 127)]);
    let uvdc = i32::from(DC_TABLE[clip(index, 117)]);
    let y2ac = ((ac * 101_581) >> 16).max(8);
    let lam = |q: i32| (q as u64 * q as u64 * LAMBDA_PERCENT) / 100;
    SegQ {
        index,
        y1: Matrix::new(dc, ac, (96, 110)),
        y2: Matrix::new(dc * 2, y2ac, (96, 108)),
        uv: Matrix::new(uvdc, ac, (110, 115)),
        lam_y: lam(ac),
        lam_uv: lam(ac),
    }
}

/// Quantiser index for a quality setting, following the curve libwebp uses.
pub(crate) fn quality_to_index(quality: f32) -> i32 {
    let q = (quality / 100.0).clamp(0.0, 1.0) as f64;
    let c = if q < 0.75 {
        q * (2.0 / 3.0)
    } else {
        2.0 * q - 1.0
    };
    let v = c.cbrt();
    (127.0 * (1.0 - v)).round() as i32
}

#[derive(Clone)]
pub(crate) struct Mb {
    pub segment: u8,
    /// No Y2 block: sub-block intra prediction or SPLITMV.
    pub is_i4: bool,
    pub ymode: u8,
    pub bmodes: [u8; 16],
    pub uvmode: u8,
    pub y2: [i16; 16],
    pub y: [[i16; 16]; 16],
    pub uv: [[i16; 16]; 8],
    /// The macroblock codes no coefficient at all.
    pub skip: bool,
    /// As the decoder sees it: no coefficient and no DC from the Y2 block, which decides
    /// whether the loop filter treats the inside of the macroblock.
    pub residual: bool,
    /// Inter prediction: reference frame, motion mode (`M_*`), vector(s) and split shape.
    pub inter: bool,
    pub ref_frame: u8,
    pub imode: u8,
    pub mv: Mv,
    pub bmv: [Mv; 16],
    pub shape: u8,
}

pub(crate) struct Src {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub ys: usize,
    pub uvs: usize,
}

fn pad_plane(src: &[u8], w: usize, h: usize, pw: usize, ph: usize) -> Vec<u8> {
    let mut out = vec![0u8; pw * ph];
    for y in 0..ph {
        let sy = y.min(h - 1);
        let row = &src[sy * w..sy * w + w];
        let dst = &mut out[y * pw..y * pw + pw];
        dst[..w].copy_from_slice(row);
        dst[w..].fill(row[w - 1]);
    }
    out
}

pub(crate) fn bitcost(bit: bool, prob: u8) -> u32 {
    let p = usize::from(prob);
    u32::from(cost_table()[if bit { 256 - p } else { p }])
}

/// Probabilities the mode decisions are costed with, and the ones a frame transmits.
#[derive(Clone)]
pub(crate) struct ModeProbs {
    /// Probability that a macroblock is intra coded.
    pub intra: u8,
    /// Probability that an inter macroblock predicts from the last frame.
    pub last: u8,
    /// Probability that a non-last reference is the golden frame.
    pub golden: u8,
    pub mv: MvProbs,
    pub ymode: [u8; 4],
    pub uvmode: [u8; 3],
}

impl Default for ModeProbs {
    fn default() -> Self {
        Self {
            intra: 60,
            last: 128,
            golden: 255,
            mv: DEFAULT_MV_PROBS,
            ymode: DEFAULT_YMODE_PROBS,
            uvmode: DEFAULT_UVMODE_PROBS,
        }
    }
}

fn ymode_bits(mode: u8, mut f: impl FnMut(bool, u8)) {
    f(true, 145);
    match mode {
        DC_PRED => {
            f(false, 156);
            f(false, 163);
        }
        V_PRED => {
            f(false, 156);
            f(true, 163);
        }
        H_PRED => {
            f(true, 156);
            f(false, 128);
        }
        _ => {
            f(true, 156);
            f(true, 128);
        }
    }
}

/// Luma mode bits of an intra macroblock in an inter frame; `None` is sub-block prediction.
pub(crate) fn ymode_bits_inter(mode: Option<u8>, p: &[u8; 4], mut f: impl FnMut(bool, u8)) {
    match mode {
        Some(DC_PRED) => f(false, p[0]),
        Some(V_PRED) => {
            f(true, p[0]);
            f(false, p[1]);
            f(false, p[2]);
        }
        Some(H_PRED) => {
            f(true, p[0]);
            f(false, p[1]);
            f(true, p[2]);
        }
        Some(_) => {
            f(true, p[0]);
            f(true, p[1]);
            f(false, p[3]);
        }
        None => {
            f(true, p[0]);
            f(true, p[1]);
            f(true, p[3]);
        }
    }
}

fn uvmode_bits(mode: u8, mut f: impl FnMut(bool, u8)) {
    match mode {
        DC_PRED => f(false, 142),
        V_PRED => {
            f(true, 142);
            f(false, 114);
        }
        H_PRED => {
            f(true, 142);
            f(true, 114);
            f(false, 183);
        }
        _ => {
            f(true, 142);
            f(true, 114);
            f(true, 183);
        }
    }
}

pub(crate) fn uvmode_bits_inter(mode: u8, p: &[u8; 3], mut f: impl FnMut(bool, u8)) {
    match mode {
        DC_PRED => f(false, p[0]),
        V_PRED => {
            f(true, p[0]);
            f(false, p[1]);
        }
        H_PRED => {
            f(true, p[0]);
            f(true, p[1]);
            f(false, p[2]);
        }
        _ => {
            f(true, p[0]);
            f(true, p[1]);
            f(true, p[2]);
        }
    }
}

/// Sub-block mode bits in inter frames, where no context applies.
pub(crate) fn bmode_bits_fixed(mode: u8, mut f: impl FnMut(bool, u8)) {
    for &(i, bit) in &bmode_paths()[usize::from(mode)] {
        f(bit, INTER_BMODE_PROBS[i]);
    }
}

fn sse(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = i64::from(x) - i64::from(y);
            (d * d) as u64
        })
        .sum()
}

struct Null;
impl Sink for Null {
    fn adaptive(&mut self, _: bool, _: usize, _: usize, _: usize, _: usize) {}
    fn fixed(&mut self, _: bool, _: u8) {}
}

struct Cand16 {
    score: u64,
    mode: u8,
    y2: [i16; 16],
    y: [[i16; 16]; 16],
    recon: [u8; 256],
    dc_nonzero: bool,
}

struct CandUv {
    score: u64,
    mode: u8,
    uv: [[i16; 16]; 8],
    recon: [[u8; 64]; 2],
}

struct Cand4 {
    score: u64,
    mode: u8,
    levels: [i16; 16],
    recon: [u8; 16],
}

pub(crate) struct Core<'a> {
    pub p: &'a Params,
    pub key: bool,
    pub threads: usize,
    pub mb_w: usize,
    pub mb_h: usize,
    pub src: Src,
    pub rec: Planes,
    pub segq: [SegQ; 4],
    pub seg_of: Vec<u8>,
    pub cost_probs: Probs,
    /// Token probabilities the decoder holds when this frame starts.
    pub base_probs: Probs,
    /// Keep this frame's token probabilities for the frames after it.
    pub persist: bool,
    pub top_modes: Vec<u8>,
    pub left_modes: [u8; 4],
    pub top_nz: Vec<Nz>,
    pub left_nz: Nz,
    pub mbs: Vec<Mb>,
    /// Mode information of the macroblocks coded so far, with a border above and to the left.
    pub mi: Vec<MbInfo>,
    pub mi_stride: usize,
    /// Reference frames by slot (`LAST_FRAME`, ...); slot 0 is unused.
    pub refs: [Option<Arc<Planes>>; 4],
    /// The motion search's best whole-macroblock vector against the last frame.
    pub me: Vec<Option<Mv>>,
    pub mp: ModeProbs,
}

impl<'a> Core<'a> {
    pub(crate) fn new(params: &'a Params, key: bool, frame: &Frame, threads: usize) -> Self {
        let (w, h) = (frame.width, frame.height);
        let (cw, ch) = (frame.chroma_width(), frame.chroma_height());
        let mb_w = w.div_ceil(16);
        let mb_h = h.div_ceil(16);
        let src = Src {
            y: pad_plane(&frame.y, w, h, mb_w * 16, mb_h * 16),
            u: pad_plane(&frame.u, cw, ch, mb_w * 8, mb_h * 8),
            v: pad_plane(&frame.v, cw, ch, mb_w * 8, mb_h * 8),
            ys: mb_w * 16,
            uvs: mb_w * 8,
        };
        let index = params
            .qindex
            .map_or_else(|| quality_to_index(params.quality), i32::from);
        let sq = seg_q(index.clamp(0, 127));
        Self {
            p: params,
            key,
            threads: threads.max(1),
            mb_w,
            mb_h,
            src,
            rec: Planes::new(mb_w, mb_h),
            segq: [sq; 4],
            seg_of: vec![0; mb_w * mb_h],
            cost_probs: COEFF_PROBS,
            base_probs: COEFF_PROBS,
            persist: false,
            top_modes: vec![0; mb_w * 4],
            left_modes: [0; 4],
            top_nz: vec![Nz::default(); mb_w],
            left_nz: Nz::default(),
            mbs: Vec::new(),
            mi: vec![MbInfo::default(); (mb_w + 1) * (mb_h + 1)],
            mi_stride: mb_w + 1,
            refs: [None, None, None, None],
            me: vec![None; mb_w * mb_h],
            mp: ModeProbs::default(),
        }
    }

    pub(crate) fn index(&self) -> i32 {
        self.segq[0].index
    }

    pub(crate) fn score(sse: u64, rate: u32, lam: u64) -> u64 {
        sse * 65_536 + u64::from(rate) * lam
    }

    /// Residual of a whole-block luma prediction: Y2 and sixteen AC blocks, the
    /// reconstruction, whether any DC survives and the squared error.
    #[allow(clippy::type_complexity)]
    pub(crate) fn luma16_residual(
        &self,
        mb_x: usize,
        mb_y: usize,
        pred: &[u8; 256],
        sq: &SegQ,
    ) -> ([i16; 16], [[i16; 16]; 16], [u8; 256], bool, u64) {
        let ys = self.src.ys;
        let origin = mb_y * 16 * ys + mb_x * 16;
        let mut coeffs = [[0i16; 16]; 16];
        let mut dcs = [0i16; 256];
        for b in 0..16 {
            let (bx, by) = (b % 4, b / 4);
            fdct(
                &self.src.y,
                origin + by * 4 * ys + bx * 4,
                ys,
                pred,
                by * 64 + bx * 4,
                16,
                &mut coeffs[b],
            );
            dcs[b * 16] = coeffs[b][0];
        }
        let mut w = [0i16; 16];
        fwht(&dcs, &mut w);
        let mut y2 = [0i16; 16];
        let mut y2deq = [0i16; 16];
        sq.y2.quantize(&w, 0, &mut y2, &mut y2deq);
        let mut dc_out = [0i16; 256];
        iwht(&y2deq, &mut dc_out);
        let mut y = [[0i16; 16]; 16];
        let mut recon = *pred;
        let mut dc_nonzero = false;
        for b in 0..16 {
            let mut deq = [0i16; 16];
            sq.y1.quantize(&coeffs[b], 1, &mut y[b], &mut deq);
            deq[0] = dc_out[b * 16];
            dc_nonzero |= deq[0] != 0;
            idct_add(&deq, &mut recon, (b / 4) * 64 + (b % 4) * 4, 16);
        }
        let mut dist = 0;
        for r in 0..16 {
            dist += sse(
                &self.src.y[origin + r * ys..origin + r * ys + 16],
                &recon[r * 16..r * 16 + 16],
            );
        }
        (y2, y, recon, dc_nonzero, dist)
    }

    /// Residual of a luma prediction coded as sixteen blocks that each carry their own DC.
    pub(crate) fn luma_split_residual(
        &self,
        mb_x: usize,
        mb_y: usize,
        pred: &[u8; 256],
        sq: &SegQ,
    ) -> ([[i16; 16]; 16], [u8; 256], u64) {
        let ys = self.src.ys;
        let origin = mb_y * 16 * ys + mb_x * 16;
        let mut y = [[0i16; 16]; 16];
        let mut recon = *pred;
        for b in 0..16 {
            let (bx, by) = (b % 4, b / 4);
            let mut c = [0i16; 16];
            fdct(
                &self.src.y,
                origin + by * 4 * ys + bx * 4,
                ys,
                pred,
                by * 64 + bx * 4,
                16,
                &mut c,
            );
            let mut deq = [0i16; 16];
            sq.y1.quantize(&c, 0, &mut y[b], &mut deq);
            idct_add(&deq, &mut recon, by * 64 + bx * 4, 16);
        }
        let mut dist = 0;
        for r in 0..16 {
            dist += sse(
                &self.src.y[origin + r * ys..origin + r * ys + 16],
                &recon[r * 16..r * 16 + 16],
            );
        }
        (y, recon, dist)
    }

    /// Residual of the two 8x8 chroma predictions.
    pub(crate) fn uv_residual(
        &self,
        mb_x: usize,
        mb_y: usize,
        preds: &[[u8; 64]; 2],
        sq: &SegQ,
    ) -> ([[i16; 16]; 8], [[u8; 64]; 2], u64) {
        let uvs = self.src.uvs;
        let origin = mb_y * 8 * uvs + mb_x * 8;
        let mut uv = [[0i16; 16]; 8];
        let mut recon = *preds;
        let mut dist = 0;
        for ch in 0..2 {
            let src = if ch == 0 { &self.src.u } else { &self.src.v };
            for b in 0..4 {
                let (bx, by) = (b % 2, b / 2);
                let mut c = [0i16; 16];
                fdct(
                    src,
                    origin + by * 4 * uvs + bx * 4,
                    uvs,
                    &preds[ch],
                    by * 32 + bx * 4,
                    8,
                    &mut c,
                );
                let mut deq = [0i16; 16];
                sq.uv.quantize(&c, 0, &mut uv[ch * 4 + b], &mut deq);
                idct_add(&deq, &mut recon[ch], by * 32 + bx * 4, 8);
            }
            for r in 0..8 {
                dist += sse(
                    &src[origin + r * uvs..origin + r * uvs + 8],
                    &recon[ch][r * 8..r * 8 + 8],
                );
            }
        }
        (uv, recon, dist)
    }

    fn eval_i16(
        &self,
        mb_x: usize,
        mb_y: usize,
        mode: u8,
        sq: &SegQ,
        edges: &([u8; 16], [u8; 16], u8),
    ) -> Cand16 {
        let mut pred = [0u8; 256];
        predict_block(
            mode,
            16,
            &edges.0,
            &edges.1,
            edges.2,
            mb_y > 0,
            mb_x > 0,
            &mut pred,
        );
        let (y2, y, recon, dc_nonzero, dist) = self.luma16_residual(mb_x, mb_y, &pred, sq);
        let mut cost = Cost::new(&self.cost_probs);
        if self.key {
            ymode_bits(mode, |b, p| cost.bit(b, p));
        } else {
            cost.bit(false, self.mp.intra);
            ymode_bits_inter(Some(mode), &self.mp.ymode, |b, p| cost.bit(b, p));
        }
        let mut top = self.top_nz[mb_x];
        let mut left = self.left_nz;
        walk_i16(&mut cost, &mut top, &mut left, &y2, &y);
        Cand16 {
            score: Self::score(dist, cost.total, sq.lam_y),
            mode,
            y2,
            y,
            recon,
            dc_nonzero,
        }
    }

    fn eval_uv(
        &self,
        mb_x: usize,
        mb_y: usize,
        mode: u8,
        sq: &SegQ,
        edges: &[([u8; 8], [u8; 8], u8); 2],
    ) -> CandUv {
        let mut preds = [[0u8; 64]; 2];
        for ch in 0..2 {
            let e = &edges[ch];
            predict_block(mode, 8, &e.0, &e.1, e.2, mb_y > 0, mb_x > 0, &mut preds[ch]);
        }
        let (uv, recon, dist) = self.uv_residual(mb_x, mb_y, &preds, sq);
        let mut cost = Cost::new(&self.cost_probs);
        if self.key {
            uvmode_bits(mode, |b, p| cost.bit(b, p));
        } else {
            uvmode_bits_inter(mode, &self.mp.uvmode, |b, p| cost.bit(b, p));
        }
        let mut top = self.top_nz[mb_x];
        let mut left = self.left_nz;
        walk_uv(&mut cost, &mut top, &mut left, &uv);
        CandUv {
            score: Self::score(dist, cost.total, sq.lam_uv),
            mode,
            uv,
            recon,
        }
    }

    /// The best of the ten modes for luma sub-block `b`.
    fn best_i4(
        &self,
        mb_x: usize,
        mb_y: usize,
        b: usize,
        tr: &[u8; 4],
        sq: &SegQ,
        modes: (u8, u8),
        ctx: usize,
    ) -> Cand4 {
        let ys = self.src.ys;
        let (top, left, x) = self.rec.i4_edges(mb_x, mb_y, b, tr);
        let origin = (mb_y * 16 + (b / 4) * 4) * ys + mb_x * 16 + (b % 4) * 4;
        let mut best: Option<Cand4> = None;
        for mode in 0..10u8 {
            let pred = predict_4x4(mode, x, &top, &left);
            let mut c = [0i16; 16];
            fdct(&self.src.y, origin, ys, &pred, 0, 4, &mut c);
            let mut levels = [0i16; 16];
            let mut deq = [0i16; 16];
            sq.y1.quantize(&c, 0, &mut levels, &mut deq);
            let mut recon = pred;
            idct_add(&deq, &mut recon, 0, 4);
            let mut dist = 0;
            for r in 0..4 {
                dist += sse(
                    &self.src.y[origin + r * ys..origin + r * ys + 4],
                    &recon[r * 4..r * 4 + 4],
                );
            }
            let mut cost = Cost::new(&self.cost_probs);
            crate::tokens::code_block(&mut cost, 3, ctx, 0, &levels);
            let mode_bits = if self.key {
                bmode_cost(modes.0, modes.1, mode)
            } else {
                let mut c = Cost::new(&self.cost_probs);
                bmode_bits_fixed(mode, |b, p| c.bit(b, p));
                c.total
            };
            let rate = cost.total + mode_bits;
            let score = Self::score(dist, rate, sq.lam_y);
            if best.as_ref().is_none_or(|bst| score < bst.score) {
                best = Some(Cand4 {
                    score,
                    mode,
                    levels,
                    recon,
                });
            }
        }
        best.unwrap_or(Cand4 {
            score: u64::MAX,
            mode: 0,
            levels: [0; 16],
            recon: [0; 16],
        })
    }

    pub(crate) fn run_pass(&mut self, progress: &dyn Progress) -> Result<()> {
        self.mbs.clear();
        self.top_modes.fill(0);
        self.top_nz.fill(Nz::default());
        for mb_y in 0..self.mb_h {
            progress.check_cancelled()?;
            progress.report(mb_y as u64, self.mb_h as u64, "encode");
            self.left_modes = [0; 4];
            self.left_nz = Nz::default();
            for mb_x in 0..self.mb_w {
                let mb = self.code_mb(mb_x, mb_y);
                self.mbs.push(mb);
            }
        }
        Ok(())
    }

    fn code_mb(&mut self, mb_x: usize, mb_y: usize) -> Mb {
        let segment = self.seg_of[mb_y * self.mb_w + mb_x];
        let sq = self.segq[usize::from(segment)];
        let idx = (mb_y + 1) * self.mi_stride + mb_x + 1;
        let edges16 = self.rec.luma_edges(mb_x, mb_y);
        let edges_uv = [
            self.rec.chroma_edges(0, mb_x, mb_y),
            self.rec.chroma_edges(1, mb_x, mb_y),
        ];

        let inter = if self.key {
            None
        } else {
            self.best_inter(mb_x, mb_y, &sq)
        };

        let mut best16: Option<Cand16> = None;
        for mode in 0..4u8 {
            let c = self.eval_i16(mb_x, mb_y, mode, &sq, &edges16);
            if best16.as_ref().is_none_or(|b| c.score < b.score) {
                best16 = Some(c);
            }
        }
        let best16 = best16.unwrap_or_else(unreachable_cand16);
        let mut best_uv: Option<CandUv> = None;
        for mode in 0..4u8 {
            let c = self.eval_uv(mb_x, mb_y, mode, &sq, &edges_uv);
            if best_uv.as_ref().is_none_or(|b| c.score < b.score) {
                best_uv = Some(c);
            }
        }
        let best_uv = best_uv.unwrap_or_else(unreachable_cand_uv);
        let intra16_total = best16.score.saturating_add(best_uv.score);

        // Sub-block prediction, block by block; each choice is reconstructed at once because
        // the next block predicts from it. Skipped when inter prediction already wins.
        let inter_wins = inter.as_ref().is_some_and(|c| c.score <= intra16_total);
        let mut i4: Option<([u8; 16], [[i16; 16]; 16])> = None;
        let mut i4_total = u64::MAX;
        if self.p.effort >= 1 && !inter_wins {
            let mut limit = best16.score;
            if let Some(c) = &inter {
                limit = limit.min(c.score.saturating_sub(best_uv.score));
            }
            let tr = self.rec.top_right(mb_x, mb_y);
            let mut modes = [0u8; 16];
            let mut levels = [[0i16; 16]; 16];
            let mut top = self.top_nz[mb_x];
            let mut left = self.left_nz;
            // Mode bits: the "sub-block" flag.
            let mut flag_bits = 0u32;
            if self.key {
                flag_bits += bitcost(false, 145);
            } else {
                flag_bits += bitcost(false, self.mp.intra);
                ymode_bits_inter(None, &self.mp.ymode, |b, p| flag_bits += bitcost(b, p));
            }
            let mut total = u64::from(flag_bits) * sq.lam_y;
            let mut viable = true;
            for b in 0..16 {
                let (bx, by) = (b % 4, b / 4);
                let above = if by == 0 {
                    self.top_modes[mb_x * 4 + bx]
                } else {
                    modes[b - 4]
                };
                let lm = if bx == 0 {
                    self.left_modes[by]
                } else {
                    modes[b - 1]
                };
                let ctx = usize::from(left.y[by]) + usize::from(top.y[bx]);
                let c = self.best_i4(mb_x, mb_y, b, &tr, &sq, (above, lm), ctx);
                total += c.score;
                modes[b] = c.mode;
                levels[b] = c.levels;
                let nz = c.levels.iter().any(|&l| l != 0);
                top.y[bx] = nz;
                left.y[by] = nz;
                self.rec.put_4x4(mb_x, mb_y, b, &c.recon);
                if total >= limit {
                    viable = false;
                    break;
                }
            }
            if viable {
                i4 = Some((modes, levels));
                i4_total = total;
            }
        }
        let intra_total = if i4.is_some() {
            i4_total.saturating_add(best_uv.score)
        } else {
            intra16_total
        };
        if let Some(c) = inter {
            if c.score <= intra_total {
                return self.finish_inter(mb_x, mb_y, idx, segment, c);
            }
        }

        let (is_i4, ymode, bmodes, y2, y, residual_dc) = match i4 {
            Some((modes, levels)) => (true, 0, modes, [0i16; 16], levels, false),
            None => {
                self.rec.put_luma(mb_x, mb_y, &best16.recon);
                (
                    false,
                    best16.mode,
                    [0u8; 16],
                    best16.y2,
                    best16.y,
                    best16.dc_nonzero,
                )
            }
        };
        self.rec.put_chroma(0, mb_x, mb_y, &best_uv.recon[0]);
        self.rec.put_chroma(1, mb_x, mb_y, &best_uv.recon[1]);

        // Contexts for the following macroblocks.
        let mut top = self.top_nz[mb_x];
        let mut left = self.left_nz;
        if is_i4 {
            walk_i4(&mut Null, &mut top, &mut left, &y);
            for bx in 0..4 {
                self.top_modes[mb_x * 4 + bx] = bmodes[12 + bx];
            }
            for by in 0..4 {
                self.left_modes[by] = bmodes[by * 4 + 3];
            }
        } else {
            walk_i16(&mut Null, &mut top, &mut left, &y2, &y);
            self.top_modes[mb_x * 4..mb_x * 4 + 4].fill(ymode);
            self.left_modes.fill(ymode);
        }
        walk_uv(&mut Null, &mut top, &mut left, &best_uv.uv);
        self.top_nz[mb_x] = top;
        self.left_nz = left;
        self.mi[idx] = MbInfo {
            mode: if is_i4 { crate::inter::M_BPRED } else { ymode },
            ..MbInfo::default()
        };

        let any_y = y.iter().any(|b| b.iter().any(|&l| l != 0));
        let any_uv = best_uv.uv.iter().any(|b| b.iter().any(|&l| l != 0));
        let any_y2 = y2.iter().any(|&l| l != 0);
        Mb {
            segment,
            is_i4,
            ymode,
            bmodes,
            uvmode: best_uv.mode,
            y2,
            y,
            uv: best_uv.uv,
            skip: !(any_y || any_uv || any_y2),
            residual: any_y || any_uv || residual_dc,
            inter: false,
            ref_frame: 0,
            imode: 0,
            mv: Mv::ZERO,
            bmv: [Mv::ZERO; 16],
            shape: 0,
        }
    }

    fn finish_inter(
        &mut self,
        mb_x: usize,
        mb_y: usize,
        idx: usize,
        segment: u8,
        c: crate::enc_inter::CandInter,
    ) -> Mb {
        self.rec.put_luma(mb_x, mb_y, &c.recon_y);
        self.rec.put_chroma(0, mb_x, mb_y, &c.recon_uv[0]);
        self.rec.put_chroma(1, mb_x, mb_y, &c.recon_uv[1]);
        let mut top = self.top_nz[mb_x];
        let mut left = self.left_nz;
        if c.no_y2 {
            walk_i4(&mut Null, &mut top, &mut left, &c.y);
        } else {
            walk_i16(&mut Null, &mut top, &mut left, &c.y2, &c.y);
        }
        walk_uv(&mut Null, &mut top, &mut left, &c.uv);
        self.top_nz[mb_x] = top;
        self.left_nz = left;
        self.mi[idx] = c.info;
        Mb {
            segment,
            is_i4: c.no_y2,
            ymode: 0,
            bmodes: [0; 16],
            uvmode: 0,
            y2: c.y2,
            y: c.y,
            uv: c.uv,
            skip: c.skip,
            residual: c.residual,
            inter: true,
            ref_frame: c.info.ref_frame,
            imode: c.info.mode,
            mv: c.info.mv,
            bmv: c.info.bmv,
            shape: c.shape,
        }
    }
}

fn unreachable_cand16() -> Cand16 {
    Cand16 {
        score: u64::MAX,
        mode: 0,
        y2: [0; 16],
        y: [[0; 16]; 16],
        recon: [0; 256],
        dc_nonzero: false,
    }
}

fn unreachable_cand_uv() -> CandUv {
    CandUv {
        score: u64::MAX,
        mode: 0,
        uv: [[0; 16]; 8],
        recon: [[0; 64]; 2],
    }
}

/// Branch cost of coding `n0` zeros and `n1` ones with probability `p`.
fn branch_cost(n0: u32, n1: u32, p: u8) -> u64 {
    u64::from(n0) * u64::from(bitcost(false, p)) + u64::from(n1) * u64::from(bitcost(true, p))
}

/// Chooses the probabilities to transmit: the optimal one per context when sending it pays
/// for its eight bits.
fn final_probs(stats: &Stats, base: &Probs) -> (Probs, Vec<(usize, usize, usize, usize)>) {
    let mut probs = *base;
    let mut updated = Vec::new();
    for t in 0..4 {
        for b in 0..8 {
            for c in 0..3 {
                for p in 0..11 {
                    let [n0, n1] = stats.counts[t][b][c][p];
                    let total = n0 + n1;
                    if total == 0 {
                        continue;
                    }
                    let new_p = if n1 == 0 {
                        255
                    } else {
                        (255 - (u64::from(n1) * 255 / u64::from(total)) as i64).clamp(1, 255) as u8
                    };
                    let old_p = base[t][b][c][p];
                    let up = COEFF_UPDATE_PROBS[t][b][c][p];
                    let old_cost = branch_cost(n0, n1, old_p) + u64::from(bitcost(false, up));
                    let new_cost =
                        branch_cost(n0, n1, new_p) + u64::from(bitcost(true, up)) + 8 * 256;
                    if old_cost > new_cost {
                        probs[t][b][c][p] = new_p;
                        updated.push((t, b, c, p));
                    }
                }
            }
        }
    }
    (probs, updated)
}

fn token_stats(mbs: &[Mb], mb_w: usize, use_skip: bool) -> Stats {
    let mut stats = Stats::default();
    let mut top = vec![Nz::default(); mb_w];
    let mut left = Nz::default();
    for (i, mb) in mbs.iter().enumerate() {
        let x = i % mb_w;
        if x == 0 {
            left = Nz::default();
        }
        if use_skip && mb.skip {
            reset_nz(&mut top[x], &mut left, mb.is_i4);
            continue;
        }
        walk_mb(&mut stats, mb, &mut top[x], &mut left);
    }
    stats
}

fn reset_nz(top: &mut Nz, left: &mut Nz, is_i4: bool) {
    let dc = (top.dc, left.dc);
    *top = Nz::default();
    *left = Nz::default();
    if is_i4 {
        top.dc = dc.0;
        left.dc = dc.1;
    }
}

fn walk_mb<S: Sink>(s: &mut S, mb: &Mb, top: &mut Nz, left: &mut Nz) {
    if mb.is_i4 {
        walk_i4(s, top, left, &mb.y);
    } else {
        walk_i16(s, top, left, &mb.y2, &mb.y);
    }
    walk_uv(s, top, left, &mb.uv);
}

fn plane_sse(
    a: &[u8],
    a_stride: usize,
    a_origin: usize,
    b: &[u8],
    b_stride: usize,
    w: usize,
    h: usize,
) -> u64 {
    let mut total = 0;
    for y in 0..h {
        total += sse(
            &a[a_origin + y * a_stride..a_origin + y * a_stride + w],
            &b[y * b_stride..y * b_stride + w],
        );
    }
    total
}

impl Core<'_> {
    fn filter_infos(&self, level: i32, sharpness: i32) -> Vec<FilterInfo> {
        let table = inter_strengths(self.key, level, sharpness, None, None);
        self.mbs
            .iter()
            .map(|mb| {
                let class = if mb.inter {
                    match mb.imode {
                        M_SPLIT => 3,
                        M_NEAREST | M_NEAR | M_NEW => 2,
                        _ => 1,
                    }
                } else if mb.is_i4 {
                    0
                } else {
                    1
                };
                let mut info = table[usize::from(mb.segment)][usize::from(mb.ref_frame)][class];
                info.inner = mb.is_i4 || mb.residual;
                info
            })
            .collect()
    }

    /// The reconstruction with the loop filter applied at `level`: what the next frame
    /// predicts from.
    pub(crate) fn filtered_planes(&self, level: i32, sharpness: i32) -> Planes {
        let infos = self.filter_infos(level, sharpness);
        let mut p = Planes::new(self.mb_w, self.mb_h);
        p.y.copy_from_slice(&self.rec.y);
        p.u.copy_from_slice(&self.rec.u);
        p.v.copy_from_slice(&self.rec.v);
        if level > 0 {
            let (ys, uvs) = (p.ys, p.uvs);
            let Planes { y, u, v, .. } = &mut p;
            let mut yp = PlaneMut {
                data: y,
                stride: ys,
                origin: ys + 1,
            };
            let mut up = PlaneMut {
                data: u,
                stride: uvs,
                origin: uvs + 1,
            };
            let mut vp = PlaneMut {
                data: v,
                stride: uvs,
                origin: uvs + 1,
            };
            for mb_y in 0..self.mb_h {
                for mb_x in 0..self.mb_w {
                    filter_mb(
                        false,
                        infos[mb_y * self.mb_w + mb_x],
                        mb_x,
                        mb_y,
                        &mut yp,
                        &mut up,
                        &mut vp,
                    );
                }
            }
        }
        p
    }

    /// Squared error against the source of the reconstruction filtered at `level`.
    fn filtered_error(&self, level: i32, sharpness: i32, width: usize, height: usize) -> u64 {
        let p = self.filtered_planes(level, sharpness);
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let (ys, uvs) = (p.ys, p.uvs);
        plane_sse(&p.y, ys, ys + 1, &self.src.y, self.src.ys, width, height)
            + plane_sse(&p.u, uvs, uvs + 1, &self.src.u, self.src.uvs, cw, ch)
            + plane_sse(&p.v, uvs, uvs + 1, &self.src.v, self.src.uvs, cw, ch)
    }

    /// Errors for several levels, evaluated on as many threads as allowed.
    fn errors_at(&self, levels: &[i32], sharp: i32, width: usize, height: usize) -> Vec<u64> {
        if self.threads <= 1 || levels.len() <= 1 {
            return levels
                .iter()
                .map(|&l| self.filtered_error(l, sharp, width, height))
                .collect();
        }
        let mut out = vec![0u64; levels.len()];
        let per = levels.len().div_ceil(self.threads);
        std::thread::scope(|s| {
            for (lv, res) in levels.chunks(per).zip(out.chunks_mut(per)) {
                s.spawn(move || {
                    for (l, r) in lv.iter().zip(res.iter_mut()) {
                        *r = self.filtered_error(*l, sharp, width, height);
                    }
                });
            }
        });
        out
    }

    fn choose_filter_level(&self, width: usize, height: usize) -> i32 {
        let sharp = i32::from(self.p.filter_sharpness);
        if let Some(l) = self.p.filter_level {
            return i32::from(l.min(63));
        }
        if self.p.effort == 0 {
            // Cheap guess from the quantiser.
            return (self.segq[0].index * 3 / 8).clamp(0, 63);
        }
        let coarse: Vec<i32> = (0..=56).step_by(8).collect();
        let errs = self.errors_at(&coarse, sharp, width, height);
        let mut best = (u64::MAX, 0);
        for (&l, &e) in coarse.iter().zip(&errs) {
            if e < best.0 {
                best = (e, l);
            }
        }
        for step in [4, 2, 1] {
            let centre = best.1;
            let cands: Vec<i32> = [centre - step, centre + step]
                .into_iter()
                .filter(|l| (0..=63).contains(l))
                .collect();
            let errs = self.errors_at(&cands, sharp, width, height);
            for (&l, &e) in cands.iter().zip(&errs) {
                if e < best.0 {
                    best = (e, l);
                }
            }
        }
        best.1
    }
}

fn sizes_ok(width: usize, height: usize) -> Result<()> {
    if width == 0 || height == 0 || width > 16383 || height > 16383 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "VP8 frames are at most 16383 pixels on a side",
        ));
    }
    Ok(())
}

/// Encodes `frame` as a VP8 key frame (the payload of a WebP `VP8 ` chunk).
pub fn encode(frame: &Frame, params: &Params, progress: &dyn Progress) -> Result<Vec<u8>> {
    encode_inner(frame, params, progress).map(|(bytes, _)| bytes)
}

pub(crate) fn encode_inner(
    frame: &Frame,
    params: &Params,
    progress: &dyn Progress,
) -> Result<(Vec<u8>, Planes)> {
    let (w, h) = (frame.width, frame.height);
    sizes_ok(w, h)?;
    let (cw, ch) = (frame.chroma_width(), frame.chroma_height());
    if frame.y.len() != w * h || frame.u.len() != cw * ch || frame.v.len() != cw * ch {
        return Err(Error::new(
            ErrorCode::InternalError,
            "VP8 plane sizes do not match",
        ));
    }
    let mut core = Core::new(params, true, frame, 1);
    let out = encode_core(&mut core, w, h, true, progress)?;
    Ok((out.bytes, core.rec))
}

pub(crate) struct Encoded {
    pub bytes: Vec<u8>,
    pub level: i32,
    /// Mode probabilities observed in this frame, to cost the next one with.
    pub mp: ModeProbs,
    pub probs: Probs,
}

/// Codes every macroblock of `core` and writes the frame. `refresh_golden` makes the
/// frame replace the golden reference (inter frames; a key frame always does).
pub(crate) fn encode_core(
    core: &mut Core,
    w: usize,
    h: usize,
    refresh_golden: bool,
    progress: &dyn Progress,
) -> Result<Encoded> {
    let mb_w = core.mb_w;
    let mb_h = core.mb_h;
    let key = core.key;
    let params = core.p;

    let passes = if params.effort >= 4 && (key || params.effort >= 5) {
        2
    } else {
        1
    };
    for pass in 0..passes {
        core.rec = Planes::new(mb_w, mb_h);
        core.run_pass(progress)?;
        if pass + 1 < passes {
            let stats = token_stats(&core.mbs, mb_w, false);
            core.cost_probs = final_probs(&stats, &core.base_probs).0;
        }
    }

    // Skipped macroblocks, token probabilities.
    let nb_skip = core.mbs.iter().filter(|m| m.skip).count();
    let total = core.mbs.len();
    let skip_prob = (255 * (total - nb_skip) / total).clamp(1, 255) as u8;
    let use_skip = skip_prob < 250;
    let stats = token_stats(&core.mbs, mb_w, use_skip);
    let (probs, updated) = final_probs(&stats, &core.base_probs);

    let level = core.choose_filter_level(w, h);

    // Reference choice statistics.
    let n_inter = core.mbs.iter().filter(|m| m.inter).count();
    let n_golden = core
        .mbs
        .iter()
        .filter(|m| m.inter && m.ref_frame == crate::inter::GOLDEN_FRAME)
        .count();
    let ratio = |zeros: usize, all: usize| -> u8 {
        (255 * zeros)
            .checked_div(all)
            .map_or(128, |v| v.clamp(1, 255) as u8)
    };
    let mp = ModeProbs {
        intra: ratio(total - n_inter, total),
        last: ratio(n_inter - n_golden, n_inter),
        golden: 255,
        ..ModeProbs::default()
    };

    // First partition: header, then the modes of every macroblock.
    let mut bw = BoolWriter::new();
    if key {
        bw.put_bit(false); // colour space
        bw.put_bit(false); // clamping required
    }
    bw.put_bit(false); // no segmentation
    bw.put_bit(false); // normal filter
    bw.put_value(level as u32, 6);
    bw.put_value(u32::from(params.filter_sharpness.min(7)), 3);
    bw.put_bit(false); // no filter deltas
    let parts_log2 = u32::from(params.partitions_log2.min(3));
    bw.put_value(parts_log2, 2);
    bw.put_value(core.index() as u32, 7);
    for _ in 0..5 {
        bw.put_bit(false); // no quantiser deltas
    }
    if !key {
        bw.put_bit(refresh_golden);
        bw.put_bit(false); // no alternate reference refresh
        if !refresh_golden {
            bw.put_value(0, 2); // no copy to golden
        }
        bw.put_value(0, 2); // no copy to the alternate reference
        bw.put_bit(false); // sign bias of golden
        bw.put_bit(false); // sign bias of the alternate reference
    }
    bw.put_bit(core.persist); // keep the probabilities for the next frame?
    if !key {
        bw.put_bit(true); // refresh the last frame
    }
    for t in 0..4 {
        for b in 0..8 {
            for c in 0..3 {
                for p in 0..11 {
                    let up = updated.contains(&(t, b, c, p));
                    bw.put(up, COEFF_UPDATE_PROBS[t][b][c][p]);
                    if up {
                        bw.put_value(u32::from(probs[t][b][c][p]), 8);
                    }
                }
            }
        }
    }
    bw.put_bit(use_skip);
    if use_skip {
        bw.put_value(u32::from(skip_prob), 8);
    }
    if !key {
        bw.put_value(u32::from(mp.intra), 8);
        bw.put_value(u32::from(mp.last), 8);
        bw.put_value(u32::from(mp.golden), 8);
        bw.put_bit(false); // luma mode probabilities stay
        bw.put_bit(false); // chroma mode probabilities stay
        for i in 0..2 {
            for j in 0..MV_PROB_COUNT {
                bw.put(false, MV_UPDATE_PROBS[i][j]);
            }
        }
    }
    let mut top_modes = vec![0u8; mb_w * 4];
    let mut left_modes = [0u8; 4];
    for (i, mb) in core.mbs.iter().enumerate() {
        let (x, y) = (i % mb_w, i / mb_w);
        if x == 0 {
            left_modes = [0; 4];
        }
        if use_skip {
            bw.put(mb.skip, skip_prob);
        }
        if !key {
            bw.put(mb.inter, mp.intra);
            if mb.inter {
                let idx = (y + 1) * core.mi_stride + x + 1;
                crate::enc_inter::emit_inter(
                    &core.mi,
                    idx,
                    core.mi_stride,
                    (x, y, mb_w, mb_h),
                    mb,
                    &mp,
                    &mut |b, p| bw.put(b, p),
                );
            } else {
                if mb.is_i4 {
                    ymode_bits_inter(None, &mp.ymode, |b, p| bw.put(b, p));
                    for b in 0..16 {
                        bmode_bits_fixed(mb.bmodes[b], |bit, p| bw.put(bit, p));
                    }
                } else {
                    ymode_bits_inter(Some(mb.ymode), &mp.ymode, |b, p| bw.put(b, p));
                }
                uvmode_bits_inter(mb.uvmode, &mp.uvmode, |b, p| bw.put(b, p));
            }
            continue;
        }
        if mb.is_i4 {
            bw.put(false, 145);
            for by in 0..4 {
                for bx in 0..4 {
                    let above = top_modes[x * 4 + bx];
                    let left = left_modes[by];
                    let mode = mb.bmodes[by * 4 + bx];
                    put_bmode(&mut bw, above, left, mode);
                    top_modes[x * 4 + bx] = mode;
                    left_modes[by] = mode;
                }
            }
        } else {
            ymode_bits(mb.ymode, |b, p| bw.put(b, p));
            top_modes[x * 4..x * 4 + 4].fill(mb.ymode);
            left_modes.fill(mb.ymode);
        }
        uvmode_bits(mb.uvmode, |b, p| bw.put(b, p));
    }
    let part0 = bw.finish();
    if part0.len() >= 1 << 19 {
        return Err(Error::new(
            ErrorCode::LimitExceeded,
            "VP8 mode data exceeds the 512 KiB first-partition limit",
        ));
    }

    // Token partitions.
    let n_parts = 1usize << parts_log2;
    let mut writers: Vec<BoolWriter> = (0..n_parts).map(|_| BoolWriter::new()).collect();
    let mut top = vec![Nz::default(); mb_w];
    let mut left = Nz::default();
    for (i, mb) in core.mbs.iter().enumerate() {
        let (x, y) = (i % mb_w, i / mb_w);
        if x == 0 {
            left = Nz::default();
        }
        if use_skip && mb.skip {
            reset_nz(&mut top[x], &mut left, mb.is_i4);
            continue;
        }
        let mut sink = Writer {
            bw: &mut writers[y & (n_parts - 1)],
            probs: &probs,
        };
        walk_mb(&mut sink, mb, &mut top[x], &mut left);
    }
    let parts: Vec<Vec<u8>> = writers.into_iter().map(BoolWriter::finish).collect();

    let mut out = Vec::new();
    if key {
        let tag = (1u32 << 4) | ((part0.len() as u32) << 5);
        out.extend_from_slice(&tag.to_le_bytes()[..3]);
        out.extend_from_slice(&[0x9d, 0x01, 0x2a]);
        out.extend_from_slice(&(w as u16).to_le_bytes());
        out.extend_from_slice(&(h as u16).to_le_bytes());
    } else {
        let tag = 1u32 | (1 << 4) | ((part0.len() as u32) << 5);
        out.extend_from_slice(&tag.to_le_bytes()[..3]);
    }
    out.extend_from_slice(&part0);
    for p in &parts[..n_parts - 1] {
        out.extend_from_slice(&(p.len() as u32).to_le_bytes()[..3]);
    }
    for p in &parts {
        out.extend_from_slice(p);
    }
    progress.report(mb_h as u64, mb_h as u64, "encode");
    let _ = BANDS;
    Ok(Encoded {
        bytes: out,
        level,
        mp,
        probs,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use oc_core::NoProgress;

    /// A deterministic picture with smooth areas, edges and noise.
    pub(crate) fn test_frame(w: usize, h: usize, seed: u32) -> Frame {
        let mut s = seed;
        let mut noise = move || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((s >> 24) as i32 % 9) - 4
        };
        let mut y = Vec::new();
        for r in 0..h {
            for c in 0..w {
                let base = (c * 255 / w.max(1)) as i32 / 2 + (r * 255 / h.max(1)) as i32 / 3;
                let edge = if (c / 7 + r / 5) % 3 == 0 { 60 } else { 0 };
                y.push((base + edge + noise()).clamp(0, 255) as u8);
            }
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut u = Vec::new();
        let mut v = Vec::new();
        for r in 0..ch {
            for c in 0..cw {
                u.push((100 + c * 80 / cw.max(1) + (noise() as usize & 3)) as u8);
                v.push((140 + r * 60 / ch.max(1) + (noise() as usize & 3)) as u8);
            }
        }
        Frame {
            width: w,
            height: h,
            y,
            u,
            v,
        }
    }

    fn cropped(p: &Planes, w: usize, h: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for (plane, stride, pw, ph) in [
            (&p.y, p.ys, w, h),
            (&p.u, p.uvs, w.div_ceil(2), h.div_ceil(2)),
            (&p.v, p.uvs, w.div_ceil(2), h.div_ceil(2)),
        ] {
            for r in 0..ph {
                let s = (r + 1) * stride + 1;
                out.extend_from_slice(&plane[s..s + pw]);
            }
        }
        out
    }

    #[test]
    fn decoder_reproduces_the_encoders_reconstruction() {
        for (w, h) in [(16, 16), (70, 50), (33, 17), (1, 1), (5, 40)] {
            let frame = test_frame(w, h, 3);
            for effort in [0, 2, 4, 6] {
                for quality in [10.0, 60.0, 95.0] {
                    for parts in [0, 2] {
                        let params = Params {
                            quality,
                            effort,
                            filter_level: Some(0),
                            partitions_log2: parts,
                            ..Params::default()
                        };
                        let (bytes, rec) = encode_inner(&frame, &params, &NoProgress).unwrap();
                        let dec = crate::decode(&bytes, &NoProgress)
                            .unwrap_or_else(|e| panic!("{w}x{h} e{effort} q{quality}: {e}"));
                        let mut got = dec.y.clone();
                        got.extend_from_slice(&dec.u);
                        got.extend_from_slice(&dec.v);
                        assert_eq!(
                            got,
                            cropped(&rec, w, h),
                            "{w}x{h} effort {effort} quality {quality} parts {parts}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn higher_quality_is_closer_and_larger() {
        let frame = test_frame(96, 64, 9);
        let mut last = (u64::MAX, 0usize);
        for quality in [5.0, 30.0, 60.0, 90.0] {
            let params = Params {
                quality,
                ..Params::default()
            };
            let bytes = encode(&frame, &params, &NoProgress).unwrap();
            let dec = crate::decode(&bytes, &NoProgress).unwrap();
            let err = sse(&dec.y, &frame.y);
            assert!(
                err <= last.0,
                "quality {quality}: error {err} after {}",
                last.0
            );
            assert!(bytes.len() >= last.1 || last.1 == 0, "quality {quality}");
            last = (err, bytes.len());
        }
    }
}
