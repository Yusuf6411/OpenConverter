//! The SVG renderer: walks the DOM and draws into an `oc-raster` canvas.
use crate::color::Rgba;
use crate::dom::{self, Doc};
use crate::parse::{self, Length};
use crate::style::{LenCtx, Style};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};
use oc_font::db::FontDb;
use oc_raster::{Blend, Canvas, Clip, FillRule, Path, Pt, Stroke, Transform};

pub type ImageDecoder<'a> = dyn Fn(&[u8]) -> Option<oc_image::Image> + 'a;

pub struct Options<'a> {
    /// Output size; one given alone keeps the aspect ratio.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Multiplier for the intrinsic size when no size is given.
    pub scale: f64,
    /// Opaque background; `None` leaves the picture transparent.
    pub background: Option<Rgba>,
    pub fonts: Option<&'a FontDb>,
    pub image_decoder: Option<&'a ImageDecoder<'a>>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            width: None,
            height: None,
            scale: 1.0,
            background: None,
            fonts: None,
            image_decoder: None,
        }
    }
}

pub struct Rendered {
    pub canvas: Canvas,
    pub notes: Notes,
    /// The size the document asks for, in CSS pixels.
    pub intrinsic: (f64, f64),
}

#[derive(Clone)]
pub struct Env {
    pub ctm: Transform,
    pub clip: Clip,
    pub lc: LenCtx,
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn union(self, o: Rect) -> Rect {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
    pub fn transform(self, t: &Transform) -> Rect {
        let pts = [
            t.apply(Pt::new(self.x, self.y)),
            t.apply(Pt::new(self.x + self.w, self.y)),
            t.apply(Pt::new(self.x, self.y + self.h)),
            t.apply(Pt::new(self.x + self.w, self.y + self.h)),
        ];
        let x0 = pts.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
        let y0 = pts.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        let x1 = pts.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
        let y1 = pts.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
    pub fn path(&self) -> Path {
        Path::rect(self.x, self.y, self.w, self.h)
    }
}

pub struct Renderer<'a> {
    pub doc: &'a Doc,
    pub opts: &'a Options<'a>,
    pub canvas: Canvas,
    pub notes: Notes,
    pub depth: u32,
    pub budget: u64,
    pub seen: std::collections::HashSet<&'static str>,
    pub limits: Limits,
    pub no_fonts: FontDb,
    /// Resources being drawn right now, to stop reference cycles.
    pub active: Vec<usize>,
}

pub fn render(text: &str, opts: &Options, limits: &Limits) -> Result<Rendered> {
    let doc = dom::parse(text, limits)?;
    let root = doc.root;
    let rstyle = Style::default();
    let probe_lc = LenCtx {
        vw: 300.0,
        vh: 150.0,
    };
    let node = doc
        .node(root)
        .ok_or_else(|| Error::new(ErrorCode::InternalError, "no root"))?;
    let fs = rstyle.derive(node, &probe_lc).font_size;
    let vb = doc.attr(root, "viewBox").and_then(parse_viewbox);
    let len = |name: &str, refv: f64| -> Option<f64> {
        let l = Length::parse(doc.attr(root, name)?)?;
        (l.unit != crate::parse::Unit::Percent).then(|| l.resolve(refv, fs))
    };
    let (mut iw, mut ih) = (len("width", 0.0), len("height", 0.0));
    match (iw, ih, vb) {
        (None, None, Some(v)) => {
            iw = Some(v.2);
            ih = Some(v.3);
        }
        (Some(w), None, Some(v)) if v.2 > 0.0 => ih = Some(w * v.3 / v.2),
        (None, Some(h), Some(v)) if v.3 > 0.0 => iw = Some(h * v.2 / v.3),
        _ => {}
    }
    let intrinsic = (iw.unwrap_or(300.0), ih.unwrap_or(150.0));
    if !(intrinsic.0 > 0.0
        && intrinsic.1 > 0.0
        && intrinsic.0.is_finite()
        && intrinsic.1.is_finite())
    {
        return Err(Error::corrupt("the SVG has no drawable size"));
    }
    let scale = if opts.scale.is_finite() && opts.scale > 0.0 {
        opts.scale
    } else {
        1.0
    };
    let (pw, ph) = match (opts.width, opts.height) {
        (Some(w), Some(h)) => (f64::from(w), f64::from(h)),
        (Some(w), None) => (f64::from(w), f64::from(w) * intrinsic.1 / intrinsic.0),
        (None, Some(h)) => (f64::from(h) * intrinsic.0 / intrinsic.1, f64::from(h)),
        (None, None) => (intrinsic.0 * scale, intrinsic.1 * scale),
    };
    let (pw, ph) = ((pw.round() as u64).max(1), (ph.round() as u64).max(1));
    limits.check_pixels(
        pw.min(u64::from(u32::MAX)) as u32,
        ph.min(u64::from(u32::MAX)) as u32,
    )?;
    if pw * ph > 1 << 28 {
        return Err(Error::limit(
            "max_pixels",
            "the SVG would render larger than 268 megapixels",
        ));
    }
    let canvas = Canvas::new(pw as usize, ph as usize)
        .ok_or_else(|| Error::limit("max_alloc", "canvas too large"))?;
    let mut r = Renderer {
        doc: &doc,
        opts,
        canvas,
        notes: Notes::new(),
        depth: 0,
        budget: 5_000_000,
        seen: std::collections::HashSet::new(),
        limits: limits.clone(),
        no_fonts: FontDb::empty(),
        active: Vec::new(),
    };
    if let Some(bg) = opts.background {
        r.canvas.fill_color(bg);
    }
    // user space → pixels
    let par = doc
        .attr(root, "preserveAspectRatio")
        .unwrap_or("xMidYMid meet");
    let (view, vw, vh) = match vb {
        Some(v) => (v, v.2, v.3),
        None => (
            (0.0, 0.0, intrinsic.0, intrinsic.1),
            intrinsic.0,
            intrinsic.1,
        ),
    };
    let ctm = view_transform(view, (0.0, 0.0, pw as f64, ph as f64), par);
    let env = Env {
        ctm,
        clip: r.canvas.clip(),
        lc: LenCtx { vw, vh },
    };
    let mut style = Style::default();
    style = style.derive(node, &env.lc);
    let kids = node.children.clone();
    // The root's own opacity, filter and clip-path apply to the whole picture.
    r.draw_group_like(root, &style, &env, &kids, true);
    Ok(Rendered {
        canvas: r.canvas,
        notes: r.notes,
        intrinsic,
    })
}

pub fn parse_viewbox(s: &str) -> Option<(f64, f64, f64, f64)> {
    let v = parse::numbers(s);
    match v.as_slice() {
        [x, y, w, h] if *w > 0.0 && *h > 0.0 => Some((*x, *y, *w, *h)),
        _ => None,
    }
}

/// The transform that fits `view` (a viewBox) into `vp` as `preserveAspectRatio` says.
pub fn view_transform(
    view: (f64, f64, f64, f64),
    vp: (f64, f64, f64, f64),
    par: &str,
) -> Transform {
    let mut it = par.split_whitespace().filter(|w| *w != "defer");
    let align = it.next().unwrap_or("xMidYMid");
    let slice = it.next() == Some("slice");
    let (sx, sy) = (vp.2 / view.2, vp.3 / view.3);
    if align == "none" {
        return Transform::translate(-view.0, -view.1)
            .then(&Transform::scale(sx, sy))
            .then(&Transform::translate(vp.0, vp.1));
    }
    let s = if slice { sx.max(sy) } else { sx.min(sy) };
    let (ax, ay) = (
        if align.contains("xMin") {
            0.0
        } else if align.contains("xMax") {
            1.0
        } else {
            0.5
        },
        if align.contains("YMin") {
            0.0
        } else if align.contains("YMax") {
            1.0
        } else {
            0.5
        },
    );
    let tx = vp.0 + (vp.2 - view.2 * s) * ax - view.0 * s;
    let ty = vp.1 + (vp.3 - view.3 * s) * ay - view.1 * s;
    Transform::new(s, 0.0, 0.0, s, tx, ty)
}

impl<'a> Renderer<'a> {
    pub fn note_once(&mut self, key: &'static str, severity: Severity, msg: &str) {
        if self.seen.insert(key) {
            self.notes.push(Note::new(severity, key, msg));
        }
    }

