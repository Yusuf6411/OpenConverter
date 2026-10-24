//! `<text>`: glyph layout from advance widths and `kern` pairs, drawn as outlines.
use crate::parse::{self, Length};
use crate::render::{Env, Rect, Renderer};
use crate::style::{LenCtx, Style};
use oc_core::Severity;
use oc_font::render::kern_pairs;
use oc_font::Font;
use oc_raster::{Blend, FillRule, Path, Stroke, Transform};
use std::collections::HashMap;
use std::sync::Arc;

struct Ch {
    c: char,
    span: usize,
    x: Option<f64>,
    y: Option<f64>,
    dx: f64,
    dy: f64,
    rot: f64,
}

struct Glyph {
    span: usize,
    gid: u16,
    x: f64,
    y: f64,
    adv: f64,
    rot: f64,
}

struct SpanFont {
    font: Arc<Font>,
    kerns: HashMap<(u16, u16), i16>,
}

struct Layout {
    glyphs: Vec<Glyph>,
    spans: Vec<Style>,
    fonts: Vec<Option<SpanFont>>,
    bbox: Option<Rect>,
}

struct Collector<'a> {
    chars: Vec<Ch>,
    spans: Vec<Style>,
    prev_space: bool,
    preserve: bool,
    lc: &'a LenCtx,
}

