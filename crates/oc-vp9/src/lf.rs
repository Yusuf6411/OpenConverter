//! The loop filter (spec 8.8).
use crate::frame::FrameBuf;
use crate::hdr::{Hdr, Persist, SEG_LVL_ALT_L};
use crate::mi::Mi;
use crate::tables::*;

const BLOCK_16X16: u8 = 6;
const NEARESTMV: u8 = 10;
const NEARMV: u8 = 11;
const NEWMV: u8 = 13;

/// `LvlLookup[segment][ref][mode type]` (8.8.1).
pub fn level_lookup(h: &Hdr, p: &Persist) -> [[[u8; 2]; 4]; 8] {
    let mut out = [[[0u8; 2]; 4]; 8];
    let n_shift = h.lf_level >> 5;
    for (seg, o) in out.iter_mut().enumerate() {
        let mut lvl_seg = i32::from(h.lf_level);
        if h.seg_enabled && p.feature_enabled[seg][SEG_LVL_ALT_L] {
            let d = i32::from(p.feature_data[seg][SEG_LVL_ALT_L]);
            lvl_seg = if p.seg_abs { d } else { d + lvl_seg };
            lvl_seg = lvl_seg.clamp(0, 63);
        }
        if !h.lf_delta_enabled {
            *o = [[lvl_seg as u8; 2]; 4];
        } else {
            let intra = lvl_seg + (i32::from(p.lf_ref_deltas[0]) << n_shift);
            o[0] = [intra.clamp(0, 63) as u8; 2];
            for r in 1..4 {
                for m in 0..2 {
                    let l = lvl_seg
                        + (i32::from(p.lf_ref_deltas[r]) << n_shift)
                        + (i32::from(p.lf_mode_deltas[m]) << n_shift);
                    o[r][m] = l.clamp(0, 63) as u8;
                }
            }
        }
    }
    out
}

struct Strength {
    limit: i32,
    blimit: i32,
    thresh: i32,
}

fn strength(lvl: i32, sharp: i32) -> Strength {
    let shift = if sharp > 4 {
        2
    } else if sharp > 0 {
        1
    } else {
        0
    };
    let limit = if sharp > 0 {
        (lvl >> shift).clamp(1, 9 - sharp)
    } else {
        (lvl >> shift).max(1)
    };
    Strength {
        limit,
        blimit: 2 * (lvl + 2) + limit,
        thresh: lvl >> 4,
    }
}

fn uv_tx(mi: &Mi, ss_x: u8, ss_y: u8) -> usize {
    if mi.size < 3 {
        return 0;
    }
    let ps = SS_SIZE_LOOKUP[usize::from(mi.size)][usize::from(ss_x)][usize::from(ss_y)];
    let max = MAX_TXSIZE_LOOKUP.get(usize::from(ps)).copied().unwrap_or(0);
    usize::from(mi.tx_size.min(max))
}

