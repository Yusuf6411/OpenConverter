//! Intra and inter prediction (spec 8.5).
use crate::frame::FrameBuf;
use crate::tables::SUBPEL_FILTERS;

pub const DC_PRED: u8 = 0;
pub const V_PRED: u8 = 1;
pub const H_PRED: u8 = 2;
pub const D45_PRED: u8 = 3;
pub const D135_PRED: u8 = 4;
pub const D117_PRED: u8 = 5;
pub const D153_PRED: u8 = 6;
pub const D207_PRED: u8 = 7;
pub const D63_PRED: u8 = 8;
pub const TM_PRED: u8 = 9;

fn r2(x: i32, n: u32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

/// Where a transform block sits, for intra prediction.
pub struct IntraEdge {
    pub plane: usize,
    pub x: usize,
    pub y: usize,
    pub have_left: bool,
    pub have_above: bool,
    pub not_on_right: bool,
    pub tx_size: usize,
    pub max_x: usize,
    pub max_y: usize,
}

pub fn predict_intra(f: &mut FrameBuf, e: &IntraEdge, mode: u8) {
    let size = 4usize << e.tx_size;
    let log2 = e.tx_size as u32 + 2;
    let bd = u32::from(f.bit_depth);
    let base = 1i32 << (bd - 1);
    let stride = f.stride[e.plane];
    let (x, y) = (e.x, e.y);
    // above[0] is aboveRow[-1].
    let mut above = [0i32; 65];
    let mut left = [0i32; 32];
    {
        let buf = &f.planes[e.plane];
        if e.have_above {
            let row = (y - 1) * stride;
            for i in 0..size {
                above[1 + i] = i32::from(buf[row + e.max_x.min(x + i)]);
            }
            if e.not_on_right && e.tx_size == 0 {
                for i in size..2 * size {
                    above[1 + i] = i32::from(buf[row + e.max_x.min(x + i)]);
                }
            } else {
                let v = above[size];
                for i in size..2 * size {
                    above[1 + i] = v;
                }
            }
            above[0] = if e.have_left {
                i32::from(buf[row + e.max_x.min(x - 1)])
            } else {
                base + 1
            };
        } else {
            for v in above.iter_mut().take(1 + 2 * size) {
                *v = base - 1;
            }
        }
        if e.have_left {
            for (i, l) in left.iter_mut().enumerate().take(size) {
                *l = i32::from(buf[e.max_y.min(y + i) * stride + x - 1]);
            }
        } else {
            for l in left.iter_mut().take(size) {
                *l = base + 1;
            }
        }
    }
    let a = |k: i32| above[(k + 1) as usize];
    let l = |k: i32| left[k as usize];
    let sz = size as i32;
    let mut pred = [0i32; 32 * 32];
    let set = |i: i32, j: i32, v: i32, pred: &mut [i32; 1024]| pred[(i * sz + j) as usize] = v;
    let get = |i: i32, j: i32, pred: &[i32; 1024]| pred[(i * sz + j) as usize];
    match mode {
        V_PRED => {
            for i in 0..sz {
                for j in 0..sz {
                    set(i, j, a(j), &mut pred);
                }
            }
        }
        H_PRED => {
            for i in 0..sz {
                for j in 0..sz {
                    set(i, j, l(i), &mut pred);
                }
            }
        }
        D207_PRED => {
            for j in 0..sz {
                set(sz - 1, j, l(sz - 1), &mut pred);
            }
            for i in 0..sz - 1 {
                set(i, 0, r2(l(i) + l(i + 1), 1), &mut pred);
            }
            for i in 0..sz - 2 {
                set(i, 1, r2(l(i) + 2 * l(i + 1) + l(i + 2), 2), &mut pred);
            }
            set(sz - 2, 1, r2(l(sz - 2) + 3 * l(sz - 1), 2), &mut pred);
            for i in (0..sz - 1).rev() {
                for j in 2..sz {
                    let v = get(i + 1, j - 2, &pred);
                    set(i, j, v, &mut pred);
                }
            }
        }
        D45_PRED => {
            for i in 0..sz {
                for j in 0..sz {
                    let v = if i + j + 2 < sz * 2 {
                        r2(a(i + j) + a(i + j + 1) * 2 + a(i + j + 2), 2)
                    } else {
                        a(2 * sz - 1)
                    };
                    set(i, j, v, &mut pred);
                }
            }
        }
        D63_PRED => {
            for i in 0..sz {
                for j in 0..sz {
                    let i2 = i / 2;
                    let v = if i & 1 == 1 {
                        r2(a(i2 + j) + a(i2 + j + 1) * 2 + a(i2 + j + 2), 2)
                    } else {
                        r2(a(i2 + j) + a(i2 + j + 1), 1)
                    };
                    set(i, j, v, &mut pred);
                }
            }
        }
        D117_PRED => {
            for j in 0..sz {
                set(0, j, r2(a(j - 1) + a(j), 1), &mut pred);
            }
            set(1, 0, r2(l(0) + 2 * a(-1) + a(0), 2), &mut pred);
            for j in 1..sz {
                set(1, j, r2(a(j - 2) + 2 * a(j - 1) + a(j), 2), &mut pred);
            }
            set(2, 0, r2(a(-1) + 2 * l(0) + l(1), 2), &mut pred);
            for i in 3..sz {
                set(i, 0, r2(l(i - 3) + 2 * l(i - 2) + l(i - 1), 2), &mut pred);
            }
            for i in 2..sz {
                for j in 1..sz {
                    let v = get(i - 2, j - 1, &pred);
                    set(i, j, v, &mut pred);
                }
            }
        }
        D135_PRED => {
            set(0, 0, r2(l(0) + 2 * a(-1) + a(0), 2), &mut pred);
            for j in 1..sz {
                set(0, j, r2(a(j - 2) + 2 * a(j - 1) + a(j), 2), &mut pred);
            }
            set(1, 0, r2(a(-1) + 2 * l(0) + l(1), 2), &mut pred);
            for i in 2..sz {
                set(i, 0, r2(l(i - 2) + 2 * l(i - 1) + l(i), 2), &mut pred);
            }
            for i in 1..sz {
                for j in 1..sz {
                    let v = get(i - 1, j - 1, &pred);
                    set(i, j, v, &mut pred);
                }
            }
        }
        D153_PRED => {
            set(0, 0, r2(l(0) + a(-1), 1), &mut pred);
            for i in 1..sz {
                set(i, 0, r2(l(i - 1) + l(i), 1), &mut pred);
            }
            set(0, 1, r2(l(0) + 2 * a(-1) + a(0), 2), &mut pred);
            set(1, 1, r2(a(-1) + 2 * l(0) + l(1), 2), &mut pred);
            for i in 2..sz {
                set(i, 1, r2(l(i - 2) + 2 * l(i - 1) + l(i), 2), &mut pred);
            }
            for j in 2..sz {
                set(0, j, r2(a(j - 3) + 2 * a(j - 2) + a(j - 1), 2), &mut pred);
            }
            for i in 1..sz {
                for j in 2..sz {
                    let v = get(i - 1, j - 2, &pred);
                    set(i, j, v, &mut pred);
                }
            }
        }
        TM_PRED => {
            let max = (1i32 << bd) - 1;
            for i in 0..sz {
                for j in 0..sz {
                    set(i, j, (a(j) + l(i) - a(-1)).clamp(0, max), &mut pred);
                }
            }
        }
        _ => {
            let v = match (e.have_left, e.have_above) {
                (true, true) => {
                    let mut sum = 0;
                    for k in 0..sz {
                        sum += l(k) + a(k);
                    }
                    (sum + sz) >> (log2 + 1)
                }
                (true, false) => {
                    let sum: i32 = (0..sz).map(l).sum();
                    (sum + (1 << (log2 - 1))) >> log2
                }
                (false, true) => {
                    let sum: i32 = (0..sz).map(a).sum();
                    (sum + (1 << (log2 - 1))) >> log2
                }
                (false, false) => base,
            };
            for p in pred.iter_mut().take(size * size) {
                *p = v;
            }
        }
    }
    let buf = &mut f.planes[e.plane];
    for i in 0..size {
        let row = (y + i) * stride + x;
        for j in 0..size {
            buf[row + j] = pred[i * size + j] as u16;
        }
    }
}

/// Reusable buffers for inter prediction.
#[derive(Default)]
pub struct InterScratch {
    inter: Vec<u16>,
}

/// Predict a `w x h` block from `r` (spec 8.5.2.4). `x`, `y` are in 1/16 sample units and `out`
/// receives `w * h` samples.
#[allow(clippy::too_many_arguments)]
pub fn predict_inter_block(
    r: &FrameBuf,
    plane: usize,
    x: i32,
    y: i32,
    step_x: i32,
    step_y: i32,
    w: usize,
    h: usize,
    filter: usize,
    out: &mut [u16],
    s: &mut InterScratch,
) {
    let (pw, ph) = r.plane_size(plane);
    let last_x = pw as i32 - 1;
    let last_y = ph as i32 - 1;
    let max = (1i32 << r.bit_depth) - 1;
    let inter_h = ((((h as i32 - 1) * step_y + 15) >> 4) + 8) as usize;
    if s.inter.len() < inter_h * w {
        s.inter.resize(inter_h * w, 0);
    }
    let stride = r.stride[plane];
    let buf = &r.planes[plane];
    let filters = &SUBPEL_FILTERS[filter];
    for row in 0..inter_h {
        let ry = ((y >> 4) + row as i32 - 3).clamp(0, last_y) as usize;
        let line = &buf[ry * stride..ry * stride + pw];
        for c in 0..w {
            let p = x + step_x * c as i32;
            let px = (p >> 4) - 3;
            let f = &filters[(p & 15) as usize];
            let mut sum = 0i32;
            if px >= 0 && px + 7 <= last_x {
                let px = px as usize;
                for t in 0..8 {
                    sum += f[t] * i32::from(line[px + t]);
                }
            } else {
                for t in 0..8 {
                    sum += f[t] * i32::from(line[(px + t as i32).clamp(0, last_x) as usize]);
                }
            }
            s.inter[row * w + c] = ((sum + 64) >> 7).clamp(0, max) as u16;
        }
    }
    for row in 0..h {
        let p = (y & 15) + step_y * row as i32;
        let f = &filters[(p & 15) as usize];
        let base = (p >> 4) as usize;
        for c in 0..w {
            let mut sum = 0i32;
            for t in 0..8 {
                sum += f[t] * i32::from(s.inter[(base + t) * w + c]);
            }
            out[row * w + c] = ((sum + 64) >> 7).clamp(0, max) as u16;
        }
    }
}
