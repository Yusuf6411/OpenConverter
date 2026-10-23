//! Draws a display list with `oc-raster`.
use crate::ops::{Fx, ImageData, MeshVertex, Op, Page, PaintSrc, PatternKind, PatternPaint, Shading, SoftMask, StrokeStyle, TextRun};
use oc_core::{Error, ErrorCode, Limits, Result};
use oc_raster::{device_polys, Blend, Cap, Canvas, Clip, FillRule, Filter, Join, Paint, Path, Pixmap, Pt, Spread, Stop, Stroke, Transform};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub struct RenderOptions {
    /// Device pixels per point.
    pub scale: f64,
    /// Background under the page; `None` keeps the page transparent.
    pub background: Option<[f32; 3]>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { scale: 96.0 / 72.0, background: Some([1.0, 1.0, 1.0]) }
    }
}

enum Prep {
    Solid([f32; 4]),
    Grad(Paint<'static>),
    Pix { pix: Pixmap, to_image: Transform, repeat: bool, filter: Filter },
}

impl Prep {
    fn paint(&self) -> Paint<'_> {
        match self {
            Prep::Solid(c) => Paint::Solid(*c),
            Prep::Grad(p) => p.clone(),
            Prep::Pix { pix, to_image, repeat, filter } => Paint::Image { pix, to_image: *to_image, filter: *filter, repeat: *repeat },
        }
    }
}

struct Renderer {
    /// Page space to device space.
    m: Transform,
    w: usize,
    h: usize,
    masks: RefCell<HashMap<usize, Rc<Vec<f32>>>>,
    pats: RefCell<HashMap<usize, Option<Rc<Prep>>>>,
    depth: RefCell<u32>,
}

pub fn render(page: &Page, opts: &RenderOptions, limits: &Limits) -> Result<Canvas> {
    let scale = if opts.scale.is_finite() && opts.scale > 0.0 { opts.scale } else { 96.0 / 72.0 };
    let w = (page.width * scale).ceil().max(1.0);
    let h = (page.height * scale).ceil().max(1.0);
    if w * h > limits.max_pixels as f64 || w > 65535.0 || h > 65535.0 {
        return Err(Error::new(ErrorCode::LimitExceeded, format!("the page would render at {w}x{h} pixels, over the limit")));
    }
    let mut cv = Canvas::new(w as usize, h as usize).ok_or_else(|| Error::new(ErrorCode::LimitExceeded, "the page is too large to render"))?;
    if let Some(bg) = opts.background {
        cv.fill_color([bg[0], bg[1], bg[2], 1.0]);
    }
    let r = Renderer { m: Transform::scale(scale, scale), w: w as usize, h: h as usize, masks: RefCell::new(HashMap::new()), pats: RefCell::new(HashMap::new()), depth: RefCell::new(0) };
    let clip = cv.clip();
    r.draw(&mut cv, &page.ops, &clip);
    Ok(cv)
}

fn q8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn cap_of(c: u8) -> Cap {
    match c {
        1 => Cap::Round,
        2 => Cap::Square,
        _ => Cap::Butt,
    }
}

fn join_of(j: u8) -> Join {
    match j {
        1 => Join::Round,
        2 => Join::Bevel,
        _ => Join::Miter,
    }
}

fn stroke_of(s: &StrokeStyle, scale: f64) -> Stroke {
    let _ = scale;
    Stroke { width: s.width, cap: cap_of(s.cap), join: join_of(s.join), miter_limit: s.miter, dash: s.dash.clone(), dash_phase: s.phase, hairline: true }
}

