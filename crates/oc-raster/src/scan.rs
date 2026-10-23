//! Scan conversion: polygons to anti-aliased coverage rows.
//!
//! Each pixel row is sampled at `SUB` evenly spaced heights. On every sample line the exact
//! horizontal extent of the inside spans is accumulated, so edges are exact across and
//! `1/SUB` steps along. Non-zero and even-odd rules are applied per sample line, which keeps
//! self-overlapping paths correct.
use crate::geom::Poly;

pub const SUB: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn width(&self) -> usize {
        (self.x1 - self.x0).max(0) as usize
    }
    pub fn height(&self) -> usize {
        (self.y1 - self.y0).max(0) as usize
    }
    pub fn intersect(&self, o: &IRect) -> IRect {
        IRect::new(
            self.x0.max(o.x0),
            self.y0.max(o.y0),
            self.x1.min(o.x1),
            self.y1.min(o.y1),
        )
    }
}

#[derive(Clone, Copy)]
struct Edge {
    x0: f64,
    y0: f64,
    y1: f64,
    slope: f64,
    dir: i32,
}

/// Device-space bounding box of the polygons, rounded outwards.
pub fn bounds_of(polys: &[Poly]) -> Option<IRect> {
    let mut lo = (f64::INFINITY, f64::INFINITY);
    let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in polys {
        for q in &p.pts {
            if q.is_finite() {
                lo.0 = lo.0.min(q.x);
                lo.1 = lo.1.min(q.y);
                hi.0 = hi.0.max(q.x);
                hi.1 = hi.1.max(q.y);
            }
        }
    }
    if lo.0 > hi.0 {
        return None;
    }
    let c = |v: f64| v.clamp(-1e9, 1e9);
    Some(IRect::new(
        c(lo.0.floor()) as i32,
        c(lo.1.floor()) as i32,
        c(hi.0.ceil()) as i32 + 1,
        c(hi.1.ceil()) as i32 + 1,
    ))
}

