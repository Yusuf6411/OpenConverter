//! WebVTT (`.vtt`).
use crate::{balance, hms, html_to_inline, inline_to_html, pad, parse_clock, Cue, Track};
use oc_core::{Note, Notes, Severity};

fn parse_time_line(line: &str) -> Option<(i64, i64, String)> {
    let (a, rest) = line.split_once("-->")?;
    let rest = rest.trim();
    let (b, settings) = match rest.split_once(char::is_whitespace) {
        Some((b, s)) => (b, s.trim().to_string()),
        None => (rest, String::new()),
    };
    Some((parse_clock(a)?, parse_clock(b)?, settings))
}

fn align_from(settings: &str) -> Option<u8> {
    let mut col = 1u8; // 0 start, 1 centre, 2 end (centre by default)
    let mut row = 2u8; // 0 top, 1 middle, 2 bottom
    let mut any = false;
    for s in settings.split_whitespace() {
        if let Some(v) = s.strip_prefix("align:") {
            col = match v {
                "start" | "left" => 0,
                "end" | "right" => 2,
                _ => 1,
            };
            any = true;
        } else if let Some(v) = s.strip_prefix("line:") {
            let v = v.split(',').next().unwrap_or("");
            if let Ok(n) = v.trim_end_matches('%').parse::<f64>() {
                let pct = v.ends_with('%');
                if (pct && n < 33.0) || (!pct && (0.0..3.0).contains(&n)) {
                    row = 0;
                    any = true;
                } else if pct && n < 66.0 {
                    row = 1;
                    any = true;
                }
            }
        }
    }
    any.then(|| match row {
        0 => 7 + col,
        1 => 4 + col,
        _ => 1 + col,
    })
}

