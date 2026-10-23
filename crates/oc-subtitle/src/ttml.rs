//! Timed Text Markup Language (`.ttml`, `.dfxp`, `.xml`).
//!
//! Reads `<p>` elements with `begin`/`end`/`dur` as clock times or offsets, nested `<span>`
//! styling (inline or through `<style xml:id>`), `<br/>` and simple alignment. Writes a flat
//! document with one `<p>` per cue and `<span>` runs for emphasis.
use crate::{
    balance, inline_tokens, normalise_colour, text_to_inline, unescape_inline, Cue, Track,
};
use oc_core::{Limits, Note, Notes, Severity};
use oc_xml::{Event, Reader, Writer};
use std::collections::HashMap;

fn local(n: &str) -> &str {
    n.rsplit(':').next().unwrap_or(n)
}

#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    colour: Option<String>,
    text_align: Option<String>,
    display_align: Option<String>,
}

impl Style {
    fn apply(&mut self, name: &str, value: &str) {
        match local(name) {
            "fontWeight" => self.bold = value == "bold",
            "fontStyle" => self.italic = value == "italic" || value == "oblique",
            "textDecoration" => self.underline = value.contains("underline"),
            "color" => self.colour = normalise_colour(value),
            "textAlign" => self.text_align = Some(value.to_string()),
            "displayAlign" => self.display_align = Some(value.to_string()),
            _ => {}
        }
    }
}

type OpenCue = (Option<i64>, Option<i64>, Option<i64>, Style, String, usize);

struct Timing {
    frame_rate: f64,
    tick_rate: f64,
}

fn offset(s: &str, tm: &Timing) -> Option<i64> {
    let s = s.trim();
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() < 3 {
            return None;
        }
        let h: f64 = parts[0].parse().ok()?;
        let m: f64 = parts[1].parse().ok()?;
        let sec: f64 = parts[2].parse().ok()?;
        let frames = match parts.get(3) {
            Some(f) => f.split('.').next()?.parse::<f64>().ok()? / tm.frame_rate,
            None => 0.0,
        };
        return Some((((h * 60.0 + m) * 60.0 + sec + frames) * 1000.0).round() as i64);
    }
    let split = s.find(|c: char| c.is_ascii_alphabetic())?;
    let v: f64 = s[..split].parse().ok()?;
    let ms = match &s[split..] {
        "h" => v * 3_600_000.0,
        "m" => v * 60_000.0,
        "s" => v * 1000.0,
        "ms" => v,
        "f" => v / tm.frame_rate * 1000.0,
        "t" => v / tm.tick_rate * 1000.0,
        _ => return None,
    };
    Some(ms.round() as i64)
}

fn keypad(style: &Style) -> Option<u8> {
    let row = match style.display_align.as_deref() {
        Some("before") => 2,
        Some("center") => 1,
        _ => 0,
    };
    let col = match style.text_align.as_deref() {
        Some("left" | "start") => 0,
        Some("right" | "end") => 2,
        Some("center") => 1,
        _ => 1,
    };
    let a = match row {
        2 => 7 + col,
        1 => 4 + col,
        _ => 1 + col,
    };
    (a != 2).then_some(a as u8)
}

pub fn looks_like_ttml(t: &str) -> bool {
    let head: String = t.chars().take(2000).collect();
    head.contains("<tt")
        && (head.contains("w3.org/ns/ttml") || head.contains("ttaf1") || head.contains("<tt "))
}

