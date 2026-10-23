//! `filter`: SVG filter primitives and the CSS filter functions.
use crate::clipmask::{linear_to_srgb, srgb_to_linear};
use crate::parse::{self, Length};
use crate::render::{Env, Rect, Renderer};
use crate::style::Style;
use oc_core::Severity;
use oc_raster::{Blend, Canvas, Path, Pt, Transform};
use std::collections::HashMap;

#[derive(Clone)]
struct Img {
    c: Canvas,
    linear: bool,
}

fn convert(img: &mut Img, to_linear: bool) {
    if img.linear == to_linear {
        return;
    }
    for p in img.c.data.chunks_exact_mut(4) {
        let a = p[3];
        if a <= 0.0 {
            continue;
        }
        for v in p.iter_mut().take(3) {
            let s = (*v / a).clamp(0.0, 1.0);
            *v = (if to_linear {
                srgb_to_linear(s)
            } else {
                linear_to_srgb(s)
            }) * a;
        }
    }
    img.linear = to_linear;
}

fn apply_matrix(c: &mut Canvas, m: &[f32; 20]) {
    for p in c.data.chunks_exact_mut(4) {
        let a = p[3];
        let (r, g, b) = if a > 0.0 {
            (p[0] / a, p[1] / a, p[2] / a)
        } else {
            (0.0, 0.0, 0.0)
        };
        let o = |k: usize| {
            (m[k] * r + m[k + 1] * g + m[k + 2] * b + m[k + 3] * a + m[k + 4]).clamp(0.0, 1.0)
        };
        let (nr, ng, nb, na) = (o(0), o(5), o(10), o(15));
        p[0] = nr * na;
        p[1] = ng * na;
        p[2] = nb * na;
        p[3] = na;
    }
}

fn saturate_matrix(s: f32) -> [f32; 20] {
    [
        0.213 + 0.787 * s,
        0.715 - 0.715 * s,
        0.072 - 0.072 * s,
        0.0,
        0.0,
        0.213 - 0.213 * s,
        0.715 + 0.285 * s,
        0.072 - 0.072 * s,
        0.0,
        0.0,
        0.213 - 0.213 * s,
        0.715 - 0.715 * s,
        0.072 + 0.928 * s,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ]
}

fn hue_matrix(deg: f32) -> [f32; 20] {
    let (s, c) = deg.to_radians().sin_cos();
    [
        0.213 + c * 0.787 - s * 0.213,
        0.715 - c * 0.715 - s * 0.715,
        0.072 - c * 0.072 + s * 0.928,
        0.0,
        0.0,
        0.213 - c * 0.213 + s * 0.143,
        0.715 + c * 0.285 + s * 0.140,
        0.072 - c * 0.072 - s * 0.283,
        0.0,
        0.0,
        0.213 - c * 0.213 - s * 0.787,
        0.715 - c * 0.715 + s * 0.715,
        0.072 + c * 0.928 + s * 0.072,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ]
}

