//! Inter-frame decisions of the encoder: motion search, the candidates an inter macroblock may
//! be coded with, and the bits that describe them.
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use crate::enc::{Core, Mb, ModeProbs, SegQ, Src};
use crate::inter::{
    find_near, mc_block, predict_mb, sub_mv_context, Bounds, MbInfo, Mv, RefPlane, ALTREF_FRAME,
    GOLDEN_FRAME, LAST_FRAME, M_NEAR, M_NEAREST, M_NEW, M_SPLIT, M_ZERO, SPLIT_COUNT, SPLIT_LAYOUT,
    SPLIT_PROBS, SUB_MV_REF_PROBS,
};
use crate::tokens::{walk_i16, walk_i4, walk_uv, Cost};
use std::sync::Arc;

pub(crate) struct CandInter {
    pub score: u64,
    pub info: MbInfo,
    pub shape: u8,
    pub y2: [i16; 16],
    pub y: [[i16; 16]; 16],
    pub uv: [[i16; 16]; 8],
    pub recon_y: [u8; 256],
    pub recon_uv: [[u8; 64]; 2],
    pub no_y2: bool,
    pub skip: bool,
    pub residual: bool,
}

fn emit_mv_component(v: i32, p: &[u8; 19], f: &mut dyn FnMut(bool, u8)) {
    let x = v.abs();
    if x < 8 {
        f(false, p[0]);
        let hi = x >= 4;
        f(hi, p[2]);
        if !hi {
            let b1 = x >= 2;
            f(b1, p[3]);
            f(x & 1 == 1, if b1 { p[5] } else { p[4] });
        } else {
            let b1 = x >= 6;
            f(b1, p[6]);
            f(x & 1 == 1, if b1 { p[8] } else { p[7] });
        }
    } else {
        f(true, p[0]);
        for i in 0..3 {
            f((x >> i) & 1 == 1, p[9 + i]);
        }
        for i in (4..10).rev() {
            f((x >> i) & 1 == 1, p[9 + i]);
        }
        if x & 0xfff0 != 0 {
            f((x >> 3) & 1 == 1, p[9 + 3]);
        }
    }
    if x != 0 {
        f(v < 0, p[1]);
    }
}

fn emit_mv(d: Mv, mp: &ModeProbs, f: &mut dyn FnMut(bool, u8)) {
    emit_mv_component(d.row / 2, &mp.mv[0], f);
    emit_mv_component(d.col / 2, &mp.mv[1], f);
}

/// The vector difference must fit the coder: at most 1023 quarter samples a side.
fn delta_fits(d: Mv) -> bool {
    d.row % 2 == 0 && d.col % 2 == 0 && (d.row / 2).abs() <= 1023 && (d.col / 2).abs() <= 1023
}