    pub fn fonts(&self) -> &FontDb {
        self.opts.fonts.unwrap_or(&self.no_fonts)
    }

    /// The style an element has, computed down from the root (for resources like gradients).
    pub fn style_chain(&self, i: usize) -> Style {
        let mut chain = Vec::new();
        let mut cur = Some(i);
        while let Some(c) = cur {
            chain.push(c);
            cur = self.doc.node(c).and_then(|n| n.parent);
        }
        let lc = LenCtx {
            vw: 300.0,
            vh: 150.0,
        };
        let mut s = Style::default();
        for c in chain.into_iter().rev() {
            if let Some(n) = self.doc.node(c) {
                s = s.derive(n, &lc);
            }
        }
        s
    }

    pub fn len_attr(&self, i: usize, name: &str, reference: f64, fs: f64) -> Option<f64> {
        Length::parse(self.doc.attr(i, name)?).map(|l| l.resolve(reference, fs))
    }

    fn element_transform(&self, i: usize) -> Transform {
        let n = match self.doc.node(i) {
            Some(n) => n,
            None => return Transform::IDENTITY,
        };
        let raw = n
            .attrs
            .get("transform")
            .or_else(|| n.style.get("transform"));
        raw.and_then(|t| parse::transform(t))
            .unwrap_or(Transform::IDENTITY)
    }