impl Renderer {
    fn draw(&self, cv: &mut Canvas, ops: &[Op], base: &Clip) {
        if *self.depth.borrow() > 24 {
            return;
        }
        *self.depth.borrow_mut() += 1;
        let mut clips: Vec<Clip> = vec![base.clone()];
        for op in ops {
            let Some(top) = clips.last() else { break };
            match op {
                Op::Save => {
                    let c = top.clone();
                    clips.push(c);
                }
                Op::Restore => {
                    if clips.len() > 1 {
                        clips.pop();
                    }
                }
                Op::Clip { path, rule } => {
                    let polys = device_polys(path, &self.m);
                    let n = top.intersect(&polys, *rule, self.h);
                    if let Some(t) = clips.last_mut() {
                        *t = n;
                    }
                }
                Op::Fill { path, rule, paint, fx } => {
                    if top.is_empty() {
                        continue;
                    }
                    let Some(clip) = self.fx_clip(top, fx, cv) else { continue };
                    if let Some(prep) = self.prep(paint) {
                        cv.fill_path(path, &self.m, *rule, &prep.paint(), fx.blend, fx.alpha, &clip);
                    }
                }
                Op::Stroke { path, ctm, style, paint, fx } => {
                    if top.is_empty() {
                        continue;
                    }
                    let Some(clip) = self.fx_clip(top, fx, cv) else { continue };
                    if let Some(prep) = self.prep(paint) {
                        let d = ctm.then(&self.m);
                        cv.stroke_path(path, &d, &stroke_of(style, 1.0), &prep.paint(), fx.blend, fx.alpha, &clip);
                    }
                }
                Op::Text(run) => {
                    if top.is_empty() {
                        continue;
                    }
                    self.text(cv, run, top);
                }
                Op::Image { image, ctm, fx, stencil_paint } => {
                    if top.is_empty() {
                        continue;
                    }
                    let Some(clip) = self.fx_clip(top, fx, cv) else { continue };
                    self.image(cv, image, ctm, fx, stencil_paint.as_ref(), &clip);
                }
                Op::Shade { shading, matrix, fx } => {
                    if top.is_empty() {
                        continue;
                    }
                    let Some(mut clip) = self.fx_clip(top, fx, cv) else { continue };
                    let t = matrix.then(&self.m);
                    if let Some(b) = shading_bbox(shading) {
                        let mut p = Path::new();
                        p.move_to(b[0], b[1]);
                        p.line_to(b[2], b[1]);
                        p.line_to(b[2], b[3]);
                        p.line_to(b[0], b[3]);
                        p.close();
                        clip = clip.intersect(&device_polys(&p, &t), FillRule::NonZero, self.h);
                    }
                    if let Some(prep) = self.shading_prep(shading, &t) {
                        let full = Path::rect(0.0, 0.0, self.w as f64, self.h as f64);
                        cv.fill_path(&full, &Transform::IDENTITY, FillRule::NonZero, &prep.paint(), fx.blend, fx.alpha, &clip);
                    }
                }
                Op::Group { ops, fx, .. } => {
                    if top.is_empty() {
                        continue;
                    }
                    let Some(clip) = self.fx_clip(top, fx, cv) else { continue };
                    let Some(mut layer) = Canvas::new(cv.width, cv.height) else { continue };
                    let full = layer.clip();
                    self.draw(&mut layer, ops, &full);
                    cv.draw_canvas(&layer, fx.blend, fx.alpha, &clip);
                }
                Op::Marked { .. } => {}
            }
        }
        *self.depth.borrow_mut() -= 1;
    }

    /// The clip with the effect's soft mask folded in.
    fn fx_clip(&self, top: &Clip, fx: &Fx, cv: &Canvas) -> Option<Clip> {
        match &fx.smask {
            None => Some(top.clone()),
            Some(sm) => {
                let key = std::sync::Arc::as_ptr(sm) as usize;
                let cached = self.masks.borrow().get(&key).cloned();
                let mask = match cached {
                    Some(m) => m,
                    None => {
                        let m = Rc::new(self.soft_mask(sm, cv.width, cv.height)?);
                        self.masks.borrow_mut().insert(key, m.clone());
                        m
                    }
                };
                Some(top.intersect_mask(&mask))
            }
        }
    }

    fn soft_mask(&self, sm: &SoftMask, w: usize, h: usize) -> Option<Vec<f32>> {
        let mut layer = Canvas::new(w, h)?;
        if sm.luminosity {
            layer.fill_color([sm.backdrop[0], sm.backdrop[1], sm.backdrop[2], 1.0]);
        }
        let clip = layer.clip();
        self.draw(&mut layer, &sm.ops, &clip);
        let mut mask = if sm.luminosity { layer.luminosity_mask() } else { layer.alpha_mask() };
        if let Some(f) = &sm.transfer {
            let lut: Vec<f32> = (0..=255).map(|i| f.eval(&[f64::from(i) / 255.0]).first().copied().unwrap_or(f64::from(i) / 255.0).clamp(0.0, 1.0) as f32).collect();
            for v in &mut mask {
                *v = lut[(v.clamp(0.0, 1.0) * 255.0 + 0.5) as usize];
            }
        }
        Some(mask)
    }

    fn prep(&self, p: &PaintSrc) -> Option<Rc<Prep>> {
        match p {
            PaintSrc::Rgb(c) => Some(Rc::new(Prep::Solid([c[0], c[1], c[2], 1.0]))),
            PaintSrc::Pattern(pat, _) => {
                let key = std::sync::Arc::as_ptr(pat) as usize;
                if let Some(c) = self.pats.borrow().get(&key) {
                    return c.clone();
                }
                let made = self.pattern_prep(pat).map(Rc::new);
                self.pats.borrow_mut().insert(key, made.clone());
                made
            }
        }
    }