/// Shift by a fractional offset with bilinear sampling.
fn shift(src: &Canvas, dx: f64, dy: f64) -> Canvas {
    let mut out = src.clone();
    let (w, h) = (src.width as i64, src.height as i64);
    let (fx, fy) = (dx.floor(), dy.floor());
    let (tx, ty) = ((dx - fx) as f32, (dy - fy) as f32);
    let (ix, iy) = (fx as i64, fy as i64);
    let at = |x: i64, y: i64, k: usize| -> f32 {
        if x < 0 || y < 0 || x >= w || y >= h {
            0.0
        } else {
            src.data
                .get((y as usize * src.width + x as usize) * 4 + k)
                .copied()
                .unwrap_or(0.0)
        }
    };
    for y in 0..h {
        for x in 0..w {
            for k in 0..4 {
                // the output pixel (x, y) takes from (x - dx, y - dy)
                let (sx, sy) = (x - ix, y - iy);
                let a = at(sx, sy, k);
                let b = at(sx - 1, sy, k);
                let c = at(sx, sy - 1, k);
                let d = at(sx - 1, sy - 1, k);
                let v = (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (c * (1.0 - tx) + d * tx) * ty;
                if let Some(o) = out
                    .data
                    .get_mut((y as usize * src.width + x as usize) * 4 + k)
                {
                    *o = v;
                }
            }
        }
    }
    out
}

fn morphology(src: &Canvas, rx: usize, ry: usize, dilate: bool) -> Canvas {
    let mut cur = src.clone();
    for horizontal in [true, false] {
        let r = if horizontal { rx } else { ry };
        if r == 0 {
            continue;
        }
        let (w, h) = (cur.width, cur.height);
        let mut out = cur.clone();
        for y in 0..h {
            for x in 0..w {
                for k in 0..4 {
                    let mut acc = if dilate { 0.0f32 } else { 1.0f32 };
                    let lo = if horizontal {
                        x.saturating_sub(r)
                    } else {
                        y.saturating_sub(r)
                    };
                    let hi = if horizontal {
                        (x + r).min(w - 1)
                    } else {
                        (y + r).min(h - 1)
                    };
                    let span_ok = (if horizontal { x } else { y }) >= r && hi - lo == 2 * r;
                    for t in lo..=hi {
                        let (px, py) = if horizontal { (t, y) } else { (x, t) };
                        let v = cur.data.get((py * w + px) * 4 + k).copied().unwrap_or(0.0);
                        acc = if dilate { acc.max(v) } else { acc.min(v) };
                    }
                    if !dilate && !span_ok {
                        acc = 0.0;
                    }
                    if let Some(o) = out.data.get_mut((y * w + x) * 4 + k) {
                        *o = acc;
                    }
                }
            }
        }
        cur = out;
    }
    cur
}

fn lerp_table(v: &[f32], t: f32) -> f32 {
    let n = v.len();
    if n == 0 {
        return t;
    }
    if n == 1 {
        return v[0];
    }
    let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
    let k = (pos.floor() as usize).min(n - 2);
    v[k] + (v[k + 1] - v[k]) * (pos - k as f32)
}

struct Region {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

fn device_region(r: Rect, ctm: &Transform, w: usize, h: usize) -> Region {
    let d = r.transform(ctm);
    Region {
        x0: (d.x.floor().max(0.0)) as i32,
        y0: (d.y.floor().max(0.0)) as i32,
        x1: ((d.x + d.w).ceil().min(w as f64)) as i32,
        y1: ((d.y + d.h).ceil().min(h as f64)) as i32,
    }
}

fn clear_outside(c: &mut Canvas, r: &Region) {
    let w = c.width;
    for (idx, p) in c.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((idx % w) as i32, (idx / w) as i32);
        if x < r.x0 || x >= r.x1 || y < r.y0 || y >= r.y1 {
            p.fill(0.0);
        }
    }
}

fn args_of(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

fn amount(s: &str, default: f32) -> f32 {
    let s = s.trim();
    s.strip_suffix('%')
        .and_then(|p| p.parse::<f32>().ok().map(|v| v / 100.0))
        .or_else(|| s.parse::<f32>().ok())
        .unwrap_or(default)
}

/// Split `a(b) c(d)` into `[("a","b"), ("c","d")]`.
fn filter_list(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().to_ascii_lowercase();
        let mut depth = 0;
        let mut close = None;
        for (k, c) in rest.char_indices().skip(open) {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { break };
        out.push((name, rest[open + 1..close].trim().to_string()));
        rest = rest[close + 1..].trim();
    }
    out
}

impl Renderer<'_> {
    pub fn apply_filter(
        &mut self,
        i: usize,
        f: &str,
        style: &Style,
        env: &Env,
        bbox_path: Option<&Path>,
        layer: &mut Canvas,
    ) {
        let bbox = bbox_path
            .and_then(|p| p.bounds())
            .map(|(lo, hi)| Rect {
                x: lo.x,
                y: lo.y,
                w: hi.x - lo.x,
                h: hi.y - lo.y,
            })
            .or_else(|| self.bbox_of(i, env));
        for (name, args) in filter_list(f) {
            if name == "url" {
                let id = args
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .trim_start_matches('#')
                    .to_string();
                self.svg_filter(&id, style, env, bbox, layer);
            } else {
                self.css_filter(&name, &args, style, env, layer);
            }
        }
    }

    fn css_filter(&mut self, name: &str, args: &str, style: &Style, env: &Env, layer: &mut Canvas) {
        let a = args_of(args);
        let first = a.first().map_or("", String::as_str);
        let sc = env.ctm.mean_scale();
        match name {
            "blur" => {
                let r = Length::parse(first).map_or(0.0, |l| l.resolve(0.0, style.font_size));
                layer.blur(r * sc, r * sc);
            }
            "brightness" | "contrast" | "grayscale" | "sepia" | "saturate" | "hue-rotate"
            | "invert" | "opacity" => {
                let v = amount(first, 1.0);
                let mut m = [0.0f32; 20];
                m[18] = 1.0;
                m[0] = 1.0;
                m[6] = 1.0;
                m[12] = 1.0;
                match name {
                    "brightness" => {
                        m[0] = v;
                        m[6] = v;
                        m[12] = v;
                    }
                    "contrast" => {
                        let off = 0.5 - 0.5 * v;
                        m[0] = v;
                        m[6] = v;
                        m[12] = v;
                        m[4] = off;
                        m[9] = off;
                        m[14] = off;
                    }
                    "grayscale" => {
                        let s = 1.0 - v.clamp(0.0, 1.0);
                        m = [
                            0.2126 + 0.7874 * s,
                            0.7152 - 0.7152 * s,
                            0.0722 - 0.0722 * s,
                            0.0,
                            0.0,
                            0.2126 - 0.2126 * s,
                            0.7152 + 0.2848 * s,
                            0.0722 - 0.0722 * s,
                            0.0,
                            0.0,
                            0.2126 - 0.2126 * s,
                            0.7152 - 0.7152 * s,
                            0.0722 + 0.9278 * s,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            1.0,
                            0.0,
                        ];
                    }
                    "sepia" => {
                        let s = 1.0 - v.clamp(0.0, 1.0);
                        m = [
                            0.393 + 0.607 * s,
                            0.769 - 0.769 * s,
                            0.189 - 0.189 * s,
                            0.0,
                            0.0,
                            0.349 - 0.349 * s,
                            0.686 + 0.314 * s,
                            0.168 - 0.168 * s,
                            0.0,
                            0.0,
                            0.272 - 0.272 * s,
                            0.534 - 0.534 * s,
                            0.131 + 0.869 * s,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            1.0,
                            0.0,
                        ];
                    }
                    "saturate" => m = saturate_matrix(v),
                    "hue-rotate" => {
                        let deg = if let Some(t) = first.strip_suffix("turn") {
                            t.parse::<f32>().unwrap_or(0.0) * 360.0
                        } else if let Some(t) = first.strip_suffix("rad") {
                            t.parse::<f32>().unwrap_or(0.0).to_degrees()
                        } else {
                            first.trim_end_matches("deg").parse::<f32>().unwrap_or(0.0)
                        };
                        m = hue_matrix(deg);
                    }
                    "invert" => {
                        let v = v.clamp(0.0, 1.0);
                        m[0] = 1.0 - 2.0 * v;
                        m[6] = 1.0 - 2.0 * v;
                        m[12] = 1.0 - 2.0 * v;
                        m[4] = v;
                        m[9] = v;
                        m[14] = v;
                    }
                    _ => m[18] = v.clamp(0.0, 1.0),
                }
                apply_matrix(layer, &m);
            }
            "drop-shadow" => {
                let mut nums = Vec::new();
                let mut colour = style.color;
                for w in &a {
                    if let Some(l) = Length::parse(w) {
                        nums.push(l.resolve(0.0, style.font_size));
                    } else if let Some(c) = crate::color::parse(w) {
                        colour = c;
                    }
                }
                let dx = nums.first().copied().unwrap_or(0.0);
                let dy = nums.get(1).copied().unwrap_or(0.0);
                let blur = nums.get(2).copied().unwrap_or(0.0) / 2.0;
                let mut sh = Img {
                    c: layer.clone(),
                    linear: false,
                };
                shadow(&mut sh.c, colour, 1.0);
                sh.c.blur(blur * sc, blur * sc);
                let d = env.ctm.apply_vec(Pt::new(dx, dy));
                let moved = shift(&sh.c, d.x, d.y);
                let src = layer.clone();
                *layer = moved;
                let full = layer.clip();
                layer.draw_canvas(&src, Blend::Normal, 1.0, &full);
            }
            _ => self.note_once(
                "svg.css_filter",
                Severity::Warning,
                "an unsupported CSS filter function was ignored",
            ),
        }
    }

    fn svg_filter(
        &mut self,
        id: &str,
        style: &Style,
        env: &Env,
        bbox: Option<Rect>,
        layer: &mut Canvas,
    ) {
        let Some(fe) = self
            .doc
            .by_id(id)
            .filter(|k| self.doc.node(*k).is_some_and(|n| n.tag == "filter"))
        else {
            return;
        };
        if self.active.contains(&fe) {
            return;
        }
        let units_bbox = self
            .doc
            .attr(fe, "filterUnits")
            .is_none_or(|v| v.trim() != "userSpaceOnUse");
        let prim_bbox = self
            .doc
            .attr(fe, "primitiveUnits")
            .is_some_and(|v| v.trim() == "objectBoundingBox");
        let b = bbox.filter(|b| b.w > 0.0 && b.h > 0.0);
        if (units_bbox || prim_bbox) && b.is_none() {
            layer.data.fill(0.0);
            return;
        }
        let (bx, by, bw, bh) = b.map_or((0.0, 0.0, 1.0, 1.0), |b| (b.x, b.y, b.w, b.h));
        let fs = style.font_size;
        let lc = env.lc;
        let region_of =
            |r: &Self, name: &str, refv: f64, default: f64, off: f64, size: f64| -> f64 {
                let l = r.doc.attr(fe, name).and_then(Length::parse);
                if units_bbox {
                    off + l.map_or(default, |l| {
                        if l.unit == parse::Unit::Percent {
                            l.v / 100.0
                        } else {
                            l.v
                        }
                    }) * size
                } else {
                    l.map_or(default * refv, |l| l.resolve(refv, fs))
                }
            };
        let region = Rect {
            x: region_of(self, "x", lc.vw, -0.1, bx, bw),
            y: region_of(self, "y", lc.vh, -0.1, by, bh),
            w: region_of(self, "width", lc.vw, 1.2, 0.0, bw),
            h: region_of(self, "height", lc.vh, 1.2, 0.0, bh),
        };
        if !(region.w > 0.0 && region.h > 0.0) {
            layer.data.fill(0.0);
            return;
        }
        let (w, h) = (layer.width, layer.height);
        let reg = device_region(region, &env.ctm, w, h);
        let (sx, sy) = (
            (env.ctm.a.powi(2) + env.ctm.b.powi(2)).sqrt(),
            (env.ctm.c.powi(2) + env.ctm.d.powi(2)).sqrt(),
        );
        let (kx, ky) = if prim_bbox { (bw, bh) } else { (1.0, 1.0) };

        let mut source = Img {
            c: layer.clone(),
            linear: false,
        };
        clear_outside(&mut source.c, &reg);
        let mut results: HashMap<String, Img> = HashMap::new();
        let mut last: Option<Img> = None;
        let mut kids = self
            .doc
            .node(fe)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        if !kids
            .iter()
            .any(|k| self.doc.node(*k).is_some_and(|n| n.tag.starts_with("fe")))
        {
            if let Some(t) = self.doc.href_target(fe) {
                kids = self
                    .doc
                    .node(t)
                    .map(|n| n.children.clone())
                    .unwrap_or_default();
            }
        }
        let fstyle_linear = self
            .doc
            .node(fe)
            .and_then(|n| n.style.get("color-interpolation-filters"))
            .map(|v| v.trim() != "sRGB");
        self.active.push(fe);
        for k in kids {
            let Some(n) = self.doc.node(k).cloned() else {
                continue;
            };
            if !n.tag.starts_with("fe") {
                continue;
            }
            let lin = n
                .style
                .get("color-interpolation-filters")
                .map(|v| v.trim() != "sRGB")
                .or(fstyle_linear)
                .unwrap_or(true);
            let attr = |name: &str| n.attrs.get(name).map(String::as_str);
            let get_in = |name: &str,
                          last: &Option<Img>,
                          results: &HashMap<String, Img>,
                          want_lin: bool|
             -> Img {
                let mut img = match attr(name).map(str::trim) {
                    Some("SourceGraphic") => source.clone(),
                    Some("SourceAlpha") => {
                        let mut s = source.clone();
                        for p in s.c.data.chunks_exact_mut(4) {
                            p[0] = 0.0;
                            p[1] = 0.0;
                            p[2] = 0.0;
                        }
                        s
                    }
                    Some("BackgroundImage" | "BackgroundAlpha" | "FillPaint" | "StrokePaint") => {
                        Img {
                            c: Canvas::new(w, h).unwrap_or_else(|| source.c.clone()),
                            linear: false,
                        }
                    }
                    Some(r) => results
                        .get(r)
                        .cloned()
                        .or_else(|| last.clone())
                        .unwrap_or_else(|| source.clone()),
                    None => last.clone().unwrap_or_else(|| source.clone()),
                };
                convert(&mut img, want_lin);
                img
            };
            let nums =
                |name: &str| -> Vec<f64> { attr(name).map(parse::numbers).unwrap_or_default() };
            let out: Option<Img> = match n.tag.as_str() {
                "feGaussianBlur" | "feDropShadow" | "feOffset" => {
                    let mut input = get_in("in", &last, &results, lin);
                    let sd = nums("stdDeviation");
                    let (sdx, sdy) = match sd.as_slice() {
                        [a] => (*a, *a),
                        [a, b, ..] => (*a, *b),
                        _ => (
                            if n.tag == "feDropShadow" { 2.0 } else { 0.0 },
                            if n.tag == "feDropShadow" { 2.0 } else { 0.0 },
                        ),
                    };
                    let (sdx, sdy) = (sdx * kx * sx, sdy * ky * sy);
                    match n.tag.as_str() {
                        "feGaussianBlur" => {
                            if sd.iter().any(|v| *v < 0.0) {
                                input.c.data.fill(0.0);
                            } else {
                                input.c.blur(sdx, sdy);
                            }
                            Some(input)
                        }
                        "feOffset" => {
                            let o = env.ctm.apply_vec(Pt::new(
                                nums("dx").first().copied().unwrap_or(0.0) * kx,
                                nums("dy").first().copied().unwrap_or(0.0) * ky,
                            ));
                            input.c = shift(&input.c, o.x, o.y);
                            Some(input)
                        }
                        _ => {
                            let dx = attr("dx")
                                .and_then(|v| v.trim().parse::<f64>().ok())
                                .unwrap_or(2.0);
                            let dy = attr("dy")
                                .and_then(|v| v.trim().parse::<f64>().ok())
                                .unwrap_or(2.0);
                            let (col, op) = self.flood_of(&n, style);
                            let mut sh = input.clone();
                            let mut flood_c = col;
                            flood_c[3] *= op;
                            let mut c = sh.c.clone();
                            shadow(&mut c, flood_c, 1.0);
                            c.blur(sdx, sdy);
                            // flood colour lives in sRGB; bring it into the working space
                            sh.c = c;
                            sh.linear = false;
                            convert(&mut sh, lin);
                            let o = env.ctm.apply_vec(Pt::new(dx * kx, dy * ky));
                            sh.c = shift(&sh.c, o.x, o.y);
                            let full = sh.c.clip();
                            sh.c.draw_canvas(&input.c, Blend::Normal, 1.0, &full);
                            Some(sh)
                        }
                    }
                }
                "feFlood" => {
                    let (col, op) = self.flood_of(&n, style);
                    let Some(mut c) = Canvas::new(w, h) else {
                        continue;
                    };
                    c.fill_color([col[0], col[1], col[2], col[3] * op]);
                    Some(Img { c, linear: false })
                }
                "feMerge" => {
                    let Some(mut c) = Canvas::new(w, h) else {
                        continue;
                    };
                    let full = c.clip();
                    for mk in &n.children {
                        let Some(mn) = self.doc.node(*mk) else {
                            continue;
                        };
                        if mn.tag != "feMergeNode" {
                            continue;
                        }
                        let nm = mn.attrs.get("in").cloned();
                        let tmp_attr = nm.unwrap_or_default();
                        let img = match tmp_attr.as_str() {
                            "SourceGraphic" => source.clone(),
                            "" => last.clone().unwrap_or_else(|| source.clone()),
                            r => results
                                .get(r)
                                .cloned()
                                .unwrap_or_else(|| last.clone().unwrap_or_else(|| source.clone())),
                        };
                        let mut img = img;
                        convert(&mut img, lin);
                        c.draw_canvas(&img.c, Blend::Normal, 1.0, &full);
                    }
                    Some(Img { c, linear: lin })
                }
                "feBlend" => {
                    let a = get_in("in", &last, &results, lin);
                    let mut bimg = get_in("in2", &last, &results, lin);
                    let mode = Blend::from_name(attr("mode").unwrap_or("normal").trim())
                        .unwrap_or(Blend::Normal);
                    let full = bimg.c.clip();
                    bimg.c.draw_canvas(&a.c, mode, 1.0, &full);
                    Some(bimg)
                }
                "feComposite" => {
                    let a = get_in("in", &last, &results, lin);
                    let b2 = get_in("in2", &last, &results, lin);
                    let op = attr("operator").unwrap_or("over").trim().to_string();
                    let k = [
                        nums("k1").first().copied().unwrap_or(0.0) as f32,
                        nums("k2").first().copied().unwrap_or(0.0) as f32,
                        nums("k3").first().copied().unwrap_or(0.0) as f32,
                        nums("k4").first().copied().unwrap_or(0.0) as f32,
                    ];
                    let mut c = a.c.clone();
                    for (idx, p) in c.data.chunks_exact_mut(4).enumerate() {
                        let q = &b2.c.data[idx * 4..idx * 4 + 4];
                        let (sa, da) = (p[3], q[3]);
                        for ch in 0..4 {
                            let (s, d) = (p[ch], q[ch]);
                            p[ch] = match op.as_str() {
                                "in" => s * da,
                                "out" => s * (1.0 - da),
                                "atop" => s * da + d * (1.0 - sa),
                                "xor" => s * (1.0 - da) + d * (1.0 - sa),
                                "arithmetic" => {
                                    (k[0] * s * d + k[1] * s + k[2] * d + k[3]).clamp(0.0, 1.0)
                                }
                                _ => s + d * (1.0 - sa),
                            };
                        }
                        if op == "arithmetic" {
                            let al = p[3];
                            for ch in p.iter_mut().take(3) {
                                *ch = ch.min(al);
                            }
                        }
                    }
                    Some(Img { c, linear: lin })
                }
                "feColorMatrix" => {
                    let mut img = get_in("in", &last, &results, lin);
                    let vals = nums("values");
                    let ty = attr("type").unwrap_or("matrix").trim();
                    let ident: [f32; 20] = [
                        1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
                        0.0, 0.0, 0.0, 1.0, 0.0,
                    ];
                    let m: [f32; 20] = match ty {
                        "saturate" => saturate_matrix(vals.first().map_or(1.0, |v| *v as f32)),
                        "hueRotate" => hue_matrix(vals.first().map_or(0.0, |v| *v as f32)),
                        "luminanceToAlpha" => {
                            let mut m = [0.0f32; 20];
                            m[15] = 0.2125;
                            m[16] = 0.7154;
                            m[17] = 0.0721;
                            m
                        }
                        _ if vals.len() == 20 => {
                            let mut m = [0.0f32; 20];
                            for (d, s) in m.iter_mut().zip(&vals) {
                                *d = *s as f32;
                            }
                            m
                        }
                        _ => ident,
                    };
                    apply_matrix(&mut img.c, &m);
                    Some(img)
                }
                "feComponentTransfer" => {
                    let mut img = get_in("in", &last, &results, lin);
                    let mut funcs: [Option<crate::filter::Func>; 4] = [None, None, None, None];
                    for fk in &n.children {
                        let Some(fnode) = self.doc.node(*fk) else {
                            continue;
                        };
                        let slot = match fnode.tag.as_str() {
                            "feFuncR" => 0,
                            "feFuncG" => 1,
                            "feFuncB" => 2,
                            "feFuncA" => 3,
                            _ => continue,
                        };
                        funcs[slot] = Func::parse(&fnode.attrs);
                    }
                    for p in img.c.data.chunks_exact_mut(4) {
                        let a = p[3];
                        let mut v = if a > 0.0 {
                            [p[0] / a, p[1] / a, p[2] / a, a]
                        } else {
                            [0.0, 0.0, 0.0, 0.0]
                        };
                        for (ch, f) in funcs.iter().enumerate() {
                            if let Some(f) = f {
                                v[ch] = f.apply(v[ch]).clamp(0.0, 1.0);
                            }
                        }
                        p[0] = v[0] * v[3];
                        p[1] = v[1] * v[3];
                        p[2] = v[2] * v[3];
                        p[3] = v[3];
                    }
                    Some(img)
                }
                "feMorphology" => {
                    let img = get_in("in", &last, &results, lin);
                    let r = nums("radius");
                    let (rx, ry) = match r.as_slice() {
                        [a] => (*a, *a),
                        [a, b, ..] => (*a, *b),
                        _ => (0.0, 0.0),
                    };
                    let (rx, ry) = (
                        (rx * kx * sx).round().clamp(0.0, 64.0) as usize,
                        (ry * ky * sy).round().clamp(0.0, 64.0) as usize,
                    );
                    let dilate = attr("operator").is_some_and(|o| o.trim() == "dilate");
                    Some(Img {
                        c: morphology(&img.c, rx, ry, dilate),
                        linear: img.linear,
                    })
                }
                other => {
                    self.note_once("svg.filter_primitive", Severity::Warning, &format!("the filter primitive <{other}> is not supported; its input was passed through"));
                    Some(get_in("in", &last, &results, lin))
                }
            };
            let Some(mut out) = out else { continue };
            // primitive subregion
            if ["x", "y", "width", "height"]
                .iter()
                .any(|a| n.attrs.contains_key(*a))
            {
                let get = |name: &str, refv: f64, default: f64, off: f64, size: f64| -> f64 {
                    match attr(name).and_then(Length::parse) {
                        Some(l) if prim_bbox => {
                            off + (if l.unit == parse::Unit::Percent {
                                l.v / 100.0
                            } else {
                                l.v
                            }) * size
                        }
                        Some(l) => l.resolve(refv, fs),
                        None => default,
                    }
                };
                let r = Rect {
                    x: get("x", lc.vw, region.x, bx, bw),
                    y: get("y", lc.vh, region.y, by, bh),
                    w: get("width", lc.vw, region.w, 0.0, bw),
                    h: get("height", lc.vh, region.h, 0.0, bh),
                };
                let dr = device_region(r, &env.ctm, w, h);
                clear_outside(&mut out.c, &dr);
            }
            clear_outside(&mut out.c, &reg);
            if let Some(name) = attr("result") {
                results.insert(name.trim().to_string(), out.clone());
            }
            last = Some(out);
        }
        self.active.pop();
        match last {
            Some(mut img) => {
                convert(&mut img, false);
                *layer = img.c;
            }
            None => layer.data.fill(0.0),
        }
    }

    fn flood_of(&self, n: &crate::dom::Node, style: &Style) -> ([f32; 4], f32) {
        let col = match n.style.get("flood-color").map(|s| s.trim()) {
            Some(v) if v.eq_ignore_ascii_case("currentcolor") => style.color,
            Some(v) => crate::color::parse(v).unwrap_or([0.0, 0.0, 0.0, 1.0]),
            None => [0.0, 0.0, 0.0, 1.0],
        };
        let op = n
            .style
            .get("flood-opacity")
            .and_then(|v| crate::style::parse_opacity_pub(v))
            .unwrap_or(1.0);
        (col, op)
    }
}

/// Replace every pixel's colour with `colour`, keeping its alpha (for shadows).
fn shadow(c: &mut Canvas, colour: [f32; 4], _k: f32) {
    for p in c.data.chunks_exact_mut(4) {
        let a = p[3] * colour[3];
        p[0] = colour[0] * a;
        p[1] = colour[1] * a;
        p[2] = colour[2] * a;
        p[3] = a;
    }
}

pub enum Func {
    Table(Vec<f32>),
    Discrete(Vec<f32>),
    Linear(f32, f32),
    Gamma(f32, f32, f32),
}

impl Func {
    fn parse(a: &HashMap<String, String>) -> Option<Func> {
        let num = |n: &str, d: f32| {
            a.get(n)
                .and_then(|v| v.trim().parse::<f32>().ok())
                .unwrap_or(d)
        };
        let table = || {
            a.get("tableValues")
                .map(|v| {
                    parse::numbers(v)
                        .iter()
                        .map(|x| *x as f32)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        match a.get("type")?.trim() {
            "table" => Some(Func::Table(table())),
            "discrete" => Some(Func::Discrete(table())),
            "linear" => Some(Func::Linear(num("slope", 1.0), num("intercept", 0.0))),
            "gamma" => Some(Func::Gamma(
                num("amplitude", 1.0),
                num("exponent", 1.0),
                num("offset", 0.0),
            )),
            _ => None,
        }
    }
    fn apply(&self, c: f32) -> f32 {
        match self {
            Func::Table(v) => lerp_table(v, c),
            Func::Discrete(v) => {
                if v.is_empty() {
                    c
                } else {
                    v[((c * v.len() as f32).floor() as usize).min(v.len() - 1)]
                }
            }
            Func::Linear(s, i) => s * c + i,
            Func::Gamma(a, e, o) => a * c.powf(*e) + o,
        }
    }
}
