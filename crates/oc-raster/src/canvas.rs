//! The drawing surface: premultiplied float RGBA, with clipping, groups and filters.
use crate::blend::{composite, Blend};
use crate::geom::{Path, Poly, Transform};
use crate::paint::Paint;
use crate::scan::{bounds_of, scan, FillRule, IRect};
use crate::stroke::{stroke, Stroke};

/// A clip region: a rectangle and an optional per-pixel coverage mask.
#[derive(Debug, Clone)]
pub struct Clip {
    pub rect: IRect,
    width: usize,
    mask: Option<Vec<f32>>,
}

impl Clip {
    pub fn full(width: usize, height: usize) -> Clip {
        Clip {
            rect: IRect::new(0, 0, width as i32, height as i32),
            width,
            mask: None,
        }
    }
    fn nothing(width: usize) -> Clip {
        Clip {
            rect: IRect::new(0, 0, 0, 0),
            width,
            mask: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.rect.is_empty()
    }
    fn at(&self, x: i32, y: i32) -> f32 {
        match &self.mask {
            None => 1.0,
            Some(m) => m
                .get(y as usize * self.width + x as usize)
                .copied()
                .unwrap_or(0.0),
        }
    }
    /// Narrow the region to what `polys` cover under `rule`.
    pub fn intersect(&self, polys: &[Poly], rule: FillRule, height: usize) -> Clip {
        let Some(b) = bounds_of(polys) else {
            return Clip::nothing(self.width);
        };
        let rect = self.rect.intersect(&b);
        if rect.is_empty() {
            return Clip::nothing(self.width);
        }
        let mut mask = vec![0.0f32; self.width * height];
        let w = self.width;
        scan(polys, rule, rect, |y, x0, cov| {
            for (i, c) in cov.iter().enumerate() {
                let x = x0 + i as i32;
                let idx = y as usize * w + x as usize;
                if let Some(m) = mask.get_mut(idx) {
                    *m = c * self.at(x, y);
                }
            }
        });
        Clip {
            rect,
            width: self.width,
            mask: Some(mask),
        }
    }
    /// Narrow the region by a per-pixel mask (soft masks).
    pub fn intersect_mask(&self, mask: &[f32]) -> Clip {
        let mut m: Vec<f32> = mask.to_vec();
        if let Some(old) = &self.mask {
            for (a, b) in m.iter_mut().zip(old) {
                *a *= *b;
            }
        }
        Clip {
            rect: self.rect,
            width: self.width,
            mask: Some(m),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// Premultiplied RGBA, row-major.
    pub data: Vec<f32>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Option<Canvas> {
        let n = width.checked_mul(height)?.checked_mul(4)?;
        if width == 0 || height == 0 || n > (1usize << 31) / 4 {
            return None;
        }
        Some(Canvas {
            width,
            height,
            data: vec![0.0; n],
        })
    }
    pub fn clip(&self) -> Clip {
        Clip::full(self.width, self.height)
    }
    pub fn fill_color(&mut self, c: [f32; 4]) {
        let p = [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]];
        for px in self.data.chunks_exact_mut(4) {
            px.copy_from_slice(&p);
        }
    }
    pub fn pixel(&self, x: usize, y: usize) -> [f32; 4] {
        let i = (y * self.width + x) * 4;
        match self.data.get(i..i + 4) {
            Some(s) => [s[0], s[1], s[2], s[3]],
            None => [0.0; 4],
        }
    }

    /// Fill device-space polygons.
    pub fn fill_polys(
        &mut self,
        polys: &[Poly],
        rule: FillRule,
        paint: &Paint,
        blend: Blend,
        alpha: f32,
        clip: &Clip,
    ) {
        let Some(b) = bounds_of(polys) else { return };
        let area = clip
            .rect
            .intersect(&b)
            .intersect(&IRect::new(0, 0, self.width as i32, self.height as i32));
        if area.is_empty() || alpha <= 0.0 {
            return;
        }
        let solid = paint
            .is_opaque_solid()
            .map(|c| [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]);
        let width = self.width;
        let data = &mut self.data;
        scan(polys, rule, area, |y, x0, cov| {
            for (i, c) in cov.iter().enumerate() {
                let x = x0 + i as i32;
                let k = *c * clip.at(x, y) * alpha;
                if k <= 0.0 {
                    continue;
                }
                let s = solid.unwrap_or_else(|| paint.shade(x, y));
                let src = [s[0] * k, s[1] * k, s[2] * k, s[3] * k];
                let idx = (y as usize * width + x as usize) * 4;
                if let Some(px) = data.get_mut(idx..idx + 4) {
                    let d = composite(blend, [px[0], px[1], px[2], px[3]], src);
                    px.copy_from_slice(&d);
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill_path(
        &mut self,
        path: &Path,
        ctm: &Transform,
        rule: FillRule,
        paint: &Paint,
        blend: Blend,
        alpha: f32,
        clip: &Clip,
    ) {
        let polys = device_polys(path, ctm);
        self.fill_polys(&polys, rule, paint, blend, alpha, clip);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_path(
        &mut self,
        path: &Path,
        ctm: &Transform,
        st: &Stroke,
        paint: &Paint,
        blend: Blend,
        alpha: f32,
        clip: &Clip,
    ) {
        let polys = stroke_polys(path, ctm, st);
        self.fill_polys(&polys, FillRule::NonZero, paint, blend, alpha, clip);
    }

    /// Composite another canvas (same size) over this one.
    pub fn draw_canvas(&mut self, src: &Canvas, blend: Blend, alpha: f32, clip: &Clip) {
        if src.width != self.width || src.height != self.height {
            return;
        }
        let r = clip.rect;
        for y in r.y0.max(0)..r.y1.min(self.height as i32) {
            for x in r.x0.max(0)..r.x1.min(self.width as i32) {
                let k = clip.at(x, y) * alpha;
                if k <= 0.0 {
                    continue;
                }
                let i = (y as usize * self.width + x as usize) * 4;
                let (Some(s), Some(d)) = (src.data.get(i..i + 4), self.data.get_mut(i..i + 4))
                else {
                    continue;
                };
                let s = [s[0] * k, s[1] * k, s[2] * k, s[3] * k];
                let o = composite(blend, [d[0], d[1], d[2], d[3]], s);
                d.copy_from_slice(&o);
            }
        }
    }

    /// Multiply every pixel by a coverage mask (soft masks).
    pub fn apply_mask(&mut self, mask: &[f32]) {
        for (px, m) in self.data.chunks_exact_mut(4).zip(mask) {
            for v in px.iter_mut() {
                *v *= *m;
            }
        }
    }

    /// Luminosity of each pixel composited on black, as a mask.
    pub fn luminosity_mask(&self) -> Vec<f32> {
        self.data
            .chunks_exact(4)
            .map(|p| 0.3 * p[0] + 0.59 * p[1] + 0.11 * p[2])
            .collect()
    }
    pub fn alpha_mask(&self) -> Vec<f32> {
        self.data.chunks_exact(4).map(|p| p[3]).collect()
    }

    /// Straight 8-bit RGBA, optionally over an opaque background.
    pub fn to_rgba8(&self, background: Option<[f32; 3]>) -> Vec<u8> {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let mut out = Vec::with_capacity(self.data.len());
        for p in self.data.chunks_exact(4) {
            if let Some(bg) = background {
                let ia = 1.0 - p[3];
                out.extend_from_slice(&[
                    q(p[0] + bg[0] * ia),
                    q(p[1] + bg[1] * ia),
                    q(p[2] + bg[2] * ia),
                    255,
                ]);
            } else if p[3] <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                out.extend_from_slice(&[
                    q(p[0] / p[3]),
                    q(p[1] / p[3]),
                    q(p[2] / p[3]),
                    q(p[3]),
                ]);
            }
        }
        out
    }

    /// Gaussian blur approximated by three box passes per axis, on all channels.
    pub fn blur(&mut self, sx: f64, sy: f64) {
        let (w, h) = (self.width, self.height);
        let mut data = std::mem::take(&mut self.data);
        box_blur(&mut data, w, h, sx, true);
        box_blur(&mut data, w, h, sy, false);
        self.data = data;
    }
}

fn box_blur(data: &mut [f32], w: usize, h: usize, sigma: f64, horizontal: bool) {
    if !(sigma.is_finite() && sigma > 0.0) {
        return;
    }
    let d = ((sigma * 3.0 * (2.0 * std::f64::consts::PI).sqrt() / 4.0 + 0.5).floor() as usize)
        .clamp(1, 1000);
    let (len, lines, stride_i, stride_l) = if horizontal {
        (w, h, 4usize, w * 4)
    } else {
        (h, w, w * 4, 4usize)
    };
    let boxes: [(usize, usize); 3] = if d % 2 == 1 {
        [(d / 2, d / 2); 3]
    } else {
        [(d / 2, d / 2 - 1), (d / 2 - 1, d / 2), (d / 2, d / 2)]
    };
    let mut line_buf = vec![0.0f32; len];
    let mut out_buf = vec![0.0f32; len];
    for line in 0..lines {
        for ch in 0..4 {
            let base = line * stride_l + ch;
            for (i, v) in line_buf.iter_mut().enumerate() {
                *v = data.get(base + i * stride_i).copied().unwrap_or(0.0);
            }
            for (l, r) in boxes {
                let n = (l + r + 1) as f32;
                let mut acc = 0.0f32;
                for v in line_buf.iter().take(r + 1) {
                    acc += *v;
                }
                for i in 0..len {
                    if let Some(o) = out_buf.get_mut(i) {
                        *o = acc / n;
                    }
                    if let Some(add) = line_buf.get(i + r + 1) {
                        acc += *add;
                    }
                    if i >= l {
                        if let Some(sub) = line_buf.get(i - l) {
                            acc -= *sub;
                        }
                    }
                }
                std::mem::swap(&mut line_buf, &mut out_buf);
            }
            for (i, v) in line_buf.iter().enumerate() {
                if let Some(dst) = data.get_mut(base + i * stride_i) {
                    *dst = *v;
                }
            }
        }
    }
}

/// Flatten `path` under `ctm` into device polygons.
pub fn device_polys(path: &Path, ctm: &Transform) -> Vec<Poly> {
    if !ctm.is_finite() {
        return Vec::new();
    }
    path.transform(ctm).flatten(0.1)
}

/// The device polygons of `path` stroked in user space and then mapped by `ctm`.
pub fn stroke_polys(path: &Path, ctm: &Transform, st: &Stroke) -> Vec<Poly> {
    if !ctm.is_finite() {
        return Vec::new();
    }
    let scale = ctm.mean_scale();
    if !(scale.is_finite() && scale > 1e-12) {
        return Vec::new();
    }
    let tol = 0.1 / scale;
    let user = path.flatten(tol);
    let mut st = st.clone();
    // A hairline is one device pixel wide.
    if st.width * scale < 1.0 {
        st.width = 1.0 / scale;
    }
    let mut polys = stroke(&user, &st, tol);
    for p in &mut polys {
        for q in &mut p.pts {
            *q = ctm.apply(*q);
        }
    }
    polys
}