pub fn parse(text: &str, notes: &mut Notes) -> Track {
    let mut track = Track::default();
    let mut tm = Timing {
        frame_rate: 30.0,
        tick_rate: 10_000_000.0,
    };
    let mut styles: HashMap<String, Style> = HashMap::new();
    let mut reader = Reader::new(text, &Limits::default());
    // Open elements: (local name, style in effect, tags opened for it).
    let mut stack: Vec<(String, Style, Vec<&'static str>)> = Vec::new();
    let mut cue: Option<OpenCue> = None;
    let mut cur_style_def: Option<(String, Style)> = None;
    let mut bad = 0usize;
    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(e) => {
                notes.push(Note::new(
                    Severity::Warning,
                    "subtitle.xml_error",
                    format!("the XML stopped being readable: {}", e.message),
                ));
                break;
            }
        };
        match ev {
            Event::Eof => break,
            Event::Start { name, attrs, .. } => {
                let n = local(&name).to_string();
                let parent = stack.last().map(|s| s.1.clone()).unwrap_or_default();
                let mut st = parent.clone();
                let mut opened: Vec<&'static str> = Vec::new();
                for a in &attrs {
                    match local(&a.name) {
                        "frameRate" if n == "tt" => tm.frame_rate = a.value.parse().unwrap_or(30.0),
                        "tickRate" if n == "tt" => {
                            tm.tick_rate = a.value.parse().unwrap_or(10_000_000.0)
                        }
                        "lang" if n == "tt" => track.language = Some(a.value.clone()),
                        _ => {}
                    }
                }
                let attr = |k: &str| {
                    attrs
                        .iter()
                        .find(|a| local(&a.name) == k)
                        .map(|a| a.value.clone())
                };
                if let Some(sref) = attr("style").and_then(|s| {
                    styles
                        .get(s.split_whitespace().next().unwrap_or(""))
                        .cloned()
                }) {
                    st = sref;
                }
                for a in &attrs {
                    st.apply(&a.name, &a.value);
                }
                match n.as_str() {
                    "style" => {
                        let mut s = Style::default();
                        for a in &attrs {
                            s.apply(&a.name, &a.value);
                        }
                        if let Some(id) = attrs
                            .iter()
                            .find(|a| a.name.ends_with("id"))
                            .map(|a| a.value.clone())
                        {
                            cur_style_def = Some((id, s));
                        }
                    }
                    "p" => {
                        let dur = attr("dur").and_then(|d| offset(&d, &tm));
                        let begin = attr("begin").and_then(|d| offset(&d, &tm));
                        let end = attr("end").and_then(|d| offset(&d, &tm));
                        cue = Some((begin, end, dur, st.clone(), String::new(), stack.len()));
                    }
                    "br" => {
                        if let Some(c) = &mut cue {
                            c.4.push('\n');
                        }
                    }
                    "span" => {
                        if let Some(c) = &mut cue {
                            if st.bold && !parent.bold {
                                c.4.push_str("<b>");
                                opened.push("b");
                            }
                            if st.italic && !parent.italic {
                                c.4.push_str("<i>");
                                opened.push("i");
                            }
                            if st.underline && !parent.underline {
                                c.4.push_str("<u>");
                                opened.push("u");
                            }
                            if st.colour != parent.colour {
                                if let Some(col) = &st.colour {
                                    c.4.push_str(&format!("<font color=\"{col}\">"));
                                    opened.push("font");
                                }
                            }
                        }
                    }
                    _ => {}
                }
                stack.push((n, st, opened));
            }
            Event::End { .. } => {
                let Some((n, _, opened)) = stack.pop() else {
                    continue;
                };
                if let Some(c) = &mut cue {
                    for t in opened.iter().rev() {
                        c.4.push_str(&format!("</{t}>"));
                    }
                }
                match n.as_str() {
                    "style" => {
                        if let Some((id, s)) = cur_style_def.take() {
                            styles.insert(id, s);
                        }
                    }
                    "p" => {
                        if let Some((begin, end, dur, st, body, _)) = cue.take() {
                            match (begin, end.or_else(|| begin.zip(dur).map(|(b, d)| b + d))) {
                                (Some(b), Some(e)) => {
                                    let text = body
                                        .trim_matches('\n')
                                        .lines()
                                        .map(str::trim)
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    track.cues.push(Cue {
                                        start_ms: b,
                                        end_ms: e,
                                        text: balance(&text),
                                        align: keypad(&st),
                                        ..Cue::default()
                                    });
                                }
                                _ => bad += 1,
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) | Event::CData(t) => {
                if let Some(c) = &mut cue {
                    // Whitespace between runs collapses to one space, as in XML rendering.
                    let mut s = String::new();
                    let mut sp = c.4.ends_with([' ', '\n']) || c.4.is_empty();
                    for ch in t.chars() {
                        if ch.is_whitespace() {
                            if !sp {
                                s.push(' ');
                            }
                            sp = true;
                        } else {
                            s.push(ch);
                            sp = false;
                        }
                    }
                    c.4.push_str(&text_to_inline(&s));
                }
            }
            _ => {}
        }
    }
    if bad > 0 {
        notes.push(Note::new(
            Severity::Warning,
            "subtitle.bad_times",
            format!("{bad} paragraph(s) without usable begin/end times were skipped"),
        ));
    }
    if track.cues.is_empty() {
        notes.push(Note::new(
            Severity::Warning,
            "subtitle.no_cues",
            "no cue was found in this file",
        ));
    }
    track
}

fn clock(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

pub fn write(track: &Track, _notes: &mut Notes) -> String {
    let mut w = Writer::new();
    w.declaration();
    w.start("tt");
    w.attr("xmlns", "http://www.w3.org/ns/ttml");
    w.attr("xmlns:tts", "http://www.w3.org/ns/ttml#styling");
    w.attr("xml:lang", track.language.as_deref().unwrap_or("und"));
    w.start("head");
    if let Some(t) = &track.title {
        w.start("metadata");
        w.start("title");
        w.text(t);
        w.end();
        w.end();
    }
    w.end();
    w.start("body");
    w.start("div");
    for c in &track.cues {
        w.start("p");
        w.attr("begin", &clock(c.start_ms));
        w.attr("end", &clock(c.end_ms));
        if let Some(a) = c.align.filter(|&a| a != 2) {
            let (disp, text) = match a {
                7..=9 => ("before", a - 7),
                4..=6 => ("center", a - 4),
                _ => ("after", a - 1),
            };
            w.attr("tts:displayAlign", disp);
            w.attr(
                "tts:textAlign",
                ["left", "center", "right"][text as usize % 3],
            );
        }
        let mut open: Vec<Vec<(&str, String)>> = Vec::new();
        for (is_tag, t) in inline_tokens(&balance(&c.text)) {
            if !is_tag {
                for (i, line) in unescape_inline(&t).split('\n').enumerate() {
                    if i > 0 {
                        w.start("br");
                        w.end();
                    }
                    w.text(line);
                }
                continue;
            }
            if t.starts_with('/') {
                if open.pop().is_some() {
                    w.end();
                }
                continue;
            }
            let lower = t.to_ascii_lowercase();
            let attrs: Vec<(&str, String)> = match lower.split_whitespace().next().unwrap_or("") {
                "b" => vec![("tts:fontWeight", "bold".into())],
                "i" => vec![("tts:fontStyle", "italic".into())],
                "u" => vec![("tts:textDecoration", "underline".into())],
                "s" => vec![("tts:textDecoration", "lineThrough".into())],
                "font" => crate::tag_attr(&t, "color")
                    .and_then(|c| normalise_colour(&c))
                    .map(|c| vec![("tts:color", c)])
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            w.start("span");
            for (k, v) in &attrs {
                w.attr(k, v);
            }
            open.push(attrs);
        }
        w.end();
    }
    w.end();
    w.end();
    w.end();
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" xml:lang="en" ttp:tickRate="1000">
 <head><styling><style xml:id="em" tts:fontStyle="italic"/></styling></head>
 <body><div>
  <p begin="00:00:01.000" end="00:00:02.500" tts:displayAlign="before" tts:textAlign="center">Hello <span style="em">there</span><br/>second &amp; third</p>
  <p begin="3000t" dur="1.5s"><span tts:color="red" tts:fontWeight="bold">Red</span></p>
 </div></body></tt>"##;

    #[test]
    fn paragraphs_styles_and_offsets() {
        let mut n = Notes::new();
        let t = parse(SRC, &mut n);
        assert_eq!(t.language.as_deref(), Some("en"));
        assert_eq!(t.cues.len(), 2);
        assert_eq!(t.cues[0].text, "Hello <i>there</i>\nsecond &amp; third");
        assert_eq!(t.cues[0].align, Some(8));
        assert_eq!((t.cues[1].start_ms, t.cues[1].end_ms), (3000, 4500));
        assert_eq!(t.cues[1].text, "<b><font color=\"#FF0000\">Red</font></b>");
    }

    #[test]
    fn write_then_read_round_trips() {
        let mut n = Notes::new();
        let t = parse(SRC, &mut n);
        let out = write(&t, &mut n);
        let t2 = parse(&out, &mut n);
        assert_eq!(t2.cues.len(), 2);
        assert_eq!(t2.cues[0].text, t.cues[0].text);
        assert_eq!(t2.cues[0].align, t.cues[0].align);
        assert_eq!(t2.cues[1].start_ms, t.cues[1].start_ms);
        assert!(looks_like_ttml(&out));
    }
}
