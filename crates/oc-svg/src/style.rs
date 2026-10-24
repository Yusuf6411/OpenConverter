//! Computed style: the inherited properties we use, derived element by element.
use crate::color::{self, Rgba};
use crate::dom::Node;
use crate::parse::{length_list, Length};
use oc_raster::{Cap, Join};

#[derive(Debug, Clone, PartialEq)]
pub enum PaintSpec {
    None,
    Color(Rgba),
    CurrentColor,
    Url(String, Option<Box<PaintSpec>>),
}

#[derive(Debug, Clone)]
pub struct Style {
    pub fill: PaintSpec,
    pub stroke: PaintSpec,
    pub fill_opacity: f32,
    pub stroke_opacity: f32,
    pub fill_evenodd: bool,
    pub clip_evenodd: bool,
    pub stroke_width: f64,
    pub cap: Cap,
    pub join: Join,
    pub miter_limit: f64,
    pub dash: Vec<f64>,
    pub dash_offset: f64,
    pub color: Rgba,
    pub font_size: f64,
    pub font_family: Vec<String>,
    pub font_weight: u16,
    pub italic: bool,
    pub text_anchor: u8,
    pub letter_spacing: f64,
    pub word_spacing: f64,
    pub decoration: u8,
    pub visible: bool,
    pub marker_start: Option<String>,
    pub marker_mid: Option<String>,
    pub marker_end: Option<String>,
    pub pixelated: bool,
    pub paint_order_stroke_first: bool,
    pub linear_filters: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            fill: PaintSpec::Color([0.0, 0.0, 0.0, 1.0]),
            stroke: PaintSpec::None,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            fill_evenodd: false,
            clip_evenodd: false,
            stroke_width: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
            miter_limit: 4.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            color: [0.0, 0.0, 0.0, 1.0],
            font_size: 16.0,
            font_family: vec!["sans-serif".to_string()],
            font_weight: 400,
            italic: false,
            text_anchor: 0,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            decoration: 0,
            visible: true,
            marker_start: None,
            marker_mid: None,
            marker_end: None,
            pixelated: false,
            paint_order_stroke_first: false,
            linear_filters: true,
        }
    }
}

/// The viewport that percentages refer to.
#[derive(Debug, Clone, Copy)]
pub struct LenCtx {
    pub vw: f64,
    pub vh: f64,
}

impl LenCtx {
    pub fn diag(&self) -> f64 {
        ((self.vw * self.vw + self.vh * self.vh) / 2.0).sqrt()
    }
}

pub fn parse_paint(v: &str) -> Option<PaintSpec> {
    let v = v.trim();
    let lower = v.to_ascii_lowercase();
    if lower == "none" {
        return Some(PaintSpec::None);
    }
    if lower == "currentcolor" {
        return Some(PaintSpec::CurrentColor);
    }
    if lower.starts_with("url(") {
        let close = v.find(')')?;
        let id = v
            .get(4..close)?
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .strip_prefix('#')?
            .to_string();
        let fallback = v
            .get(close + 1..)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(parse_paint)
            .map(Box::new);
        return Some(PaintSpec::Url(id, fallback));
    }
    color::parse(v).map(PaintSpec::Color)
}

fn parse_opacity(v: &str) -> Option<f32> {
    let v = v.trim();
    let x = if let Some(p) = v.strip_suffix('%') {
        p.trim().parse::<f32>().ok()? / 100.0
    } else {
        v.parse::<f32>().ok()?
    };
    Some(x.clamp(0.0, 1.0))
}

pub fn parse_opacity_pub(v: &str) -> Option<f32> {
    parse_opacity(v)
}

fn parse_url_ref(v: &str) -> Option<String> {
    let v = v.trim();
    if !v.to_ascii_lowercase().starts_with("url(") {
        return None;
    }
    let close = v.find(')')?;
    v.get(4..close)?
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .strip_prefix('#')
        .map(str::to_string)
}

pub fn url_ref(v: &str) -> Option<String> {
    parse_url_ref(v)
}