/// Everything after the intra flag of an inter macroblock: reference, mode and vectors.
pub(crate) fn emit_modes(
    mi: &[MbInfo],
    idx: usize,
    stride: usize,
    geom: (usize, usize, usize, usize),
    info: &MbInfo,
    shape: u8,
    mp: &ModeProbs,
    f: &mut dyn FnMut(bool, u8),
) {
    let (x, y, mb_w, mb_h) = geom;
    let bounds = Bounds::new(x, y, mb_w, mb_h);
    let near = find_near(mi, idx, stride, info.ref_frame, &[false; 4], &bounds);
    f(info.ref_frame != LAST_FRAME, mp.last);
    if info.ref_frame != LAST_FRAME {
        f(info.ref_frame == ALTREF_FRAME, mp.golden);
    }
    let p = near.probs;
    match info.mode {
        M_ZERO => f(false, p[0]),
        M_NEAREST => {
            f(true, p[0]);
            f(false, p[1]);
        }
        M_NEAR => {
            f(true, p[0]);
            f(true, p[1]);
            f(false, p[2]);
        }
        M_NEW => {
            f(true, p[0]);
            f(true, p[1]);
            f(true, p[2]);
            f(false, p[3]);
            emit_mv(
                Mv {
                    row: info.mv.row - near.best.row,
                    col: info.mv.col - near.best.col,
                },
                mp,
                f,
            );
        }
        _ => {
            f(true, p[0]);
            f(true, p[1]);
            f(true, p[2]);
            f(true, p[3]);
            match shape {
                3 => f(false, SPLIT_PROBS[0]),
                2 => {
                    f(true, SPLIT_PROBS[0]);
                    f(false, SPLIT_PROBS[1]);
                }
                0 => {
                    f(true, SPLIT_PROBS[0]);
                    f(true, SPLIT_PROBS[1]);
                    f(false, SPLIT_PROBS[2]);
                }
                _ => {
                    f(true, SPLIT_PROBS[0]);
                    f(true, SPLIT_PROBS[1]);
                    f(true, SPLIT_PROBS[2]);
                }
            }
            let layout = &SPLIT_LAYOUT[usize::from(shape)];
            let mut bmv = [Mv::ZERO; 16];
            for part in 0..SPLIT_COUNT[usize::from(shape)] {
                let k = layout
                    .iter()
                    .position(|&q| usize::from(q) == part)
                    .unwrap_or(0);
                let l = if k & 3 == 0 {
                    mi[idx - 1].bmv[k + 3]
                } else {
                    bmv[k - 1]
                };
                let a = if k < 4 {
                    mi[idx - stride].bmv[k + 12]
                } else {
                    bmv[k - 4]
                };
                let pr = &SUB_MV_REF_PROBS[sub_mv_context(l, a)];
                let mv = info.bmv[k];
                if mv == l {
                    f(false, pr[0]);
                } else if mv == a {
                    f(true, pr[0]);
                    f(false, pr[1]);
                } else if mv.is_zero() {
                    f(true, pr[0]);
                    f(true, pr[1]);
                    f(false, pr[2]);
                } else {
                    f(true, pr[0]);
                    f(true, pr[1]);
                    f(true, pr[2]);
                    emit_mv(
                        Mv {
                            row: mv.row - near.best.row,
                            col: mv.col - near.best.col,
                        },
                        mp,
                        f,
                    );
                }
                for (b, &q) in layout.iter().enumerate() {
                    if usize::from(q) == part {
                        bmv[b] = mv;
                    }
                }
            }
        }
    }
}

/// Writes the mode of a coded inter macroblock.
pub(crate) fn emit_inter(
    mi: &[MbInfo],
    idx: usize,
    stride: usize,
    geom: (usize, usize, usize, usize),
    mb: &Mb,
    mp: &ModeProbs,
    f: &mut dyn FnMut(bool, u8),
) {
    let info = MbInfo {
        ref_frame: mb.ref_frame,
        mode: mb.imode,
        mv: mb.mv,
        bmv: mb.bmv,
    };
    emit_modes(mi, idx, stride, geom, &info, mb.shape, mp, f);
}

/// How far outside the picture, in samples, a part of a split macroblock may point.
const EDGE_SLACK: i32 = 4;

fn mv_bits(d: Mv) -> u64 {
    let one = |v: i32| -> u64 {
        let q = (v.abs() / 2) as u32;
        if q == 0 {
            1
        } else {
            2 * u64::from(32 - q.leading_zeros()) + 1
        }
    };
    one(d.row) + one(d.col)
}

fn sub(a: Mv, b: Mv) -> Mv {
    Mv {
        row: a.row - b.row,
        col: a.col - b.col,
    }
}

fn sad_block(
    src: &Src,
    sx: usize,
    sy: usize,
    r: &RefPlane,
    rx: i32,
    ry: i32,
    size: usize,
    limit: u64,
) -> u64 {
    let mut total = 0u64;
    let inside = rx >= 0 && ry >= 0 && rx + size as i32 <= r.w && ry + size as i32 <= r.h;
    for j in 0..size {
        let srow = &src.y[(sy + j) * src.ys + sx..(sy + j) * src.ys + sx + size];
        if inside {
            let o = (ry as usize + j + 1) * r.stride + rx as usize + 1;
            let rrow = &r.data[o..o + size];
            for (a, b) in srow.iter().zip(rrow) {
                total += u64::from(a.abs_diff(*b));
            }
        } else {
            for (i, a) in srow.iter().enumerate() {
                let x = (rx + i as i32).clamp(0, r.w - 1) as usize;
                let y = (ry + j as i32).clamp(0, r.h - 1) as usize;
                let b = r.data[(y + 1) * r.stride + x + 1];
                total += u64::from(a.abs_diff(b));
            }
        }
        if total > limit {
            return total;
        }
    }
    total
}

