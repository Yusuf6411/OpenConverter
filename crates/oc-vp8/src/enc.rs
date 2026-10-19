//! VP8 key-frame encoder.
//!
//! Every macroblock is coded with the intra mode, 16x16 or sub-block, that minimises
//! distortion plus a rate term estimated from the very probabilities that end up in the
//! stream. The encoder reconstructs with the decoder's own prediction and transform code, so
//! what it predicts from is exactly what a decoder will have.
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use crate::bool::BoolWriter;
use crate::dec::Frame;
use crate::filter::{filter_mb, strengths, FilterInfo, PlaneMut};
use crate::predict::{predict_4x4, predict_block, DC_PRED, H_PRED, V_PRED};
use crate::recon::Planes;
use crate::tables::{AC_TABLE, BANDS, COEFF_PROBS, COEFF_UPDATE_PROBS, DC_TABLE, ZIGZAG};
use crate::tokens::{
    bmode_cost, cost_table, put_bmode, walk_i16, walk_i4, walk_uv, Cost, Nz, Probs, Sink, Stats,
    Writer,
};
use crate::transform::{fdct, fwht, idct_add, iwht};
use oc_core::{Error, ErrorCode, Progress, Result};

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
}

impl Default for Params {
    fn default() -> Self {
        Self {
            quality: 75.0,
            effort: 4,
            filter_level: None,
            filter_sharpness: 0,
            partitions_log2: 0,
        }
    }
}

const QFIX: u32 = 17;

