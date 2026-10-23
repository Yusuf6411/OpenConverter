//! Advanced SubStation Alpha (`.ass`, `.ssa`).
//!
//! Reading keeps the part before `[Events]` verbatim in `Track::ass_header`, so ASS to ASS keeps
//! fonts, sizes and colours. Override blocks are reduced to bold, italic, underline, strike-out,
//! primary colour and `\an` alignment; drawing, karaoke, fades and motion are dropped.
use crate::{
    balance, inline_tokens, normalise_colour, pad, text_to_inline, unescape_inline, Cue, Track,
};
use oc_core::{Note, Notes, Severity};

const DEFAULT_HEADER: &str = "[Script Info]\nScriptType: v4.00+\nWrapStyle: 0\nScaledBorderAndShadow: yes\nPlayResX: 1920\nPlayResY: 1080\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,64,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,40,40,40,1\n";

fn clock(s: &str) -> Option<i64> {
    let s = s.trim();
    let (main, cs) = s.split_once('.').unwrap_or((s, "0"));
    let mut it = main.split(':');
    let h: i64 = it.next()?.trim().parse().ok()?;
    let m: i64 = it.next()?.trim().parse().ok()?;
    let sec: i64 = it.next()?.trim().parse().ok()?;
    if it.next().is_some() || !cs.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits: String = cs.chars().take(3).collect();
    let frac = if digits.is_empty() {
        0
    } else {
        digits.parse::<i64>().ok()? * 10i64.pow(3 - digits.len() as u32)
    };
    Some(((h * 60 + m) * 60 + sec) * 1000 + frac)
}

/// ASS colour `&HBBGGRR&` or `&HAABBGGRR` to `#RRGGBB`.
fn colour_from_ass(v: &str) -> Option<String> {
    let h = v
        .trim()
        .trim_matches('&')
        .trim_start_matches(['H', 'h'])
        .trim_end_matches('&');
    if h.is_empty() || h.len() > 8 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    let (b, g, r) = ((n >> 16) & 0xFF, (n >> 8) & 0xFF, n & 0xFF);
    Some(format!("#{r:02X}{g:02X}{b:02X}"))
}

fn colour_to_ass(c: &str) -> Option<String> {
    let c = normalise_colour(c)?;
    let h = c.strip_prefix('#')?;
    Some(format!("&H{}{}{}&", &h[4..6], &h[2..4], &h[0..2]))
}

