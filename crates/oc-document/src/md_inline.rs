//! CommonMark inline parsing: code spans, emphasis, links, images, autolinks, raw HTML,
//! entities and escapes, plus GitHub strikethrough and autolink literals.
use crate::entities;
use crate::model::Inline;
use std::collections::HashMap;

#[derive(Default)]
pub struct RefMap {
    map: HashMap<String, (String, String)>,
}

pub fn normalize_label(s: &str) -> String {
    let mut out = String::new();
    let mut in_space = false;
    for c in s.trim().chars() {
        if c.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        for l in c.to_lowercase() {
            if l == 'ß' {
                out.push_str("ss");
            } else {
                out.push(l);
            }
        }
    }
    out
}

fn is_ascii_punct(b: u8) -> bool {
    b.is_ascii_punctuation()
}

fn is_punct_char(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_punctuation()
    } else {
        !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control()
    }
}

/// Resolve backslash escapes and character references.
pub fn unescape_string(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() && is_ascii_punct(b[i + 1]) {
            out.push(b[i + 1] as char);
            i += 2;
        } else if b[i] == b'&' {
            if let Some((t, n)) = entity_at(s, i) {
                out.push_str(&t);
                i += n;
            } else {
                out.push('&');
                i += 1;
            }
        } else {
            let ch = s[i..].chars().next().unwrap_or('\u{FFFD}');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// A character reference starting at `i` (which holds `&`): its text and length.
pub fn entity_at(s: &str, i: usize) -> Option<(String, usize)> {
    let rest = s.get(i + 1..)?;
    if let Some(num) = rest.strip_prefix('#') {
        let (digits, radix, skip) = if let Some(h) = num.strip_prefix(['x', 'X']) {
            (h, 16, 4)
        } else {
            (num, 10, 3)
        };
        let n = digits
            .bytes()
            .take_while(|b| b.is_ascii_hexdigit() && (radix == 16 || b.is_ascii_digit()))
            .count();
        if n == 0 || n > if radix == 16 { 6 } else { 7 } {
            return None;
        }
        if digits.as_bytes().get(n) != Some(&b';') {
            return None;
        }
        let v = u32::from_str_radix(&digits[..n], radix).ok()?;
        let c = if v == 0 || v > 0x10FFFF || (0xD800..0xE000).contains(&v) {
            '\u{FFFD}'
        } else {
            char::from_u32(v).unwrap_or('\u{FFFD}')
        };
        return Some((c.to_string(), skip + n));
    }
    let n = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric())
        .count();
    if n == 0 || rest.as_bytes().get(n) != Some(&b';') {
        return None;
    }
    let name = &rest[..n];
    let idx = entities::NAMED
        .binary_search_by(|(k, _)| (*k).cmp(name))
        .ok()?;
    Some((entities::NAMED[idx].1.to_string(), n + 2))
}

// ---------------------------------------------------------------------------------------------
// Links: destinations, titles, labels

fn skip_ws_one_newline(b: &[u8], mut i: usize) -> usize {
    let mut nl = false;
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' => i += 1,
            b'\n' if !nl => {
                nl = true;
                i += 1;
            }
            _ => break,
        }
    }
    i
}

/// Parse `<dest>` or a bare destination at `i`; returns the raw text and the next position.
fn parse_destination(s: &str, i: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.get(i) == Some(&b'<') {
        let mut j = i + 1;
        while j < b.len() {
            match b[j] {
                b'\\' if j + 1 < b.len() && is_ascii_punct(b[j + 1]) => j += 2,
                b'>' => return Some((unescape_string(&s[i + 1..j]), j + 1)),
                b'<' | b'\n' => return None,
                _ => j += 1,
            }
        }
        return None;
    }
    let mut j = i;
    let mut depth = 0i32;
    while j < b.len() {
        match b[j] {
            b'\\' if j + 1 < b.len() && is_ascii_punct(b[j + 1]) => j += 2,
            b'(' => {
                depth += 1;
                if depth > 32 {
                    return None;
                }
                j += 1;
            }
            b')' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                j += 1;
            }
            c if c <= b' ' || c == 0x7f => break,
            _ => j += 1,
        }
    }
    if depth != 0 {
        return None;
    }
    if j == i {
        return None;
    }
    Some((unescape_string(&s[i..j]), j))
}