pub fn parse(text: &str, notes: &mut Notes) -> Track {
    let text = text
        .trim_start_matches('\u{FEFF}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut track = Track::default();
    let blocks = text.split("\n\n");
    let mut skipped = 0;
    let mut first = true;
    for block in blocks {
        let block = block.trim_matches('\n');
        if first {
            first = false;
            if block.starts_with("WEBVTT") {
                // the header block may continue with metadata lines
                continue;
            }
        }
        if block.is_empty()
            || block.starts_with("NOTE")
            || block.starts_with("STYLE")
            || block.starts_with("REGION")
        {
            if block.starts_with("STYLE") || block.starts_with("REGION") {
                skipped += 1;
            }
            continue;
        }
        let lines: Vec<&str> = block.lines().collect();
        let Some(ti) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let Some((start, end, settings)) = parse_time_line(lines[ti]) else {
            continue;
        };
        let raw = lines[ti + 1..].join("\n");
        // voice spans `<v Name>` and class spans have no inline equivalent; the text stays
        let inline = balance(&html_to_inline(&strip_timestamps(&raw)));
        track.cues.push(Cue {
            start_ms: start,
            end_ms: end,
            text: inline,
            align: align_from(&settings),
            ..Cue::default()
        });
    }
    if skipped > 0 {
        notes.push(Note::new(
            Severity::Info,
            "subtitle.vtt_styles_ignored",
            "STYLE and REGION blocks were not carried over",
        ));
    }
    track
}

/// Karaoke timestamps like `<00:00:01.000>` inside cue text.
fn strip_timestamps(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('>') {
            Some(e)
                if after[..e]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b == b':' || b == b'.')
                    && !after[..e].is_empty() =>
            {
                rest = &after[e + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// WebVTT has no `<font>` or `<s>`: colours become `<c.colorRRGGBB>` class spans (styled by a
/// STYLE block) and strike-out is dropped.
fn vtt_colours(body: &str, used: &mut Vec<String>) -> String {
    const OPEN: &str = "<font color=\"#";
    let mut out = String::new();
    let mut rest = body;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(after) = rest.strip_prefix(OPEN) {
            if let Some(e) = after.find("\">") {
                let hex = after[..e].to_ascii_uppercase();
                out.push_str(&format!("<c.color{hex}>"));
                if !used.contains(&hex) {
                    used.push(hex);
                }
                rest = &after[e + 2..];
                continue;
            }
        }
        let mut matched = false;
        for (from, to) in [("</font>", "</c>"), ("<s>", ""), ("</s>", "")] {
            if let Some(r) = rest.strip_prefix(from) {
                out.push_str(to);
                rest = r;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push('<');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

pub fn write(track: &Track, notes: &mut Notes) -> String {
    let mut out = String::from("WEBVTT\n\n");
    let mut dropped_style = false;
    let mut colours: Vec<String> = Vec::new();
    for c in &track.cues {
        let t = |ms: i64| {
            let (h, m, s, f) = hms(ms);
            format!("{}:{}:{}.{}", pad(h, 2), pad(m, 2), pad(s, 2), pad(f, 3))
        };
        out.push_str(&format!("{} --> {}", t(c.start_ms), t(c.end_ms)));
        if let Some(a) = c.align.filter(|&a| a != 2) {
            let line = match (a - 1) / 3 {
                2 => " line:0",
                1 => " line:50%",
                _ => "",
            };
            let align = match (a - 1) % 3 {
                0 => " align:start",
                2 => " align:end",
                _ => "",
            };
            out.push_str(line);
            out.push_str(align);
        }
        out.push('\n');
        if c.style.as_deref().is_some_and(|s| s != "Default") {
            dropped_style = true;
        }
        // a blank line would end the cue
        let body = inline_to_html(&balance(&c.text), true);
        let body = vtt_colours(&body, &mut colours);
        let body = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        out.push_str(&body);
        out.push_str("\n\n");
    }
    if !colours.is_empty() {
        let mut style = String::from("STYLE\n");
        for c in &colours {
            style.push_str(&format!("::cue(.color{c}) {{ color: #{c}; }}\n"));
        }
        style.push('\n');
        out.insert_str("WEBVTT\n\n".len(), &style);
    }
    if dropped_style {
        notes.push(Note::new(Severity::Info, "subtitle.styles_dropped", "named styles have no WebVTT equivalent here; bold, italic, underline and colour were kept"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_settings_and_notes() {
        let src = "WEBVTT - title\n\nNOTE a comment\n\nSTYLE\n::cue { color: red }\n\nintro\n00:01.000 --> 00:02.500 align:start line:0\n<v Bob><b>Hi</b></v> there &amp; <00:01.500>you\n\n01:00:00.000 --> 01:00:01.000\nlate\n";
        let mut n = Notes::new();
        let t = parse(src, &mut n);
        assert_eq!(t.cues.len(), 2);
        assert_eq!(t.cues[0].start_ms, 1000);
        assert_eq!(t.cues[0].text, "<b>Hi</b> there &amp; you");
        assert_eq!(t.cues[0].align, Some(7));
        assert_eq!(t.cues[1].start_ms, 3_600_000);
        let out = write(&t, &mut n);
        let t2 = parse(&out, &mut n);
        assert_eq!(t2.cues, t.cues);
        assert!(n.iter().any(|x| x.code == "subtitle.vtt_styles_ignored"));
    }

    #[test]
    fn colours_use_class_spans_and_style_block() {
        let t = Track {
            cues: vec![Cue {
                start_ms: 0,
                end_ms: 1000,
                text: "<font color=\"#ff0000\">red</font><b>b</b><s>x</s>".into(),
                ..Cue::default()
            }],
            ..Track::default()
        };
        let mut n = Notes::new();
        let out = write(&t, &mut n);
        assert!(out.contains("::cue(.colorFF0000)"), "{out}");
        assert!(out.contains("<c.colorFF0000>red</c><b>b</b>x"), "{out}");
        let back = parse(&out, &mut n);
        assert_eq!(
            back.cues[0].text,
            "<font color=\"#FF0000\">red</font><b>b</b>x"
        );
    }
}
