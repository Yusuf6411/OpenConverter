//! Stroking: a flattened path becomes polygons that, filled non-zero, cover the stroke.
use crate::geom::{Poly, Pt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    Miter,
    Round,
    Bevel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub width: f64,
    pub cap: Cap,
    pub join: Join,
    pub miter_limit: f64,
    pub dash: Vec<f64>,
    pub dash_phase: f64,
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            width: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
            miter_limit: 4.0,
            dash: Vec::new(),
            dash_phase: 0.0,
        }
    }
}

fn orient(mut pts: Vec<Pt>) -> Poly {
    let mut a = 0.0;
    for i in 0..pts.len() {
        let p = pts[i];
        let q = pts[(i + 1) % pts.len()];
        a += p.x * q.y - q.x * p.y;
    }
    if a < 0.0 {
        pts.reverse();
    }
    Poly { pts, closed: true }
}

fn dedupe(p: &Poly) -> Vec<Pt> {
    let mut v: Vec<Pt> = Vec::with_capacity(p.pts.len());
    for &q in &p.pts {
        if !q.is_finite() {
            continue;
        }
        if v.last().is_none_or(|l| l.sub(q).len() > 1e-9) {
            v.push(q);
        }
    }
    if p.closed && v.len() > 1 {
        if let (Some(f), Some(l)) = (v.first().copied(), v.last().copied()) {
            if f.sub(l).len() <= 1e-9 {
                v.pop();
            }
        }
    }
    v
}

fn arc(out: &mut Vec<Pt>, c: Pt, from: Pt, to: Pt, hw: f64, tol: f64, ccw_hint: bool) {
    // Sweep the shorter way from `from` to `to` around `c`.
    let a0 = (from.y - c.y).atan2(from.x - c.x);
    let a1 = (to.y - c.y).atan2(to.x - c.x);
    let mut d = a1 - a0;
    while d > std::f64::consts::PI {
        d -= std::f64::consts::TAU;
    }
    while d < -std::f64::consts::PI {
        d += std::f64::consts::TAU;
    }
    let _ = ccw_hint;
    let step = 2.0 * (1.0 - (tol / hw).min(1.0)).acos().max(0.05);
    let n = ((d.abs() / step).ceil() as usize).clamp(1, 256);
    for i in 1..n {
        let a = a0 + d * i as f64 / n as f64;
        out.push(Pt::new(c.x + hw * a.cos(), c.y + hw * a.sin()));
    }
}

fn circle(c: Pt, hw: f64, tol: f64) -> Poly {
    let step = 2.0 * (1.0 - (tol / hw).min(1.0)).acos().max(0.05);
    let n = ((std::f64::consts::TAU / step).ceil() as usize).clamp(8, 512);
    let pts = (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            Pt::new(c.x + hw * a.cos(), c.y + hw * a.sin())
        })
        .collect();
    orient(pts)
}

