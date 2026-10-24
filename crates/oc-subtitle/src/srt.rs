//! SubRip (`.srt`).
use crate::{balance, hms, html_to_inline, inline_to_html, pad, parse_clock, Cue, Track};
use oc_core::{Note, Notes, Severity};

pub fn looks_like_srt(t: &str) -> bool {
    t.lines().take(40).any(|l| {
        l.contains(" --> ")
            && l.split(" --> ")
                .next()
                .is_some_and(|a| a.contains(':') && a.contains(','))
    })
}

fn time_pair(line: &str) -> Option<(i64, i64)> {
    let (a, b) = line.split_once("-->")?;
    let b = b.split_whitespace().next()?;
    Some((parse_clock(a)?, parse_clock(b)?))
}

pub fn parse(text: &str, notes: &mut Notes) -> Track {
    let text = text
        .trim_start_matches('\u{FEFF}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let lines: Vec<&str> = text.lines().collect();
    let mut track = Track::default();
    let mut i = 0;
    let mut bad = 0;
    while i < lines.len() {
        let Some((start, end)) = time_pair(lines[i]) else {
            i += 1;
            continue;
        };
        i += 1;
        let mut body: Vec<&str> = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() {
            // A number line followed by a time line starts the next cue even without a blank line.
            if lines[i].trim().bytes().all(|b| b.is_ascii_digit())
                && lines.get(i + 1).is_some_and(|l| time_pair(l).is_some())
            {
                break;
            }
            body.push(lines[i]);
            i += 1;
        }
        let mut raw = body.join("\n");
        let mut align = None;
        // ASS-style override blocks occur in SRT files: keep the alignment, drop the rest.
        while let Some(a) = raw.find('{') {
            let Some(e) = raw[a..].find('}') else { break };
            let block = raw[a + 1..a + e].to_string();
            if let Some(p) = block.find("\\an") {
                align = block[p + 3..]
                    .chars()
                    .next()
                    .and_then(|c| c.to_digit(10))
                    .map(|d| d as u8)
                    .filter(|d| (1..=9).contains(d));
            }
            raw.replace_range(a..=a + e, "");
        }
        let inline = balance(&html_to_inline(&raw));
        if end < start {
            bad += 1;
        }
        track.cues.push(Cue {
            start_ms: start,
            end_ms: end,
            text: inline,
            align,
            ..Cue::default()
        });
    }
    if track.cues.is_empty() && !text.trim().is_empty() {
        notes.push(Note::new(
            Severity::Warning,
            "subtitle.no_cues",
            "no cue was found in this file",
        ));
    }
    if bad > 0 {
        notes.push(Note::new(
            Severity::Warning,
            "subtitle.bad_times",
            format!("{bad} cue(s) end before they start"),
        ));
    }
    track
}

pub fn write(track: &Track, notes: &mut Notes) -> String {
    let mut out = String::new();
    let mut dropped_style = false;
    for (n, c) in track.cues.iter().enumerate() {
        let t = |ms: i64| {
            let (h, m, s, f) = hms(ms);
            format!("{}:{}:{},{}", pad(h, 2), pad(m, 2), pad(s, 2), pad(f, 3))
        };
        out.push_str(&format!(
            "{}\n{} --> {}\n",
            n + 1,
            t(c.start_ms),
            t(c.end_ms)
        ));
        if let Some(a) = c.align.filter(|&a| a != 2) {
            out.push_str(&format!("{{\\an{a}}}"));
        }
        if c.style.as_deref().is_some_and(|s| s != "Default") {
            dropped_style = true;
        }
        out.push_str(&inline_to_html(&balance(&c.text), false));
        out.push_str("\n\n");
    }
    if dropped_style {
        notes.push(Note::new(
            Severity::Info,
            "subtitle.styles_dropped",
            "named styles have no SRT equivalent; bold, italic, underline and colour were kept",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_markup_and_alignment() {
        let src = "\u{FEFF}1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i> &amp; <b>bye</b>\r\nsecond line\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000 X1:100\r\n{\\an8}Top\r\n";
        let mut n = Notes::new();
        let t = parse(src, &mut n);
        assert_eq!(t.cues.len(), 2);
        assert_eq!(t.cues[0].start_ms, 1000);
        assert_eq!(t.cues[0].end_ms, 2500);
        assert_eq!(t.cues[0].text, "<i>Hello</i> &amp; <b>bye</b>\nsecond line");
        assert_eq!(t.cues[1].align, Some(8));
        let out = write(&t, &mut n);
        let t2 = parse(&out, &mut n);
        assert_eq!(t2.cues, t.cues);
        assert!(out.contains("<i>Hello</i> & <b>bye</b>"));
    }

    #[test]
    fn missing_blank_lines_and_indices_are_tolerated() {
        let src = "00:00:01,000 --> 00:00:02,000\nA\n2\n00:00:03,000 --> 00:00:04,000\nB\n";
        let mut n = Notes::new();
        let t = parse(src, &mut n);
        assert_eq!(t.cues.len(), 2);
        assert_eq!(t.cues[0].text, "A");
    }
}