    /// Draw a child list under `parent` (the style of the element that owns them).
    pub fn draw_children(&mut self, kids: &[usize], parent: &Style, env: &Env) {
        for &k in kids {
            self.draw_element(k, parent, env);
        }
    }

    /// Draw one element.
    pub fn draw_element(&mut self, i: usize, parent: &Style, env: &Env) {
        if self.depth > 48 || self.budget == 0 {
            return;
        }
        let Some(node) = self.doc.node(i) else { return };
        let tag = node.tag.as_str();
        if !matches!(
            tag,
            "g" | "svg"
                | "switch"
                | "a"
                | "use"
                | "path"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "image"
                | "text"
        ) {
            return;
        }
        if node
            .style
            .get("display")
            .is_some_and(|d| d.trim() == "none")
        {
            return;
        }
        self.budget -= 1;
        self.depth += 1;
        let style = parent.derive(node, &env.lc);
        let local = self.element_transform(i);
        let mut env2 = env.clone();
        env2.ctm = local.then(&env.ctm);
        match tag {
            "g" | "a" | "switch" | "svg" | "use" | "text" | "image" => {
                self.draw_container(i, &style, &env2, env)
            }
            _ => self.draw_shape_element(i, &style, &env2, env),
        }
        self.depth -= 1;
    }

    fn opacity_of(&self, i: usize) -> f32 {
        self.doc
            .node(i)
            .and_then(|n| n.style.get("opacity"))
            .and_then(|v| crate::style::parse_opacity_pub(v))
            .unwrap_or(1.0)
    }

    fn effects_of(&self, i: usize) -> Effects {
        let n = match self.doc.node(i) {
            Some(n) => n,
            None => return Effects::default(),
        };
        Effects {
            opacity: self.opacity_of(i),
            clip_path: n
                .style
                .get("clip-path")
                .and_then(|v| crate::style::url_ref(v)),
            mask: n.style.get("mask").and_then(|v| crate::style::url_ref(v)),
            filter: n
                .style
                .get("filter")
                .cloned()
                .filter(|f| f.trim() != "none"),
            blend: n
                .style
                .get("mix-blend-mode")
                .and_then(|b| Blend::from_name(b.trim()))
                .unwrap_or(Blend::Normal),
            isolate: n
                .style
                .get("isolation")
                .is_some_and(|v| v.trim() == "isolate"),
        }
    }