/// Squared error of the prediction of a `w` x `h` block at (`x`, `y`) displaced by `mv`.
fn sse_pred(src: &Src, r: &RefPlane, x: usize, y: usize, w: usize, h: usize, mv: Mv) -> u64 {
    let mut pred = [0u8; 256];
    mc_block(r, x as i32, y as i32, mv, w, h, false, &mut pred, 16);
    let mut total = 0u64;
    for j in 0..h {
        for i in 0..w {
            let d = i64::from(src.y[(y + j) * src.ys + x + i]) - i64::from(pred[j * 16 + i]);
            total += (d * d) as u64;
        }
    }
    total
}

/// Searches one macroblock: whole-sample steps on the sum of absolute differences, then
/// half and quarter samples on the squared error of the actual prediction.
fn search_mb(
    src: &Src,
    r: &RefPlane,
    mb_x: usize,
    mb_y: usize,
    mb_w: usize,
    mb_h: usize,
    starts: &[Mv],
    range: i32,
    ac_q: i32,
    subpel: bool,
) -> Mv {
    let (sx, sy) = (mb_x * 16, mb_y * 16);
    let pred = starts.first().copied().unwrap_or(Mv::ZERO);
    let lam_sad = u64::from((ac_q / 6).max(1) as u32);
    let lam_sse = u64::from((ac_q * ac_q / 40).max(1) as u32);
    let min_dx = (-((mb_x as i32 + 1) * 16)).max(-range);
    let max_dx = (((mb_w - mb_x) as i32) * 16).min(range);
    let min_dy = (-((mb_y as i32 + 1) * 16)).max(-range);
    let max_dy = (((mb_h - mb_y) as i32) * 16).min(range);
    let cost_fp = |dx: i32, dy: i32, limit: u64| -> u64 {
        let mv = Mv {
            row: dy * 8,
            col: dx * 8,
        };
        let bits = mv_bits(sub(mv, pred));
        let base = lam_sad * bits;
        base + sad_block(src, sx, sy, r, sx as i32 + dx, sy as i32 + dy, 16, limit)
    };
    let clampd = |dx: i32, dy: i32| (dx.clamp(min_dx, max_dx), dy.clamp(min_dy, max_dy));
    let mut best = (u64::MAX, 0i32, 0i32);
    let consider = |dx: i32, dy: i32, best: &mut (u64, i32, i32)| {
        let (dx, dy) = clampd(dx, dy);
        let c = cost_fp(dx, dy, best.0);
        if c < best.0 {
            *best = (c, dx, dy);
        }
    };
    consider(0, 0, &mut best);
    for s in starts {
        consider((s.col + 4) >> 3, (s.row + 4) >> 3, &mut best);
    }
    let mut step = 1;
    while step * 2 <= range / 2 && step < 16 {
        step *= 2;
    }
    while step >= 1 {
        for _ in 0..8 {
            let (c0, bx, by) = best;
            for (ox, oy) in [
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                consider(bx + ox * step, by + oy * step, &mut best);
            }
            if best.0 >= c0 {
                break;
            }
        }
        step /= 2;
    }
    let mut mv = Mv {
        row: best.2 * 8,
        col: best.1 * 8,
    };
    if !subpel {
        return mv;
    }
    let cost_sub =
        |mv: Mv| -> u64 { sse_pred(src, r, sx, sy, 16, 16, mv) + lam_sse * mv_bits(sub(mv, pred)) };
    let mut best_cost = cost_sub(mv);
    for step in [4, 2] {
        let centre = mv;
        for (oy, ox) in [
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (0, -1),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ] {
            let cand = Mv {
                row: centre.row + oy * step,
                col: centre.col + ox * step,
            };
            let (dx, dy) = ((cand.col >> 3), (cand.row >> 3));
            if dx < min_dx - 1 || dx > max_dx || dy < min_dy - 1 || dy > max_dy {
                continue;
            }
            let c = cost_sub(cand);
            if c < best_cost {
                best_cost = c;
                mv = cand;
            }
        }
    }
    mv
}

