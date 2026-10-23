//! `clip-path`, `mask` and bounding boxes.
use crate::parse::{self, Length};
use crate::render::{Env, Rect, Renderer};
use crate::style::{LenCtx, Style};
use oc_raster::{Blend, Canvas, Clip, FillRule, Paint, Path, Transform};

impl Renderer<'_> {
    fn local_transform(&self, i: usize) -> Transform {
        self.doc
            .node(i)
            .and_then(|n| {
                n.attrs
                    .get("transform")
                    .or_else(|| n.style.get("transform"))
            })
            .and_then(|t| parse::transform(t))
            .unwrap_or(Transform::IDENTITY)
    }

    /// The bounding box of element `i` in its own user space (fill geometry only).
    pub fn bbox_of(&mut self, i: usize, env: &Env) -> Option<Rect> {
        self.bbox_rec(i, &env.lc, 0)
    }

    fn bbox_rec(&mut self, i: usize, lc: &LenCtx, depth: u32) -> Option<Rect> {
        if depth > 24 {
            return None;
        }
        let tag = self.doc.node(i)?.tag.clone();
        match tag.as_str() {
            "g" | "a" | "switch" | "svg" | "symbol" => {
                let kids = self.doc.node(i)?.children.clone();
                self.union_children(&kids, lc, depth)
            }
            "use" => {
                let t = self.doc.href_target(i)?;
                let x = self.len_attr(i, "x", lc.vw, 16.0).unwrap_or(0.0);
                let y = self.len_attr(i, "y", lc.vh, 16.0).unwrap_or(0.0);
                let inner = self.bbox_rec(t, lc, depth + 1)?;
                let tt = self.local_transform(t);
                Some(inner.transform(&tt.then(&Transform::translate(x, y))))
            }
            "image" => {
                let (x, y) = (
                    self.len_attr(i, "x", lc.vw, 16.0).unwrap_or(0.0),
                    self.len_attr(i, "y", lc.vh, 16.0).unwrap_or(0.0),
                );
                let (w, h) = (
                    self.len_attr(i, "width", lc.vw, 16.0)?,
                    self.len_attr(i, "height", lc.vh, 16.0)?,
                );
                Some(Rect { x, y, w, h })
            }
            "text" => self.text_bbox(i, lc),
            _ => {
                let style = self.style_chain(i);
                let p = self.shape_path(i, &style, lc)?;
                p.bounds().map(|(lo, hi)| Rect {
                    x: lo.x,
                    y: lo.y,
                    w: hi.x - lo.x,
                    h: hi.y - lo.y,
                })
            }
        }
    }

    fn union_children(&mut self, kids: &[usize], lc: &LenCtx, depth: u32) -> Option<Rect> {
        let mut acc: Option<Rect> = None;
        for &k in kids {
            if self
                .doc
                .node(k)
                .is_none_or(|n| n.style.get("display").is_some_and(|d| d.trim() == "none"))
            {
                continue;
            }
            if let Some(b) = self.bbox_rec(k, lc, depth + 1) {
                let b = b.transform(&self.local_transform(k));
                acc = Some(acc.map_or(b, |a| a.union(b)));
            }
        }
        acc
    }

    fn nothing(&self, clip: &Clip) -> Clip {
        clip.intersect(&[], FillRule::NonZero, self.canvas.height)
    }

    /// Narrow `clip` by the `<clipPath>` with id `id`.
    pub fn clip_by_path(&mut self, id: &str, clip: &Clip, env: &Env, bbox: Option<Rect>) -> Clip {
        let Some(cp) = self.doc.by_id(id) else {
            return clip.clone();
        };
        if self.doc.node(cp).is_none_or(|n| n.tag != "clipPath") {
            return clip.clone();
        }
        if self.active.contains(&cp) || self.active.len() > 8 {
            return self.nothing(clip);
        }
        let bbox_units = self
            .doc
            .attr(cp, "clipPathUnits")
            .is_some_and(|v| v.trim() == "objectBoundingBox");
        let mut base = self.local_transform(cp);
        if bbox_units {
            let Some(b) = bbox.filter(|b| b.w > 0.0 && b.h > 0.0) else {
                return self.nothing(clip);
            };
            base = Transform::new(b.w, 0.0, 0.0, b.h, b.x, b.y).then(&base);
        }
        let base = base.then(&env.ctm);
        let cstyle = self.style_chain(cp);
        let kids = self
            .doc
            .node(cp)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        self.active.push(cp);
        let (w, h) = (self.canvas.width, self.canvas.height);
        let mut acc = Canvas::new(w, h);
        let full = Clip::full(w, h);
        for k in kids {
            self.clip_child(k, &cstyle, &base, env, &full, acc.as_mut());
        }
        // The clipPath's own clip-path intersects the result.
        let own = self
            .doc
            .node(cp)
            .and_then(|n| n.style.get("clip-path"))
            .and_then(|v| crate::style::url_ref(v));
        let mut out = match acc {
            Some(c) => clip.intersect_mask(&c.alpha_mask()),
            None => self.nothing(clip),
        };
        if let Some(o) = own {
            let e2 = Env {
                ctm: env.ctm,
                clip: out.clone(),
                lc: env.lc,
            };
            out = self.clip_by_path(&o, &out, &e2, bbox);
        }
        self.active.pop();
        out
    }

    fn clip_child(
        &mut self,
        k: usize,
        parent: &Style,
        base: &Transform,
        env: &Env,
        full: &Clip,
        acc: Option<&mut Canvas>,
    ) {
        let Some(acc) = acc else { return };
        let Some(n) = self.doc.node(k) else { return };
        if n.style.get("display").is_some_and(|d| d.trim() == "none") {
            return;
        }
        let tag = n.tag.clone();
        let style = parent.derive(n, &env.lc);
        if !style.visible {
            return;
        }
        let local = self.local_transform(k);
        let ctm = local.then(base);
        let mut target = k;
        let mut ctm2 = ctm;
        let mut style2 = style.clone();
        if tag == "use" {
            let Some(t) = self.doc.href_target(k) else {
                return;
            };
            let x = self
                .len_attr(k, "x", env.lc.vw, style.font_size)
                .unwrap_or(0.0);
            let y = self
                .len_attr(k, "y", env.lc.vh, style.font_size)
                .unwrap_or(0.0);
            let Some(tn) = self.doc.node(t) else { return };
            if matches!(tn.tag.as_str(), "g" | "use" | "svg" | "symbol") {
                return;
            }
            style2 = style.derive(tn, &env.lc);
            ctm2 = self
                .local_transform(t)
                .then(&Transform::translate(x, y))
                .then(&ctm);
            target = t;
        }
        let path = if tag == "text" {
            None
        } else {
            self.shape_path(target, &style2, &env.lc)
        };
        let Some(path) = path else {
            if tag == "text" {
                self.note_once(
                    "svg.text_in_clip",
                    oc_core::Severity::Warning,
                    "text inside a clipPath is not supported and clips nothing",
                );
            }
            return;
        };
        // The child's own clip-path narrows what it adds.
        let mut cclip = full.clone();
        if let Some(id) = self
            .doc
            .node(k)
            .and_then(|n| n.style.get("clip-path"))
            .and_then(|v| crate::style::url_ref(v))
        {
            let bb = path.bounds().map(|(lo, hi)| Rect {
                x: lo.x,
                y: lo.y,
                w: hi.x - lo.x,
                h: hi.y - lo.y,
            });
            let e = Env {
                ctm: ctm2,
                clip: cclip.clone(),
                lc: env.lc,
            };
            cclip = self.clip_by_path(&id, &cclip, &e, bb);
        }
        let rule = if style2.clip_evenodd {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
        acc.fill_path(
            &path,
            &ctm2,
            rule,
            &Paint::Solid([0.0, 0.0, 0.0, 1.0]),
            Blend::Normal,
            1.0,
            &cclip,
        );
    }

    /// Narrow `clip` by the `<mask>` with id `id`.
    pub fn clip_by_mask(&mut self, id: &str, clip: &Clip, env: &Env, bbox: Option<Rect>) -> Clip {
        let Some(m) = self.doc.by_id(id) else {
            return clip.clone();
        };
        if self.doc.node(m).is_none_or(|n| n.tag != "mask") {
            return clip.clone();
        }
        if self.active.contains(&m) || self.active.len() > 8 {
            return self.nothing(clip);
        }
        let units_bbox = self
            .doc
            .attr(m, "maskUnits")
            .is_none_or(|v| v.trim() != "userSpaceOnUse");
        let content_bbox = self
            .doc
            .attr(m, "maskContentUnits")
            .is_some_and(|v| v.trim() == "objectBoundingBox");
        let b = bbox.filter(|b| b.w > 0.0 && b.h > 0.0);
        if (units_bbox || content_bbox) && b.is_none() {
            return self.nothing(clip);
        }
        let (bx, by, bw, bh) = b.map_or((0.0, 0.0, 1.0, 1.0), |b| (b.x, b.y, b.w, b.h));
        let lc = env.lc;
        let fs = self.style_chain(m).font_size;
        let region = |r: &Self, name: &str, refv: f64, default: f64, off: f64, size: f64| -> f64 {
            let l = r.doc.attr(m, name).and_then(Length::parse);
            if units_bbox {
                let f = l.map_or(default, |l| {
                    if l.unit == parse::Unit::Percent {
                        l.v / 100.0
                    } else {
                        l.v
                    }
                });
                off + f * size
            } else {
                l.map_or(default * refv, |l| l.resolve(refv, fs))
            }
        };
        let rx = region(self, "x", lc.vw, -0.1, bx, bw);
        let ry = region(self, "y", lc.vh, -0.1, by, bh);
        let rw = region(self, "width", lc.vw, 1.2, 0.0, bw);
        let rh = region(self, "height", lc.vh, 1.2, 0.0, bh);
        if !(rw > 0.0 && rh > 0.0) {
            return self.nothing(clip);
        }
        let (w, h) = (self.canvas.width, self.canvas.height);
        let Some(layer) = Canvas::new(w, h) else {
            return self.nothing(clip);
        };
        let polys = oc_raster::device_polys(&Path::rect(rx, ry, rw, rh), &env.ctm);
        let mut lclip = Clip::full(w, h).intersect(&polys, FillRule::NonZero, h);
        if lclip.is_empty() {
            return self.nothing(clip);
        }
        let saved = std::mem::replace(&mut self.canvas, layer);
        self.active.push(m);
        let mut ctm = env.ctm;
        if content_bbox {
            ctm = Transform::new(bw, 0.0, 0.0, bh, bx, by).then(&env.ctm);
        }
        let mstyle = self.style_chain(m);
        let menv = Env {
            ctm,
            clip: std::mem::replace(&mut lclip, Clip::full(1, 1)),
            lc,
        };
        let kids = self
            .doc
            .node(m)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        self.draw_children(&kids, &mstyle, &menv);
        self.active.pop();
        let layer = std::mem::replace(&mut self.canvas, saved);
        let alpha_type = self
            .doc
            .node(m)
            .and_then(|n| {
                n.style
                    .get("mask-type")
                    .or_else(|| n.attrs.get("mask-type"))
            })
            .is_some_and(|v| v.trim() == "alpha");
        let values: Vec<f32> = if alpha_type {
            layer.alpha_mask()
        } else {
            let linear = self
                .doc
                .node(m)
                .and_then(|n| n.style.get("color-interpolation"))
                .is_some_and(|v| v.trim() == "linearRGB");
            layer
                .data
                .chunks_exact(4)
                .map(|p| {
                    if linear && p[3] > 0.0 {
                        let l = |v: f32| srgb_to_linear(v / p[3]);
                        (0.2125 * l(p[0]) + 0.7154 * l(p[1]) + 0.0721 * l(p[2])) * p[3]
                    } else {
                        0.2125 * p[0] + 0.7154 * p[1] + 0.0721 * p[2]
                    }
                })
                .collect()
        };
        let mut out = clip.intersect_mask(&values);
        if let Some(o) = self
            .doc
            .node(m)
            .and_then(|n| n.style.get("mask"))
            .and_then(|v| crate::style::url_ref(v))
        {
            out = self.clip_by_mask(&o, &out, env, bbox);
        }
        out
    }
}

pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