    fn draw_shape_element(&mut self, i: usize, style: &Style, env2: &Env, outer: &Env) {
        let fx = self.effects_of(i);
        let Some(path) = self.shape_path(i, style, &env2.lc) else {
            return;
        };
        let leaf_ok = fx.filter.is_none() && fx.blend == Blend::Normal;
        let both = !matches!(style.fill, crate::style::PaintSpec::None)
            && !matches!(style.stroke, crate::style::PaintSpec::None)
            && style.stroke_width > 0.0;
        if leaf_ok && (fx.opacity >= 1.0 || !both) {
            let mut e = env2.clone();
            e.clip = self.apply_clip_and_mask(i, &fx, env2, Some(&path));
            self.paint_shape(i, style, &e, &path, fx.opacity);
            return;
        }
        let bbox_path = path.clone();
        self.with_layer(i, &fx, style, env2, outer, Some(&bbox_path), |r, e| {
            r.paint_shape(i, style, e, &path, 1.0);
        });
    }

    fn draw_container(&mut self, i: usize, style: &Style, env2: &Env, outer: &Env) {
        let tag = self.doc.node(i).map_or("", |n| n.tag.as_str());
        let fx = self.effects_of(i);
        let leaf = matches!(tag, "image");
        if leaf && fx.filter.is_none() && fx.blend == Blend::Normal {
            let mut e = env2.clone();
            e.clip = self.apply_clip_and_mask(i, &fx, env2, None);
            self.draw_image(i, style, &e, fx.opacity);
            return;
        }
        self.with_layer(i, &fx, style, env2, outer, None, |r, e| match tag {
            "g" | "a" => {
                let kids = r
                    .doc
                    .node(i)
                    .map(|n| n.children.clone())
                    .unwrap_or_default();
                r.draw_children(&kids, style, e);
            }
            "switch" => {
                let kids = r
                    .doc
                    .node(i)
                    .map(|n| n.children.clone())
                    .unwrap_or_default();
                for k in kids {
                    if r.doc.node(k).is_some_and(|n| n.tag != "#text") && r.passes_conditions(k) {
                        r.draw_element(k, style, e);
                        break;
                    }
                }
            }
            "svg" => r.draw_nested_svg(i, style, e, None),
            "use" => r.draw_use(i, style, e),
            "text" => r.draw_text(i, style, e),
            "image" => r.draw_image(i, style, e, 1.0),
            _ => {}
        });
    }

    fn passes_conditions(&self, i: usize) -> bool {
        let Some(n) = self.doc.node(i) else {
            return false;
        };
        if n.attrs.contains_key("requiredExtensions") {
            return false;
        }
        if let Some(l) = n.attrs.get("systemLanguage") {
            return l
                .split(',')
                .any(|x| x.trim().to_ascii_lowercase().starts_with("en"));
        }
        true
    }

    /// Run `body` either directly or on a transparent layer that is then composited with the
    /// element's opacity, filter, blend mode, clip-path and mask.
    #[allow(clippy::too_many_arguments)]
    fn with_layer(
        &mut self,
        i: usize,
        fx: &Effects,
        style: &Style,
        env2: &Env,
        outer: &Env,
        bbox_path: Option<&Path>,
        body: impl FnOnce(&mut Self, &Env),
    ) {
        let needs_layer =
            fx.opacity < 1.0 || fx.filter.is_some() || fx.blend != Blend::Normal || fx.isolate;
        if !needs_layer {
            let mut e = env2.clone();
            e.clip = self.apply_clip_and_mask(i, fx, env2, bbox_path);
            body(self, &e);
            return;
        }
        let (w, h) = (self.canvas.width, self.canvas.height);
        let Some(layer) = Canvas::new(w, h) else {
            return;
        };
        let saved = std::mem::replace(&mut self.canvas, layer);
        let shaped_clip = self.apply_clip_and_mask(i, fx, env2, bbox_path);
        let (inner_clip, composite_clip) = if fx.filter.is_some() {
            (env2.clip.clone(), shaped_clip)
        } else {
            (shaped_clip, outer.clip.clone())
        };
        let mut inner = env2.clone();
        inner.clip = inner_clip;
        body(self, &inner);
        let mut layer = std::mem::replace(&mut self.canvas, saved);
        if let Some(f) = &fx.filter {
            self.apply_filter(i, f, style, env2, bbox_path, &mut layer);
        }
        self.canvas
            .draw_canvas(&layer, fx.blend, fx.opacity, &composite_clip);
    }