fn parse_title(s: &str, i: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let open = *b.get(i)?;
    let close = match open {
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' if j + 1 < b.len() && is_ascii_punct(b[j + 1]) => j += 2,
            c if c == close => return Some((unescape_string(&s[i + 1..j]), j + 1)),
            b'(' if open == b'(' => return None,
            b'\n' => {
                // a blank line ends the title
                let mut k = j + 1;
                while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                    k += 1;
                }
                if k < b.len() && b[k] == b'\n' {
                    return None;
                }
                j += 1;
            }
            _ => j += 1,
        }
    }
    None
}

/// `[label]` at `i`: returns the raw label and the position after `]`.
fn parse_label(s: &str, i: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' if j + 1 < b.len() && is_ascii_punct(b[j + 1]) => j += 2,
            b'[' => return None,
            b']' => {
                let raw = &s[i + 1..j];
                if raw.chars().count() > 999 {
                    return None;
                }
                return Some((raw.to_string(), j + 1));
            }
            _ => j += 1,
        }
    }
    None
}

/// Parse a link reference definition at the start of `s`; returns the bytes consumed (0 = none).
pub fn parse_reference_definition(s: &str, refs: &mut RefMap) -> usize {
    let Some((label, mut i)) = parse_label(s, 0) else {
        return 0;
    };
    let b = s.as_bytes();
    if b.get(i) != Some(&b':') {
        return 0;
    }
    if label.trim().is_empty() {
        return 0;
    }
    i += 1;
    i = skip_ws_one_newline(b, i);
    let Some((dest, after_dest)) = parse_destination(s, i) else {
        return 0;
    };
    if after_dest == i && dest.is_empty() && b.get(i) != Some(&b'<') {
        return 0;
    }
    i = after_dest;
    // optional title
    let line_end = |k: usize| -> Option<usize> {
        let mut j = k;
        while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
        }
        if j >= b.len() {
            Some(j)
        } else if b[j] == b'\n' {
            Some(j + 1)
        } else {
            None
        }
    };
    let mut title = String::new();
    let mut end = None;
    let ws = skip_ws_one_newline(b, i);
    if ws > i && ws < b.len() {
        if let Some((t, after_t)) = parse_title(s, ws) {
            if let Some(e) = line_end(after_t) {
                title = t;
                end = Some(e);
            }
        }
    }
    let end = match end {
        Some(e) => e,
        None => match line_end(i) {
            Some(e) => e,
            None => return 0,
        },
    };
    let key = normalize_label(&label);
    refs.map.entry(key).or_insert((dest, title));
    end
}

// ---------------------------------------------------------------------------------------------
// Raw HTML

fn is_tag_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

/// Match an HTML open or closing tag, comment, processing instruction, declaration or CDATA at
/// the start of `s`. With `tag_only` only open and closing tags count (HTML block type 7).
pub fn scan_html_tag(s: &str, tag_only: bool) -> Option<usize> {
    let b = s.as_bytes();
    if b.first() != Some(&b'<') {
        return None;
    }
    let c1 = *b.get(1)?;
    if is_tag_name_start(c1) {
        // open tag
        let mut i = 1;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
            i += 1;
        }
        loop {
            let ws_start = i;
            while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n') {
                i += 1;
            }
            let had_ws = i > ws_start;
            match b.get(i)? {
                b'>' => return Some(i + 1),
                b'/' => {
                    return if b.get(i + 1) == Some(&b'>') {
                        Some(i + 2)
                    } else {
                        None
                    };
                }
                c if had_ws && (c.is_ascii_alphabetic() || *c == b'_' || *c == b':') => {
                    i += 1;
                    while i < b.len()
                        && (b[i].is_ascii_alphanumeric()
                            || matches!(b[i], b'_' | b'.' | b':' | b'-'))
                    {
                        i += 1;
                    }
                    // optional value
                    let save = i;
                    let mut j = i;
                    while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\n') {
                        j += 1;
                    }
                    if b.get(j) == Some(&b'=') {
                        j += 1;
                        while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\n') {
                            j += 1;
                        }
                        match b.get(j)? {
                            q @ (b'"' | b'\'') => {
                                let q = *q;
                                j += 1;
                                while j < b.len() && b[j] != q {
                                    j += 1;
                                }
                                if j >= b.len() {
                                    return None;
                                }
                                j += 1;
                            }
                            _ => {
                                let st = j;
                                while j < b.len()
                                    && !matches!(
                                        b[j],
                                        b' ' | b'\t'
                                            | b'\n'
                                            | b'"'
                                            | b'\''
                                            | b'='
                                            | b'<'
                                            | b'>'
                                            | b'`'
                                    )
                                {
                                    j += 1;
                                }
                                if j == st {
                                    return None;
                                }
                            }
                        }
                        i = j;
                    } else {
                        i = save;
                    }
                }
                _ => return None,
            }
        }
    }
    if c1 == b'/' {
        let mut i = 2;
        if !b.get(i).is_some_and(|c| is_tag_name_start(*c)) {
            return None;
        }
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
            i += 1;
        }
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        return if b.get(i) == Some(&b'>') {
            Some(i + 1)
        } else {
            None
        };
    }
    if tag_only {
        return None;
    }
    if s.starts_with("<!--") {
        if s.starts_with("<!-->") {
            return Some(5);
        }
        if s.starts_with("<!--->") {
            return Some(6);
        }
        return s[4..].find("-->").map(|p| 4 + p + 3);
    }
    if s.starts_with("<?") {
        return s[2..].find("?>").map(|p| 2 + p + 2);
    }
    if s.starts_with("<![CDATA[") {
        return s[9..].find("]]>").map(|p| 9 + p + 3);
    }
    if s.starts_with("<!") && b.get(2).is_some_and(|c| c.is_ascii_alphabetic()) {
        return s[2..].find('>').map(|p| 2 + p + 1);
    }
    None
}