/// Stroke every polygon. `tol` is the flattening tolerance of round joins and caps.
pub fn stroke(polys: &[Poly], st: &Stroke, tol: f64) -> Vec<Poly> {
    let hw = st.width / 2.0;
    let mut out = Vec::new();
    if !(hw.is_finite() && hw > 0.0) {
        return out;
    }
    let tol = tol.max(1e-6);
    let source: Vec<Poly> = if st.dash.iter().any(|d| *d > 0.0) {
        dash(polys, &st.dash, st.dash_phase)
    } else {
        polys.to_vec()
    };
    for poly in &source {
        let pts = dedupe(poly);
        let n = pts.len();
        if n == 0 {
            continue;
        }
        if n == 1 {
            let c = pts[0];
            match st.cap {
                Cap::Round => out.push(circle(c, hw, tol)),
                Cap::Square => out.push(orient(vec![
                    Pt::new(c.x - hw, c.y - hw),
                    Pt::new(c.x + hw, c.y - hw),
                    Pt::new(c.x + hw, c.y + hw),
                    Pt::new(c.x - hw, c.y + hw),
                ])),
                Cap::Butt => {}
            }
            continue;
        }
        let closed = poly.closed && n > 2;
        let segs = if closed { n } else { n - 1 };
        let dir = |i: usize| -> Pt {
            let a = pts[i % n];
            let b = pts[(i + 1) % n];
            let d = b.sub(a);
            let l = d.len();
            Pt::new(d.x / l, d.y / l)
        };
        for i in 0..segs {
            let a = pts[i % n];
            let b = pts[(i + 1) % n];
            let d = dir(i);
            let nrm = Pt::new(-d.y * hw, d.x * hw);
            out.push(orient(vec![
                a.add(nrm),
                b.add(nrm),
                b.sub(nrm),
                a.sub(nrm),
            ]));
        }
        // Joins at the interior vertices (and at every vertex of a closed path).
        let joins: Vec<usize> = if closed {
            (0..n).collect()
        } else {
            (1..n - 1).collect()
        };
        for v in joins {
            let d0 = dir((v + n - 1) % n);
            let d1 = dir(v);
            let cross = d0.x * d1.y - d0.y * d1.x;
            let dot = d0.x * d1.x + d0.y * d1.y;
            if cross.abs() < 1e-9 && dot > 0.0 {
                continue;
            }
            let c = pts[v];
            let s = if cross > 0.0 { -1.0 } else { 1.0 };
            let n0 = Pt::new(-d0.y * hw * s, d0.x * hw * s);
            let n1 = Pt::new(-d1.y * hw * s, d1.x * hw * s);
            let p0 = c.add(n0);
            let p1 = c.add(n1);
            let mut poly_pts = vec![c, p0];
            match st.join {
                Join::Round => arc(&mut poly_pts, c, p0, p1, hw, tol, cross > 0.0),
                Join::Miter => {
                    let cos_half = ((1.0 + dot) / 2.0).max(0.0).sqrt();
                    // Turn angle alpha: cos(alpha/2) = sqrt((1+dot)/2); ratio = 1/cos(alpha/2).
                    if cos_half > 1e-9 && 1.0 / cos_half <= st.miter_limit.max(1.0) {
                        let m = Pt::new(
                            (n0.x + n1.x) / (1.0 + dot),
                            (n0.y + n1.y) / (1.0 + dot),
                        );
                        poly_pts.push(c.add(m));
                    }
                }
                Join::Bevel => {}
            }
            poly_pts.push(p1);
            out.push(orient(poly_pts));
        }
        if !closed {
            for (end, d, sign) in [(pts[0], dir(0), -1.0), (pts[n - 1], dir(n - 2), 1.0)] {
                match st.cap {
                    Cap::Butt => {}
                    Cap::Square => {
                        let ext = Pt::new(d.x * hw * sign, d.y * hw * sign);
                        let nrm = Pt::new(-d.y * hw, d.x * hw);
                        out.push(orient(vec![
                            end.add(nrm),
                            end.add(nrm).add(ext),
                            end.sub(nrm).add(ext),
                            end.sub(nrm),
                        ]));
                    }
                    Cap::Round => {
                        let nrm = Pt::new(-d.y * hw, d.x * hw);
                        let ext = Pt::new(d.x * hw * sign, d.y * hw * sign);
                        let mut v = vec![end.add(nrm)];
                        let tip = end.add(ext);
                        arc(&mut v, end, end.add(nrm), tip, hw, tol, true);
                        v.push(tip);
                        arc(&mut v, end, tip, end.sub(nrm), hw, tol, true);
                        v.push(end.sub(nrm));
                        out.push(orient(v));
                    }
                }
            }
        }
    }
    out
}

