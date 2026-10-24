//! Gradients and patterns: resolving `fill="url(#id)"` into something `oc-raster` can paint.
use crate::parse::{self, Length};
use crate::render::{parse_viewbox, view_transform, Env, Rect, Renderer};
use crate::style::{LenCtx, PaintSpec, Style};
use oc_raster::{Canvas, Paint, Pixmap, Pt, Spread, Stop, Transform};

pub enum Resolved {
    Solid([f32; 4]),
    Linear {
        p0: Pt,
        p1: Pt,
        stops: Vec<Stop>,
        spread: Spread,
        to_gradient: Transform,
    },
    Radial {
        c0: Pt,
        r0: f64,
        c1: Pt,
        r1: f64,
        stops: Vec<Stop>,
        spread: Spread,
        to_gradient: Transform,
    },
    Pattern {
        pix: Pixmap,
        to_image: Transform,
    },
}

impl Resolved {
    pub fn with<R>(&self, f: impl FnOnce(&Paint, f32) -> R) -> R {
        match self {
            Resolved::Solid(c) => f(&Paint::Solid(*c), 1.0),
            Resolved::Linear {
                p0,
                p1,
                stops,
                spread,
                to_gradient,
            } => f(
                &Paint::Linear {
                    p0: *p0,
                    p1: *p1,
                    stops: stops.clone(),
                    spread: *spread,
                    extend: (true, true),
                    to_gradient: *to_gradient,
                },
                1.0,
            ),
            Resolved::Radial {
                c0,
                r0,
                c1,
                r1,
                stops,
                spread,
                to_gradient,
            } => f(
                &Paint::Radial {
                    c0: *c0,
                    r0: *r0,
                    c1: *c1,
                    r1: *r1,
                    stops: stops.clone(),
                    spread: *spread,
                    extend: (true, true),
                    to_gradient: *to_gradient,
                },
                1.0,
            ),
            Resolved::Pattern { pix, to_image } => f(
                &Paint::Image {
                    pix,
                    to_image: *to_image,
                    filter: oc_raster::Filter::Bilinear,
                    repeat: true,
                },
                1.0,
            ),
        }
    }
}

/// Opacity-aware result: the paint and the alpha to composite it with.
pub struct ResolvedPaint {
    pub paint: Resolved,
    pub alpha: f32,
}

impl ResolvedPaint {
    pub fn with<R>(&self, f: impl FnOnce(&Paint, f32) -> R) -> R {
        let a = self.alpha;
        self.paint.with(|p, k| f(p, a * k))
    }
}