fn scan_autolink(s: &str) -> Option<(String, bool, usize)> {
    // s starts with '<'
    let b = s.as_bytes();
    let end = s.find('>')?;
    let inner = &s[1..end];
    if inner.is_empty() || inner.bytes().any(|c| c <= b' ' || c == b'<') {
        return None;
    }
    let _ = b;
    // absolute URI: scheme of 2-32 chars then ':'
    if let Some(colon) = inner.find(':') {
        let scheme = &inner[..colon];
        if (2..=32).contains(&scheme.len())
            && scheme.as_bytes()[0].is_ascii_alphabetic()
            && scheme
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-'))
        {
            return Some((inner.to_string(), false, end + 1));
        }
    }
    // email
    if let Some(at) = inner.find('@') {
        let (local, domain) = (&inner[..at], &inner[at + 1..]);
        let ok_local = !local.is_empty()
            && local
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&c));
        let ok_domain = !domain.is_empty()
            && domain.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    && !l.starts_with('-')
                    && !l.ends_with('-')
            });
        if ok_local && ok_domain {
            return Some((inner.to_string(), true, end + 1));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The inline scanner

#[derive(Debug, Clone)]
enum Tok {
    Text(String),
    Node(Inline),
    Open(bool),
    Delim {
        ch: u8,
        count: usize,
        orig: usize,
        can_open: bool,
        can_close: bool,
    },
}

struct Bracket {
    tok: usize,
    image: bool,
    active: bool,
    src: usize,
}

struct Scanner<'a> {
    s: &'a str,
    b: &'a [u8],
    pos: usize,
    toks: Vec<Tok>,
    brackets: Vec<Bracket>,
    refs: &'a RefMap,
    gfm: bool,
}

pub fn parse_inlines(text: &str, refs: &RefMap, gfm: bool) -> Vec<Inline> {
    let mut sc = Scanner {
        s: text,
        b: text.as_bytes(),
        pos: 0,
        toks: Vec::new(),
        brackets: Vec::new(),
        refs,
        gfm,
    };
    sc.run();
    let n = sc.toks.len();
    let toks = std::mem::take(&mut sc.toks);
    let toks = process_emphasis(toks, 0, n, gfm);
    let mut out = tokens_to_inlines(toks);
    if gfm {
        out = autolink_literals(out);
    }
    out
}

fn push_text(toks: &mut Vec<Tok>, t: &str) {
    if let Some(Tok::Text(last)) = toks.last_mut() {
        last.push_str(t);
    } else {
        toks.push(Tok::Text(t.to_string()));
    }
}