/// Cut polygons into dashes (`pattern` alternates on and off lengths).
pub fn dash(polys: &[Poly], pattern: &[f64], phase: f64) -> Vec<Poly> {
    let mut pat: Vec<f64> = pattern
        .iter()
        .map(|d| if d.is_finite() { d.max(0.0) } else { 0.0 })
        .collect();
    let total: f64 = pat.iter().sum();
    if pat.is_empty() || total <= 1e-9 {
        return polys.to_vec();
    }
    if pat.len() % 2 == 1 {
        let copy = pat.clone();
        pat.extend(copy);
    }
    let total: f64 = pat.iter().sum();
    let mut out = Vec::new();
    for poly in polys {
        let mut pts = dedupe(poly);
        if poly.closed && pts.len() > 1 {
            pts.push(pts[0]);
        }
        if pts.len() < 2 {
            continue;
        }
        // Position in the pattern at the start of the subpath.
        let mut ph = if phase.is_finite() { phase } else { 0.0 };
        ph = ph.rem_euclid(total);
        let mut idx = 0usize;
        while ph >= pat[idx % pat.len()] {
            ph -= pat[idx % pat.len()];
            idx += 1;
            if idx > 10_000 {
                break;
            }
        }
        let mut remain = pat[idx % pat.len()] - ph;
        let mut on = idx % 2 == 0;
        let mut cur: Vec<Pt> = if on { vec![pts[0]] } else { Vec::new() };
        let mut count = 0usize;
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let len = b.sub(a).len();
            let mut pos = 0.0;
            while len - pos > remain {
                count += 1;
                if count > 200_000 {
                    return out;
                }
                pos += remain;
                let p = a.lerp(b, pos / len);
                if on {
                    cur.push(p);
                    out.push(Poly {
                        pts: std::mem::take(&mut cur),
                        closed: false,
                    });
                } else {
                    cur = vec![p];
                }
                on = !on;
                idx += 1;
                remain = pat[idx % pat.len()];
                if remain <= 0.0 {
                    // Zero-length on-dash: keep it as a point so round caps show.
                    if on {
                        out.push(Poly {
                            pts: vec![p],
                            closed: false,
                        });
                    }
                    on = !on;
                    idx += 1;
                    remain = pat[idx % pat.len()];
                    if on {
                        cur = vec![p];
                    } else {
                        cur.clear();
                    }
                }
            }
            remain -= len - pos;
            if on {
                cur.push(b);
            }
        }
        if on && cur.len() > 1 {
            out.push(Poly {
                pts: cur,
                closed: false,
            });
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::geom::Path;
    use crate::scan::{scan, FillRule, IRect};

    fn area(polys: &[Poly]) -> f64 {
        let mut t = 0.0f64;
        scan(polys, FillRule::NonZero, IRect::new(0, 0, 100, 100), |_, _, c| {
            t += c.iter().map(|v| f64::from(*v)).sum::<f64>();
        });
        t
    }

    #[test]
    fn butt_line_area() {
        let mut p = Path::new();
        p.move_to(10.0, 10.0);
        p.line_to(30.0, 10.0);
        let s = stroke(&p.flatten(0.1), &Stroke { width: 4.0, ..Stroke::default() }, 0.1);
        assert!((area(&s) - 80.0).abs() < 0.01);
    }

    #[test]
    fn square_cap_and_miter_corner() {
        let mut p = Path::new();
        p.move_to(10.0, 10.0);
        p.line_to(30.0, 10.0);
        p.line_to(30.0, 30.0);
        let st = Stroke { width: 4.0, cap: Cap::Square, ..Stroke::default() };
        let a = area(&stroke(&p.flatten(0.1), &st, 0.1));
        // Legs 80+80, minus their 4 overlap, plus the 4 of the mitred corner and two 2x4 caps.
        assert!((a - 176.0).abs() < 0.05, "{a}");
    }

    #[test]
    fn dashes_halve_the_area() {
        let mut p = Path::new();
        p.move_to(0.0, 5.0);
        p.line_to(40.0, 5.0);
        let st = Stroke { width: 2.0, dash: vec![5.0, 5.0], ..Stroke::default() };
        let a = area(&stroke(&p.flatten(0.1), &st, 0.1));
        assert!((a - 40.0).abs() < 0.05, "{a}");
    }

    #[test]
    fn round_dot_for_zero_length_subpath() {
        let mut p = Path::new();
        p.move_to(10.0, 10.0);
        p.line_to(10.0, 10.0);
        let st = Stroke { width: 6.0, cap: Cap::Round, ..Stroke::default() };
        let a = area(&stroke(&p.flatten(0.1), &st, 0.05));
        assert!((a - std::f64::consts::PI * 9.0).abs() < 0.8, "{a}");
    }
}
