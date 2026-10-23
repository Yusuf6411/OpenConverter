//! Text extraction: glyphs to words, lines, blocks and a reading order, then plain text,
//! Markdown or HTML.
use crate::ops::{Op, Page, TextRun};
use oc_raster::Pt;

#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    /// Extent along the baseline and baseline position across it (page units, text frame).
    pub u0: f64,
    pub u1: f64,
    pub v: f64,
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub words: Vec<Word>,
    pub v: f64,
    pub u0: f64,
    pub u1: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct Paragraph {
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, Default)]
pub struct PageText {
    pub paragraphs: Vec<Paragraph>,
    /// The page has no text but does draw images (a scan).
    pub image_only: bool,
}

impl Line {
    pub fn text(&self) -> String {
        join_words(self.words.iter().map(|w| w.text.as_str()))
    }
}

fn join_words<'a>(it: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for w in it {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

fn clean_char(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => {
                out.push(' ')
            }
            '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}' | '\u{FEFF}' | '\u{FFFD}' => {}
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

struct Frame {
    d: Pt,
    n: Pt,
    class: u8,
}

fn frame_of(run: &TextRun) -> Frame {
    let g = run.glyphs.first();
    let d = g.map_or(Pt::new(1.0, 0.0), |g| {
        g.to_page.apply_vec(Pt::new(1.0, 0.0))
    });
    let len = d.len();
    let d = if len > 1e-12 {
        Pt::new(d.x / len, d.y / len)
    } else {
        Pt::new(1.0, 0.0)
    };
    let class = if d.x.abs() >= d.y.abs() {
        if d.x >= 0.0 {
            0
        } else {
            2
        }
    } else if d.y > 0.0 {
        1
    } else {
        3
    };
    Frame {
        d,
        n: Pt::new(-d.y, d.x),
        class,
    }
}

fn font_style(run: &TextRun) -> (bool, bool, bool) {
    let n = run.font.name.to_ascii_lowercase();
    let bold = run.font.bold_hint
        || n.contains("bold")
        || n.contains("black")
        || n.contains("heavy")
        || n.contains("semibold")
        || n.ends_with("-bd");
    let italic =
        run.font.italic_hint || n.contains("italic") || n.contains("oblique") || n.ends_with("-it");
    let mono = n.contains("mono")
        || n.contains("courier")
        || n.contains("consolas")
        || n.contains("typewriter");
    (bold, italic, mono)
}

struct Builder {
    words: Vec<Vec<Word>>,
    cur: Option<(u8, Word)>,
}

impl Builder {
    fn flush(&mut self) {
        if let Some((class, w)) = self.cur.take() {
            if !w.text.trim().is_empty() {
                if let Some(v) = self.words.get_mut(usize::from(class)) {
                    v.push(w);
                }
            }
        }
    }
}

/// For each marked-content sequence with ActualText, whether the text should replace what the
/// glyphs inside it say: only when those glyphs carry no Unicode of their own.
fn actual_text_spans(page: &Page) -> Vec<bool> {
    let mut out: Vec<bool> = Vec::new();
    // (index into `out`, glyph text seen)
    let mut stack: Vec<Option<(usize, bool)>> = Vec::new();
    for op in &page.ops {
        match op {
            Op::Marked {
                actual_text,
                begin: true,
                ..
            } => {
                if actual_text.is_some() {
                    out.push(true);
                    stack.push(Some((out.len() - 1, false)));
                } else {
                    stack.push(None);
                }
            }
            Op::Marked { begin: false, .. } => {
                if let Some(Some((i, seen))) = stack.pop() {
                    if let Some(o) = out.get_mut(i) {
                        *o = !seen;
                    }
                }
            }
            Op::Text(run) => {
                let any = run
                    .glyphs
                    .iter()
                    .any(|g| !clean_char(&g.unicode).trim().is_empty());
                if any {
                    for s in stack.iter_mut().flatten() {
                        s.1 = true;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Extract the structure of one page.
pub fn extract_page(page: &Page) -> PageText {
    let mut b = Builder {
        words: vec![Vec::new(); 4],
        cur: None,
    };
    let mut has_image = false;
    let mut has_text = false;
    // ActualText replacement: depth of the marked-content sequence that holds it
    let mut stack: Vec<bool> = Vec::new();
    let mut actual: Option<(String, usize, Option<(u8, f64, f64, f64, f64)>)> = None;
    let use_actual = actual_text_spans(page);
    let mut span_no = 0usize;
    for op in &page.ops {
        match op {
            Op::Image { .. } => has_image = true,
            Op::Marked {
                actual_text, begin, ..
            } => {
                if *begin {
                    let mut replaces = false;
                    if actual_text.is_some() {
                        replaces = use_actual.get(span_no).copied().unwrap_or(false);
                        span_no += 1;
                    }
                    stack.push(replaces);
                    if let (Some(t), true, None) = (actual_text, replaces, &actual) {
                        b.flush();
                        actual = Some((t.clone(), stack.len(), None));
                    }
                } else if let Some(had) = stack.pop() {
                    if had {
                        if let Some((t, depth, pos)) = actual.take_if(|a| a.1 == stack.len() + 1) {
                            let _ = depth;
                            if let Some((class, u, v, size, _)) = pos {
                                let text = clean_char(&t);
                                let mut x = u;
                                for piece in text.split_whitespace() {
                                    let w = piece.chars().count() as f64 * size * 0.5;
                                    if let Some(list) = b.words.get_mut(usize::from(class)) {
                                        list.push(Word {
                                            text: piece.to_string(),
                                            u0: x,
                                            u1: x + w,
                                            v,
                                            size,
                                            bold: false,
                                            italic: false,
                                            mono: false,
                                        });
                                    }
                                    x += w + size * 0.3;
                                }
                                has_text = true;
                            }
                        }
                    }
                }
            }
            Op::Text(run) => {
                if run.glyphs.is_empty() {
                    continue;
                }
                let fr = frame_of(run);
                if let Some(a) = actual.as_mut() {
                    if a.2.is_none() {
                        if let Some(g) = run.glyphs.first() {
                            a.2 = Some((
                                fr.class,
                                g.origin.x * fr.d.x + g.origin.y * fr.d.y,
                                g.origin.x * fr.n.x + g.origin.y * fr.n.y,
                                run.size,
                                0.0,
                            ));
                        }
                    }
                    continue;
                }
                let (bold, italic, mono) = font_style(run);
                let size = run.size.max(0.1);
                for g in &run.glyphs {
                    if g.origin.x < -1.0
                        || g.origin.y < -1.0
                        || g.origin.x > page.width + 1.0
                        || g.origin.y > page.height + 1.0
                    {
                        // outside the page: not shown, so not extracted
                        b.flush();
                        continue;
                    }
                    let u = g.origin.x * fr.d.x + g.origin.y * fr.d.y;
                    let v = g.origin.x * fr.n.x + g.origin.y * fr.n.y;
                    let text = clean_char(&g.unicode);
                    let ws = g.is_space || (!text.is_empty() && text.trim().is_empty());
                    if let Some((class, w)) = b.cur.as_mut() {
                        let gap = u - w.u1;
                        let same_line = (v - w.v).abs() < 0.35 * size.max(w.size);
                        let joins = *class == fr.class
                            && same_line
                            && gap > -0.6 * size
                            && gap < 0.22 * size.max(w.size)
                            && !ws;
                        if joins {
                            w.text.push_str(&text);
                            w.u1 = u + g.advance.max(0.0);
                            w.size = w.size.max(size);
                            has_text |= !text.is_empty();
                            continue;
                        }
                        b.flush();
                    }
                    if ws || text.is_empty() {
                        continue;
                    }
                    has_text = true;
                    b.cur = Some((
                        fr.class,
                        Word {
                            text,
                            u0: u,
                            u1: u + g.advance.max(0.0),
                            v,
                            size,
                            bold,
                            italic,
                            mono,
                        },
                    ));
                }
                // keep the word open across runs: adjacent runs continue it
            }
            _ => {}
        }
    }
    b.flush();
    let mut paragraphs = Vec::new();
    for words in b.words {
        if !words.is_empty() {
            paragraphs.extend(layout(words));
        }
    }
    PageText {
        paragraphs,
        image_only: has_image && !has_text,
    }
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 10.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// Words to ordered paragraphs.
fn layout(words: Vec<Word>) -> Vec<Paragraph> {
    let mut sizes: Vec<f64> = words.iter().map(|w| w.size).collect();
    let med = median(&mut sizes).max(1.0);
    let mut groups = Vec::new();
    xy_cut(words, med, 0, &mut groups);
    let mut out = Vec::new();
    for g in groups {
        out.extend(paragraphs_of(lines_of(g)));
    }
    out
}

fn top(w: &Word) -> f64 {
    w.v - 0.85 * w.size
}
fn bottom(w: &Word) -> f64 {
    w.v + 0.25 * w.size
}

fn intervals(mut iv: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    iv.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (a, b) in iv {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

fn line_count(words: &[Word]) -> usize {
    let mut v: Vec<f64> = words.iter().map(|w| w.v).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut n = 0;
    let mut last = f64::MIN;
    for x in v {
        if x - last > 2.0 {
            n += 1;
            last = x;
        }
    }
    n
}

fn xy_cut(words: Vec<Word>, med: f64, depth: u32, out: &mut Vec<Vec<Word>>) {
    if words.len() < 4 || depth > 10 {
        out.push(words);
        return;
    }
    // columns: a vertical gutter that no word crosses, with prose on both sides
    let ivs = intervals(words.iter().map(|w| (w.u0, w.u1)).collect());
    let gutter = (1.6 * med).max(10.0);
    for pair in ivs.windows(2) {
        if let [a, b] = pair {
            if b.0 - a.1 >= gutter {
                let mid = (a.1 + b.0) / 2.0;
                let (left, right): (Vec<Word>, Vec<Word>) = words
                    .iter()
                    .cloned()
                    .partition(|w| (w.u0 + w.u1) / 2.0 < mid);
                let prose = |ws: &[Word]| {
                    ws.len() as f64 / line_count(ws).max(1) as f64 >= 3.5 && line_count(ws) >= 3
                };
                if prose(&left) && prose(&right) {
                    xy_cut(left, med, depth + 1, out);
                    xy_cut(right, med, depth + 1, out);
                    return;
                }
            }
        }
    }
    // a wide horizontal gap separates stacked regions
    let hv = intervals(words.iter().map(|w| (top(w), bottom(w))).collect());
    let mut cuts = Vec::new();
    for pair in hv.windows(2) {
        if let [a, b] = pair {
            if b.0 - a.1 >= 0.9 * med {
                cuts.push((a.1 + b.0) / 2.0);
            }
        }
    }
    if !cuts.is_empty() && has_columns(&words, med) {
        let mut rest = words;
        for c in cuts {
            let (upper, lower): (Vec<Word>, Vec<Word>) = rest.into_iter().partition(|w| w.v < c);
            if !upper.is_empty() {
                xy_cut(upper, med, depth + 1, out);
            }
            rest = lower;
        }
        if !rest.is_empty() {
            xy_cut(rest, med, depth + 1, out);
        }
        return;
    }
    out.push(words);
}

/// True when some stacked region would split into columns, so horizontal cuts matter.
fn has_columns(words: &[Word], med: f64) -> bool {
    let ivs = intervals(words.iter().map(|w| (w.u0, w.u1)).collect());
    let _ = med;
    ivs.len() > 1 || words.len() > 40
}

fn lines_of(mut words: Vec<Word>) -> Vec<Line> {
    words.sort_by(|a, b| {
        a.v.partial_cmp(&b.v)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.u0.partial_cmp(&b.u0).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut lines: Vec<Line> = Vec::new();
    for w in words {
        let slot = lines
            .iter_mut()
            .rev()
            .take(3)
            .find(|l| (w.v - l.v).abs() < 0.4 * w.size.max(l.size) && w.size > 0.0);
        match slot {
            Some(l) => {
                l.v = (l.v * l.words.len() as f64 + w.v) / (l.words.len() + 1) as f64;
                l.u0 = l.u0.min(w.u0);
                l.u1 = l.u1.max(w.u1);
                l.size = l.size.max(w.size);
                l.words.push(w);
            }
            None => lines.push(Line {
                v: w.v,
                u0: w.u0,
                u1: w.u1,
                size: w.size,
                words: vec![w],
            }),
        }
    }
    for l in &mut lines {
        l.words
            .sort_by(|a, b| a.u0.partial_cmp(&b.u0).unwrap_or(std::cmp::Ordering::Equal));
    }
    lines.sort_by(|a, b| a.v.partial_cmp(&b.v).unwrap_or(std::cmp::Ordering::Equal));
    lines
}

fn paragraphs_of(lines: Vec<Line>) -> Vec<Paragraph> {
    let mut out: Vec<Paragraph> = Vec::new();
    for l in lines {
        let split = match out.last().and_then(|p| p.lines.last()) {
            None => true,
            Some(prev) => {
                let pitch = l.v - prev.v;
                let size = l.size.max(prev.size);
                pitch > 1.9 * size
                    || (l.size - prev.size).abs() > 0.15 * size
                    || starts_item(&l)
                    || starts_item(prev) && !continues(prev, &l)
            }
        };
        if split {
            out.push(Paragraph { lines: vec![l] });
        } else if let Some(p) = out.last_mut() {
            p.lines.push(l);
        }
    }
    out
}

fn continues(prev: &Line, l: &Line) -> bool {
    // a wrapped list item is indented past the bullet
    l.u0 > prev.u0 + 2.0 && !starts_item(l)
}

const BULLETS: [char; 9] = ['•', '●', '◦', '▪', '■', '‣', '∙', '·', '○'];

pub fn bullet_of(line: &str) -> Option<(&'static str, usize)> {
    let t = line.trim_start();
    let c = t.chars().next()?;
    if BULLETS.contains(&c) {
        return Some((
            "-",
            c.len_utf8() + usize::from(t[c.len_utf8()..].starts_with(' ')),
        ));
    }
    if (c == '-' || c == '–' || c == '*') && t[c.len_utf8()..].starts_with(' ') {
        return Some(("-", c.len_utf8() + 1));
    }
    None
}

pub fn numbered_of(line: &str) -> Option<(String, usize)> {
    let t = line.trim_start();
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let rest = &t[digits.len()..];
    let mut chars = rest.chars();
    let sep = chars.next()?;
    if (sep == '.' || sep == ')') && chars.next() == Some(' ') {
        return Some((format!("{digits}."), digits.len() + 2));
    }
    None
}

fn starts_item(l: &Line) -> bool {
    let t = l.text();
    bullet_of(&t).is_some() || numbered_of(&t).is_some()
}

fn paragraph_text(p: &Paragraph) -> String {
    let mut out = String::new();
    for l in &p.lines {
        let t = l.text();
        if out.is_empty() {
            out = t;
        } else if out.ends_with('-')
            && out.chars().rev().nth(1).is_some_and(char::is_alphabetic)
            && t.chars().next().is_some_and(char::is_lowercase)
        {
            out.pop();
            out.push_str(&t);
        } else {
            out.push(' ');
            out.push_str(&t);
        }
    }
    out
}

/// Plain text: lines as laid out, a blank line between paragraphs.
pub fn to_text(pages: &[PageText]) -> String {
    let mut out = String::new();
    for (i, pt) in pages.iter().enumerate() {
        if i > 0 {
            out.push('\u{c}');
        }
        for (j, p) in pt.paragraphs.iter().enumerate() {
            if j > 0 {
                out.push('\n');
            }
            for l in &p.lines {
                out.push_str(&l.text());
                out.push('\n');
            }
        }
    }
    out
}

fn body_size(pages: &[PageText]) -> f64 {
    let mut hist: Vec<(i64, usize)> = Vec::new();
    for l in pages
        .iter()
        .flat_map(|p| p.paragraphs.iter())
        .flat_map(|p| p.lines.iter())
    {
        let key = (l.size * 2.0).round() as i64;
        let n: usize = l.words.iter().map(|w| w.text.chars().count()).sum();
        match hist.iter_mut().find(|h| h.0 == key) {
            Some(h) => h.1 += n,
            None => hist.push((key, n)),
        }
    }
    hist.into_iter()
        .max_by_key(|h| h.1)
        .map_or(10.0, |h| h.0 as f64 / 2.0)
}

enum Kind {
    Heading(usize),
    Item,
    Numbered,
    Code,
    Para,
}

fn classify(p: &Paragraph, body: f64) -> Kind {
    let first = &p.lines[0];
    let text = paragraph_text(p);
    let chars = text.chars().count();
    let ratio = first.size / body.max(1.0);
    let all_bold = p.lines.iter().all(|l| l.words.iter().all(|w| w.bold));
    if chars < 160 && p.lines.len() <= 3 {
        if ratio >= 1.7 {
            return Kind::Heading(1);
        }
        if ratio >= 1.35 {
            return Kind::Heading(2);
        }
        if ratio >= 1.12 || (all_bold && ratio >= 0.95 && chars < 90 && !text.ends_with('.')) {
            return Kind::Heading(3);
        }
    }
    if bullet_of(&first.text()).is_some() {
        return Kind::Item;
    }
    if numbered_of(&first.text()).is_some() {
        return Kind::Numbered;
    }
    if p.lines.iter().all(|l| l.words.iter().all(|w| w.mono)) && p.lines.len() >= 2 {
        return Kind::Code;
    }
    Kind::Para
}

fn strip_marker(text: &str) -> String {
    if let Some((_, n)) = bullet_of(text) {
        let t = text.trim_start();
        return t.get(n..).unwrap_or(t).trim_start().to_string();
    }
    if let Some((_, n)) = numbered_of(text) {
        let t = text.trim_start();
        return t.get(n..).unwrap_or(t).trim_start().to_string();
    }
    text.to_string()
}

/// Styled inline text: runs of bold/italic words joined.
fn inline(p: &Paragraph, fmt: &dyn Fn(&str, bool, bool) -> String, skip_marker: bool) -> String {
    let mut parts: Vec<(String, bool, bool)> = Vec::new();
    let mut first = true;
    for l in &p.lines {
        for w in &l.words {
            parts.push((w.text.clone(), w.bold, w.italic));
        }
        if first && skip_marker {
            first = false;
        }
        // mark line breaks with an empty separator entry handled below
        parts.push((String::new(), false, false));
    }
    // dehyphenate across line ends, then merge runs with the same style
    let mut words: Vec<(String, bool, bool)> = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let (t, b, it) = parts[i].clone();
        if t.is_empty() {
            // line break: join a hyphenated word with the next one
            if let (Some(last), Some(next)) = (words.last_mut(), parts.get(i + 1)) {
                if last.0.ends_with('-')
                    && last.0.chars().rev().nth(1).is_some_and(char::is_alphabetic)
                    && next.0.chars().next().is_some_and(char::is_lowercase)
                {
                    last.0.pop();
                    last.0.push_str(&next.0);
                    last.1 &= next.1;
                    i += 2;
                    continue;
                }
            }
            i += 1;
            continue;
        }
        words.push((t, b, it));
        i += 1;
    }
    let mut out = String::new();
    let mut k = 0;
    while k < words.len() {
        let (_, b, it) = (&words[k].0, words[k].1, words[k].2);
        let mut j = k;
        let mut run = String::new();
        while j < words.len() && words[j].1 == b && words[j].2 == it {
            if !run.is_empty() {
                run.push(' ');
            }
            run.push_str(&words[j].0);
            j += 1;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&fmt(&run, b, it));
        k = j;
    }
    let _ = skip_marker;
    out
}

fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

pub fn to_markdown(pages: &[PageText]) -> String {
    let body = body_size(pages);
    let mut out = String::new();
    let fmt = |t: &str, b: bool, i: bool| {
        let e = md_escape(t);
        match (b, i) {
            (true, true) => format!("***{e}***"),
            (true, false) => format!("**{e}**"),
            (false, true) => format!("*{e}*"),
            _ => e,
        }
    };
    let plain = |t: &str, _: bool, _: bool| md_escape(t);
    let mut last_item = false;
    let mut last_numbered = false;
    for (pi, pt) in pages.iter().enumerate() {
        if pi > 0 {
            out.push_str("\n---\n\n");
            last_item = false;
        }
        for p in &pt.paragraphs {
            match classify(p, body) {
                Kind::Heading(n) => {
                    if last_item {
                        out.push('\n');
                    }
                    out.push_str(&format!(
                        "{} {}\n\n",
                        "#".repeat(n),
                        inline(p, &plain, false)
                    ));
                    last_item = false;
                }
                Kind::Item => {
                    if last_item && last_numbered {
                        out.push('\n');
                    }
                    last_numbered = false;
                    let text = inline(p, &fmt, true);
                    let text = strip_marker(&text.replace("\\*", "*"));
                    out.push_str(&format!("- {text}\n"));
                    last_item = true;
                }
                Kind::Numbered => {
                    if last_item && !last_numbered {
                        out.push('\n');
                    }
                    let first = p.lines[0].text();
                    let n = numbered_of(&first).map_or("1.".to_string(), |v| v.0);
                    let text = inline(p, &fmt, true);
                    out.push_str(&format!(
                        "{n} {}\n",
                        strip_marker(&text.replace("\\.", "."))
                    ));
                    last_item = true;
                    last_numbered = true;
                }
                Kind::Code => {
                    if last_item {
                        out.push('\n');
                    }
                    out.push_str("```\n");
                    for l in &p.lines {
                        out.push_str(&l.text());
                        out.push('\n');
                    }
                    out.push_str("```\n\n");
                    last_item = false;
                }
                Kind::Para => {
                    if last_item {
                        out.push('\n');
                    }
                    out.push_str(&inline(p, &fmt, false));
                    out.push_str("\n\n");
                    last_item = false;
                }
            }
        }
        if pt.image_only {
            out.push_str("*(this page has no text layer; it is an image)*\n\n");
        }
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

pub fn to_html(pages: &[PageText], title: &str) -> String {
    let body = body_size(pages);
    let mut out = String::new();
    out.push_str(&format!("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n</head>\n<body>\n", html_escape(title)));
    let fmt = |t: &str, b: bool, i: bool| {
        let e = html_escape(t);
        match (b, i) {
            (true, true) => format!("<strong><em>{e}</em></strong>"),
            (true, false) => format!("<strong>{e}</strong>"),
            (false, true) => format!("<em>{e}</em>"),
            _ => e,
        }
    };
    let plain = |t: &str, _: bool, _: bool| html_escape(t);
    let mut open_list: Option<&'static str> = None;
    let close = |out: &mut String, open: &mut Option<&'static str>| {
        if let Some(t) = open.take() {
            out.push_str(&format!("</{t}>\n"));
        }
    };
    for (pi, pt) in pages.iter().enumerate() {
        if pi > 0 {
            close(&mut out, &mut open_list);
            out.push_str("<hr>\n");
        }
        for p in &pt.paragraphs {
            match classify(p, body) {
                Kind::Heading(n) => {
                    close(&mut out, &mut open_list);
                    out.push_str(&format!("<h{n}>{}</h{n}>\n", inline(p, &plain, false)));
                }
                Kind::Item | Kind::Numbered => {
                    let tag = if matches!(classify(p, body), Kind::Item) {
                        "ul"
                    } else {
                        "ol"
                    };
                    if open_list != Some(tag) {
                        close(&mut out, &mut open_list);
                        out.push_str(&format!("<{tag}>\n"));
                        open_list = Some(tag);
                    }
                    let text = inline(p, &fmt, true);
                    out.push_str(&format!("<li>{}</li>\n", strip_marker(&text)));
                }
                Kind::Code => {
                    close(&mut out, &mut open_list);
                    out.push_str("<pre>");
                    for l in &p.lines {
                        out.push_str(&html_escape(&l.text()));
                        out.push('\n');
                    }
                    out.push_str("</pre>\n");
                }
                Kind::Para => {
                    close(&mut out, &mut open_list);
                    out.push_str(&format!("<p>{}</p>\n", inline(p, &fmt, false)));
                }
            }
        }
        if pt.image_only {
            close(&mut out, &mut open_list);
            out.push_str("<p><em>(this page has no text layer; it is an image)</em></p>\n");
        }
    }
    close(&mut out, &mut open_list);
    out.push_str("</body>\n</html>\n");
    out
}