/// Call `row(y, x_start, coverage)` for each pixel row of `clip` that the polygons touch.
/// Coverage values are in `0.0..=1.0`.
pub fn scan(
    polys: &[Poly],
    rule: FillRule,
    clip: IRect,
    mut row: impl FnMut(i32, i32, &[f32]),
) {
    if clip.is_empty() {
        return;
    }
    let mut edges: Vec<Edge> = Vec::new();
    for p in polys {
        let n = p.pts.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let a = p.pts[i];
            let b = p.pts[(i + 1) % n];
            if !(a.is_finite() && b.is_finite()) || a.y == b.y {
                continue;
            }
            let (top, bot, dir) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
            if bot.y <= f64::from(clip.y0) || top.y >= f64::from(clip.y1) {
                continue;
            }
            let c = |v: f64| v.clamp(-1e9, 1e9);
            let (tx, ty, bx, by) = (c(top.x), c(top.y), c(bot.x), c(bot.y));
            if by <= ty {
                continue;
            }
            edges.push(Edge {
                x0: tx,
                y0: ty,
                y1: by,
                slope: (bx - tx) / (by - ty),
                dir,
            });
        }
    }
    if edges.is_empty() {
        return;
    }
    edges.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    let width = clip.width();
    let ymin = edges.first().map_or(0.0, |e| e.y0).floor() as i32;
    let ymax = edges.iter().fold(0.0f64, |m, e| m.max(e.y1)).ceil() as i32;
    let y_start = ymin.max(clip.y0);
    let y_end = ymax.min(clip.y1);
    let mut cover = vec![0.0f32; width + 2];
    let mut diff = vec![0.0f32; width + 2];
    let mut out = vec![0.0f32; width + 1];
    let mut next = 0usize;
    let mut active: Vec<usize> = Vec::new();
    let mut xs: Vec<(f64, i32)> = Vec::new();
    let w = width as f64;
    let ox = f64::from(clip.x0);
    for y in y_start..y_end {
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        for k in 0..SUB {
            let sy = f64::from(y) + (k as f64 + 0.5) / SUB as f64;
            while let Some(e) = edges.get(next) {
                if e.y0 <= sy {
                    active.push(next);
                    next += 1;
                } else {
                    break;
                }
            }
            active.retain(|&i| edges.get(i).is_some_and(|e| e.y1 > sy));
            xs.clear();
            for &i in &active {
                if let Some(e) = edges.get(i) {
                    if e.y0 <= sy {
                        xs.push((e.x0 + (sy - e.y0) * e.slope - ox, e.dir));
                    }
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0i32;
            for j in 0..xs.len() - 1 {
                wind += xs[j].1;
                let inside = match rule {
                    FillRule::NonZero => wind != 0,
                    FillRule::EvenOdd => wind & 1 != 0,
                };
                if !inside {
                    continue;
                }
                let a = xs[j].0.clamp(0.0, w);
                let b = xs[j + 1].0.clamp(0.0, w);
                if b <= a {
                    continue;
                }
                let i0 = a.floor() as usize;
                let i1 = (b.floor() as usize).min(width);
                lo = lo.min(i0);
                hi = hi.max(i1);
                if i0 == i1 {
                    cover[i0] += (b - a) as f32;
                } else {
                    cover[i0] += ((i0 + 1) as f64 - a) as f32;
                    diff[i0 + 1] += 1.0;
                    diff[i1] -= 1.0;
                    cover[i1] += (b - i1 as f64) as f32;
                }
            }
        }
        if lo == usize::MAX {
            continue;
        }
        let hi = hi.min(width.saturating_sub(1));
        if lo > hi {
            cover[..width + 2].fill(0.0);
            diff[..width + 2].fill(0.0);
            continue;
        }
        let inv = 1.0 / SUB as f32;
        let mut run = 0.0f32;
        for x in lo..=hi {
            run += diff[x];
            out[x - lo] = ((cover[x] + run) * inv).clamp(0.0, 1.0);
        }
        row(y, clip.x0 + lo as i32, &out[..=hi - lo]);
        cover[..width + 2].fill(0.0);
        diff[..width + 2].fill(0.0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::geom::Path;

    fn area(path: &Path, rule: FillRule, w: i32, h: i32) -> f64 {
        let polys = path.flatten(0.002);
        let mut total = 0.0f64;
        scan(&polys, rule, IRect::new(0, 0, w, h), |_, _, cov| {
            total += cov.iter().map(|c| f64::from(*c)).sum::<f64>();
        });
        total
    }

    #[test]
    fn rectangle_area_is_exact_across() {
        let a = area(&Path::rect(1.25, 2.0, 5.5, 3.0), FillRule::NonZero, 20, 20);
        assert!((a - 16.5).abs() < 1e-3, "{a}");
    }

    #[test]
    fn circle_area_is_close() {
        let a = area(&Path::ellipse(10.0, 10.0, 6.0, 6.0), FillRule::NonZero, 20, 20);
        let want = std::f64::consts::PI * 36.0;
        assert!((a - want).abs() / want < 0.005, "{a} vs {want}");
    }

    #[test]
    fn rules_differ_on_overlap() {
        let mut p = Path::rect(0.0, 0.0, 8.0, 8.0);
        p.append(&Path::rect(2.0, 2.0, 4.0, 4.0));
        let nz = area(&p, FillRule::NonZero, 10, 10);
        let eo = area(&p, FillRule::EvenOdd, 10, 10);
        assert!((nz - 64.0).abs() < 1e-3);
        assert!((eo - 48.0).abs() < 1e-3);
    }

    #[test]
    fn clipping_and_nonfinite_input_do_not_panic() {
        let mut p = Path::rect(-100.0, -100.0, 1e12, 1e12);
        p.move_to(f64::NAN, 1.0);
        p.line_to(f64::INFINITY, 3.0);
        let a = area(&p, FillRule::NonZero, 10, 10);
        assert!((a - 100.0).abs() < 1e-3, "{a}");
    }
}