impl Scanner<'_> {
    fn run(&mut self) {
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            match c {
                b'\n' => self.newline(),
                b'\\' => self.backslash(),
                b'`' => self.backticks(),
                b'&' => {
                    if let Some((t, n)) = entity_at(self.s, self.pos) {
                        push_text(&mut self.toks, &t);
                        self.pos += n;
                    } else {
                        push_text(&mut self.toks, "&");
                        self.pos += 1;
                    }
                }
                b'<' => self.angle(),
                b'*' | b'_' => self.delimiter_run(c),
                b'~' if self.gfm => self.delimiter_run(c),
                b'[' => {
                    self.brackets.push(Bracket {
                        tok: self.toks.len(),
                        image: false,
                        active: true,
                        src: self.pos + 1,
                    });
                    self.toks.push(Tok::Open(false));
                    self.pos += 1;
                }
                b'!' => {
                    if self.b.get(self.pos + 1) == Some(&b'[') {
                        self.brackets.push(Bracket {
                            tok: self.toks.len(),
                            image: true,
                            active: true,
                            src: self.pos + 2,
                        });
                        self.toks.push(Tok::Open(true));
                        self.pos += 2;
                    } else {
                        push_text(&mut self.toks, "!");
                        self.pos += 1;
                    }
                }
                b']' => self.close_bracket(),
                _ => {
                    let start = self.pos;
                    while self.pos < self.b.len()
                        && !matches!(
                            self.b[self.pos],
                            b'\n' | b'\\' | b'`' | b'&' | b'<' | b'*' | b'_' | b'[' | b']' | b'!'
                        )
                        && !(self.gfm && self.b[self.pos] == b'~')
                    {
                        self.pos += 1;
                    }
                    if self.pos == start {
                        self.pos += 1;
                    }
                    let end = self.pos;
                    let t = self.s[start..end].to_string();
                    push_text(&mut self.toks, &t);
                }
            }
        }
    }

    fn newline(&mut self) {
        // trailing spaces on the previous text decide hard or soft break
        let mut hard = false;
        if let Some(Tok::Text(t)) = self.toks.last_mut() {
            let trimmed = t.trim_end_matches(' ');
            let spaces = t.len() - trimmed.len();
            if spaces >= 2 {
                hard = true;
            }
            t.truncate(trimmed.len());
            if t.is_empty() {
                self.toks.pop();
            }
        }
        self.toks.push(Tok::Node(if hard {
            Inline::LineBreak
        } else {
            Inline::SoftBreak
        }));
        self.pos += 1;
        while self.pos < self.b.len() && matches!(self.b[self.pos], b' ' | b'\t') {
            self.pos += 1;
        }
    }

    fn backslash(&mut self) {
        match self.b.get(self.pos + 1) {
            Some(&c) if is_ascii_punct(c) => {
                push_text(&mut self.toks, &(c as char).to_string());
                self.pos += 2;
            }
            Some(b'\n') => {
                self.toks.push(Tok::Node(Inline::LineBreak));
                self.pos += 2;
                while self.pos < self.b.len() && matches!(self.b[self.pos], b' ' | b'\t') {
                    self.pos += 1;
                }
            }
            _ => {
                push_text(&mut self.toks, "\\");
                self.pos += 1;
            }
        }
    }

    fn backticks(&mut self) {
        let start = self.pos;
        let mut n = 0;
        while self.b.get(self.pos) == Some(&b'`') {
            n += 1;
            self.pos += 1;
        }
        let content_start = self.pos;
        let mut j = self.pos;
        while j < self.b.len() {
            if self.b[j] == b'`' {
                let mut m = 0;
                while self.b.get(j + m) == Some(&b'`') {
                    m += 1;
                }
                if m == n {
                    let raw = self.s[content_start..j].replace('\n', " ");
                    let stripped = if raw.len() >= 2
                        && raw.starts_with(' ')
                        && raw.ends_with(' ')
                        && !raw.chars().all(|c| c == ' ')
                    {
                        raw[1..raw.len() - 1].to_string()
                    } else {
                        raw
                    };
                    self.toks.push(Tok::Node(Inline::Code(stripped)));
                    self.pos = j + m;
                    return;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        push_text(&mut self.toks, &self.s[start..content_start]);
    }

    fn angle(&mut self) {
        let rest = &self.s[self.pos..];
        if let Some((target, email, n)) = scan_autolink(rest) {
            let url = if email {
                format!("mailto:{target}")
            } else {
                target.clone()
            };
            self.toks.push(Tok::Node(Inline::Link {
                url,
                title: String::new(),
                content: vec![Inline::Text(target)],
            }));
            self.pos += n;
            return;
        }
        if let Some(n) = scan_html_tag(rest, false) {
            self.toks
                .push(Tok::Node(Inline::Html(rest[..n].to_string())));
            self.pos += n;
            return;
        }
        push_text(&mut self.toks, "<");
        self.pos += 1;
    }

    fn delimiter_run(&mut self, c: u8) {
        let start = self.pos;
        let mut n = 0;
        while self.b.get(self.pos) == Some(&c) {
            n += 1;
            self.pos += 1;
        }
        let before = self.s[..start].chars().next_back().unwrap_or('\n');
        let after = self.s[self.pos..].chars().next().unwrap_or('\n');
        let (bw, aw) = (before.is_whitespace(), after.is_whitespace());
        let (bp, ap) = (is_punct_char(before), is_punct_char(after));
        let left = !aw && (!ap || bw || bp);
        let right = !bw && (!bp || aw || ap);
        let (can_open, can_close) = if c == b'_' {
            (left && (!right || bp), right && (!left || ap))
        } else {
            (left, right)
        };
        self.toks.push(Tok::Delim {
            ch: c,
            count: n,
            orig: n,
            can_open,
            can_close,
        });
    }

    fn close_bracket(&mut self) {
        let close = self.pos;
        self.pos += 1;
        let Some(opener) = self.brackets.pop() else {
            push_text(&mut self.toks, "]");
            return;
        };
        if !opener.active {
            push_text(&mut self.toks, "]");
            return;
        }
        let after = self.pos;
        // inline link
        let mut found: Option<(String, String, usize)> = None;
        if self.b.get(after) == Some(&b'(') {
            let mut i = skip_ws_one_newline(self.b, after + 1);
            let dest = if self.b.get(i) == Some(&b')') {
                Some((String::new(), i))
            } else {
                parse_destination(self.s, i)
            };
            if let Some((d, mut j)) = dest {
                let k = skip_ws_one_newline(self.b, j);
                let mut title = String::new();
                let mut ok = true;
                if k > j {
                    if let Some((t, e)) = parse_title(self.s, k) {
                        title = t;
                        j = e;
                    }
                }
                i = skip_ws_one_newline(self.b, j);
                if self.b.get(i) != Some(&b')') {
                    ok = false;
                }
                if ok {
                    found = Some((d, title, i + 1));
                }
            }
        }
        // reference link
        if found.is_none() {
            let raw = &self.s[opener.src..close];
            let mut label = raw.to_string();
            let mut end = after;
            if self.b.get(after) == Some(&b'[') {
                match parse_label(self.s, after) {
                    Some((l, e)) if !l.trim().is_empty() => {
                        label = l;
                        end = e;
                    }
                    Some((_, e)) => {
                        end = e;
                    }
                    None => {}
                }
            }
            if let Some((u, t)) = self.refs.map.get(&normalize_label(&label)) {
                if raw.chars().count() <= 999 {
                    found = Some((u.clone(), t.clone(), end));
                }
            }
        }
        match found {
            Some((url, title, end)) => {
                let tail: Vec<Tok> = self.toks.drain(opener.tok + 1..).collect();
                self.toks.pop(); // the opening bracket text
                let n = tail.len();
                let tail = process_emphasis(tail, 0, n, self.gfm);
                let content = tokens_to_inlines(tail);
                if depth_of(&content) >= MAX_INLINE_DEPTH {
                    // too deep: the link or image is left as text
                    self.toks
                        .push(Tok::Text(if opener.image { "![" } else { "[" }.into()));
                    self.toks.extend(content.into_iter().map(Tok::Node));
                    push_text(&mut self.toks, "]");
                    return;
                }
                let node = if opener.image {
                    Inline::Image {
                        src: url,
                        title,
                        alt: content,
                    }
                } else {
                    Inline::Link {
                        url,
                        title,
                        content,
                    }
                };
                self.toks.push(Tok::Node(node));
                if !opener.image {
                    for b in &mut self.brackets {
                        if !b.image {
                            b.active = false;
                        }
                    }
                }
                self.pos = end;
            }
            None => push_text(&mut self.toks, "]"),
        }
    }
}

fn convert_leftover(t: Tok) -> Tok {
    match t {
        Tok::Delim { ch, count, .. } => Tok::Text((ch as char).to_string().repeat(count)),
        Tok::Open(img) => Tok::Text(if img { "![" } else { "[" }.into()),
        o => o,
    }
}

fn process_emphasis(mut toks: Vec<Tok>, bottom: usize, _end: usize, gfm: bool) -> Vec<Tok> {
    let mut closer = bottom;
    // lowest index worth searching for an opener, per (char, closer can open, closer length mod 3)
    let mut floor: HashMap<(u8, bool, usize), usize> = HashMap::new();
    while closer < toks.len() {
        let (cch, ccount, corig, ccan_open, ccan_close) = match &toks[closer] {
            Tok::Delim {
                ch,
                count,
                orig,
                can_open,
                can_close,
            } => (*ch, *count, *orig, *can_open, *can_close),
            _ => {
                closer += 1;
                continue;
            }
        };
        if !ccan_close {
            closer += 1;
            continue;
        }
        let key = (cch, ccan_open, corig % 3);
        let lo = floor.get(&key).copied().unwrap_or(bottom).max(bottom);
        let mut opener = None;
        let mut o = closer;
        while o > lo {
            o -= 1;
            if let Tok::Delim {
                ch,
                count,
                orig,
                can_open,
                can_close,
            } = &toks[o]
            {
                if *ch == cch && *can_open {
                    let odd = (*can_close || ccan_open)
                        && (orig + corig) % 3 == 0
                        && !(orig % 3 == 0 && corig % 3 == 0);
                    let tilde_ok = cch != b'~' || *count == ccount;
                    if !odd && tilde_ok {
                        opener = Some(o);
                        break;
                    }
                }
            }
        }
        let Some(o) = opener else {
            floor.insert(key, closer);
            if !ccan_open {
                toks[closer] = convert_leftover(toks[closer].clone());
            }
            closer += 1;
            continue;
        };
        let ocount = match &toks[o] {
            Tok::Delim { count, .. } => *count,
            _ => 0,
        };
        let use_n = if cch == b'~' {
            if ccount > 2 {
                closer += 1;
                continue;
            }
            ccount
        } else if ccount >= 2 && ocount >= 2 {
            2
        } else {
            1
        };
        let inner: Vec<Tok> = toks.drain(o + 1..closer).map(convert_leftover).collect();
        let children = tokens_to_inlines(inner);
        if depth_of(&children) >= MAX_INLINE_DEPTH {
            // too deep: keep the markers as text
            let mut flat = vec![Tok::Text((cch as char).to_string().repeat(use_n))];
            flat.extend(children.into_iter().map(Tok::Node));
            flat.push(Tok::Text((cch as char).to_string().repeat(use_n)));
            let n = flat.len();
            toks.splice(o + 1..o + 1, flat);
            let cidx = o + 1 + n;
            if let Tok::Delim { count, .. } = &mut toks[o] {
                *count -= use_n;
            }
            if let Tok::Delim { count, .. } = &mut toks[cidx] {
                *count -= use_n;
            }
            closer = cidx;
            if matches!(&toks[cidx], Tok::Delim { count: 0, .. }) {
                toks.remove(cidx);
            }
            continue;
        }
        let node = match (cch, use_n) {
            (b'~', _) => Inline::Strike(children),
            (_, 2) => Inline::Strong(children),
            _ => Inline::Emph(children),
        };
        toks.insert(o + 1, Tok::Node(node));
        let mut cidx = o + 2;
        if let Tok::Delim { count, .. } = &mut toks[o] {
            *count -= use_n;
        }
        if let Tok::Delim { count, .. } = &mut toks[cidx] {
            *count -= use_n;
        }
        let ocount_left = match &toks[o] {
            Tok::Delim { count, .. } => *count,
            _ => 0,
        };
        let ccount_left = match &toks[cidx] {
            Tok::Delim { count, .. } => *count,
            _ => 0,
        };
        if ccount_left == 0 {
            toks.remove(cidx);
        }
        if ocount_left == 0 {
            toks.remove(o);
            cidx -= 1;
        }
        closer = cidx;
        let _ = gfm;
    }
    toks.into_iter().map(convert_leftover).collect()
}

const MAX_INLINE_DEPTH: usize = 64;

fn depth_of(v: &[Inline]) -> usize {
    v.iter()
        .map(|i| match i {
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strike(c)
            | Inline::Underline(c)
            | Inline::Sup(c)
            | Inline::Sub(c) => 1 + depth_of(c),
            Inline::Link { content, .. } => 1 + depth_of(content),
            Inline::Image { alt, .. } => 1 + depth_of(alt),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

fn tokens_to_inlines(toks: Vec<Tok>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for t in toks {
        match convert_leftover(t) {
            Tok::Text(s) => {
                if s.is_empty() {
                    continue;
                }
                if let Some(Inline::Text(last)) = out.last_mut() {
                    last.push_str(&s);
                } else {
                    out.push(Inline::Text(s));
                }
            }
            Tok::Node(n) => out.push(n),
            Tok::Delim { .. } | Tok::Open(_) => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// GitHub autolink literals

fn autolink_literals(inl: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::new();
    for i in inl {
        match i {
            Inline::Text(t) => split_autolinks(&t, &mut out),
            Inline::Emph(c) => out.push(Inline::Emph(autolink_literals(c))),
            Inline::Strong(c) => out.push(Inline::Strong(autolink_literals(c))),
            Inline::Strike(c) => out.push(Inline::Strike(autolink_literals(c))),
            other => out.push(other),
        }
    }
    out
}

fn split_autolinks(t: &str, out: &mut Vec<Inline>) {
    let b = t.as_bytes();
    let mut last = 0;
    let mut i = 0;
    let boundary =
        |i: usize| i == 0 || matches!(b[i - 1], b' ' | b'\t' | b'\n' | b'*' | b'_' | b'~' | b'(');
    while i < b.len() {
        if !t.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &t[i..];
        let mut url_start = None;
        if boundary(i)
            && (rest.starts_with("http://")
                || rest.starts_with("https://")
                || rest.starts_with("ftp://")
                || rest.starts_with("www."))
        {
            url_start = Some(i);
        }
        if let Some(st) = url_start {
            let mut end = st;
            while end < b.len() && !matches!(b[end], b' ' | b'\t' | b'\n' | b'<') {
                end += 1;
            }
            // trailing punctuation and unbalanced parentheses
            loop {
                let seg = &t[st..end];
                let Some(lastc) = seg.chars().next_back() else {
                    break;
                };
                if matches!(
                    lastc,
                    '?' | '!' | '.' | ',' | ':' | '*' | '_' | '~' | '\'' | '"'
                ) {
                    end -= lastc.len_utf8();
                } else if lastc == ';'
                    && seg.rfind('&').is_some_and(|p| {
                        let name = &seg[p + 1..seg.len() - 1];
                        !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric())
                    })
                {
                    end = st + seg.rfind('&').unwrap_or(seg.len());
                } else if lastc == ')' && seg.matches(')').count() > seg.matches('(').count() {
                    end -= 1;
                } else {
                    break;
                }
            }
            let url = &t[st..end];
            let host_ok = url.split("://").last().is_some_and(|h| {
                h.split(['/', '?', '#'])
                    .next()
                    .is_some_and(|h| h.contains('.') && !h.starts_with('.'))
            });
            if host_ok && end > st + 4 {
                if st > last {
                    out.push(Inline::Text(t[last..st].to_string()));
                }
                let href = if url.starts_with("www.") {
                    format!("http://{url}")
                } else {
                    url.to_string()
                };
                out.push(Inline::Link {
                    url: href,
                    title: String::new(),
                    content: vec![Inline::Text(url.to_string())],
                });
                last = end;
                i = end;
                continue;
            }
        }
        if b[i] == b'@' && i > 0 {
            let mut s = i;
            while s > 0
                && (b[s - 1].is_ascii_alphanumeric()
                    || matches!(b[s - 1], b'.' | b'_' | b'+' | b'-'))
            {
                s -= 1;
            }
            let mut e = i + 1;
            while e < b.len()
                && (b[e].is_ascii_alphanumeric() || matches!(b[e], b'.' | b'-' | b'_'))
            {
                e += 1;
            }
            while e > i + 1 && matches!(b[e - 1], b'.' | b'-' | b'_') {
                e -= 1;
            }
            let domain = &t[i + 1..e];
            if s < i
                && s >= last
                && domain.contains('.')
                && !domain.starts_with('.')
                && !t[e..].starts_with(['-', '_'])
                && domain.split('.').all(|l| !l.is_empty())
            {
                if s > last {
                    out.push(Inline::Text(t[last..s].to_string()));
                }
                let addr = &t[s..e];
                out.push(Inline::Link {
                    url: format!("mailto:{addr}"),
                    title: String::new(),
                    content: vec![Inline::Text(addr.to_string())],
                });
                last = e;
                i = e;
                continue;
            }
        }
        i += 1;
    }
    if last < t.len() {
        out.push(Inline::Text(t[last..].to_string()));
    }
}