    /// The clip for an element's content: the surrounding clip narrowed by `clip-path` and `mask`.
    fn apply_clip_and_mask(
        &mut self,
        i: usize,
        fx: &Effects,
        env: &Env,
        bbox_path: Option<&Path>,
    ) -> Clip {
        let mut clip = env.clip.clone();
        if clip.is_empty() {
            return clip;
        }
        if let Some(id) = &fx.clip_path {
            let bbox = self.bbox_of(i, env);
            let _ = bbox_path;
            clip = self.clip_by_path(id, &clip, env, bbox);
        }
        if let Some(id) = &fx.mask {
            let bbox = self.bbox_of(i, env);
            clip = self.clip_by_mask(id, &clip, env, bbox);
        }
        clip
    }

    // ---- shapes ----

    pub fn shape_path(&self, i: usize, style: &Style, lc: &LenCtx) -> Option<Path> {
        let n = self.doc.node(i)?;
        let fs = style.font_size;
        let l = |name: &str, r: f64| self.len_attr(i, name, r, fs);
        match n.tag.as_str() {
            "rect" => {
                let (x, y) = (l("x", lc.vw).unwrap_or(0.0), l("y", lc.vh).unwrap_or(0.0));
                let (w, h) = (l("width", lc.vw)?, l("height", lc.vh)?);
                if w <= 0.0 || h <= 0.0 {
                    return None;
                }
                let (mut rx, mut ry) = (l("rx", lc.vw), l("ry", lc.vh));
                if rx.is_none() {
                    rx = ry;
                }
                if ry.is_none() {
                    ry = rx;
                }
                let (rx, ry) = (rx.unwrap_or(0.0).max(0.0), ry.unwrap_or(0.0).max(0.0));
                Some(Path::round_rect(x, y, w, h, rx, ry))
            }
            "circle" => {
                let r = l("r", lc.diag())?;
                if r <= 0.0 {
                    return None;
                }
                Some(Path::ellipse(
                    l("cx", lc.vw).unwrap_or(0.0),
                    l("cy", lc.vh).unwrap_or(0.0),
                    r,
                    r,
                ))
            }
            "ellipse" => {
                let (mut rx, mut ry) = (l("rx", lc.vw), l("ry", lc.vh));
                if rx.is_none() {
                    rx = ry;
                }
                if ry.is_none() {
                    ry = rx;
                }
                let (rx, ry) = (rx?, ry?);
                if rx <= 0.0 || ry <= 0.0 {
                    return None;
                }
                Some(Path::ellipse(
                    l("cx", lc.vw).unwrap_or(0.0),
                    l("cy", lc.vh).unwrap_or(0.0),
                    rx,
                    ry,
                ))
            }
            "line" => {
                let mut p = Path::new();
                p.move_to(l("x1", lc.vw).unwrap_or(0.0), l("y1", lc.vh).unwrap_or(0.0));
                p.line_to(l("x2", lc.vw).unwrap_or(0.0), l("y2", lc.vh).unwrap_or(0.0));
                Some(p)
            }
            "polyline" | "polygon" => {
                let pts = parse::points(n.attrs.get("points")?);
                if pts.len() < 2 {
                    return None;
                }
                let mut p = Path::new();
                for (k, q) in pts.iter().enumerate() {
                    if k == 0 {
                        p.move_to(q.x, q.y);
                    } else {
                        p.line_to(q.x, q.y);
                    }
                }
                if n.tag == "polygon" {
                    p.close();
                }
                Some(p)
            }
            "path" => {
                let p = parse::path_data(n.attrs.get("d")?);
                (!p.is_empty()).then_some(p)
            }
            _ => None,
        }
    }