/// ASS dialogue text to the inline form; returns it and any `\an` alignment found.
fn text_from_ass(raw: &str) -> (String, Option<u8>) {
    let mut out = String::new();
    let mut align = None;
    let (mut b, mut i, mut u, mut s, mut font) = (false, false, false, false, false);
    let mut rest = raw;
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut String| {
        if !plain.is_empty() {
            let t = plain
                .replace("\\N", "\n")
                .replace("\\n", "\n")
                .replace("\\h", "\u{A0}");
            out.push_str(&text_to_inline(&t));
            plain.clear();
        }
    };
    while !rest.is_empty() {
        if rest.starts_with('{') {
            if let Some(e) = rest.find('}') {
                flush(&mut plain, &mut out);
                for tag in rest[1..e].split('\\').skip(1) {
                    let t = tag.trim();
                    let toggle =
                        |cur: &mut bool, on: bool, open: &str, close: &str, out: &mut String| {
                            if on && !*cur {
                                out.push_str(open);
                            } else if !on && *cur {
                                out.push_str(close);
                            }
                            *cur = on;
                        };
                    if let Some(v) = t.strip_prefix("an") {
                        if let Some(d) = v
                            .chars()
                            .next()
                            .and_then(|c| c.to_digit(10))
                            .filter(|d| (1..=9).contains(d))
                        {
                            align = Some(d as u8);
                        }
                    } else if let Some(v) = t
                        .strip_prefix('b')
                        .filter(|v| !v.starts_with('e') && !v.starts_with('l'))
                    {
                        toggle(&mut b, v.trim() != "0", "<b>", "</b>", &mut out);
                    } else if let Some(v) = t
                        .strip_prefix('i')
                        .filter(|v| v.is_empty() || v.bytes().all(|c| c.is_ascii_digit()))
                    {
                        toggle(&mut i, v.trim() != "0", "<i>", "</i>", &mut out);
                    } else if let Some(v) = t
                        .strip_prefix('u')
                        .filter(|v| v.is_empty() || v.bytes().all(|c| c.is_ascii_digit()))
                    {
                        toggle(&mut u, v.trim() != "0", "<u>", "</u>", &mut out);
                    } else if let Some(v) = t
                        .strip_prefix('s')
                        .filter(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
                    {
                        toggle(&mut s, v.trim() != "0", "<s>", "</s>", &mut out);
                    } else if let Some(v) = t
                        .strip_prefix("1c")
                        .or_else(|| t.strip_prefix('c'))
                        .filter(|v| v.starts_with('&') || v.is_empty())
                    {
                        if font {
                            out.push_str("</font>");
                            font = false;
                        }
                        if let Some(c) = colour_from_ass(v) {
                            out.push_str(&format!("<font color=\"{c}\">"));
                            font = true;
                        }
                    } else if t == "r"
                        || t.starts_with('r')
                            && t[1..].chars().all(|c| c.is_alphanumeric() || c == ' ')
                    {
                        for (cur, close) in [
                            (&mut b, "</b>"),
                            (&mut i, "</i>"),
                            (&mut u, "</u>"),
                            (&mut s, "</s>"),
                            (&mut font, "</font>"),
                        ] {
                            if *cur {
                                out.push_str(close);
                                *cur = false;
                            }
                        }
                    }
                }
                rest = &rest[e + 1..];
                continue;
            }
        }
        let ch = rest.chars().next().unwrap_or(' ');
        plain.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    flush(&mut plain, &mut out);
    (balance(&out), align)
}

fn text_to_ass(inline: &str) -> String {
    let mut out = String::new();
    for (is_tag, t) in inline_tokens(&balance(inline)) {
        if !is_tag {
            let raw = unescape_inline(&t);
            out.push_str(
                &raw.replace('\r', "")
                    .replace('\n', "\\N")
                    .replace('{', "(")
                    .replace('}', ")"),
            );
            continue;
        }
        let lower = t.to_ascii_lowercase();
        match lower.as_str() {
            "b" => out.push_str("{\\b1}"),
            "/b" => out.push_str("{\\b0}"),
            "i" => out.push_str("{\\i1}"),
            "/i" => out.push_str("{\\i0}"),
            "u" => out.push_str("{\\u1}"),
            "/u" => out.push_str("{\\u0}"),
            "s" => out.push_str("{\\s1}"),
            "/s" => out.push_str("{\\s0}"),
            "/font" => out.push_str("{\\c}"),
            _ if lower.starts_with("font") => {
                if let Some(c) = crate::tag_attr(&t, "color").and_then(|c| colour_to_ass(&c)) {
                    out.push_str(&format!("{{\\c{c}}}"));
                }
            }
            _ => {}
        }
    }
    out
}

pub fn looks_like_ass(t: &str) -> bool {
    let t = t.trim_start_matches('\u{FEFF}').trim_start();
    t.starts_with("[Script Info]") || t.contains("\n[Events]") || t.contains("\nDialogue:")
}

pub fn parse(text: &str, notes: &mut Notes) -> Track {
    let text = text
        .trim_start_matches('\u{FEFF}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut track = Track::default();
    let mut header = String::new();
    let mut in_events = false;
    let mut format: Vec<String> = "Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text"
        .split(',')
        .map(str::to_string)
        .collect();
    let mut bad = 0usize;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            in_events = t.eq_ignore_ascii_case("[events]");
            if in_events {
                continue;
            }
        }
        if !in_events {
            if !t.is_empty() || !header.is_empty() {
                if let Some(v) = t.strip_prefix("Title:") {
                    track.title = Some(v.trim().to_string()).filter(|s| !s.is_empty());
                }
                header.push_str(line);
                header.push('\n');
            }
            continue;
        }
        if let Some(f) = t.strip_prefix("Format:") {
            format = f.split(',').map(|s| s.trim().to_string()).collect();
            continue;
        }
        let Some(body) = t.strip_prefix("Dialogue:") else {
            continue;
        };
        let fields: Vec<&str> = body.trim_start().splitn(format.len().max(1), ',').collect();
        let get = |name: &str| {
            format
                .iter()
                .position(|f| f.eq_ignore_ascii_case(name))
                .and_then(|p| fields.get(p))
                .copied()
        };
        let (Some(s), Some(e)) = (get("Start").and_then(clock), get("End").and_then(clock)) else {
            bad += 1;
            continue;
        };
        let (inline, align) = text_from_ass(get("Text").unwrap_or(""));
        track.cues.push(Cue {
            start_ms: s,
            end_ms: e,
            text: inline,
            align,
            style: get("Style")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            layer: get("Layer")
                .and_then(|l| l.trim().parse().ok())
                .unwrap_or(0),
        });
    }
    if bad > 0 {
        notes.push(Note::new(
            Severity::Warning,
            "subtitle.bad_times",
            format!("{bad} dialogue line(s) had unreadable times and were skipped"),
        ));
    }
    if track.cues.is_empty() && !text.trim().is_empty() {
        notes.push(Note::new(
            Severity::Warning,
            "subtitle.no_cues",
            "no cue was found in this file",
        ));
    }
    if !header.trim().is_empty() {
        track.ass_header = Some(header);
    }
    track
}

pub fn write(track: &Track, _notes: &mut Notes) -> String {
    let mut out = match &track.ass_header {
        Some(h) => {
            let mut h = h.trim_end().to_string();
            h.push_str("\n\n");
            h
        }
        None => {
            let mut h = DEFAULT_HEADER.to_string();
            if let Some(t) = &track.title {
                h = h.replacen(
                    "[Script Info]\n",
                    &format!("[Script Info]\nTitle: {}\n", t.replace('\n', " ")),
                    1,
                );
            }
            h.push('\n');
            h
        }
    };
    out.push_str("[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    let t = |ms: i64| {
        let cs = (ms.max(0) + 5) / 10;
        format!(
            "{}:{}:{}.{}",
            cs / 360_000,
            pad(cs / 6000 % 60, 2),
            pad(cs / 100 % 60, 2),
            pad(cs % 100, 2)
        )
    };
    for c in &track.cues {
        let an = c
            .align
            .filter(|&a| a != 2)
            .map(|a| format!("{{\\an{a}}}"))
            .unwrap_or_default();
        let style = c
            .style
            .as_deref()
            .filter(|s| !s.contains(','))
            .unwrap_or("Default");
        out.push_str(&format!(
            "Dialogue: {},{},{},{},,0,0,0,,{}{}\n",
            c.layer,
            t(c.start_ms),
            t(c.end_ms),
            style,
            an,
            text_to_ass(&c.text)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "[Script Info]\nTitle: Demo\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,20\nStyle: Sign,Impact,30\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.50,Default,,0,0,0,,{\\an8}{\\i1}Hello{\\i0}, world\\Nline {\\c&H0000FF&}red{\\r}\nDialogue: 1,0:00:03.00,0:00:04.00,Sign,Bob,0,0,0,,Plain\n";

    #[test]
    fn dialogue_maps_to_inline_markup() {
        let mut n = Notes::new();
        let t = parse(SRC, &mut n);
        assert_eq!(t.title.as_deref(), Some("Demo"));
        assert_eq!(t.cues.len(), 2);
        assert_eq!(t.cues[0].start_ms, 1000);
        assert_eq!(t.cues[0].end_ms, 2500);
        assert_eq!(t.cues[0].align, Some(8));
        assert_eq!(
            t.cues[0].text,
            "<i>Hello</i>, world\nline <font color=\"#FF0000\">red</font>"
        );
        assert_eq!(t.cues[1].style.as_deref(), Some("Sign"));
        assert_eq!(t.cues[1].layer, 1);
    }

    #[test]
    fn ass_to_ass_keeps_styles_and_round_trips_cues() {
        let mut n = Notes::new();
        let t = parse(SRC, &mut n);
        let out = write(&t, &mut n);
        assert!(out.contains("Style: Sign,Impact,30"));
        let t2 = parse(&out, &mut n);
        assert_eq!(t2.cues, t.cues);
    }

    #[test]
    fn default_header_when_converted_from_another_format() {
        let mut n = Notes::new();
        let t = Track {
            cues: vec![Cue {
                start_ms: 61_230,
                end_ms: 62_000,
                text: "<b>x</b>\ny".into(),
                ..Cue::default()
            }],
            ..Track::default()
        };
        let out = write(&t, &mut n);
        assert!(out.contains("Style: Default,"));
        assert!(out.contains("0:01:01.23,0:01:02.00"));
        assert!(out.contains("{\\b1}x{\\b0}\\Ny"));
    }
}