impl Renderer<'_> {
    fn collect(&self, i: usize, style: &Style, col: &mut Collector, depth: u32) {
        if depth > 16 {
            return;
        }
        let Some(node) = self.doc.node(i) else { return };
        let span = col.spans.len();
        col.spans.push(style.clone());
        let start = col.chars.len();
        for &k in &node.children {
            let Some(kn) = self.doc.node(k) else { continue };
            match kn.tag.as_str() {
                "#text" => {
                    for c in kn.text.chars() {
                        let ws = matches!(c, ' ' | '\n' | '\r' | '\t');
                        let c = if ws && col.preserve {
                            ' '
                        } else if ws {
                            if col.prev_space {
                                continue;
                            }
                            ' '
                        } else {
                            c
                        };
                        col.prev_space = c == ' ';
                        col.chars.push(Ch {
                            c,
                            span,
                            x: None,
                            y: None,
                            dx: 0.0,
                            dy: 0.0,
                            rot: 0.0,
                        });
                    }
                }
                "tspan" | "a" | "textPath" => {
                    if kn.style.get("display").is_some_and(|d| d.trim() == "none") {
                        continue;
                    }
                    let st = style.derive(kn, col.lc);
                    self.collect(k, &st, col, depth + 1);
                }
                _ => {}
            }
        }
        let fs = style.font_size;
        let list = |name: &str, refv: f64| -> Vec<f64> {
            node.attrs
                .get(name)
                .map(|v| {
                    parse::length_list(v)
                        .iter()
                        .map(|l| l.resolve(refv, fs))
                        .collect()
                })
                .unwrap_or_default()
        };
        let (xs, ys) = (list("x", col.lc.vw), list("y", col.lc.vh));
        let (dxs, dys) = (list("dx", col.lc.vw), list("dy", col.lc.vh));
        let rots: Vec<f64> = node
            .attrs
            .get("rotate")
            .map(|v| parse::numbers(v))
            .unwrap_or_default();
        let n = col.chars.len() - start;
        for k in 0..n {
            let Some(ch) = col.chars.get_mut(start + k) else {
                break;
            };
            if let Some(v) = xs.get(k) {
                ch.x = Some(*v);
            }
            if let Some(v) = ys.get(k) {
                ch.y = Some(*v);
            }
            if let Some(v) = dxs.get(k) {
                ch.dx += v;
            }
            if let Some(v) = dys.get(k) {
                ch.dy += v;
            }
            if let Some(v) = rots.get(k).or(rots.last()) {
                ch.rot = v.to_radians();
            }
        }
    }

    fn layout_text(&mut self, i: usize, style: &Style, lc: &LenCtx) -> Option<Layout> {
        let node = self.doc.node(i)?;
        let preserve = node.attrs.get("xml:space").is_some_and(|v| v == "preserve");
        let mut col = Collector {
            chars: Vec::new(),
            spans: Vec::new(),
            prev_space: true,
            preserve,
            lc,
        };
        self.collect(i, style, &mut col, 0);
        if !preserve && col.chars.last().is_some_and(|c| c.c == ' ') {
            col.chars.pop();
        }
        if col.chars.is_empty() {
            return None;
        }
        if self.doc.node(i)?.children.iter().any(|k| {
            self.doc.node(*k).is_some_and(|n| {
                n.tag == "tspan"
                    && n.children
                        .iter()
                        .any(|g| self.doc.node(*g).is_some_and(|c| c.tag == "textPath"))
            })
        }) {
            // nothing: textPath inside tspan is not valid
        }
        let has_text_path = self
            .doc
            .node(i)?
            .children
            .iter()
            .any(|k| self.doc.node(*k).is_some_and(|n| n.tag == "textPath"));
        if has_text_path {
            self.note_once(
                "svg.textpath",
                Severity::Warning,
                "text on a path (<textPath>) is drawn along a straight line",
            );
        }
        // fonts per span
        let mut fonts: Vec<Option<SpanFont>> = Vec::new();
        let mut warned_missing = false;
        let mut any_font = false;
        for st in &col.spans {
            let found = self
                .fonts()
                .find(&st.font_family, st.font_weight, st.italic);
            match found {
                Some(f) => {
                    any_font = true;
                    let generic = st.font_family.first().is_none_or(|n| {
                        matches!(
                            n.to_ascii_lowercase().as_str(),
                            "serif"
                                | "sans-serif"
                                | "monospace"
                                | "cursive"
                                | "fantasy"
                                | "system-ui"
                        )
                    });
                    if f.substituted && !generic {
                        self.note_once(
                            "svg.font_substituted",
                            Severity::Info,
                            &format!(
                                "a requested font is not installed; {} was used instead",
                                f.family
                            ),
                        );
                    }
                    let kerns = kern_pairs(&f.font);
                    fonts.push(Some(SpanFont {
                        font: f.font,
                        kerns,
                    }));
                }
                None => fonts.push(None),
            }
        }
        if !any_font {
            self.note_once(
                "svg.no_fonts",
                Severity::Warning,
                "no usable font was found, so text was left out",
            );
            return None;
        }
        let mut glyphs: Vec<Glyph> = Vec::new();
        let (mut cx, mut cy) = (0.0f64, 0.0f64);
        let mut chunk_start = 0usize;
        let mut chunk_x0 = 0.0f64;
        let finish = |glyphs: &mut Vec<Glyph>, from: usize, x0: f64, x1: f64, spans: &[Style]| {
            let Some(first) = glyphs.get(from) else {
                return;
            };
            let anchor = spans.get(first.span).map_or(0, |s| s.text_anchor);
            let shift = match anchor {
                1 => -(x1 - x0) / 2.0,
                2 => -(x1 - x0),
                _ => 0.0,
            };
            if shift != 0.0 {
                for g in glyphs.iter_mut().skip(from) {
                    g.x += shift;
                }
            }
        };
        let mut prev: Option<(usize, u16)> = None;
        for ch in &col.chars {
            if ch.x.is_some() || ch.y.is_some() {
                finish(&mut glyphs, chunk_start, chunk_x0, cx, &col.spans);
                chunk_start = glyphs.len();
                if let Some(x) = ch.x {
                    cx = x;
                }
                if let Some(y) = ch.y {
                    cy = y;
                }
                chunk_x0 = cx;
                prev = None;
            }
            cx += ch.dx;
            cy += ch.dy;
            if glyphs.len() == chunk_start && ch.dx != 0.0 {
                chunk_x0 = cx;
            }
            let Some(Some(sf)) = fonts.get(ch.span) else {
                continue;
            };
            let Some(st) = col.spans.get(ch.span) else {
                continue;
            };
            let gid = match sf.font.glyph_for_char(ch.c) {
                Some(g) => g,
                None => {
                    if !warned_missing && ch.c != ' ' {
                        warned_missing = true;
                        self.note_once(
                            "svg.missing_glyph",
                            Severity::Warning,
                            "some characters have no glyph in the chosen font",
                        );
                    }
                    0
                }
            };
            let scale = st.font_size / f64::from(sf.font.units_per_em);
            if let Some((ps, pg)) = prev {
                if ps == ch.span {
                    cx += f64::from(sf.kerns.get(&(pg, gid)).copied().unwrap_or(0)) * scale;
                }
            }
            let adv = f64::from(sf.font.advance(gid)) * scale
                + st.letter_spacing
                + if ch.c == ' ' { st.word_spacing } else { 0.0 };
            glyphs.push(Glyph {
                span: ch.span,
                gid,
                x: cx,
                y: cy,
                adv,
                rot: ch.rot,
            });
            cx += adv;
            prev = Some((ch.span, gid));
        }
        finish(&mut glyphs, chunk_start, chunk_x0, cx, &col.spans);
        let mut bbox: Option<Rect> = None;
        for g in &glyphs {
            let (Some(Some(sf)), Some(st)) = (fonts.get(g.span), col.spans.get(g.span)) else {
                continue;
            };
            let scale = st.font_size / f64::from(sf.font.units_per_em);
            let asc = f64::from(sf.font.ascent) * scale;
            let desc = f64::from(sf.font.descent) * scale;
            let r = Rect {
                x: g.x,
                y: g.y - asc,
                w: g.adv.max(0.0),
                h: asc - desc,
            };
            bbox = Some(bbox.map_or(r, |b| b.union(r)));
        }
        Some(Layout {
            glyphs,
            spans: col.spans,
            fonts,
            bbox,
        })
    }

    pub fn text_bbox(&mut self, i: usize, lc: &LenCtx) -> Option<Rect> {
        let style = self.style_chain(i);
        self.layout_text(i, &style, lc)?.bbox
    }

    pub fn draw_text(&mut self, i: usize, style: &Style, env: &Env) {
        let Some(lay) = self.layout_text(i, style, &env.lc) else {
            return;
        };
        self.note_once(
            "svg.text_layout_basic",
            Severity::Info,
            "text is laid out from advance widths and kern pairs only (no OpenType shaping), so ligatures and complex scripts may differ",
        );
        if env.clip.is_empty() {
            return;
        }
        let bbox = lay.bbox;
        // resolve each span's paints once
        let mut paints = Vec::new();
        for st in &lay.spans {
            let f = self.resolve_paint(&st.fill, st.fill_opacity, st, bbox, env);
            let s = if st.stroke_width > 0.0 {
                self.resolve_paint(&st.stroke, st.stroke_opacity, st, bbox, env)
            } else {
                None
            };
            paints.push((f, s));
        }
        for g in &lay.glyphs {
            let (Some(Some(sf)), Some(st), Some((fill, stroke))) = (
                lay.fonts.get(g.span),
                lay.spans.get(g.span),
                paints.get(g.span),
            ) else {
                continue;
            };
            if !st.visible {
                continue;
            }
            let scale = st.font_size / f64::from(sf.font.units_per_em);
            let to_user = Transform::scale(scale, -scale)
                .then(&Transform::rotate(g.rot))
                .then(&Transform::translate(g.x, g.y));
            let mut parts: Vec<Path> = Vec::new();
            if let Some(p) = oc_font::outline(&sf.font, g.gid) {
                parts.push(p.transform(&to_user));
            }
            // text-decoration as thin rectangles along the advance
            let th = (st.font_size / 14.0).max(0.5);
            if st.decoration & 1 != 0 {
                parts.push(Path::rect(g.x, g.y + st.font_size * 0.1, g.adv, th));
            }
            if st.decoration & 2 != 0 {
                parts.push(Path::rect(
                    g.x,
                    g.y - f64::from(sf.font.ascent) * scale,
                    g.adv,
                    th,
                ));
            }
            if st.decoration & 4 != 0 {
                parts.push(Path::rect(g.x, g.y - st.font_size * 0.3, g.adv, th));
            }
            let stroke_cfg = Stroke {
                width: st.stroke_width,
                cap: st.cap,
                join: st.join,
                miter_limit: st.miter_limit,
                dash: st.dash.clone(),
                dash_phase: st.dash_offset,
                hairline: false,
            };
            for p in &parts {
                if let Some(f) = fill {
                    f.with(|paint, a| {
                        self.canvas.fill_path(
                            p,
                            &env.ctm,
                            FillRule::NonZero,
                            paint,
                            Blend::Normal,
                            a,
                            &env.clip,
                        );
                    });
                }
                if let Some(s) = stroke {
                    s.with(|paint, a| {
                        self.canvas.stroke_path(
                            p,
                            &env.ctm,
                            &stroke_cfg,
                            paint,
                            Blend::Normal,
                            a,
                            &env.clip,
                        );
                    });
                }
            }
        }
        let _ = Length::px;
    }
}