    pub fn paint_shape(&mut self, i: usize, style: &Style, env: &Env, path: &Path, opacity: f32) {
        if !style.visible || env.clip.is_empty() {
            return;
        }
        let bbox = path.bounds().map(|(lo, hi)| Rect {
            x: lo.x,
            y: lo.y,
            w: hi.x - lo.x,
            h: hi.y - lo.y,
        });
        let rule = if style.fill_evenodd {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
        let stroke = Stroke {
            width: style.stroke_width,
            cap: style.cap,
            join: style.join,
            miter_limit: style.miter_limit,
            dash: style.dash.clone(),
            dash_phase: style.dash_offset,
            hairline: false,
        };
        let do_fill = |r: &mut Self| {
            if let Some(rp) =
                r.resolve_paint(&style.fill, style.fill_opacity * opacity, style, bbox, env)
            {
                rp.with(|paint, alpha| {
                    r.canvas.fill_path(
                        path,
                        &env.ctm,
                        rule,
                        paint,
                        Blend::Normal,
                        alpha,
                        &env.clip,
                    );
                });
            }
        };
        let do_stroke = |r: &mut Self| {
            if style.stroke_width <= 0.0 {
                return;
            }
            if let Some(rp) = r.resolve_paint(
                &style.stroke,
                style.stroke_opacity * opacity,
                style,
                bbox,
                env,
            ) {
                rp.with(|paint, alpha| {
                    r.canvas.stroke_path(
                        path,
                        &env.ctm,
                        &stroke,
                        paint,
                        Blend::Normal,
                        alpha,
                        &env.clip,
                    );
                });
            }
        };
        if style.paint_order_stroke_first {
            do_stroke(self);
            do_fill(self);
        } else {
            do_fill(self);
            do_stroke(self);
        }
        if matches!(
            self.doc.node(i).map(|n| n.tag.as_str()),
            Some("path" | "line" | "polyline" | "polygon")
        ) {
            self.draw_markers(i, style, env, path);
        }
    }

    // ---- nested svg, symbol, use ----

    pub fn draw_nested_svg(
        &mut self,
        i: usize,
        style: &Style,
        env: &Env,
        override_size: Option<(f64, f64)>,
    ) {
        let Some(n) = self.doc.node(i) else { return };
        let fs = style.font_size;
        let lc = env.lc;
        let x = self.len_attr(i, "x", lc.vw, fs).unwrap_or(0.0);
        let y = self.len_attr(i, "y", lc.vh, fs).unwrap_or(0.0);
        let mut w = self.len_attr(i, "width", lc.vw, fs).unwrap_or(lc.vw);
        let mut h = self.len_attr(i, "height", lc.vh, fs).unwrap_or(lc.vh);
        if let Some((ow, oh)) = override_size {
            w = ow;
            h = oh;
        }
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let vb = n.attrs.get("viewBox").and_then(|v| parse_viewbox(v));
        let par = n
            .attrs
            .get("preserveAspectRatio")
            .map_or("xMidYMid meet", String::as_str);
        let mut e = env.clone();
        let inner_lc = match vb {
            Some(v) => {
                let t = view_transform(v, (x, y, w, h), par);
                e.ctm = t.then(&env.ctm);
                LenCtx { vw: v.2, vh: v.3 }
            }
            None => {
                e.ctm = Transform::translate(x, y).then(&env.ctm);
                LenCtx { vw: w, vh: h }
            }
        };
        e.lc = inner_lc;
        let overflow = n.style.get("overflow").map_or("hidden", |s| s.trim());
        if overflow == "hidden" || overflow == "scroll" {
            let polys = oc_raster::device_polys(&Path::rect(x, y, w, h), &env.ctm);
            e.clip = e
                .clip
                .intersect(&polys, FillRule::NonZero, self.canvas.height);
        }
        let kids = n.children.clone();
        self.draw_children(&kids, style, &e);
    }

    fn draw_use(&mut self, i: usize, style: &Style, env: &Env) {
        let Some(target) = self.doc.href_target(i) else {
            return;
        };
        // A use that points at one of its own ancestors would never end.
        let mut cur = Some(i);
        while let Some(c) = cur {
            if c == target {
                return;
            }
            cur = self.doc.node(c).and_then(|n| n.parent);
        }
        let fs = style.font_size;
        let x = self.len_attr(i, "x", env.lc.vw, fs).unwrap_or(0.0);
        let y = self.len_attr(i, "y", env.lc.vh, fs).unwrap_or(0.0);
        let mut e = env.clone();
        e.ctm = Transform::translate(x, y).then(&env.ctm);
        let Some(t) = self.doc.node(target) else {
            return;
        };
        match t.tag.as_str() {
            "symbol" | "svg" => {
                let w = self.len_attr(i, "width", env.lc.vw, fs);
                let h = self.len_attr(i, "height", env.lc.vh, fs);
                let size = match (w, h) {
                    (None, None) => None,
                    (w, h) => Some((w.unwrap_or(env.lc.vw), h.unwrap_or(env.lc.vh))),
                };
                let tstyle = style.derive(t, &e.lc);
                // symbol's own x/y do not apply; the use's do (already in e.ctm)
                self.draw_symbol(target, &tstyle, &e, size);
            }
            _ => {
                self.depth += 1;
                self.draw_element(target, style, &e);
                self.depth -= 1;
            }
        }
    }

    fn draw_symbol(&mut self, i: usize, style: &Style, env: &Env, size: Option<(f64, f64)>) {
        let Some(n) = self.doc.node(i) else { return };
        let (w, h) = size.unwrap_or((env.lc.vw, env.lc.vh));
        let vb = n.attrs.get("viewBox").and_then(|v| parse_viewbox(v));
        let par = n
            .attrs
            .get("preserveAspectRatio")
            .map_or("xMidYMid meet", String::as_str);
        let mut e = env.clone();
        match vb {
            Some(v) => {
                e.ctm = view_transform(v, (0.0, 0.0, w, h), par).then(&env.ctm);
                e.lc = LenCtx { vw: v.2, vh: v.3 };
            }
            None => e.lc = LenCtx { vw: w, vh: h },
        }
        let overflow = n.style.get("overflow").map_or("hidden", |s| s.trim());
        if overflow == "hidden" {
            let polys = oc_raster::device_polys(&Path::rect(0.0, 0.0, w, h), &env.ctm);
            e.clip = e
                .clip
                .intersect(&polys, FillRule::NonZero, self.canvas.height);
        }
        let kids = n.children.clone();
        self.draw_children(&kids, style, &e);
    }

    /// Draw the children of `i` as a group. Used for the root element.
    pub fn draw_group_like(
        &mut self,
        i: usize,
        style: &Style,
        env: &Env,
        kids: &[usize],
        _root: bool,
    ) {
        let fx = self.effects_of(i);
        let outer = env.clone();
        self.with_layer(i, &fx, style, env, &outer, None, |r, e| {
            r.draw_children(kids, style, e);
        });
    }

    // ---- images ----

    pub fn draw_image(&mut self, i: usize, style: &Style, env: &Env, opacity: f32) {
        if !style.visible {
            return;
        }
        let Some(href) = self.doc.attr(i, "href") else {
            return;
        };
        let fs = style.font_size;
        let x = self.len_attr(i, "x", env.lc.vw, fs).unwrap_or(0.0);
        let y = self.len_attr(i, "y", env.lc.vh, fs).unwrap_or(0.0);
        let w = self.len_attr(i, "width", env.lc.vw, fs);
        let h = self.len_attr(i, "height", env.lc.vh, fs);
        let Some(bytes) = data_uri(href) else {
            self.note_once(
                "svg.external_image",
                Severity::Warning,
                "an <image> refers to a file or address outside the SVG; only data: URIs are used and nothing is fetched",
            );
            return;
        };
        let par = self
            .doc
            .attr(i, "preserveAspectRatio")
            .unwrap_or("xMidYMid meet")
            .to_string();
        // A nested SVG picture is rendered at the size it will be shown.
        let pix = if bytes.windows(4).any(|w| w == b"<svg") && !bytes.starts_with(b"\x89PNG") {
            self.render_nested_svg(&bytes, w, h, env)
        } else {
            self.decode_pixmap(&bytes)
        };
        let Some((pix, iw, ih)) = pix else {
            self.note_once(
                "svg.image_undecodable",
                Severity::Warning,
                "an embedded <image> could not be decoded",
            );
            return;
        };
        let (w, h) = match (w, h) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, w * ih / iw),
            (None, Some(h)) => (h * iw / ih, h),
            (None, None) => (iw, ih),
        };
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let view_t = view_transform((0.0, 0.0, iw, ih), (x, y, w, h), &par);
        let to_user = view_t; // image pixel → user space
        let full = to_user.then(&env.ctm);
        let Some(inv) = full.invert() else { return };
        let mut e = env.clone();
        // Clip to the viewport box (the image may overflow with `slice`).
        let polys = oc_raster::device_polys(&Path::rect(x, y, w, h), &env.ctm);
        e.clip = e
            .clip
            .intersect(&polys, FillRule::NonZero, self.canvas.height);
        let paint = oc_raster::Paint::Image {
            pix: &pix,
            to_image: inv,
            filter: if style.pixelated {
                oc_raster::Filter::Nearest
            } else {
                oc_raster::Filter::Bilinear
            },
            repeat: false,
        };
        let img_rect = Path::rect(0.0, 0.0, iw, ih);
        self.canvas.fill_path(
            &img_rect,
            &full,
            FillRule::NonZero,
            &paint,
            Blend::Normal,
            opacity,
            &e.clip,
        );
    }

