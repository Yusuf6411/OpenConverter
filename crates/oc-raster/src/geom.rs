//! Points, affine transforms and paths, with flattening of curves.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

impl Pt {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn sub(self, o: Pt) -> Pt {
        Pt::new(self.x - o.x, self.y - o.y)
    }
    pub fn add(self, o: Pt) -> Pt {
        Pt::new(self.x + o.x, self.y + o.y)
    }
    pub fn scale(self, k: f64) -> Pt {
        Pt::new(self.x * k, self.y * k)
    }
    pub fn len(self) -> f64 {
        self.x.hypot(self.y)
    }
    pub fn lerp(self, o: Pt, t: f64) -> Pt {
        Pt::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// `x' = a*x + c*y + e`, `y' = b*x + d*y + f` (the PDF and SVG matrix convention).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }
    pub fn translate(x: f64, y: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, x, y)
    }
    pub fn scale(x: f64, y: f64) -> Self {
        Self::new(x, 0.0, 0.0, y, 0.0, 0.0)
    }
    pub fn rotate(rad: f64) -> Self {
        let (s, c) = rad.sin_cos();
        Self::new(c, s, -s, c, 0.0, 0.0)
    }
    pub fn skew_x(rad: f64) -> Self {
        Self::new(1.0, 0.0, rad.tan(), 1.0, 0.0, 0.0)
    }
    pub fn skew_y(rad: f64) -> Self {
        Self::new(1.0, rad.tan(), 0.0, 1.0, 0.0, 0.0)
    }
    /// Apply `self` first, then `next`.
    pub fn then(&self, next: &Transform) -> Transform {
        Transform {
            a: self.a * next.a + self.b * next.c,
            b: self.a * next.b + self.b * next.d,
            c: self.c * next.a + self.d * next.c,
            d: self.c * next.b + self.d * next.d,
            e: self.e * next.a + self.f * next.c + next.e,
            f: self.e * next.b + self.f * next.d + next.f,
        }
    }
    pub fn apply(&self, p: Pt) -> Pt {
        Pt::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }
    /// The vector part only (no translation).
    pub fn apply_vec(&self, p: Pt) -> Pt {
        Pt::new(self.a * p.x + self.c * p.y, self.b * p.x + self.d * p.y)
    }
    pub fn det(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }
    pub fn invert(&self) -> Option<Transform> {
        let det = self.det();
        if !det.is_finite() || det.abs() < 1e-18 {
            return None;
        }
        let id = 1.0 / det;
        Some(Transform {
            a: self.d * id,
            b: -self.b * id,
            c: -self.c * id,
            d: self.a * id,
            e: (self.c * self.f - self.d * self.e) * id,
            f: (self.b * self.e - self.a * self.f) * id,
        })
    }
    /// Mean scale factor, used to turn a device tolerance into a path tolerance.
    pub fn mean_scale(&self) -> f64 {
        self.det().abs().sqrt()
    }
    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    Move(Pt),
    Line(Pt),
    Quad(Pt, Pt),
    Cubic(Pt, Pt, Pt),
    Close,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Path {
    pub segs: Vec<Seg>,
}

/// A flattened subpath.
#[derive(Debug, Clone, PartialEq)]
pub struct Poly {
    pub pts: Vec<Pt>,
    pub closed: bool,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }
    pub fn move_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::Move(Pt::new(x, y)));
    }
    pub fn line_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::Line(Pt::new(x, y)));
    }
    pub fn quad_to(&mut self, x1: f64, y1: f64, x: f64, y: f64) {
        self.segs.push(Seg::Quad(Pt::new(x1, y1), Pt::new(x, y)));
    }
    pub fn cubic_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x: f64, y: f64) {
        self.segs.push(Seg::Cubic(
            Pt::new(x1, y1),
            Pt::new(x2, y2),
            Pt::new(x, y),
        ));
    }
    pub fn close(&mut self) {
        self.segs.push(Seg::Close);
    }
    pub fn rect(x: f64, y: f64, w: f64, h: f64) -> Path {
        let mut p = Path::new();
        p.move_to(x, y);
        p.line_to(x + w, y);
        p.line_to(x + w, y + h);
        p.line_to(x, y + h);
        p.close();
        p
    }
    /// An ellipse made of four cubic arcs.
    pub fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Path {
        const K: f64 = 0.552_284_749_830_793_4;
        let mut p = Path::new();
        p.move_to(cx + rx, cy);
        p.cubic_to(cx + rx, cy + ry * K, cx + rx * K, cy + ry, cx, cy + ry);
        p.cubic_to(cx - rx * K, cy + ry, cx - rx, cy + ry * K, cx - rx, cy);
        p.cubic_to(cx - rx, cy - ry * K, cx - rx * K, cy - ry, cx, cy - ry);
        p.cubic_to(cx + rx * K, cy - ry, cx + rx, cy - ry * K, cx + rx, cy);
        p.close();
        p
    }
    pub fn round_rect(x: f64, y: f64, w: f64, h: f64, rx: f64, ry: f64) -> Path {
        let rx = rx.clamp(0.0, w / 2.0);
        let ry = ry.clamp(0.0, h / 2.0);
        if rx <= 0.0 || ry <= 0.0 {
            return Path::rect(x, y, w, h);
        }
        const K: f64 = 0.552_284_749_830_793_4;
        let (kx, ky) = (rx * K, ry * K);
        let mut p = Path::new();
        p.move_to(x + rx, y);
        p.line_to(x + w - rx, y);
        p.cubic_to(x + w - rx + kx, y, x + w, y + ry - ky, x + w, y + ry);
        p.line_to(x + w, y + h - ry);
        p.cubic_to(x + w, y + h - ry + ky, x + w - rx + kx, y + h, x + w - rx, y + h);
        p.line_to(x + rx, y + h);
        p.cubic_to(x + rx - kx, y + h, x, y + h - ry + ky, x, y + h - ry);
        p.line_to(x, y + ry);
        p.cubic_to(x, y + ry - ky, x + rx - kx, y, x + rx, y);
        p.close();
        p
    }

    /// SVG elliptical arc from the current point, as cubic segments.
    #[allow(clippy::too_many_arguments)]
    pub fn arc_to(
        &mut self,
        from: Pt,
        rx: f64,
        ry: f64,
        x_rot_deg: f64,
        large: bool,
        sweep: bool,
        to: Pt,
    ) {
        let (mut rx, mut ry) = (rx.abs(), ry.abs());
        if !(from.is_finite() && to.is_finite() && rx.is_finite() && ry.is_finite()) {
            return;
        }
        if from == to {
            return;
        }
        if rx < 1e-12 || ry < 1e-12 {
            self.line_to(to.x, to.y);
            return;
        }
        let phi = x_rot_deg.to_radians();
        let (sp, cp) = phi.sin_cos();
        let dx = (from.x - to.x) / 2.0;
        let dy = (from.y - to.y) / 2.0;
        let x1 = cp * dx + sp * dy;
        let y1 = -sp * dx + cp * dy;
        let lam = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
        if lam > 1.0 {
            let s = lam.sqrt();
            rx *= s;
            ry *= s;
        }
        let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
        let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
        let mut co = if den == 0.0 { 0.0 } else { (num / den).max(0.0).sqrt() };
        if large == sweep {
            co = -co;
        }
        let cxp = co * rx * y1 / ry;
        let cyp = -co * ry * x1 / rx;
        let cx = cp * cxp - sp * cyp + (from.x + to.x) / 2.0;
        let cy = sp * cxp + cp * cyp + (from.y + to.y) / 2.0;
        let ang = |ux: f64, uy: f64, vx: f64, vy: f64| {
            let dot = ux * vx + uy * vy;
            let l = (ux.hypot(uy) * vx.hypot(vy)).max(1e-300);
            let mut a = (dot / l).clamp(-1.0, 1.0).acos();
            if ux * vy - uy * vx < 0.0 {
                a = -a;
            }
            a
        };
        let th1 = ang(1.0, 0.0, (x1 - cxp) / rx, (y1 - cyp) / ry);
        let mut dth = ang(
            (x1 - cxp) / rx,
            (y1 - cyp) / ry,
            (-x1 - cxp) / rx,
            (-y1 - cyp) / ry,
        );
        if !sweep && dth > 0.0 {
            dth -= std::f64::consts::TAU;
        } else if sweep && dth < 0.0 {
            dth += std::f64::consts::TAU;
        }
        let n = (dth.abs() / (std::f64::consts::FRAC_PI_2)).ceil().max(1.0) as usize;
        let step = dth / n as f64;
        let t = 4.0 / 3.0 * (step / 4.0).tan();
        let mut a0 = th1;
        for i in 0..n {
            let a1 = a0 + step;
            let (s0, c0) = a0.sin_cos();
            let (s1, c1) = a1.sin_cos();
            let p = |ex: f64, ey: f64| {
                Pt::new(
                    cx + cp * rx * ex - sp * ry * ey,
                    cy + sp * rx * ex + cp * ry * ey,
                )
            };
            let q1 = p(c0 - t * s0, s0 + t * c0);
            let q2 = p(c1 + t * s1, s1 - t * c1);
            let q3 = if i + 1 == n { to } else { p(c1, s1) };
            self.cubic_to(q1.x, q1.y, q2.x, q2.y, q3.x, q3.y);
            a0 = a1;
        }
    }

    pub fn transform(&self, t: &Transform) -> Path {
        Path {
            segs: self
                .segs
                .iter()
                .map(|s| match *s {
                    Seg::Move(p) => Seg::Move(t.apply(p)),
                    Seg::Line(p) => Seg::Line(t.apply(p)),
                    Seg::Quad(a, b) => Seg::Quad(t.apply(a), t.apply(b)),
                    Seg::Cubic(a, b, c) => Seg::Cubic(t.apply(a), t.apply(b), t.apply(c)),
                    Seg::Close => Seg::Close,
                })
                .collect(),
        }
    }

    pub fn append(&mut self, other: &Path) {
        self.segs.extend_from_slice(&other.segs);
    }

    /// Bounding box of the control points: `(min, max)`.
    pub fn bounds(&self) -> Option<(Pt, Pt)> {
        let mut lo = Pt::new(f64::INFINITY, f64::INFINITY);
        let mut hi = Pt::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut any = false;
        let mut add = |p: Pt| {
            if p.is_finite() {
                any = true;
                lo.x = lo.x.min(p.x);
                lo.y = lo.y.min(p.y);
                hi.x = hi.x.max(p.x);
                hi.y = hi.y.max(p.y);
            }
        };
        for s in &self.segs {
            match *s {
                Seg::Move(p) | Seg::Line(p) => add(p),
                Seg::Quad(a, b) => {
                    add(a);
                    add(b);
                }
                Seg::Cubic(a, b, c) => {
                    add(a);
                    add(b);
                    add(c);
                }
                Seg::Close => {}
            }
        }
        any.then_some((lo, hi))
    }

    /// Turn curves into line segments no further than `tol` from the true curve.
    pub fn flatten(&self, tol: f64) -> Vec<Poly> {
        let tol = if tol.is_finite() && tol > 1e-6 { tol } else { 0.1 };
        let mut out: Vec<Poly> = Vec::new();
        let mut cur: Vec<Pt> = Vec::new();
        let mut start = Pt::default();
        let mut last = Pt::default();
        let flush = |cur: &mut Vec<Pt>, out: &mut Vec<Poly>, closed: bool| {
            if !cur.is_empty() {
                out.push(Poly {
                    pts: std::mem::take(cur),
                    closed,
                });
            }
        };
        for s in &self.segs {
            match *s {
                Seg::Move(p) => {
                    flush(&mut cur, &mut out, false);
                    if !p.is_finite() {
                        continue;
                    }
                    start = p;
                    last = p;
                    cur.push(p);
                }
                Seg::Line(p) => {
                    if !p.is_finite() {
                        continue;
                    }
                    if cur.is_empty() {
                        cur.push(last);
                    }
                    cur.push(p);
                    last = p;
                }
                Seg::Quad(c, p) => {
                    if !(c.is_finite() && p.is_finite()) {
                        continue;
                    }
                    if cur.is_empty() {
                        cur.push(last);
                    }
                    let dd = last.sub(c.scale(2.0)).add(p).len();
                    let n = ((dd / (4.0 * tol)).sqrt().ceil() as usize).clamp(1, 500);
                    for i in 1..=n {
                        let t = i as f64 / n as f64;
                        let a = last.lerp(c, t);
                        let b = c.lerp(p, t);
                        cur.push(a.lerp(b, t));
                    }
                    last = p;
                }
                Seg::Cubic(c1, c2, p) => {
                    if !(c1.is_finite() && c2.is_finite() && p.is_finite()) {
                        continue;
                    }
                    if cur.is_empty() {
                        cur.push(last);
                    }
                    let d1 = last.sub(c1.scale(2.0)).add(c2).len();
                    let d2 = c1.sub(c2.scale(2.0)).add(p).len();
                    let dd = d1.max(d2);
                    let n = ((0.75 * dd / tol).sqrt().ceil() as usize).clamp(1, 500);
                    for i in 1..=n {
                        let t = i as f64 / n as f64;
                        let u = 1.0 - t;
                        let x = u * u * u * last.x
                            + 3.0 * u * u * t * c1.x
                            + 3.0 * u * t * t * c2.x
                            + t * t * t * p.x;
                        let y = u * u * u * last.y
                            + 3.0 * u * u * t * c1.y
                            + 3.0 * u * t * t * c2.y
                            + t * t * t * p.y;
                        cur.push(Pt::new(x, y));
                    }
                    last = p;
                }
                Seg::Close => {
                    if !cur.is_empty() {
                        flush(&mut cur, &mut out, true);
                    }
                    last = start;
                }
            }
        }
        flush(&mut cur, &mut out, false);
        out
    }
}