/// Quantiser for one kind of block: steps for DC and AC, with the reciprocal and the
/// rounding offset (a dead zone: values just under half a step round down).
#[derive(Clone, Copy, Default)]
struct Matrix {
    q: [i32; 2],
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
    fn quantize(
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
struct SegQ {
    index: i32,
    y1: Matrix,
    y2: Matrix,
    uv: Matrix,
    lam_y: u64,
    lam_uv: u64,
}

/// Rate-distortion trade-off: squared error per bit is about this fraction of the squared
/// step (in percent, times 256 because rates are counted in 1/256 bit).
const LAMBDA_PERCENT: u64 = 500;

fn seg_q(index: i32) -> SegQ {
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
fn quality_to_index(quality: f32) -> i32 {
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
struct Mb {
    segment: u8,
    is_i4: bool,
    ymode: u8,
    bmodes: [u8; 16],
    uvmode: u8,
    y2: [i16; 16],
    y: [[i16; 16]; 16],
    uv: [[i16; 16]; 8],
    /// The macroblock codes no coefficient at all.
    skip: bool,
    /// As the decoder sees it: no coefficient and no DC from the Y2 block, which decides
    /// whether the loop filter treats the inside of the macroblock.
    residual: bool,
}

struct Src {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    ys: usize,
    uvs: usize,
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

fn bitcost(bit: bool, prob: u8) -> u32 {
    let p = usize::from(prob);
    u32::from(cost_table()[if bit { 256 - p } else { p }])
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

struct Core<'a> {
    p: &'a Params,
    mb_w: usize,
    mb_h: usize,
    src: Src,
    rec: Planes,
    segq: [SegQ; 4],
    seg_of: Vec<u8>,
    cost_probs: Probs,
    top_modes: Vec<u8>,
    left_modes: [u8; 4],
    top_nz: Vec<Nz>,
    left_nz: Nz,
    mbs: Vec<Mb>,
}

impl Core<'_> {
    fn score(sse: u64, rate: u32, lam: u64) -> u64 {
        sse * 65_536 + u64::from(rate) * lam
    }

    fn eval_i16(
        &self,
        mb_x: usize,
        mb_y: usize,
        mode: u8,
        sq: &SegQ,
        edges: &([u8; 16], [u8; 16], u8),
    ) -> Cand16 {
        let ys = self.src.ys;
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
        let origin = mb_y * 16 * ys + mb_x * 16;
        let mut coeffs = [[0i16; 16]; 16];
        let mut dcs = [0i16; 256];
        for b in 0..16 {
            let (bx, by) = (b % 4, b / 4);
            fdct(
                &self.src.y,
                origin + by * 4 * ys + bx * 4,
                ys,
                &pred,
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
        let mut recon = pred;
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
        let mut cost = Cost::new(&self.cost_probs);
        ymode_bits(mode, |b, p| cost.bit(b, p));
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
        let uvs = self.src.uvs;
        let origin = mb_y * 8 * uvs + mb_x * 8;
        let mut uv = [[0i16; 16]; 8];
        let mut recon = [[0u8; 64]; 2];
        let mut dist = 0;
        for ch in 0..2 {
            let src = if ch == 0 { &self.src.u } else { &self.src.v };
            let e = &edges[ch];
            let mut pred = [0u8; 64];
            predict_block(mode, 8, &e.0, &e.1, e.2, mb_y > 0, mb_x > 0, &mut pred);
            recon[ch] = pred;
            for b in 0..4 {
                let (bx, by) = (b % 2, b / 2);
                let mut c = [0i16; 16];
                fdct(
                    src,
                    origin + by * 4 * uvs + bx * 4,
                    uvs,
                    &pred,
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
        let mut cost = Cost::new(&self.cost_probs);
        uvmode_bits(mode, |b, p| cost.bit(b, p));
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
            let rate = cost.total + bmode_cost(modes.0, modes.1, mode);
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

    fn run_pass(&mut self, progress: &dyn Progress) -> Result<()> {
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
        let edges16 = self.rec.luma_edges(mb_x, mb_y);
        let edges_uv = [
            self.rec.chroma_edges(0, mb_x, mb_y),
            self.rec.chroma_edges(1, mb_x, mb_y),
        ];

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

        // Sub-block prediction, block by block; each choice is reconstructed at once because
        // the next block predicts from it.
        let mut i4: Option<([u8; 16], [[i16; 16]; 16])> = None;
        if self.p.effort >= 1 {
            let tr = self.rec.top_right(mb_x, mb_y);
            let mut modes = [0u8; 16];
            let mut levels = [[0i16; 16]; 16];
            let mut top = self.top_nz[mb_x];
            let mut left = self.left_nz;
            // Mode bits: the "sub-block" flag.
            let mut total = u64::from(bitcost(false, 145)) * sq.lam_y;
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
                if total >= best16.score {
                    viable = false;
                    break;
                }
            }
            if viable {
                i4 = Some((modes, levels));
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
fn final_probs(stats: &Stats) -> (Probs, Vec<(usize, usize, usize, usize)>) {
    let mut probs = COEFF_PROBS;
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
                    let old_p = COEFF_PROBS[t][b][c][p];
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
        let table = strengths(level, sharpness, None, None);
        self.mbs
            .iter()
            .map(|mb| {
                let mut info = table[usize::from(mb.segment)][usize::from(mb.is_i4)];
                info.inner |= mb.residual;
                info
            })
            .collect()
    }

    /// Squared error against the source of the reconstruction filtered at `level`.
    fn filtered_error(&self, level: i32, sharpness: i32, width: usize, height: usize) -> u64 {
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
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let (ys, uvs) = (p.ys, p.uvs);
        plane_sse(&p.y, ys, ys + 1, &self.src.y, self.src.ys, width, height)
            + plane_sse(&p.u, uvs, uvs + 1, &self.src.u, self.src.uvs, cw, ch)
            + plane_sse(&p.v, uvs, uvs + 1, &self.src.v, self.src.uvs, cw, ch)
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
        let mut best = (u64::MAX, 0);
        let eval = |l: i32, best: &mut (u64, i32)| {
            let e = self.filtered_error(l, sharp, width, height);
            if e < best.0 {
                *best = (e, l);
            }
        };
        for l in (0..=56).step_by(8) {
            eval(l, &mut best);
        }
        for step in [4, 2, 1] {
            let centre = best.1;
            for l in [centre - step, centre + step] {
                if (0..=63).contains(&l) {
                    eval(l, &mut best);
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
    let mb_w = w.div_ceil(16);
    let mb_h = h.div_ceil(16);
    let src = Src {
        y: pad_plane(&frame.y, w, h, mb_w * 16, mb_h * 16),
        u: pad_plane(&frame.u, cw, ch, mb_w * 8, mb_h * 8),
        v: pad_plane(&frame.v, cw, ch, mb_w * 8, mb_h * 8),
        ys: mb_w * 16,
        uvs: mb_w * 8,
    };
    let index = quality_to_index(params.quality);
    let sq = seg_q(index);
    let mut core = Core {
        p: params,
        mb_w,
        mb_h,
        src,
        rec: Planes::new(mb_w, mb_h),
        segq: [sq; 4],
        seg_of: vec![0; mb_w * mb_h],
        cost_probs: COEFF_PROBS,
        top_modes: vec![0; mb_w * 4],
        left_modes: [0; 4],
        top_nz: vec![Nz::default(); mb_w],
        left_nz: Nz::default(),
        mbs: Vec::new(),
    };

    let passes = if params.effort >= 4 { 2 } else { 1 };
    for pass in 0..passes {
        core.rec = Planes::new(mb_w, mb_h);
        core.run_pass(progress)?;
        if pass + 1 < passes {
            let stats = token_stats(&core.mbs, mb_w, false);
            core.cost_probs = final_probs(&stats).0;
        }
    }

    // Skipped macroblocks, token probabilities.
    let nb_skip = core.mbs.iter().filter(|m| m.skip).count();
    let total = core.mbs.len();
    let skip_prob = (255 * (total - nb_skip) / total).clamp(1, 255) as u8;
    let use_skip = skip_prob < 250;
    let stats = token_stats(&core.mbs, mb_w, use_skip);
    let (probs, updated) = final_probs(&stats);

    let level = core.choose_filter_level(w, h);

    // First partition: header, then the modes of every macroblock.
    let mut bw = BoolWriter::new();
    bw.put_bit(false); // colour space
    bw.put_bit(false); // clamping required
    bw.put_bit(false); // no segmentation
    bw.put_bit(false); // normal filter
    bw.put_value(level as u32, 6);
    bw.put_value(u32::from(params.filter_sharpness.min(7)), 3);
    bw.put_bit(false); // no filter deltas
    let parts_log2 = u32::from(params.partitions_log2.min(3));
    bw.put_value(parts_log2, 2);
    bw.put_value(index as u32, 7);
    for _ in 0..5 {
        bw.put_bit(false); // no quantiser deltas
    }
    bw.put_bit(false); // do not keep the probabilities for the next frame
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
    let mut top_modes = vec![0u8; mb_w * 4];
    let mut left_modes = [0u8; 4];
    for (i, mb) in core.mbs.iter().enumerate() {
        let x = i % mb_w;
        if x == 0 {
            left_modes = [0; 4];
        }
        if use_skip {
            bw.put(mb.skip, skip_prob);
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
    let tag = (1u32 << 4) | ((part0.len() as u32) << 5);
    out.extend_from_slice(&tag.to_le_bytes()[..3]);
    out.extend_from_slice(&[0x9d, 0x01, 0x2a]);
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.extend_from_slice(&part0);
    for p in &parts[..n_parts - 1] {
        out.extend_from_slice(&(p.len() as u32).to_le_bytes()[..3]);
    }
    for p in &parts {
        out.extend_from_slice(p);
    }
    progress.report(mb_h as u64, mb_h as u64, "encode");
    let _ = BANDS;
    Ok((out, core.rec))
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