/// Fills `core.me` with the best whole-macroblock vector of every macroblock against the last
/// frame, spreading rows over the threads. `prev` holds the previous frame's vectors, which
/// serve as search starting points.
pub(crate) fn estimate_motion(core: &mut Core, prev: &[Mv]) {
    let Some(last) = core.refs[usize::from(LAST_FRAME)].clone() else {
        return;
    };
    let (mb_w, mb_h) = (core.mb_w, core.mb_h);
    let effort = core.p.effort;
    let range = match effort {
        0..=1 => 12,
        2..=3 => 24,
        _ => 40,
    };
    let subpel = effort >= 1;
    let ac_q = core.segq[0].ac_step();
    let threads = core.threads.min(mb_h).max(1);
    let src = &core.src;
    let mut rows: Vec<Vec<Mv>> = vec![Vec::new(); mb_h];
    let work = |t: usize| -> Vec<(usize, Vec<Mv>)> {
        let r = RefPlane::luma(&last);
        let mut out = Vec::new();
        for mb_y in (t..mb_h).step_by(threads) {
            let mut row = Vec::with_capacity(mb_w);
            let mut left = Mv::ZERO;
            for mb_x in 0..mb_w {
                let temporal = prev.get(mb_y * mb_w + mb_x).copied().unwrap_or(Mv::ZERO);
                let starts = [left, temporal];
                let mv = search_mb(
                    src, &r, mb_x, mb_y, mb_w, mb_h, &starts, range, ac_q, subpel,
                );
                row.push(mv);
                left = mv;
            }
            out.push((mb_y, row));
        }
        out
    };
    if threads == 1 {
        for (y, row) in work(0) {
            rows[y] = row;
        }
    } else {
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    let work = &work;
                    s.spawn(move || work(t))
                })
                .collect();
            for h in handles {
                if let Ok(part) = h.join() {
                    for (y, row) in part {
                        rows[y] = row;
                    }
                }
            }
        });
    }
    for (y, row) in rows.into_iter().enumerate() {
        for (x, mv) in row.into_iter().enumerate() {
            core.me[y * mb_w + x] = Some(mv);
        }
    }
}

impl SegQ {
    pub(crate) fn ac_step(&self) -> i32 {
        self.y1.q[1]
    }
}