    fn decode_pixmap(&mut self, bytes: &[u8]) -> Option<(oc_raster::Pixmap, f64, f64)> {
        let dec = self.opts.image_decoder?;
        let img = dec(bytes)?;
        let (img, _) = oc_image::ops::convert_depth(&img, oc_image::Depth::Eight);
        let (img, _) = oc_image::ops::convert_layout(&img, oc_image::Layout::Rgba, [255, 255, 255]);
        let (w, h) = (img.width as usize, img.height as usize);
        let pm = oc_raster::Pixmap::new(w, h, img.data)?;
        Some((pm, w as f64, h as f64))
    }

    fn render_nested_svg(
        &mut self,
        bytes: &[u8],
        w: Option<f64>,
        h: Option<f64>,
        env: &Env,
    ) -> Option<(oc_raster::Pixmap, f64, f64)> {
        if self.limits.max_depth == 0 || self.seen.contains("svg.nested") {
            return None;
        }
        self.seen.insert("svg.nested");
        let text = String::from_utf8_lossy(bytes);
        let scale = env.ctm.mean_scale().max(0.01);
        let inner = Options {
            width: w.map(|v| (v * scale).ceil().clamp(1.0, 8192.0) as u32),
            height: h.map(|v| (v * scale).ceil().clamp(1.0, 8192.0) as u32),
            scale,
            background: None,
            fonts: self.opts.fonts,
            image_decoder: self.opts.image_decoder,
        };
        let r = render(&text, &inner, &self.limits).ok();
        self.seen.remove("svg.nested");
        let r = r?;
        let (pw, ph) = (r.canvas.width, r.canvas.height);
        let rgba = r.canvas.to_rgba8(None);
        let pm = oc_raster::Pixmap::new(pw, ph, rgba)?;
        // report the size in user units so the picture lands in the right box
        Some((pm, pw as f64 / scale, ph as f64 / scale)).map(|(p, a, b)| {
            let _ = (a, b);
            (p, pw as f64, ph as f64)
        })
    }
}

#[derive(Default, Clone)]
pub struct Effects {
    pub opacity: f32,
    pub clip_path: Option<String>,
    pub mask: Option<String>,
    pub filter: Option<String>,
    pub blend: Blend,
    pub isolate: bool,
}

pub fn data_uri(href: &str) -> Option<Vec<u8>> {
    let rest = href.trim().strip_prefix("data:")?;
    let (meta, body) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        parse::base64_decode(body)
    } else {
        Some(parse::percent_decode(body))
    }
}