/// Filter the whole frame in place.
pub fn loop_filter(f: &mut FrameBuf, mi: &[Mi], h: &Hdr, lvl_lookup: &[[[u8; 2]; 4]; 8]) {
    let bd = u32::from(f.bit_depth);
    for row in (0..h.mi_rows).step_by(8) {
        for col in (0..h.mi_cols).step_by(8) {
            for plane in 0..3 {
                for pass in 0..2 {
                    sb_filter(f, mi, h, lvl_lookup, plane, pass, row, col, bd);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sb_filter(
    f: &mut FrameBuf,
    mi: &[Mi],
    h: &Hdr,
    lvl_lookup: &[[[u8; 2]; 4]; 8],
    plane: usize,
    pass: usize,
    row: usize,
    col: usize,
    bd: u32,
) {
    let (sub_x, sub_y) = if plane == 0 {
        (0usize, 0usize)
    } else {
        (usize::from(f.ss_x), usize::from(f.ss_y))
    };
    let sub = if pass == 0 { sub_x } else { sub_y };
    let edge_len = if pass == 0 { 64 >> sub_y } else { 64 >> sub_x };
    let stride = f.stride[plane];
    let mi_cols = h.mi_cols;
    let mi_rows = h.mi_rows;
    let step: isize = if pass == 0 { 1 } else { stride as isize };
    for edge in 0..(16usize >> sub) {
        for g in 0..edge_len / 8 {
            let i0 = g * 8;
            // luma position of the first sample of the group
            let (x0, y0) = if pass == 0 {
                (col * 8 + edge * (4 << sub_x), row * 8 + (i0 << sub_y))
            } else {
                (col * 8 + (i0 << sub_x), row * 8 + edge * (4 << sub_y))
            };
            if x0 >= 8 * mi_cols || y0 >= 8 * mi_rows {
                continue;
            }
            if (pass == 0 && x0 == 0) || (pass == 1 && y0 == 0) {
                continue;
            }
            let loop_col = ((x0 >> 3) >> sub_x) << sub_x;
            let loop_row = ((y0 >> 3) >> sub_y) << sub_y;
            let m = &mi[loop_row * mi_cols + loop_col];
            let msize = m.size;
            let tx_sz = if plane > 0 {
                uv_tx(m, f.ss_x, f.ss_y)
            } else {
                usize::from(m.tx_size)
            };
            let sb_size = if sub == 0 {
                msize
            } else {
                msize.max(BLOCK_16X16)
            };
            let is_intra = m.ref_frame[0] <= 0;
            let is_block_edge = if pass == 0 {
                x0 % (8 * usize::from(NUM_8X8_BLOCKS_WIDE_LOOKUP[usize::from(sb_size)])) == 0
            } else {
                y0 % (8 * usize::from(NUM_8X8_BLOCKS_HIGH_LOOKUP[usize::from(sb_size)])) == 0
            };
            let is_32_edge = edge % 8 == 0;
            let base_size = if tx_sz == 0 && is_32_edge {
                1
            } else {
                tx_sz.min(2)
            };
            let filter_size =
                if (pass == 0 && sub_x == 1 && base_size == 2 && (x0 >> 3) == mi_cols - 1)
                    || (pass == 1 && sub_y == 1 && base_size == 2 && (y0 >> 3) == mi_rows - 1)
                {
                    1
                } else {
                    base_size
                };
            let mode_type = usize::from(matches!(m.y_mode, NEARESTMV | NEARMV | NEWMV));
            let ref0 = m.ref_frame[0].max(0) as usize;
            let lvl = i32::from(lvl_lookup[usize::from(m.seg_id)][ref0][mode_type]);
            if lvl == 0 {
                continue;
            }
            let st = strength(lvl, i32::from(h.lf_sharp));
            for k in 0..8 {
                let i = i0 + k;
                let (x, y) = if pass == 0 {
                    (x0, row * 8 + (i << sub_y))
                } else {
                    (col * 8 + (i << sub_x), y0)
                };
                if x >= 8 * mi_cols || y >= 8 * mi_rows {
                    continue;
                }
                let is_tx_edge = if pass == 1
                    && sub_x == 1
                    && mi_cols & 1 == 1
                    && edge & 1 == 1
                    && x + 8 >= mi_cols * 8
                {
                    false
                } else {
                    edge % (1 << tx_sz) == 0
                };
                let apply = is_block_edge || (is_tx_edge && (is_intra || !m.skip));
                if !apply {
                    continue;
                }
                let pos = (y >> sub_y) * stride + (x >> sub_x);
                filter_sample(&mut f.planes[plane], pos, step, filter_size, &st, bd);
            }
        }
    }
}

fn filter_sample(
    buf: &mut [u16],
    pos: usize,
    step: isize,
    filter_size: usize,
    st: &Strength,
    bd: u32,
) {
    let at = |k: isize| -> usize { (pos as isize + k * step) as usize };
    let px = |buf: &[u16], k: isize| -> i32 { i32::from(buf[at(k)]) };
    let q0 = px(buf, 0);
    let q1 = px(buf, 1);
    let q2 = px(buf, 2);
    let q3 = px(buf, 3);
    let p0 = px(buf, -1);
    let p1 = px(buf, -2);
    let p2 = px(buf, -3);
    let p3 = px(buf, -4);
    let sh = bd - 8;
    let thresh = st.thresh << sh;
    let limit = st.limit << sh;
    let blimit = st.blimit << sh;
    let hev = (p1 - p0).abs() > thresh || (q1 - q0).abs() > thresh;
    let mask = (p3 - p2).abs() > limit
        || (p2 - p1).abs() > limit
        || (p1 - p0).abs() > limit
        || (q1 - q0).abs() > limit
        || (q2 - q1).abs() > limit
        || (q3 - q2).abs() > limit
        || (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 > blimit;
    if mask {
        return;
    }
    let one = 1i32 << sh;
    let flat = filter_size >= 1
        && (p1 - p0).abs() <= one
        && (q1 - q0).abs() <= one
        && (p2 - p0).abs() <= one
        && (q2 - q0).abs() <= one
        && (p3 - p0).abs() <= one
        && (q3 - q0).abs() <= one;
    if filter_size == 0 || !flat {
        // narrow filter
        let lo = -(1i32 << (bd - 1));
        let hi = (1i32 << (bd - 1)) - 1;
        let c = |v: i32| v.clamp(lo, hi);
        let off = 0x80 << sh;
        let ps1 = p1 - off;
        let ps0 = p0 - off;
        let qs0 = q0 - off;
        let qs1 = q1 - off;
        let mut filter = if hev { c(ps1 - qs1) } else { 0 };
        filter = c(filter + 3 * (qs0 - ps0));
        let filter1 = c(filter + 4) >> 3;
        let filter2 = c(filter + 3) >> 3;
        buf[at(0)] = (c(qs0 - filter1) + off) as u16;
        buf[at(-1)] = (c(ps0 + filter2) + off) as u16;
        if !hev {
            let f = (filter1 + 1) >> 1;
            buf[at(1)] = (c(qs1 - f) + off) as u16;
            buf[at(-2)] = (c(ps1 + f) + off) as u16;
        }
        return;
    }
    let flat2 = filter_size >= 2 && {
        let q4 = px(buf, 4);
        let q5 = px(buf, 5);
        let q6 = px(buf, 6);
        let q7 = px(buf, 7);
        let p4 = px(buf, -5);
        let p5 = px(buf, -6);
        let p6 = px(buf, -7);
        let p7 = px(buf, -8);
        (p7 - p0).abs() <= one
            && (q7 - q0).abs() <= one
            && (p6 - p0).abs() <= one
            && (q6 - q0).abs() <= one
            && (p5 - p0).abs() <= one
            && (q5 - q0).abs() <= one
            && (p4 - p0).abs() <= one
            && (q4 - q0).abs() <= one
    };
    let log2 = if flat2 { 4 } else { 3 };
    let n = (1isize << (log2 - 1)) - 1;
    let mut vals = [0i32; 16];
    for k in -(n + 1)..=n {
        vals[(k + 8) as usize] = px(buf, k);
    }
    let mut out = [0i32; 16];
    for i in -n..n {
        let mut t = vals[(i + 8) as usize];
        for j in -n..=n {
            let p = (i + j).clamp(-(n + 1), n);
            t += vals[(p + 8) as usize];
        }
        out[(i + 8) as usize] = (t + (1 << (log2 - 1))) >> log2;
    }
    for i in -n..n {
        buf[at(i)] = out[(i + 8) as usize] as u16;
    }
}