    fn pattern_prep(&self, pat: &PatternPaint) -> Option<Prep> {
        let t = pat.matrix.then(&self.m);
        match &pat.kind {
            PatternKind::Shading(sh) => self.shading_prep(sh, &t),
            PatternKind::Tiling { ops, bbox, xstep, ystep, .. } => {
                let (xs, ys) = (xstep.abs(), ystep.abs());
                let ux = t.apply_vec(Pt::new(xs, 0.0)).len();
                let uy = t.apply_vec(Pt::new(0.0, ys)).len();
                let tw = (ux.ceil() as usize).clamp(1, 2048);
                let th = (uy.ceil() as usize).clamp(1, 2048);
                let mut tile = Canvas::new(tw, th)?;
                let tm = Transform::translate(-bbox[0], -bbox[1]).then(&Transform::scale(tw as f64 / xs, th as f64 / ys));
                let sub = Renderer { m: tm, w: tw, h: th, masks: RefCell::new(HashMap::new()), pats: RefCell::new(HashMap::new()), depth: RefCell::new(*self.depth.borrow()) };
                let nx = if bbox[2] - bbox[0] > xs * 1.01 { 1 } else { 0 };
                let ny = if bbox[3] - bbox[1] > ys * 1.01 { 1 } else { 0 };
                let mut bp = Path::new();
                bp.move_to(bbox[0], bbox[1]);
                bp.line_to(bbox[2], bbox[1]);
                bp.line_to(bbox[2], bbox[3]);
                bp.line_to(bbox[0], bbox[3]);
                bp.close();
                for iy in -ny..=ny {
                    for ix in -nx..=nx {
                        let shift = Transform::translate(ix as f64 * xs, iy as f64 * ys);
                        let sm = shift.then(&tm);
                        let sub2 = Renderer { m: sm, w: tw, h: th, masks: RefCell::new(HashMap::new()), pats: RefCell::new(HashMap::new()), depth: RefCell::new(*sub.depth.borrow()) };
                        let clip = tile.clip().intersect(&device_polys(&bp, &sm), FillRule::NonZero, th);
                        sub2.draw(&mut tile, ops, &clip);
                    }
                }
                let data = tile.to_rgba8(None);
                let pix = Pixmap::new(tw, th, data)?;
                let inv = t.invert()?;
                let to_image = inv.then(&tm);
                Some(Prep::Pix { pix, to_image, repeat: true, filter: Filter::Bilinear })
            }
        }
    }