impl Renderer<'_> {
    /// Follow `href` links to find the first element in the chain that has `name`.
    fn inherited_attr(&self, i: usize, name: &str) -> Option<String> {
        let mut cur = i;
        for _ in 0..16 {
            if let Some(v) = self.doc.attr(cur, name) {
                return Some(v.to_string());
            }
            cur = self.doc.href_target(cur)?;
        }
        None
    }

    fn inherited_children(&self, i: usize, want: &str) -> Vec<usize> {
        let mut cur = i;
        for _ in 0..16 {
            let kids: Vec<usize> = self
                .doc
                .node(cur)
                .map(|n| {
                    n.children
                        .iter()
                        .copied()
                        .filter(|k| self.doc.node(*k).is_some_and(|c| c.tag == want))
                        .collect()
                })
                .unwrap_or_default();
            if !kids.is_empty() {
                return kids;
            }
            match self.doc.href_target(cur) {
                Some(n) => cur = n,
                None => break,
            }
        }
        Vec::new()
    }

    fn parse_stops(&self, g: usize, current: [f32; 4]) -> Vec<Stop> {
        let mut out: Vec<Stop> = Vec::new();
        let mut last = 0.0f32;
        for k in self.inherited_children(g, "stop") {
            let Some(n) = self.doc.node(k) else { continue };
            let off = n
                .attrs
                .get("offset")
                .and_then(|v| Length::parse(v))
                .map(|l| {
                    if l.unit == parse::Unit::Percent {
                        (l.v / 100.0) as f32
                    } else {
                        l.v as f32
                    }
                })
                .unwrap_or(0.0)
                .clamp(0.0, 1.0)
                .max(last);
            last = off;
            let mut c = match n.style.get("stop-color").map(|s| s.trim()) {
                Some(v) if v.eq_ignore_ascii_case("currentcolor") => current,
                Some(v) => crate::color::parse(v).unwrap_or([0.0, 0.0, 0.0, 1.0]),
                None => [0.0, 0.0, 0.0, 1.0],
            };
            if let Some(o) = n
                .style
                .get("stop-opacity")
                .and_then(|v| crate::style::parse_opacity_pub(v))
            {
                c[3] *= o;
            }
            out.push(Stop {
                offset: off,
                color: c,
            });
        }
        out
    }

    pub fn resolve_paint(
        &mut self,
        spec: &PaintSpec,
        alpha: f32,
        style: &Style,
        bbox: Option<Rect>,
        env: &Env,
    ) -> Option<ResolvedPaint> {
        let solid = |c: [f32; 4]| {
            Some(ResolvedPaint {
                paint: Resolved::Solid(c),
                alpha,
            })
        };
        match spec {
            PaintSpec::None => None,
            PaintSpec::Color(c) => solid(*c),
            PaintSpec::CurrentColor => solid(style.color),
            PaintSpec::Url(id, fallback) => {
                let target = self.doc.by_id(id);
                let tag = target
                    .and_then(|t| self.doc.node(t))
                    .map(|n| n.tag.clone())
                    .unwrap_or_default();
                let res = match (target, tag.as_str()) {
                    (Some(t), "linearGradient" | "radialGradient") => {
                        self.gradient(t, &tag, style, bbox, env)
                    }
                    (Some(t), "pattern") => self.pattern(t, bbox, env),
                    _ => {
                        return fallback
                            .as_ref()
                            .and_then(|f| self.resolve_paint(f, alpha, style, bbox, env))
                    }
                };
                match res {
                    GradientResult::Paint(p) => Some(ResolvedPaint { paint: p, alpha }),
                    GradientResult::Solid(c) => solid(c),
                    GradientResult::Nothing => None,
                }
            }
        }
    }

    fn gradient(
        &mut self,
        g: usize,
        tag: &str,
        style: &Style,
        bbox: Option<Rect>,
        env: &Env,
    ) -> GradientResult {
        let stops = self.parse_stops(g, style.color);
        match stops.len() {
            0 => return GradientResult::Nothing,
            1 => return GradientResult::Solid(stops[0].color),
            _ => {}
        }
        let bbox_units = self
            .inherited_attr(g, "gradientUnits")
            .is_none_or(|v| v.trim() != "userSpaceOnUse");
        let mut base = Transform::IDENTITY;
        if bbox_units {
            let Some(b) = bbox.filter(|b| b.w > 0.0 && b.h > 0.0) else {
                return GradientResult::Nothing;
            };
            base = Transform::new(b.w, 0.0, 0.0, b.h, b.x, b.y);
        }
        let gt = self
            .inherited_attr(g, "gradientTransform")
            .and_then(|t| parse::transform(&t))
            .unwrap_or(Transform::IDENTITY);
        let full = gt.then(&base).then(&env.ctm);
        let Some(to_gradient) = full.invert() else {
            return GradientResult::Nothing;
        };
        let spread = match self
            .inherited_attr(g, "spreadMethod")
            .as_deref()
            .map(str::trim)
        {
            Some("reflect") => Spread::Reflect,
            Some("repeat") => Spread::Repeat,
            _ => Spread::Pad,
        };
        let lc = env.lc;
        let fs = style.font_size;
        let get = |r: &Self, name: &str, refv: f64, default: &str| -> f64 {
            let raw = r.inherited_attr(g, name);
            let l = raw
                .as_deref()
                .and_then(Length::parse)
                .or_else(|| Length::parse(default))
                .unwrap_or(Length::px(0.0));
            if bbox_units {
                if l.unit == parse::Unit::Percent {
                    l.v / 100.0
                } else {
                    l.v
                }
            } else {
                l.resolve(refv, fs)
            }
        };
        if tag == "linearGradient" {
            let (x1, y1) = (get(self, "x1", lc.vw, "0%"), get(self, "y1", lc.vh, "0%"));
            let (x2, y2) = (get(self, "x2", lc.vw, "100%"), get(self, "y2", lc.vh, "0%"));
            if x1 == x2 && y1 == y2 {
                return GradientResult::Solid(stops.last().map_or([0.0; 4], |s| s.color));
            }
            GradientResult::Paint(Resolved::Linear {
                p0: Pt::new(x1, y1),
                p1: Pt::new(x2, y2),
                stops,
                spread,
                to_gradient,
            })
        } else {
            let diag = lc.diag();
            let cx = get(self, "cx", lc.vw, "50%");
            let cy = get(self, "cy", lc.vh, "50%");
            let r = get(self, "r", diag, "50%");
            let fx = if self.inherited_attr(g, "fx").is_some() {
                get(self, "fx", lc.vw, "50%")
            } else {
                cx
            };
            let fy = if self.inherited_attr(g, "fy").is_some() {
                get(self, "fy", lc.vh, "50%")
            } else {
                cy
            };
            let fr = get(self, "fr", diag, "0%");
            if r <= 0.0 {
                return GradientResult::Solid(stops.last().map_or([0.0; 4], |s| s.color));
            }
            GradientResult::Paint(Resolved::Radial {
                c0: Pt::new(fx, fy),
                r0: fr.max(0.0),
                c1: Pt::new(cx, cy),
                r1: r,
                stops,
                spread,
                to_gradient,
            })
        }
    }

    fn pattern(&mut self, p: usize, bbox: Option<Rect>, env: &Env) -> GradientResult {
        if self.active.contains(&p) || self.active.len() > 6 {
            return GradientResult::Nothing;
        }
        // The element that carries the pattern's content.
        let mut holder = p;
        {
            let mut cur = p;
            for _ in 0..16 {
                if self.doc.node(cur).is_some_and(|n| {
                    n.children
                        .iter()
                        .any(|k| self.doc.node(*k).is_some_and(|c| c.tag != "#text"))
                }) {
                    holder = cur;
                    break;
                }
                match self.doc.href_target(cur) {
                    Some(n) => cur = n,
                    None => break,
                }
            }
        }
        let units_bbox = self
            .inherited_attr(p, "patternUnits")
            .is_none_or(|v| v.trim() != "userSpaceOnUse");
        let content_bbox = self
            .inherited_attr(p, "patternContentUnits")
            .is_some_and(|v| v.trim() == "objectBoundingBox");
        let b = bbox.filter(|b| b.w > 0.0 && b.h > 0.0);
        if (units_bbox || content_bbox) && b.is_none() {
            return GradientResult::Nothing;
        }
        let pstyle = self.style_chain(p);
        let fs = pstyle.font_size;
        let lc = env.lc;
        let num = |r: &Self, name: &str, refv: f64, bb: f64, off: f64| -> f64 {
            let l = r
                .inherited_attr(p, name)
                .as_deref()
                .and_then(Length::parse)
                .unwrap_or(Length::px(0.0));
            if units_bbox {
                let f = if l.unit == parse::Unit::Percent {
                    l.v / 100.0
                } else {
                    l.v
                };
                off + f * bb
            } else {
                l.resolve(refv, fs)
            }
        };
        let (bx, by, bw, bh) = b.map_or((0.0, 0.0, 1.0, 1.0), |b| (b.x, b.y, b.w, b.h));
        let x = num(self, "x", lc.vw, bw, bx);
        let y = num(self, "y", lc.vh, bh, by);
        let w = num(self, "width", lc.vw, bw, 0.0);
        let h = num(self, "height", lc.vh, bh, 0.0);
        if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
            return GradientResult::Nothing;
        }
        let pt = self
            .inherited_attr(p, "patternTransform")
            .and_then(|t| parse::transform(&t))
            .unwrap_or(Transform::IDENTITY);
        let full = pt.then(&env.ctm);
        let Some(inv) = full.invert() else {
            return GradientResult::Nothing;
        };
        let sx = (full.a * full.a + full.b * full.b).sqrt();
        let sy = (full.c * full.c + full.d * full.d).sqrt();
        let tw = (w * sx).ceil().clamp(1.0, 2048.0);
        let th = (h * sy).ceil().clamp(1.0, 2048.0);
        let (kx, ky) = (tw / w, th / h);
        let Some(tile) = Canvas::new(tw as usize, th as usize) else {
            return GradientResult::Nothing;
        };
        // Content coordinates → tile pixels.
        let vb = self
            .inherited_attr(p, "viewBox")
            .and_then(|v| parse_viewbox(&v));
        let content_t = if let Some(v) = vb {
            let par = self
                .inherited_attr(p, "preserveAspectRatio")
                .unwrap_or_else(|| "xMidYMid meet".to_string());
            view_transform(v, (0.0, 0.0, w, h), &par)
        } else if content_bbox {
            Transform::scale(bw, bh)
        } else {
            Transform::IDENTITY
        };
        let tile_ctm = content_t.then(&Transform::scale(kx, ky));
        let inner_lc = vb.map_or(lc, |v| LenCtx { vw: v.2, vh: v.3 });
        let saved = std::mem::replace(&mut self.canvas, tile);
        self.active.push(p);
        let tenv = Env {
            ctm: tile_ctm,
            clip: self.canvas.clip(),
            lc: inner_lc,
        };
        let kids = self
            .doc
            .node(holder)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        self.draw_children(&kids, &pstyle, &tenv);
        self.active.pop();
        let tile = std::mem::replace(&mut self.canvas, saved);
        let rgba = tile.to_rgba8(None);
        let Some(pix) = Pixmap::new(tile.width, tile.height, rgba) else {
            return GradientResult::Nothing;
        };
        // device → pattern space → tile pixels
        let to_image = inv
            .then(&Transform::translate(-x, -y))
            .then(&Transform::scale(kx, ky));
        GradientResult::Paint(Resolved::Pattern { pix, to_image })
    }
}

enum GradientResult {
    Paint(Resolved),
    Solid([f32; 4]),
    Nothing,
}