impl Core<'_> {
    fn eval_cand(
        &self,
        mb_x: usize,
        mb_y: usize,
        idx: usize,
        info: MbInfo,
        shape: u8,
        sq: &SegQ,
    ) -> Option<CandInter> {
        let refp: &Arc<crate::recon::Planes> = self.refs[usize::from(info.ref_frame)].as_ref()?;
        let (ly, cb, cr) = predict_mb(refp, mb_x, mb_y, &info, false, false);
        let no_y2 = info.mode == M_SPLIT;
        let (mut y2, mut y, recon_y, dc_nonzero, dist_y) = if no_y2 {
            let (y, recon, dist) = self.luma_split_residual(mb_x, mb_y, &ly, sq);
            ([0i16; 16], y, recon, false, dist)
        } else {
            let (y2, y, recon, dc, dist) = self.luma16_residual(mb_x, mb_y, &ly, sq);
            (y2, y, recon, dc, dist)
        };
        let (uv, recon_uv, dist_uv) = self.uv_residual(mb_x, mb_y, &[cb, cr], sq);
        let any_y = y.iter().any(|b| b.iter().any(|&l| l != 0));
        let any_uv = uv.iter().any(|b| b.iter().any(|&l| l != 0));
        let any_y2 = y2.iter().any(|&l| l != 0);
        let skip = !(any_y || any_uv || any_y2);
        if skip {
            y2 = [0; 16];
            y = [[0; 16]; 16];
        }
        let mut cost = Cost::new(&self.cost_probs);
        cost.bit(true, self.mp.intra);
        let geom = (mb_x, mb_y, self.mb_w, self.mb_h);
        emit_modes(
            &self.mi,
            idx,
            self.mi_stride,
            geom,
            &info,
            shape,
            &self.mp,
            &mut |b, p| cost.bit(b, p),
        );
        if !skip {
            let mut top = self.top_nz[mb_x];
            let mut left = self.left_nz;
            if no_y2 {
                walk_i4(&mut cost, &mut top, &mut left, &y);
            } else {
                walk_i16(&mut cost, &mut top, &mut left, &y2, &y);
            }
            walk_uv(&mut cost, &mut top, &mut left, &uv);
        }
        Some(CandInter {
            score: Core::score(dist_y + dist_uv, cost.total, sq.lam_y),
            info,
            shape,
            y2,
            y,
            uv,
            recon_y,
            recon_uv,
            no_y2,
            skip,
            residual: any_y || any_uv || dc_nonzero,
        })
    }

    /// A split-vector candidate with the given shape, each part searched around `base`.
    fn split_info(
        &self,
        mb_x: usize,
        mb_y: usize,
        idx: usize,
        shape: usize,
        base: Mv,
        best_mv: Mv,
        bounds: &Bounds,
    ) -> Option<MbInfo> {
        let last = self.refs[usize::from(LAST_FRAME)].as_ref()?;
        let r = RefPlane::luma(last);
        let layout = &SPLIT_LAYOUT[shape];
        let ac_q = self.segq[0].ac_step();
        let lam_sse = u64::from((ac_q * ac_q / 40).max(1) as u32);
        let mut bmv = [Mv::ZERO; 16];
        for part in 0..SPLIT_COUNT[shape] {
            let blocks: Vec<usize> = (0..16)
                .filter(|&b| usize::from(layout[b]) == part)
                .collect();
            let k = *blocks.first()?;
            let l = if k & 3 == 0 {
                self.mi[idx - 1].bmv[k + 3]
            } else {
                bmv[k - 1]
            };
            let a = if k < 4 {
                self.mi[idx - self.mi_stride].bmv[k + 12]
            } else {
                bmv[k - 4]
            };
            let (bx0, by0) = (
                blocks.iter().map(|b| b % 4).min()?,
                blocks.iter().map(|b| b / 4).min()?,
            );
            let (bx1, by1) = (
                blocks.iter().map(|b| b % 4).max()? + 1,
                blocks.iter().map(|b| b / 4).max()? + 1,
            );
            let (x, y) = (mb_x * 16 + bx0 * 4, mb_y * 16 + by0 * 4);
            let (w, h) = ((bx1 - bx0) * 4, (by1 - by0) * 4);
            let (frame_w, frame_h) = (r.w, r.h);
            let cost = |mv: Mv| -> u64 {
                // Keep parts of a split macroblock near the picture: decoders disagree about
                // vectors that send them far outside it.
                let (px, py) = (x as i32 + (mv.col >> 3), y as i32 + (mv.row >> 3));
                if px < -EDGE_SLACK
                    || py < -EDGE_SLACK
                    || px + w as i32 > frame_w + EDGE_SLACK
                    || py + h as i32 > frame_h + EDGE_SLACK
                {
                    return u64::MAX;
                }
                let fits = delta_fits(sub(mv, best_mv));
                let bits = if mv == l || mv == a {
                    2
                } else if mv.is_zero() {
                    3
                } else if fits {
                    4 + mv_bits(sub(mv, best_mv))
                } else {
                    return u64::MAX;
                };
                sse_pred(self.src_ref(), &r, x, y, w, h, mv) + lam_sse * bits
            };
            let mut best = (cost(base), base);
            for cand in [l, a, Mv::ZERO, best_mv] {
                let c = cost(cand);
                if c < best.0 {
                    best = (c, cand);
                }
            }
            for step in [8, 4, 2] {
                let centre = best.1;
                for (oy, ox) in [
                    (-1, -1),
                    (-1, 0),
                    (-1, 1),
                    (0, -1),
                    (0, 1),
                    (1, -1),
                    (1, 0),
                    (1, 1),
                ] {
                    let cand = bounds.clamp(Mv {
                        row: centre.row + oy * step,
                        col: centre.col + ox * step,
                    });
                    if cand.row % 2 != 0 || cand.col % 2 != 0 {
                        continue;
                    }
                    let c = cost(cand);
                    if c < best.0 {
                        best = (c, cand);
                    }
                }
            }
            if best.0 == u64::MAX {
                return None;
            }
            for &b in &blocks {
                bmv[b] = best.1;
            }
        }
        Some(MbInfo {
            ref_frame: LAST_FRAME,
            mode: M_SPLIT,
            mv: bmv[15],
            bmv,
        })
    }

    fn src_ref(&self) -> &Src {
        &self.src
    }

    /// The cheapest inter coding of a macroblock, if inter prediction is possible at all.
    pub(crate) fn best_inter(&self, mb_x: usize, mb_y: usize, sq: &SegQ) -> Option<CandInter> {
        let last = self.refs[usize::from(LAST_FRAME)].as_ref()?;
        let idx = (mb_y + 1) * self.mi_stride + mb_x + 1;
        let bounds = Bounds::new(mb_x, mb_y, self.mb_w, self.mb_h);
        let me = self.me[mb_y * self.mb_w + mb_x];
        let mut best: Option<CandInter> = None;
        let offer = |c: Option<CandInter>, best: &mut Option<CandInter>| {
            if let Some(c) = c {
                if best.as_ref().is_none_or(|b| c.score < b.score) {
                    *best = Some(c);
                }
            }
        };
        let mut slots = vec![LAST_FRAME];
        if let Some(g) = &self.refs[usize::from(GOLDEN_FRAME)] {
            if !Arc::ptr_eq(g, last) {
                slots.push(GOLDEN_FRAME);
            }
        }
        let mut last_near = None;
        for &rf in &slots {
            let near = find_near(&self.mi, idx, self.mi_stride, rf, &[false; 4], &bounds);
            let mut list = vec![
                (M_ZERO, Mv::ZERO),
                (M_NEAREST, near.nearest),
                (M_NEAR, near.near),
            ];
            if rf == LAST_FRAME {
                if let Some(m) = me {
                    list.push((M_NEW, bounds.clamp(m)));
                }
            }
            let mut seen: Vec<Mv> = Vec::new();
            for (mode, mv) in list {
                if seen.contains(&mv) {
                    continue;
                }
                seen.push(mv);
                if mode == M_NEW && !delta_fits(sub(mv, near.best)) {
                    continue;
                }
                let info = MbInfo {
                    ref_frame: rf,
                    mode,
                    mv,
                    bmv: [mv; 16],
                };
                offer(self.eval_cand(mb_x, mb_y, idx, info, 0, sq), &mut best);
            }
            if rf == LAST_FRAME {
                last_near = Some(near.best);
            }
        }
        // A macroblock that already codes without residual has little left to gain.
        let settled = best.as_ref().is_some_and(|b| b.skip);
        if self.p.effort >= 3 && !settled {
            if let (Some(m), Some(best_mv)) = (me, last_near) {
                let shapes: &[usize] = if self.p.effort >= 5 {
                    &[0, 1, 2, 3]
                } else {
                    &[0, 1, 2]
                };
                for &shape in shapes {
                    if let Some(info) =
                        self.split_info(mb_x, mb_y, idx, shape, bounds.clamp(m), best_mv, &bounds)
                    {
                        offer(
                            self.eval_cand(mb_x, mb_y, idx, info, shape as u8, sq),
                            &mut best,
                        );
                    }
                }
            }
        }
        best
    }
}