    /// A paint for a shading drawn under `t` (shading space to device space).
    fn shading_prep(&self, sh: &Shading, t: &Transform) -> Option<Prep> {
        let inv = t.invert()?;
        match sh {
            Shading::Axial { coords, domain, func, extend, cs, .. } => {
                let stops = func_stops(func.as_ref()?, cs, *domain);
                Some(Prep::Grad(Paint::Linear { p0: Pt::new(coords[0], coords[1]), p1: Pt::new(coords[2], coords[3]), stops, spread: Spread::Pad, extend: (extend[0], extend[1]), to_gradient: inv }))
            }
            Shading::Radial { coords, domain, func, extend, cs, .. } => {
                let stops = func_stops(func.as_ref()?, cs, *domain);
                Some(Prep::Grad(Paint::Radial { c0: Pt::new(coords[0], coords[1]), r0: coords[2].abs(), c1: Pt::new(coords[3], coords[4]), r1: coords[5].abs(), stops, spread: Spread::Pad, extend: (extend[0], extend[1]), to_gradient: inv }))
            }
            Shading::FunctionBased { domain, matrix, func, cs, background } => {
                let minv = matrix.invert()?;
                let (w, h) = (self.w, self.h);
                let mut data = vec![0u8; w * h * 4];
                if w * h > 1 << 26 {
                    return None;
                }
                for y in 0..h {
                    for x in 0..w {
                        let p = minv.apply(inv.apply(Pt::new(x as f64 + 0.5, y as f64 + 0.5)));
                        let i = (y * w + x) * 4;
                        let rgb = if p.x >= domain[0] && p.x <= domain[1] && p.y >= domain[2] && p.y <= domain[3] {
                            Some(cs.to_rgb(&func.eval(&[p.x, p.y])))
                        } else {
                            *background
                        };
                        if let (Some(c), Some(px)) = (rgb, data.get_mut(i..i + 4)) {
                            px.copy_from_slice(&[q8(c[0]), q8(c[1]), q8(c[2]), 255]);
                        }
                    }
                }
                Some(Prep::Pix { pix: Pixmap::new(w, h, data)?, to_image: Transform::IDENTITY, repeat: false, filter: Filter::Nearest })
            }
            Shading::Mesh { triangles, func, cs, background } => {
                let (w, h) = (self.w, self.h);
                if w * h > 1 << 26 {
                    return None;
                }
                let mut data = vec![0u8; w * h * 4];
                if let Some(bg) = background {
                    for px in data.chunks_exact_mut(4) {
                        px.copy_from_slice(&[q8(bg[0]), q8(bg[1]), q8(bg[2]), 255]);
                    }
                }
                let (mut tmin, mut tmax) = (f32::MAX, f32::MIN);
                for tri in triangles {
                    for v in tri {
                        tmin = tmin.min(v.c[0]);
                        tmax = tmax.max(v.c[0]);
                    }
                }
                let lut: Option<Vec<[f32; 3]>> = func.as_ref().map(|f| {
                    (0..256)
                        .map(|i| {
                            let tt = f64::from(tmin) + (f64::from(tmax) - f64::from(tmin)) * f64::from(i) / 255.0;
                            cs.to_rgb(&f.eval(&[tt]))
                        })
                        .collect()
                });
                for tri in triangles {
                    let dev: Vec<Pt> = tri.iter().map(|v| t.apply(Pt::new(v.p[0], v.p[1]))).collect();
                    gouraud(&mut data, w, h, &dev, tri, lut.as_deref(), (tmin, tmax));
                }
                Some(Prep::Pix { pix: Pixmap::new(w, h, data)?, to_image: Transform::IDENTITY, repeat: false, filter: Filter::Nearest })
            }
        }
    }

    fn text(&self, cv: &mut Canvas, run: &TextRun, top: &Clip) {
        let mode = run.mode;
        if mode == 3 {
            return;
        }
        let fill = matches!(mode, 0 | 2);
        let stroke = matches!(mode, 1 | 2);
        let fclip = if fill { self.fx_clip(top, &run.fill_fx, cv) } else { None };
        let sclip = if stroke { self.fx_clip(top, &run.stroke_fx, cv) } else { None };
        let fprep = if fclip.is_some() { self.prep(&run.fill) } else { None };
        let sprep = if sclip.is_some() { self.prep(&run.stroke) } else { None };
        let scale = run.ctm.mean_scale();
        for g in &run.glyphs {
            let Some(gid) = g.gid else { continue };
            let Some(outline) = run.font.outline(gid) else { continue };
            if let (Some(clip), Some(prep)) = (&fclip, &fprep) {
                let d = g.to_page.then(&self.m);
                cv.fill_path(&outline, &d, FillRule::NonZero, &prep.paint(), run.fill_fx.blend, run.fill_fx.alpha, clip);
            }
            if let (Some(clip), Some(prep)) = (&sclip, &sprep) {
                let page_path = outline.transform(&g.to_page);
                let mut st = stroke_of(&run.stroke_style, 1.0);
                st.width *= scale;
                for d in &mut st.dash {
                    *d *= scale;
                }
                st.dash_phase *= scale;
                cv.stroke_path(&page_path, &self.m, &st, &prep.paint(), run.stroke_fx.blend, run.stroke_fx.alpha, clip);
            }
        }
    }

