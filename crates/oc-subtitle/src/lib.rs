//! Subtitles: SRT, WebVTT, ASS/SSA and TTML, one cue model, timing operations.
//!
//! Cue text is plain text with a small inline markup shared by all formats: `<b>`, `<i>`,
//! `<u>`, `<s>` and `<font color="#RRGGBB">`, with `\n` for a line break. Every reader maps its
//! own styling to that, and every writer maps it back, so a conversion keeps emphasis and
//! colour. Positioning is reduced to a numeric-keypad alignment (1 to 9, 2 is bottom centre).
#![forbid(unsafe_code)]
#![allow(clippy::indexing_slicing)]

pub mod ass;
pub mod srt;
pub mod ttml;
pub mod vtt;

use oc_core::{Note, Notes, Severity};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// Keypad alignment 1 to 9; `None` is the default (bottom centre).
    pub align: Option<u8>,
    /// ASS style name.
    pub style: Option<String>,
    pub layer: i32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    pub cues: Vec<Cue>,
    pub title: Option<String>,
    pub language: Option<String>,
    /// ASS text before `[Events]` (script info and styles), kept so ASS to ASS keeps its styling.
    pub ass_header: Option<String>,
}

impl Track {
    /// Move every cue by `ms` (negative earlier). Cues that end up entirely before zero are dropped.
    pub fn shift(&mut self, ms: i64, notes: &mut Notes) {
        for c in &mut self.cues {
            c.start_ms += ms;
            c.end_ms += ms;
        }
        let before = self.cues.len();
        self.cues.retain(|c| c.end_ms > 0);
        for c in &mut self.cues {
            c.start_ms = c.start_ms.max(0);
        }
        if self.cues.len() != before {
            notes.push(Note::new(
                Severity::Warning,
                "subtitle.cues_dropped",
                format!(
                    "{} cue(s) fell before the start and were dropped",
                    before - self.cues.len()
                ),
            ));
        }
    }

    /// Multiply every time by `factor` (frame-rate retiming: subtitles made for `a` fps shown on
    /// a `b` fps video use `a / b`).
    pub fn scale(&mut self, factor: f64) {
        for c in &mut self.cues {
            c.start_ms = (c.start_ms as f64 * factor).round() as i64;
            c.end_ms = (c.end_ms as f64 * factor).round() as i64;
        }
    }

    /// Sort by start time, drop empty cues and fix cues whose end is not after their start.
    pub fn tidy(&mut self, notes: &mut Notes) {
        let n = self.cues.len();
        self.cues
            .retain(|c| !strip_markup(&c.text).trim().is_empty());
        self.cues.sort_by_key(|c| (c.start_ms, c.end_ms));
        let mut fixed = 0;
        for c in &mut self.cues {
            if c.end_ms <= c.start_ms {
                c.end_ms = c.start_ms + 1000;
                fixed += 1;
            }
        }
        if n != self.cues.len() {
            notes.push(Note::new(
                Severity::Info,
                "subtitle.empty_dropped",
                format!("{} empty cue(s) were left out", n - self.cues.len()),
            ));
        }
        if fixed > 0 {
            notes.push(Note::new(
                Severity::Warning,
                "subtitle.end_fixed",
                format!("{fixed} cue(s) ended before they began; they now last one second"),
            ));
        }
    }

    pub fn strip_markup(&mut self) {
        for c in &mut self.cues {
            c.text = strip_markup(&c.text);
        }
    }
}