fn font_families(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in v.chars() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            (',', None) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Style {
    /// The style of `node` given its parent's computed style.
    pub fn derive(&self, node: &Node, lc: &LenCtx) -> Style {
        let mut s = self.clone();
        // font-size first, since em units elsewhere depend on it
        if let Some(v) = node.style.get("font-size") {
            let v = v.trim().to_ascii_lowercase();
            s.font_size = match v.as_str() {
                "xx-small" => 9.0,
                "x-small" => 10.0,
                "small" => 13.0,
                "medium" => 16.0,
                "large" => 18.0,
                "x-large" => 24.0,
                "xx-large" => 32.0,
                "larger" => self.font_size * 1.2,
                "smaller" => self.font_size / 1.2,
                "inherit" => self.font_size,
                _ => Length::parse(&v)
                    .map(|l| l.resolve(self.font_size, self.font_size))
                    .filter(|x| *x >= 0.0)
                    .unwrap_or(self.font_size),
            };
        }
        let fs = s.font_size;
        let len = |v: &str, reference: f64| Length::parse(v).map(|l| l.resolve(reference, fs));
        for (k, v) in &node.style {
            let v = v.trim();
            if v == "inherit" {
                continue;
            }
            match k.as_str() {
                "color" => {
                    if let Some(c) = color::parse(v) {
                        s.color = c;
                    }
                }
                "fill" => {
                    if let Some(p) = parse_paint(v) {
                        s.fill = p;
                    }
                }
                "stroke" => {
                    if let Some(p) = parse_paint(v) {
                        s.stroke = p;
                    }
                }
                "fill-opacity" => {
                    if let Some(o) = parse_opacity(v) {
                        s.fill_opacity = o;
                    }
                }
                "stroke-opacity" => {
                    if let Some(o) = parse_opacity(v) {
                        s.stroke_opacity = o;
                    }
                }
                "fill-rule" => s.fill_evenodd = v == "evenodd",
                "clip-rule" => s.clip_evenodd = v == "evenodd",
                "stroke-width" => {
                    if let Some(w) = len(v, lc.diag()).filter(|w| *w >= 0.0) {
                        s.stroke_width = w;
                    }
                }
                "stroke-linecap" => {
                    s.cap = match v {
                        "round" => Cap::Round,
                        "square" => Cap::Square,
                        _ => Cap::Butt,
                    }
                }
                "stroke-linejoin" => {
                    s.join = match v {
                        "round" => Join::Round,
                        "bevel" => Join::Bevel,
                        _ => Join::Miter,
                    }
                }
                "stroke-miterlimit" => {
                    if let Ok(m) = v.parse::<f64>() {
                        if m >= 1.0 {
                            s.miter_limit = m;
                        }
                    }
                }
                "stroke-dasharray" => {
                    if v == "none" {
                        s.dash.clear();
                    } else {
                        let d: Vec<f64> = length_list(v)
                            .iter()
                            .map(|l| l.resolve(lc.diag(), fs))
                            .collect();
                        if d.iter().all(|x| *x >= 0.0) && d.iter().sum::<f64>() > 0.0 {
                            s.dash = d;
                        } else {
                            s.dash.clear();
                        }
                    }
                }
                "stroke-dashoffset" => {
                    if let Some(o) = len(v, lc.diag()) {
                        s.dash_offset = o;
                    }
                }
                "font-family" => {
                    let f = font_families(v);
                    if !f.is_empty() {
                        s.font_family = f;
                    }
                }
                "font-weight" => {
                    s.font_weight = match v {
                        "normal" => 400,
                        "bold" => 700,
                        "bolder" => (self.font_weight + 300).min(900),
                        "lighter" => self.font_weight.saturating_sub(300).max(100),
                        n => n
                            .parse::<u16>()
                            .map_or(self.font_weight, |w| w.clamp(1, 1000)),
                    }
                }
                "font-style" => s.italic = v == "italic" || v == "oblique",
                "text-anchor" => {
                    s.text_anchor = match v {
                        "middle" => 1,
                        "end" => 2,
                        _ => 0,
                    }
                }
                "letter-spacing" => {
                    s.letter_spacing = if v == "normal" {
                        0.0
                    } else {
                        len(v, lc.diag()).unwrap_or(0.0)
                    }
                }
                "word-spacing" => {
                    s.word_spacing = if v == "normal" {
                        0.0
                    } else {
                        len(v, lc.diag()).unwrap_or(0.0)
                    }
                }
                "text-decoration" => {
                    let mut d = 0u8;
                    for w in v.split_whitespace() {
                        d |= match w {
                            "underline" => 1,
                            "overline" => 2,
                            "line-through" => 4,
                            _ => 0,
                        };
                    }
                    s.decoration = d;
                }
                "visibility" => s.visible = v == "visible",
                "marker-start" => s.marker_start = parse_url_ref(v),
                "marker-mid" => s.marker_mid = parse_url_ref(v),
                "marker-end" => s.marker_end = parse_url_ref(v),
                "marker" => {
                    let m = parse_url_ref(v);
                    s.marker_start = m.clone();
                    s.marker_mid = m.clone();
                    s.marker_end = m;
                }
                "image-rendering" => {
                    s.pixelated = matches!(v, "pixelated" | "crisp-edges" | "optimizeSpeed")
                }
                "paint-order" => {
                    let first = v.split_whitespace().next().unwrap_or("");
                    s.paint_order_stroke_first = first == "stroke";
                }
                "color-interpolation-filters" => s.linear_filters = v != "sRGB",
                _ => {}
            }
        }
        s
    }
}
