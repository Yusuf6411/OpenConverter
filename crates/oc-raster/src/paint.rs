//! Paints: solid colours, gradients and image patterns.
use crate::geom::{Pt, Transform};

/// Straight (non-premultiplied) RGBA in `0.0..=1.0`.
pub type Rgba = [f32; 4];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spread {
    Pad,
    Reflect,
    Repeat,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub offset: f32,
    pub color: Rgba,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Bilinear,
}

/// An 8-bit straight-alpha RGBA picture used as a pattern source.
#[derive(Debug, Clone, PartialEq)]
pub struct Pixmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Pixmap {
    pub fn new(width: usize, height: usize, data: Vec<u8>) -> Option<Pixmap> {
        (data.len() == width.checked_mul(height)?.checked_mul(4)? && width > 0 && height > 0)
            .then_some(Pixmap {
                width,
                height,
                data,
            })
    }
    fn texel(&self, x: i64, y: i64, repeat: bool) -> [f32; 4] {
        let (w, h) = (self.width as i64, self.height as i64);
        let (x, y) = if repeat {
            (x.rem_euclid(w), y.rem_euclid(h))
        } else {
            (x.clamp(0, w - 1), y.clamp(0, h - 1))
        };
        let i = (y as usize * self.width + x as usize) * 4;
        let g = |k: usize| f32::from(self.data.get(i + k).copied().unwrap_or(0)) / 255.0;
        let a = g(3);
        [g(0) * a, g(1) * a, g(2) * a, a]
    }
    /// Premultiplied colour at pixel-space position `(x, y)`.
    pub fn sample(&self, x: f64, y: f64, filter: Filter, repeat: bool) -> [f32; 4] {
        match filter {
            Filter::Nearest => self.texel(x.floor() as i64, y.floor() as i64, repeat),
            Filter::Bilinear => {
                let fx = x - 0.5;
                let fy = y - 0.5;
                let (x0, y0) = (fx.floor(), fy.floor());
                let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
                let (x0, y0) = (x0 as i64, y0 as i64);
                let a = self.texel(x0, y0, repeat);
                let b = self.texel(x0 + 1, y0, repeat);
                let c = self.texel(x0, y0 + 1, repeat);
                let d = self.texel(x0 + 1, y0 + 1, repeat);
                let mut o = [0.0f32; 4];
                for k in 0..4 {
                    let top = a[k] + (b[k] - a[k]) * tx;
                    let bot = c[k] + (d[k] - c[k]) * tx;
                    o[k] = top + (bot - top) * ty;
                }
                o
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum Paint<'a> {
    Solid(Rgba),
    /// `to_unit` maps device space to gradient space.
    Linear {
        p0: Pt,
        p1: Pt,
        stops: Vec<Stop>,
        spread: Spread,
        extend: (bool, bool),
        to_gradient: Transform,
    },
    /// Two-circle gradient from `(c0, r0)` to `(c1, r1)`.
    Radial {
        c0: Pt,
        r0: f64,
        c1: Pt,
        r1: f64,
        stops: Vec<Stop>,
        spread: Spread,
        extend: (bool, bool),
        to_gradient: Transform,
    },
    /// `to_image` maps device space to pixel positions of `pix`.
    Image {
        pix: &'a Pixmap,
        to_image: Transform,
        filter: Filter,
        repeat: bool,
    },
}

fn color_at(stops: &[Stop], t: f32) -> [f32; 4] {
    let straight = match (stops.first(), stops.last()) {
        (None, _) | (_, None) => [0.0; 4],
        (Some(f), Some(l)) => {
            if t <= f.offset {
                f.color
            } else if t >= l.offset {
                l.color
            } else {
                let mut c = l.color;
                for w in stops.windows(2) {
                    if let [a, b] = w {
                        if t >= a.offset && t <= b.offset {
                            let span = b.offset - a.offset;
                            let u = if span <= 0.0 { 1.0 } else { (t - a.offset) / span };
                            c = [
                                a.color[0] + (b.color[0] - a.color[0]) * u,
                                a.color[1] + (b.color[1] - a.color[1]) * u,
                                a.color[2] + (b.color[2] - a.color[2]) * u,
                                a.color[3] + (b.color[3] - a.color[3]) * u,
                            ];
                            break;
                        }
                    }
                }
                c
            }
        }
    };
    [
        straight[0] * straight[3],
        straight[1] * straight[3],
        straight[2] * straight[3],
        straight[3],
    ]
}

fn spread_t(t: f64, spread: Spread, extend: (bool, bool)) -> Option<f32> {
    match spread {
        Spread::Pad => {
            if (t < 0.0 && !extend.0) || (t > 1.0 && !extend.1) {
                None
            } else {
                Some(t.clamp(0.0, 1.0) as f32)
            }
        }
        Spread::Repeat => Some(t.rem_euclid(1.0) as f32),
        Spread::Reflect => {
            let m = t.rem_euclid(2.0);
            Some(if m > 1.0 { 2.0 - m } else { m } as f32)
        }
    }
}

impl Paint<'_> {
    pub fn solid(c: Rgba) -> Paint<'static> {
        Paint::Solid(c)
    }

    pub fn is_opaque_solid(&self) -> Option<Rgba> {
        match self {
            Paint::Solid(c) => Some(*c),
            _ => None,
        }
    }

    /// Premultiplied colour at the centre of device pixel `(x, y)`.
    pub fn shade(&self, x: i32, y: i32) -> [f32; 4] {
        let dp = Pt::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
        match self {
            Paint::Solid(c) => [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]],
            Paint::Linear {
                p0,
                p1,
                stops,
                spread,
                extend,
                to_gradient,
            } => {
                let p = to_gradient.apply(dp);
                let d = p1.sub(*p0);
                let l2 = d.x * d.x + d.y * d.y;
                let t = if l2 <= 1e-30 {
                    0.0
                } else {
                    ((p.x - p0.x) * d.x + (p.y - p0.y) * d.y) / l2
                };
                match spread_t(t, *spread, *extend) {
                    Some(t) => color_at(stops, t),
                    None => [0.0; 4],
                }
            }
            Paint::Radial {
                c0,
                r0,
                c1,
                r1,
                stops,
                spread,
                extend,
                to_gradient,
            } => {
                let p = to_gradient.apply(dp);
                // Largest s with |p - (c0 + s*dc)| = r0 + s*dr and a non-negative radius.
                let dc = c1.sub(*c0);
                let dr = r1 - r0;
                let pc = p.sub(*c0);
                let a = dc.x * dc.x + dc.y * dc.y - dr * dr;
                let b = pc.x * dc.x + pc.y * dc.y + r0 * dr;
                let c = pc.x * pc.x + pc.y * pc.y - r0 * r0;
                let mut best: Option<f64> = None;
                let mut consider = |s: f64| {
                    if !s.is_finite() || r0 + s * dr < 0.0 {
                        return;
                    }
                    let ok = match spread {
                        Spread::Pad => (s >= 0.0 || extend.0) && (s <= 1.0 || extend.1),
                        _ => true,
                    };
                    if ok && best.is_none_or(|b| s > b) {
                        best = Some(s);
                    }
                };
                if a.abs() < 1e-12 {
                    if b.abs() > 1e-12 {
                        consider(c / (2.0 * b));
                    }
                } else {
                    let disc = b * b - a * c;
                    if disc >= 0.0 {
                        let sq = disc.sqrt();
                        consider((b + sq) / a);
                        consider((b - sq) / a);
                    }
                }
                match best.and_then(|s| spread_t(s, *spread, *extend)) {
                    Some(t) => color_at(stops, t),
                    None => [0.0; 4],
                }
            }
            Paint::Image {
                pix,
                to_image,
                filter,
                repeat,
            } => {
                let p = to_image.apply(dp);
                pix.sample(p.x, p.y, *filter, *repeat)
            }
        }
    }
}