/// Remove the inline markup, leaving the text.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Escape `<` and `&` that are text, not markup, before it goes into the inline form.
pub(crate) fn text_to_inline(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Split the inline form into runs of text and tags: `(is_tag, content)`.
pub(crate) fn inline_tokens(s: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            let mut closed = false;
            for t in chars.by_ref() {
                if t == '>' {
                    closed = true;
                    break;
                }
                tag.push(t);
            }
            if closed {
                if !cur.is_empty() {
                    out.push((false, std::mem::take(&mut cur)));
                }
                out.push((true, tag));
            } else {
                cur.push('<');
                cur.push_str(&tag);
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push((false, cur));
    }
    out
}

/// Text that came out of the inline form: entities back to characters.
pub(crate) fn unescape_inline(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

pub(crate) fn pad(n: i64, w: usize) -> String {
    format!("{n:0w$}")
}

/// `H:MM:SS` pieces of a millisecond count.
pub(crate) fn hms(ms: i64) -> (i64, i64, i64, i64) {
    let ms = ms.max(0);
    (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Parse `[[H:]M:]S[.,]fraction` into milliseconds.
pub(crate) fn parse_clock(s: &str) -> Option<i64> {
    let s = s.trim();
    let (main, frac) = match s.find(['.', ',']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() > 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let mut secs: i64 = 0;
    for p in &parts {
        secs = secs.checked_mul(60)?.checked_add(p.parse::<i64>().ok()?)?;
    }
    let mut ms = 0i64;
    if !frac.is_empty() {
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits: String = frac.chars().take(3).collect();
        ms = digits.parse::<i64>().ok()? * 10i64.pow(3 - digits.len() as u32);
    }
    secs.checked_mul(1000)?.checked_add(ms)
}

/// Detect the subtitle format of decoded text from its content.
pub fn sniff(text: &str) -> Option<&'static str> {
    let t = text.trim_start_matches('\u{FEFF}').trim_start();
    if t.starts_with("WEBVTT") {
        Some("vtt")
    } else if t.starts_with("[Script Info]") {
        Some("ass")
    } else if t.starts_with('<') && t[..t.len().min(2000)].contains("<tt") {
        Some("ttml")
    } else if srt::looks_like_srt(t) {
        Some("srt")
    } else {
        None
    }
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.bytes().take(12).position(|b| b == b';');
        let Some(e) = end else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..e];
        let rep: Option<String> = match name {
            "amp" => Some("&".into()),
            "lt" => Some("<".into()),
            "gt" => Some(">".into()),
            "quot" => Some("\"".into()),
            "apos" => Some("'".into()),
            "nbsp" => Some("\u{A0}".into()),
            "lrm" => Some("\u{200E}".into()),
            "rlm" => Some("\u{200F}".into()),
            n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16)
                .ok()
                .and_then(char::from_u32)
                .map(|c| c.to_string()),
            n if n.starts_with('#') => n[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .map(|c| c.to_string()),
            _ => None,
        };
        match rep {
            Some(r) => {
                out.push_str(&r);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` in the inside of a tag like `font color="#fff" size=3`.
fn tag_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let i = lower.find(&format!("{name}="))?;
    let rest = &tag[i + name.len() + 1..];
    let (q, body) = match rest.chars().next()? {
        c @ ('"' | '\'') => (Some(c), &rest[1..]),
        _ => (None, rest),
    };
    let end = match q {
        Some(c) => body.find(c)?,
        None => body.find(|c: char| c.is_whitespace()).unwrap_or(body.len()),
    };
    Some(body[..end].to_string())
}

/// `#RRGGBB` for a colour written as a name or as `#rgb`/`#rrggbb`; `None` when unknown.
pub(crate) fn normalise_colour(c: &str) -> Option<String> {
    let c = c.trim();
    if let Some(h) = c.strip_prefix('#') {
        return match h.len() {
            6 | 8 if h.bytes().all(|b| b.is_ascii_hexdigit()) => {
                Some(format!("#{}", h[..6].to_ascii_uppercase()))
            }
            3 if h.bytes().all(|b| b.is_ascii_hexdigit()) => Some(format!(
                "#{}",
                h.chars()
                    .flat_map(|x| [x, x])
                    .collect::<String>()
                    .to_ascii_uppercase()
            )),
            _ => None,
        };
    }
    let named = [
        ("white", "#FFFFFF"),
        ("black", "#000000"),
        ("red", "#FF0000"),
        ("lime", "#00FF00"),
        ("green", "#008000"),
        ("blue", "#0000FF"),
        ("yellow", "#FFFF00"),
        ("cyan", "#00FFFF"),
        ("aqua", "#00FFFF"),
        ("magenta", "#FF00FF"),
        ("fuchsia", "#FF00FF"),
        ("gray", "#808080"),
        ("grey", "#808080"),
        ("silver", "#C0C0C0"),
        ("orange", "#FFA500"),
        ("maroon", "#800000"),
        ("navy", "#000080"),
        ("purple", "#800080"),
        ("teal", "#008080"),
        ("olive", "#808000"),
    ];
    named
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(c))
        .map(|(_, v)| (*v).to_string())
}

/// SRT/WebVTT/TTML-style markup to the inline form: known tags kept, others dropped, entities
/// decoded, `<br>` a line break.
pub(crate) fn html_to_inline(raw: &str) -> String {
    let mut out = String::new();
    // WebVTT class spans `<c.colorFF0000>` / `<c.red>`: whether each open one became a font.
    let mut class_spans: Vec<bool> = Vec::new();
    for (is_tag, t) in inline_tokens(raw) {
        if !is_tag {
            out.push_str(&text_to_inline(&decode_entities(&t)));
            continue;
        }
        let lower = t.trim().to_ascii_lowercase();
        if lower == "/c" {
            if class_spans.pop() == Some(true) {
                out.push_str("</font>");
            }
            continue;
        }
        if lower == "c" || lower.starts_with("c.") || lower.starts_with("c ") {
            let colour = lower
                .split(|ch: char| ch == '.' || ch.is_whitespace())
                .skip(1)
                .find_map(|cls| match cls.strip_prefix("color") {
                    Some(hex) if hex.len() == 6 => normalise_colour(&format!("#{hex}")),
                    _ => normalise_colour(cls),
                });
            class_spans.push(colour.is_some());
            if let Some(c) = colour {
                out.push_str(&format!("<font color=\"{c}\">"));
            }
            continue;
        }
        let closing = lower.starts_with('/');
        let name: String = lower
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        match (name.as_str(), closing) {
            ("b" | "strong", c) => out.push_str(if c { "</b>" } else { "<b>" }),
            ("i" | "em", c) => out.push_str(if c { "</i>" } else { "<i>" }),
            ("u", c) => out.push_str(if c { "</u>" } else { "<u>" }),
            ("s" | "strike", c) => out.push_str(if c { "</s>" } else { "<s>" }),
            ("font", true) => out.push_str("</font>"),
            ("font", false) => match tag_attr(&t, "color").and_then(|c| normalise_colour(&c)) {
                Some(c) => out.push_str(&format!("<font color=\"{c}\">")),
                None => out.push_str("<font>"),
            },
            ("br", _) => out.push('\n'),
            _ => {}
        }
    }
    out
}

/// The inline form as HTML-ish text. `escape_amp` also turns `&` into `&amp;` (WebVTT, TTML).
pub(crate) fn inline_to_html(s: &str, escape_amp: bool) -> String {
    let mut out = String::new();
    for (is_tag, t) in inline_tokens(s) {
        if is_tag {
            out.push('<');
            out.push_str(&t);
            out.push('>');
        } else {
            let raw = unescape_inline(&t);
            let mut e = if escape_amp {
                raw.replace('&', "&amp;")
            } else {
                raw
            };
            e = e.replace('<', "&lt;");
            if escape_amp {
                e = e.replace('>', "&gt;");
            }
            out.push_str(&e);
        }
    }
    out
}

/// Close tags left open at the end of a cue so one cue never styles the next.
pub(crate) fn balance(s: &str) -> String {
    let mut stack: Vec<String> = Vec::new();
    let mut out = String::new();
    for (is_tag, t) in inline_tokens(s) {
        if !is_tag {
            out.push_str(&t);
            continue;
        }
        if let Some(name) = t.strip_prefix('/') {
            if let Some(p) = stack.iter().rposition(|n| n == name) {
                while stack.len() > p {
                    let top = stack.pop().unwrap_or_default();
                    out.push_str(&format!("</{top}>"));
                }
            }
        } else {
            let name = t.split_whitespace().next().unwrap_or("").to_string();
            stack.push(name);
            out.push('<');
            out.push_str(&t);
            out.push('>');
        }
    }
    while let Some(top) = stack.pop() {
        out.push_str(&format!("</{top}>"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_parse() {
        assert_eq!(parse_clock("00:01:02,500"), Some(62_500));
        assert_eq!(parse_clock("1:02.5"), Some(62_500));
        assert_eq!(parse_clock("2.25"), Some(2250));
        assert_eq!(parse_clock("x"), None);
    }

    #[test]
    fn shift_and_scale() {
        let mut t = Track::default();
        t.cues.push(Cue {
            start_ms: 1000,
            end_ms: 2000,
            text: "a".into(),
            ..Cue::default()
        });
        t.cues.push(Cue {
            start_ms: 100,
            end_ms: 500,
            text: "b".into(),
            ..Cue::default()
        });
        let mut n = Notes::new();
        t.shift(-600, &mut n);
        assert_eq!(t.cues.len(), 1);
        assert_eq!((t.cues[0].start_ms, t.cues[0].end_ms), (400, 1400));
        t.scale(25.0 / 23.976);
        assert_eq!(t.cues[0].start_ms, 417);
    }

    #[test]
    fn entity_scan_never_splits_a_character() {
        assert_eq!(
            decode_entities("&\u{fcff}\u{fcff}\u{fcff}\u{fcff}x &amp; &#65;"),
            "&\u{fcff}\u{fcff}\u{fcff}\u{fcff}x & A"
        );
    }
}
