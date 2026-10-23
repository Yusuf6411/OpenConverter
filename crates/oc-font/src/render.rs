//! Laying out and drawing text with a font: advances and `kern` pairs, no shaping.
use crate::reader::R;
use crate::sfnt::Font;
use oc_raster::{Blend, Canvas, FillRule, Paint, Transform};
use std::collections::HashMap;

/// Pair adjustments of the first format-0 `kern` subtable.
pub fn kern_pairs(font: &Font) -> HashMap<(u16, u16), i16> {
    let mut m = HashMap::new();
    let Some(t) = font.table(b"kern") else {
        return m;
    };
    let r = R::new(t);
    let n = r.u16(2).unwrap_or(0) as usize;
    let mut o = 4usize;
    for _ in 0..n.min(16) {
        let (Some(len), Some(cov)) = (r.u16(o + 2), r.u16(o + 4)) else {
            break;
        };
        if cov >> 8 == 0 && cov & 1 != 0 {
            let np = r.u16(o + 6).unwrap_or(0) as usize;
            for i in 0..np.min(60_000) {
                let e = o + 14 + i * 6;
                if let (Some(l), Some(rt), Some(v)) = (r.u16(e), r.u16(e + 2), r.i16(e + 4)) {
                    m.insert((l, rt), v);
                }
            }
        }
        o += len as usize;
        if len == 0 {
            break;
        }
    }
    m
}

/// Glyphs and x positions (in font units) for `text`.
pub fn layout(font: &Font, text: &str, kerns: &HashMap<(u16, u16), i16>) -> Vec<(u16, f64)> {
    let mut out = Vec::new();
    let mut x = 0.0;
    let mut prev: Option<u16> = None;
    for c in text.chars() {
        let gid = font.glyph_for_char(c).unwrap_or(0);
        if let Some(p) = prev {
            x += f64::from(kerns.get(&(p, gid)).copied().unwrap_or(0));
        }
        out.push((gid, x));
        x += f64::from(font.advance(gid));
        prev = Some(gid);
    }
    out
}

pub fn text_width(font: &Font, text: &str, size: f64, kerns: &HashMap<(u16, u16), i16>) -> f64 {
    let scale = size / f64::from(font.units_per_em);
    let l = layout(font, text, kerns);
    l.last()
        .map_or(0.0, |(g, x)| (x + f64::from(font.advance(*g))) * scale)
}

/// Draw `text` with its baseline origin at `(x, y)` (device pixels, y down).
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    canvas: &mut Canvas,
    font: &Font,
    text: &str,
    x: f64,
    y: f64,
    size: f64,
    color: [f32; 4],
    kerns: &HashMap<(u16, u16), i16>,
) {
    let scale = size / f64::from(font.units_per_em);
    let clip = canvas.clip();
    let paint = Paint::Solid(color);
    for (gid, gx) in layout(font, text, kerns) {
        let Some(path) = crate::outline(font, gid) else {
            continue;
        };
        let ctm = Transform::new(scale, 0.0, 0.0, -scale, x + gx * scale, y);
        canvas.fill_path(&path, &ctm, FillRule::NonZero, &paint, Blend::Normal, 1.0, &clip);
    }
}

/// A sheet showing the font: its name, the alphabet and a sample at several sizes.
pub fn specimen(font: &Font, width: usize) -> Option<Canvas> {
    let width = width.clamp(320, 4096);
    let kerns = kern_pairs(font);
    let name = font
        .name(4)
        .or_else(|| font.name(1))
        .unwrap_or_else(|| "Untitled font".to_string());
    let has_latin = font.glyph_for_char('A').is_some();
    let mut lines: Vec<(String, f64)> = vec![(name, 40.0)];
    if has_latin {
        lines.push(("ABCDEFGHIJKLMNOPQRSTUVWXYZ".to_string(), 36.0));
        lines.push(("abcdefghijklmnopqrstuvwxyz".to_string(), 36.0));
        lines.push(("0123456789 !?&@#$%()[]{}".to_string(), 36.0));
        for size in [64.0, 40.0, 28.0, 18.0, 12.0] {
            lines.push((
                "The quick brown fox jumps over the lazy dog".to_string(),
                size,
            ));
        }
    } else {
        // No Latin letters: show the first characters the font does map.
        let mut map = font.unicode_map();
        map.sort();
        for chunk in map.chunks(24).take(8) {
            let s: String = chunk.iter().filter_map(|(c, _)| char::from_u32(*c)).collect();
            lines.push((s, 36.0));
        }
    }
    let pad = 24.0;
    let height: f64 = pad * 2.0 + lines.iter().map(|(_, s)| s * 1.35).sum::<f64>();
    let mut canvas = Canvas::new(width, height.ceil() as usize)?;
    canvas.fill_color([1.0, 1.0, 1.0, 1.0]);
    let mut y = pad;
    for (text, size) in lines {
        y += size * 1.1;
        // Shrink a line that would run off the sheet.
        let w = text_width(font, &text, size, &kerns);
        let avail = width as f64 - pad * 2.0;
        let s = if w > avail && w > 0.0 { size * avail / w } else { size };
        draw_text(&mut canvas, font, &text, pad, y, s, [0.0, 0.0, 0.0, 1.0], &kerns);
        y += size * 0.25;
    }
    Some(canvas)
}