    fn image(&self, cv: &mut Canvas, img: &ImageData, ctm: &Transform, fx: &Fx, _stencil_paint: Option<&PaintSrc>, clip: &Clip) {
        let d = ctm.then(&self.m);
        let Some(inv) = d.invert() else { return };
        let sx = d.apply_vec(Pt::new(1.0, 0.0)).len();
        let sy = d.apply_vec(Pt::new(0.0, 1.0)).len();
        if !(sx.is_finite() && sy.is_finite()) {
            return;
        }
        let kx = if sx > 0.0 { ((1.0 / sx).floor() as usize).clamp(1, 64) } else { 1 };
        let ky = if sy > 0.0 { ((1.0 / sy).floor() as usize).clamp(1, 64) } else { 1 };
        let (pix, to_image) = if kx > 1 || ky > 1 {
            let (nw, nh) = (img.width.div_ceil(kx), img.height.div_ceil(ky));
            let mut out = vec![0u8; nw * nh * 4];
            for y in 0..nh {
                for x in 0..nw {
                    let mut acc = [0f32; 4];
                    let mut n = 0f32;
                    for yy in y * ky..((y + 1) * ky).min(img.height) {
                        for xx in x * kx..((x + 1) * kx).min(img.width) {
                            let i = (yy * img.width + xx) * 4;
                            if let Some(p) = img.rgba.get(i..i + 4) {
                                let a = f32::from(p[3]) / 255.0;
                                acc[0] += f32::from(p[0]) * a;
                                acc[1] += f32::from(p[1]) * a;
                                acc[2] += f32::from(p[2]) * a;
                                acc[3] += a;
                                n += 1.0;
                            }
                        }
                    }
                    if n > 0.0 && acc[3] > 0.0 {
                        let o = (y * nw + x) * 4;
                        if let Some(px) = out.get_mut(o..o + 4) {
                            px.copy_from_slice(&[(acc[0] / acc[3] + 0.5) as u8, (acc[1] / acc[3] + 0.5) as u8, (acc[2] / acc[3] + 0.5) as u8, (acc[3] / n * 255.0 + 0.5) as u8]);
                        }
                    }
                }
            }
            let Some(p) = Pixmap::new(nw, nh, out) else { return };
            (p, inv.then(&Transform::scale(1.0 / kx as f64, 1.0 / ky as f64)))
        } else {
            let Some(p) = Pixmap::new(img.width, img.height, img.rgba.clone()) else { return };
            (p, inv)
        };
        let filter = if kx > 1 || ky > 1 || img.interpolate || (sx < 4.0 && sy < 4.0) { Filter::Bilinear } else { Filter::Nearest };
        let path = Path::rect(0.0, 0.0, img.width as f64, img.height as f64);
        cv.fill_path(&path, &d, FillRule::NonZero, &Paint::Image { pix: &pix, to_image, filter, repeat: false }, fx.blend, fx.alpha, clip);
    }
}

fn shading_bbox(sh: &Shading) -> Option<[f64; 4]> {
    match sh {
        Shading::Axial { bbox, .. } | Shading::Radial { bbox, .. } => *bbox,
        _ => None,
    }
}

fn func_stops(f: &crate::function::Function, cs: &crate::color::ColorSpace, domain: [f64; 2]) -> Vec<Stop> {
    const N: usize = 128;
    (0..=N)
        .map(|i| {
            let u = i as f64 / N as f64;
            let c = cs.to_rgb(&f.eval(&[domain[0] + (domain[1] - domain[0]) * u]));
            Stop { offset: u as f32, color: [c[0], c[1], c[2], 1.0] }
        })
        .collect()
}

fn gouraud(data: &mut [u8], w: usize, h: usize, dev: &[Pt], tri: &[MeshVertex; 3], lut: Option<&[[f32; 3]]>, trange: (f32, f32)) {
    let (a, b, c) = (dev[0], dev[1], dev[2]);
    let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    if !area.is_finite() || area.abs() < 1e-9 {
        return;
    }
    let x0 = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
    let x1 = (a.x.max(b.x).max(c.x).ceil().max(0.0) as usize).min(w);
    let y0 = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
    let y1 = (a.y.max(b.y).max(c.y).ceil().max(0.0) as usize).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let w0 = ((b.x - px) * (c.y - py) - (b.y - py) * (c.x - px)) / area;
            let w1 = ((c.x - px) * (a.y - py) - (c.y - py) * (a.x - px)) / area;
            let w2 = 1.0 - w0 - w1;
            let eps = -0.02;
            if w0 < eps || w1 < eps || w2 < eps {
                continue;
            }
            let (w0, w1, w2) = (w0 as f32, w1 as f32, w2 as f32);
            let rgb = match lut {
                Some(l) => {
                    let t = w0 * tri[0].c[0] + w1 * tri[1].c[0] + w2 * tri[2].c[0];
                    let span = (trange.1 - trange.0).max(1e-9);
                    l.get((((t - trange.0) / span).clamp(0.0, 1.0) * 255.0 + 0.5) as usize).copied().unwrap_or([0.0; 3])
                }
                None => [0, 1, 2].map(|k| w0 * tri[0].c[k] + w1 * tri[1].c[k] + w2 * tri[2].c[k]),
            };
            let i = (y * w + x) * 4;
            if let Some(px) = data.get_mut(i..i + 4) {
                px.copy_from_slice(&[q8(rgb[0]), q8(rgb[1]), q8(rgb[2]), 255]);
            }
        }
    }
}

#[allow(dead_code)]
fn _unused(_: Blend) {}
