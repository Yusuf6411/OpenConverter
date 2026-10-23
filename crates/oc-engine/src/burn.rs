//! Subtitle burn-in: each cue is drawn once as a transparent overlay (through the SVG renderer
//! and the installed fonts) and blended onto the frames it covers.
#![allow(clippy::chunks_exact_to_as_chunks, clippy::indexing_slicing)]
use oc_core::limits::Limits;
use oc_core::Notes;
use oc_subtitle::Track;
use oc_video::pic::Picture;

pub struct Burner {
    track: Track,
    fonts: oc_font::db::FontDb,
    limits: Limits,
    cache: Option<(usize, usize, usize, Option<Vec<u8>>)>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Greedy wrap on an estimated average glyph width.
fn wrap(text: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut cur = String::new();
        for word in raw.split_whitespace() {
            if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max_chars {
                lines.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }
    lines
}

impl Burner {
    pub fn new(mut track: Track, limits: &Limits) -> Self {
        track.cues.sort_by_key(|c| c.start_ms);
        Self {
            track,
            fonts: oc_font::db::FontDb::system(),
            limits: limits.clone(),
            cache: None,
        }
    }

    fn overlay(&self, idx: usize, w: usize, h: usize) -> Option<Vec<u8>> {
        let cue = self.track.cues.get(idx)?;
        let text = oc_subtitle::strip_markup(&cue.text);
        let fs = (h as f64 / 18.0).clamp(10.0, 96.0);
        let max_chars = ((w as f64 * 0.9) / (fs * 0.52)).max(8.0) as usize;
        let lines = wrap(&text, max_chars);
        if lines.is_empty() {
            return None;
        }
        let align = cue.align.unwrap_or(2).clamp(1, 9);
        let (anchor, x) = match (align - 1) % 3 {
            0 => ("start", w as f64 * 0.05),
            1 => ("middle", w as f64 / 2.0),
            _ => ("end", w as f64 * 0.95),
        };
        let lh = fs * 1.2;
        let block = lh * lines.len() as f64;
        let top = match (align - 1) / 3 {
            0 => h as f64 * 0.94 - block,
            1 => (h as f64 - block) / 2.0,
            _ => h as f64 * 0.06,
        };
        let mut svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">"
        );
        for (i, l) in lines.iter().enumerate() {
            let y = top + lh * (i as f64 + 0.8);
            svg.push_str(&format!(
                "<text x=\"{x:.1}\" y=\"{y:.1}\" font-family=\"Liberation Sans, DejaVu Sans, Arial, Helvetica, sans-serif\" font-size=\"{fs:.1}\" font-weight=\"bold\" text-anchor=\"{anchor}\" fill=\"#fff\" stroke=\"#000\" stroke-width=\"{:.1}\" stroke-linejoin=\"round\" paint-order=\"stroke\">{}</text>",
                fs / 8.0,
                esc(l)
            ));
        }
        svg.push_str("</svg>");
        let opts = oc_svg::Options {
            width: Some(w as u32),
            height: Some(h as u32),
            scale: 1.0,
            background: None,
            fonts: Some(&self.fonts),
            image_decoder: None,
        };
        let r = oc_svg::render(&svg, &opts, &self.limits).ok()?;
        Some(r.canvas.to_rgba8(None))
    }

    /// Draw the cues alive at `ms` onto the picture.
    pub fn apply(&mut self, pic: &mut Picture, ms: i64, _notes: &mut Notes) {
        let alive: Vec<usize> = self
            .track
            .cues
            .iter()
            .enumerate()
            .filter(|(_, c)| c.start_ms <= ms && ms < c.end_ms)
            .map(|(i, _)| i)
            .collect();
        // Overlapping cues are stacked by the layout of the first; one overlay per frame.
        let Some(&idx) = alive.first() else { return };
        let hit =
            matches!(&self.cache, Some((i, w, h, _)) if *i == idx && *w == pic.w && *h == pic.h);
        if !hit {
            let o = self.overlay(idx, pic.w, pic.h);
            self.cache = Some((idx, pic.w, pic.h, o));
        }
        let Some((_, _, _, Some(ov))) = &self.cache else {
            return;
        };
        let mut rgba = pic.to_rgba();
        for (px, o) in rgba.chunks_exact_mut(4).zip(ov.chunks_exact(4)) {
            let a = u32::from(o[3]);
            if a == 0 {
                continue;
            }
            for c in 0..3 {
                px[c] = ((u32::from(o[c]) * a + u32::from(px[c]) * (255 - a) + 127) / 255) as u8;
            }
        }
        if let Ok(p) = Picture::from_rgba(pic.w, pic.h, &rgba) {
            *pic = p;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_subtitle::Cue;

    #[test]
    fn cue_text_changes_only_frames_it_covers() {
        let track = Track {
            cues: vec![Cue {
                start_ms: 100,
                end_ms: 200,
                text: "Hello".into(),
                ..Cue::default()
            }],
            ..Track::default()
        };
        let mut b = Burner::new(track, &Limits::default());
        let base = Picture::black(160, 90);
        let mut before = base.clone();
        b.apply(&mut before, 50, &mut Notes::new());
        assert_eq!(before, base);
        let mut during = base.clone();
        b.apply(&mut during, 150, &mut Notes::new());
        assert_ne!(during, base);
        let mut after = base.clone();
        b.apply(&mut after, 200, &mut Notes::new());
        assert_eq!(after, base);
    }
}
