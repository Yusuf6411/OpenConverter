//! RTF to the document model: runs, paragraphs, lists, tables, pictures, fields.
use crate::build::Builder;
use crate::imgsize;
use crate::model::{Align, Block, Cell, Document, Inline, ListItem};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dest {
    Normal,
    Skip,
    FontTbl,
    Info,
    InfoItem(u8),
    Pict,
    FldInst,
    FldRslt,
    ListText,
    Style,
}

#[derive(Clone)]
struct St {
    bold: bool,
    italic: bool,
    ul: bool,
    strike: bool,
    sup: bool,
    sub: bool,
    font: i32,
    /// Characters to skip after a `\u` escape.
    uc: usize,
    dest: Dest,
}

#[derive(Default)]
struct Par {
    outline: Option<u8>,
    ls: Option<i32>,
    ilvl: usize,
    indent: i32,
    rule: bool,
    in_tbl: bool,
    align: Align,
    list_text: String,
    has_listtext: bool,
    style: Option<i32>,
}

#[derive(Default)]
struct Pict {
    hex: String,
    kind: Option<&'static str>,
    other: bool,
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
    stack: Vec<St>,
    cur: St,
    fonts: HashMap<i32, bool>,
    font_def: Option<i32>,
    style_def: Option<i32>,
    styles: HashMap<i32, String>,
    cp: oc_text::Encoding,
    cp_num: u32,
    pending: Vec<u8>,
    doc: Document,
    builder: Builder,
    par: Par,
    inl: Vec<Inline>,
    mono_only: bool,
    any_text: bool,
    rows: Vec<Vec<Cell>>,
    cells: Vec<Cell>,
    field_url: Option<String>,
    field_start: Vec<usize>,
    pict: Pict,
    info_text: String,
    notes: &'a mut Notes,
    pict_loss: u32,
    after: Vec<Block>,
    max_depth: usize,
    hi: Option<u32>,
}

fn wrap(s: &St, mono: bool, mut inner: Vec<Inline>) -> Vec<Inline> {
    if mono {
        let t = crate::model::plain(&inner);
        inner = vec![Inline::Code(t)];
    }
    if s.sup {
        inner = vec![Inline::Sup(inner)];
    }
    if s.sub {
        inner = vec![Inline::Sub(inner)];
    }
    if s.ul {
        inner = vec![Inline::Underline(inner)];
    }
    if s.strike {
        inner = vec![Inline::Strike(inner)];
    }
    if s.italic {
        inner = vec![Inline::Emph(inner)];
    }
    if s.bold {
        inner = vec![Inline::Strong(inner)];
    }
    inner
}

fn is_mono_name(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    n.contains("courier") || n.contains("mono") || n.contains("consolas") || n.contains("menlo")
}

impl P<'_> {
    fn flush_bytes(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.pending);
        let (t, _) = oc_text::decode(&bytes, self.cp, oc_text::Policy::Replace)
            .unwrap_or_else(|_| (String::from("\u{fffd}"), Default::default()));
        self.text(&t);
    }

    fn text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        match self.cur.dest {
            Dest::Normal | Dest::FldRslt => {
                let mono = self.fonts.get(&self.cur.font).copied().unwrap_or(false);
                if t.chars().any(|c| !c.is_whitespace()) {
                    if !mono {
                        self.mono_only = false;
                    }
                    self.any_text = true;
                }
                let run = wrap(&self.cur, mono, vec![Inline::Text(t.to_string())]);
                self.inl.extend(run);
            }
            Dest::ListText => self.par.list_text.push_str(t),
            Dest::FldInst => {
                let u = self.field_url.get_or_insert_with(String::new);
                u.push_str(t);
            }
            Dest::InfoItem(_) => self.info_text.push_str(t),
            Dest::FontTbl | Dest::Info | Dest::Skip | Dest::Pict | Dest::Style => {}
        }
    }

    fn merge_runs(&mut self) {
        // Merge neighbouring styled runs so `**a****b**` becomes `**ab**`.
        let mut out: Vec<Inline> = Vec::with_capacity(self.inl.len());
        for i in std::mem::take(&mut self.inl) {
            match (out.last_mut(), i) {
                (Some(Inline::Text(a)), Inline::Text(b)) => a.push_str(&b),
                (Some(Inline::Strong(a)), Inline::Strong(b)) => a.extend(b),
                (Some(Inline::Emph(a)), Inline::Emph(b)) => a.extend(b),
                (Some(Inline::Code(a)), Inline::Code(b)) => a.push_str(&b),
                (_, i) => out.push(i),
            }
        }
        // merge again one level down
        for o in &mut out {
            match o {
                Inline::Strong(v) | Inline::Emph(v) => {
                    let inner = std::mem::take(v);
                    let mut keep: Vec<Inline> = Vec::new();
                    for i in inner {
                        match (keep.last_mut(), i) {
                            (Some(Inline::Text(a)), Inline::Text(b)) => a.push_str(&b),
                            (Some(Inline::Emph(a)), Inline::Emph(b)) => a.extend(b),
                            (Some(Inline::Strong(a)), Inline::Strong(b)) => a.extend(b),
                            (_, i) => keep.push(i),
                        }
                    }
                    *v = keep;
                }
                _ => {}
            }
        }
        self.inl = out;
    }

    fn end_paragraph(&mut self) {
        self.flush_bytes();
        self.merge_runs();
        let mut inl = std::mem::take(&mut self.inl);
        let mut par = std::mem::take(&mut self.par);
        if let Some(name) = par.style.and_then(|n| self.styles.get(&n)) {
            let n = name.to_ascii_lowercase();
            if par.outline.is_none() {
                if let Some(l) = n.strip_prefix("heading ").and_then(|r| r.trim().parse::<u8>().ok()) {
                    par.outline = Some(l.clamp(1, 6) - 1);
                } else if n == "title" {
                    par.outline = Some(0);
                }
            }
            if n.contains("quote") || n.contains("block text") || n.contains("quotation") {
                par.indent = par.indent.max(720);
            }
        }
        let mono_only = std::mem::replace(&mut self.mono_only, true);
        let any = std::mem::replace(&mut self.any_text, false);
        if let Some(Inline::Text(t)) = inl.first_mut() {
            *t = t.trim_start().to_string();
        }
        if let Some(Inline::Text(t)) = inl.last_mut() {
            *t = t.trim_end().to_string();
        }
        if par.in_tbl {
            // table cell paragraphs are collected by \cell
            self.inl = inl;
            self.par = Par { in_tbl: true, ..Par::default() };
            return;
        }
        self.end_table();
        let block_after = std::mem::take(&mut self.after);
        if par.rule && inl.is_empty() {
            self.builder.push(Block::Rule);
        } else if inl.is_empty() {
            // an empty paragraph only separates
        } else if par.has_listtext || par.ls.is_some_and(|l| l > 0) {
            let ordered = par.list_text.trim().chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
            let start = par
                .list_text
                .trim()
                .trim_end_matches(['.', ')'])
                .parse::<u32>()
                .unwrap_or(1);
            let key = format!("{}", par.ls.unwrap_or(0));
            let block = match par.outline {
                Some(l) => Block::Heading { level: l + 1, content: inl },
                None => Block::Paragraph(inl),
            };
            self.builder.list_item(
                par.ilvl,
                &key,
                ordered,
                start,
                ListItem { checked: None, blocks: vec![block] },
            );
        } else if let Some(l) = par.outline {
            let inl = match <[Inline; 1]>::try_from(inl) {
                Ok([Inline::Strong(i)]) => i,
                Ok([o]) => vec![o],
                Err(i) => i,
            };
            self.builder.push(Block::Heading { level: (l + 1).min(6), content: inl });
        } else if mono_only && any {
            let text = crate::model::plain(&inl);
            self.builder.close_lists();
            self.builder.flush_quote();
            match &mut self.builder.code {
                Some(c) => {
                    c.push('\n');
                    c.push_str(&text);
                }
                None => self.builder.code = Some(text),
            }
        } else if par.indent >= 720 {
            self.builder.close_lists();
            self.builder.flush_code();
            self.builder.quote.push(Block::Paragraph(inl));
        } else {
            self.builder.push(Block::Paragraph(inl));
        }
        for b in block_after {
            self.builder.push(b);
        }
    }

    fn end_cell(&mut self) {
        self.flush_bytes();
        self.merge_runs();
        let mut content = std::mem::take(&mut self.inl);
        let par = std::mem::take(&mut self.par);
        self.par = Par { in_tbl: true, ..Par::default() };
        self.mono_only = true;
        self.any_text = false;
        if let Some(Inline::Text(t)) = content.first_mut() {
            *t = t.trim_start().to_string();
        }
        if let Some(Inline::Text(t)) = content.last_mut() {
            *t = t.trim_end().to_string();
        }
        self.cells.push(Cell {
            header: false,
            align: par.align,
            content,
        });
    }

    fn end_row(&mut self) {
        let cells = std::mem::take(&mut self.cells);
        if !cells.is_empty() {
            self.rows.push(cells);
        }
    }

    fn end_table(&mut self) {
        let mut rows = std::mem::take(&mut self.rows);
        if rows.is_empty() {
            return;
        }
        // the first row is the header when every cell in it is bold, or by convention
        let bold_row = rows.first().is_some_and(|r| {
            r.iter().all(|c| matches!(c.content.as_slice(), [Inline::Strong(_)] | []))
        });
        if let Some(r) = rows.first_mut() {
            for c in r.iter_mut() {
                c.header = true;
                if bold_row {
                    if let [Inline::Strong(i)] = c.content.as_slice() {
                        c.content = i.clone();
                    }
                }
            }
        }
        self.builder.push(Block::Table { rows });
    }

    fn control(&mut self, word: &str, arg: Option<i32>) {
        // pending hex bytes end on any control word other than \'
        self.flush_bytes();
        let on = arg != Some(0);
        match word {
            "b" => self.cur.bold = on,
            "i" => self.cur.italic = on,
            "ul" | "uld" | "uldb" | "ulw" => self.cur.ul = on,
            "ulnone" => self.cur.ul = false,
            "strike" | "striked" => self.cur.strike = on,
            "super" => {
                self.cur.sup = on;
                self.cur.sub = false;
            }
            "sub" => {
                self.cur.sub = on;
                self.cur.sup = false;
            }
            "nosupersub" => {
                self.cur.sup = false;
                self.cur.sub = false;
            }
            "plain" => {
                self.cur.bold = false;
                self.cur.italic = false;
                self.cur.ul = false;
                self.cur.strike = false;
                self.cur.sup = false;
                self.cur.sub = false;
            }
            "f" => {
                if let Some(n) = arg {
                    if self.cur.dest == Dest::FontTbl {
                        self.font_def = Some(n);
                    } else {
                        self.cur.font = n;
                    }
                }
            }
            "fmodern" => {
                if let Some(f) = self.font_def {
                    self.fonts.insert(f, true);
                }
            }
            "uc" => self.cur.uc = arg.unwrap_or(1).clamp(0, 8) as usize,
            "ansicpg" => {
                if let Some(n) = arg {
                    self.cp_num = n as u32;
                    if let Some(e) = oc_text::Encoding::from_name(&format!("windows-{n}")) {
                        self.cp = e;
                    } else if n != 1252 {
                        self.notes.push(Note::new(
                            Severity::Warning,
                            "rtf.codepage",
                            format!("code page {n} is not supported; accented text may be wrong"),
                        ));
                    }
                }
            }
            "pard" => {
                self.par = Par::default();
            }
            "intbl" => self.par.in_tbl = true,
            "outlinelevel" => self.par.outline = arg.map(|n| n.clamp(0, 5) as u8),
            "ls" => self.par.ls = arg,
            "ilvl" => self.par.ilvl = arg.unwrap_or(0).clamp(0, 8) as usize,
            "li" => self.par.indent = arg.unwrap_or(0),
            "brdrb" => self.par.rule = true,
            "qc" => self.par.align = Align::Center,
            "qr" => self.par.align = Align::Right,
            "ql" => self.par.align = Align::Left,
            "par" | "sect" => {
                if self.cur.dest == Dest::Normal {
                    self.end_paragraph();
                }
            }
            "line" => {
                if matches!(self.cur.dest, Dest::Normal | Dest::FldRslt) {
                    self.inl.push(Inline::LineBreak);
                }
            }
            "tab" => self.text("\t"),
            "emdash" => self.text("\u{2014}"),
            "endash" => self.text("\u{2013}"),
            "bullet" => self.text("\u{2022}"),
            "lquote" => self.text("\u{2018}"),
            "rquote" => self.text("\u{2019}"),
            "ldblquote" => self.text("\u{201c}"),
            "rdblquote" => self.text("\u{201d}"),
            "emspace" | "enspace" | "qmspace" => self.text(" "),
            "page" => {
                if self.cur.dest == Dest::Normal {
                    self.after.push(Block::PageBreak);
                }
            }
            "cell" => {
                if self.cur.dest == Dest::Normal {
                    self.end_cell();
                }
            }
            "row" => {
                if self.cur.dest == Dest::Normal {
                    self.end_row();
                }
            }
            "pngblip" => self.pict.kind = Some("image/png"),
            "jpegblip" => self.pict.kind = Some("image/jpeg"),
            "emfblip" | "wmetafile" | "macpict" | "dibitmap" | "wbitmap" => self.pict.other = true,
            "fonttbl" => self.cur.dest = Dest::FontTbl,
            "stylesheet" => self.cur.dest = Dest::Style,
            "s" => {
                if self.cur.dest == Dest::Style {
                    self.style_def = arg;
                } else {
                    self.par.style = arg;
                }
            }
            "colortbl" | "listtable" | "listoverridetable" | "rsidtbl" | "header"
            | "footer" | "headerl" | "headerr" | "footerl" | "footerr" | "headerf" | "footerf"
            | "private" | "generator" | "xmlnstbl" | "themedata" | "colorschememapping"
            | "datastore" | "latentstyles" | "pnseclvl" | "falt" | "fldtype" | "bkmkstart"
            | "bkmkend" | "object" | "footnote" | "annotation" => self.cur.dest = Dest::Skip,
            "info" => self.cur.dest = Dest::Info,
            "title" if self.cur.dest == Dest::Info => self.cur.dest = Dest::InfoItem(0),
            "author" if self.cur.dest == Dest::Info => self.cur.dest = Dest::InfoItem(1),
            "subject" if self.cur.dest == Dest::Info => self.cur.dest = Dest::InfoItem(2),
            "creatim" if self.cur.dest == Dest::Info => self.cur.dest = Dest::InfoItem(3),
            "pict" => {
                self.cur.dest = Dest::Pict;
                self.pict = Pict::default();
            }
            "fldinst" => {
                self.cur.dest = Dest::FldInst;
                self.field_url = None;
            }
            "fldrslt" => {
                self.cur.dest = Dest::FldRslt;
                self.field_start.push(self.inl.len());
            }
            "listtext" | "pntext" => {
                self.cur.dest = Dest::ListText;
                self.par.has_listtext = true;
                self.par.list_text.clear();
            }
            "yr" | "mo" | "dy" if matches!(self.cur.dest, Dest::InfoItem(3)) => {
                if let Some(n) = arg {
                    self.info_text.push_str(&format!("{n:02}-"));
                }
            }
            _ => {}
        }
    }

    fn end_group(&mut self, closing: &St) {
        match closing.dest {
            Dest::FontTbl => {}
            Dest::InfoItem(k) => {
                let t = std::mem::take(&mut self.info_text).trim().trim_end_matches('-').to_string();
                let t = if k == 3 && t.starts_with("00") { String::new() } else { t };
                if !t.is_empty() {
                    match k {
                        0 => self.doc.meta.title = Some(t),
                        1 => self.doc.meta.author = Some(t),
                        2 => self.doc.meta.subject = Some(t),
                        _ => self.doc.meta.date = Some(t),
                    }
                }
            }
            Dest::Pict => {
                let hex = std::mem::take(&mut self.pict.hex);
                let mut data = Vec::with_capacity(hex.len() / 2);
                let hb = hex.as_bytes();
                let mut k = 0;
                while k + 1 < hb.len() {
                    let h = |c: u8| (c as char).to_digit(16);
                    if let (Some(a), Some(b)) = (hb.get(k).and_then(|c| h(*c)), hb.get(k + 1).and_then(|c| h(*c))) {
                        data.push((a * 16 + b) as u8);
                    }
                    k += 2;
                }
                match imgsize::mime_of(&data) {
                    Some(m) if self.pict.kind.is_some() || !self.pict.other => {
                        let name = self.doc.add_image(m, data);
                        self.inl.push(Inline::Image {
                            src: name,
                            title: String::new(),
                            alt: vec![],
                        });
                    }
                    _ => self.pict_loss += 1,
                }
            }
            Dest::FldRslt => {
                if let Some(start) = self.field_start.pop() {
                    self.flush_bytes();
                    let url = self
                        .field_url
                        .as_deref()
                        .map(str::trim)
                        .and_then(|f| f.strip_prefix("HYPERLINK").or_else(|| f.strip_prefix("hyperlink")))
                        .map(|u| u.trim().trim_matches('"').to_string())
                        .filter(|u| !u.is_empty() && !u.starts_with("\\l"));
                    if let Some(url) = url {
                        let content: Vec<Inline> = self.inl.drain(start.min(self.inl.len())..).collect();
                        let content = strip_ul(content);
                        if !content.is_empty() {
                            self.inl.push(Inline::Link {
                                url,
                                title: String::new(),
                                content,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn run(&mut self) {
        let n = self.b.len();
        while self.i < n {
            let c = self.b[self.i];
            match c {
                b'{' => {
                    self.flush_bytes();
                    self.i += 1;
                    if self.stack.len() < 256 && self.stack.len() < self.max_depth * 4 {
                        self.stack.push(self.cur.clone());
                    } else {
                        // too deep: treat the contents as skipped
                        self.stack.push(self.cur.clone());
                        self.cur.dest = Dest::Skip;
                    }
                    // `{\*\x` starts an ignorable destination
                    if self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'*') {
                        self.i += 2;
                        // keep fldinst, listtext and pntext; everything else is skipped
                        let rest = &self.b[self.i..];
                        let known = [&b"\\fldinst"[..], b"\\listtext", b"\\pntext", b"\\shppict", b"\\nonshppict"];
                        if !known.iter().any(|k| rest.starts_with(k)) {
                            self.cur.dest = Dest::Skip;
                        } else if rest.starts_with(b"\\nonshppict") {
                            self.cur.dest = Dest::Skip;
                        }
                    }
                }
                b'}' => {
                    self.flush_bytes();
                    self.i += 1;
                    let closing = self.cur.clone();
                    self.end_group(&closing);
                    if let Some(prev) = self.stack.pop() {
                        // a font definition group ends here
                        if closing.dest == Dest::FontTbl || self.font_def.is_some() {
                            // handled at text time
                        }
                        self.cur = prev;
                    }
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&d) = self.b.get(self.i) else { break };
                    match d {
                        b'\\' | b'{' | b'}' => {
                            self.flush_bytes();
                            self.i += 1;
                            self.text(&(d as char).to_string());
                        }
                        b'\'' => {
                            self.i += 1;
                            let h = |o: usize| self.b.get(self.i + o).and_then(|c| (*c as char).to_digit(16));
                            if let (Some(a), Some(b)) = (h(0), h(1)) {
                                if matches!(self.cur.dest, Dest::Normal | Dest::FldRslt | Dest::ListText | Dest::FldInst | Dest::InfoItem(_) | Dest::Style) {
                                    self.pending.push((a * 16 + b) as u8);
                                }
                                self.i += 2;
                            }
                        }
                        b'~' => {
                            self.i += 1;
                            self.text("\u{a0}");
                        }
                        b'-' | b'_' => {
                            self.i += 1;
                            self.text(if d == b'-' { "\u{ad}" } else { "\u{2011}" });
                        }
                        b'\n' | b'\r' => {
                            self.i += 1;
                            self.control("par", None);
                        }
                        b'*' => {
                            self.i += 1;
                        }
                        c if c.is_ascii_alphabetic() => {
                            let s = self.i;
                            while self.i < n && self.b[self.i].is_ascii_alphabetic() {
                                self.i += 1;
                            }
                            let word = String::from_utf8_lossy(&self.b[s..self.i]).into_owned();
                            let mut arg = None;
                            let neg = self.b.get(self.i) == Some(&b'-');
                            let ds = if neg { self.i + 1 } else { self.i };
                            let mut e = ds;
                            while e < n && self.b[e].is_ascii_digit() {
                                e += 1;
                            }
                            if e > ds {
                                let v: i64 = String::from_utf8_lossy(&self.b[ds..e]).parse().unwrap_or(0);
                                arg = Some(if neg { -v } else { v }.clamp(-1_000_000_000, 1_000_000_000) as i32);
                                self.i = e;
                            }
                            if self.b.get(self.i) == Some(&b' ') {
                                self.i += 1;
                            }
                            if word == "u" {
                                self.flush_bytes();
                                if let Some(v) = arg {
                                    let cu = (v as i64).rem_euclid(65536) as u32;
                                    self.unicode(cu);
                                }
                                // skip the fallback characters
                                let mut skip = self.cur.uc;
                                while skip > 0 && self.i < n {
                                    match self.b[self.i] {
                                        b'\\' if self.b.get(self.i + 1) == Some(&b'\'') => self.i += 4,
                                        b'\\' | b'{' | b'}' => break,
                                        _ => self.i += 1,
                                    }
                                    skip -= 1;
                                }
                            } else {
                                self.control(&word, arg);
                            }
                        }
                        _ => {
                            self.i += 1;
                        }
                    }
                }
                b'\r' | b'\n' => self.i += 1,
                _ => {
                    // plain text up to the next special character
                    let s = self.i;
                    while self.i < n && !matches!(self.b[self.i], b'{' | b'}' | b'\\' | b'\r' | b'\n') {
                        self.i += 1;
                    }
                    let chunk = &self.b[s..self.i];
                    match self.cur.dest {
                        Dest::Pict => {
                            self.pict
                                .hex
                                .extend(chunk.iter().filter(|c| c.is_ascii_hexdigit()).map(|c| *c as char));
                        }
                        Dest::FontTbl => {
                            let name = String::from_utf8_lossy(chunk);
                            if let Some(f) = self.font_def {
                                let mono = is_mono_name(&name);
                                let e = self.fonts.entry(f).or_insert(false);
                                *e |= mono;
                            }
                        }
                        Dest::Style => {
                            if let Some(n) = self.style_def {
                                let name = String::from_utf8_lossy(chunk);
                                let name = name.trim().trim_end_matches(';').trim();
                                if !name.is_empty() {
                                    self.styles.insert(n, name.to_string());
                                }
                            }
                        }
                        Dest::Skip | Dest::Info => {}
                        _ => {
                            if self.cur.dest == Dest::Normal || self.cur.dest == Dest::FldRslt || self.cur.dest == Dest::ListText || self.cur.dest == Dest::FldInst || matches!(self.cur.dest, Dest::InfoItem(_)) {
                                // bytes above 0x7f in a non-UTF-8 stream are in the code page
                                if chunk.iter().all(|c| *c < 0x80) {
                                    self.flush_bytes();
                                    let t = String::from_utf8_lossy(chunk).into_owned();
                                    self.text(&t);
                                } else {
                                    self.pending.extend_from_slice(chunk);
                                }
                            }
                        }
                    }
                }
            }
        }
        self.flush_bytes();
    }

    fn unicode(&mut self, cu: u32) {
        // surrogate pairs arrive as two \u escapes
        if (0xD800..0xDC00).contains(&cu) {
            self.hi = Some(cu);
            return;
        }
        let ch = if (0xDC00..0xE000).contains(&cu) {
            match self.hi.take() {
                Some(h) => char::from_u32(0x10000 + ((h - 0xD800) << 10) + (cu - 0xDC00)),
                None => None,
            }
        } else {
            char::from_u32(cu)
        };
        let s = ch.unwrap_or('\u{fffd}').to_string();
        self.text(&s);
    }
}

fn strip_ul(c: Vec<Inline>) -> Vec<Inline> {
    c.into_iter()
        .flat_map(|i| match i {
            Inline::Underline(i) => strip_ul(i),
            Inline::Strong(i) => vec![Inline::Strong(strip_ul(i))],
            Inline::Emph(i) => vec![Inline::Emph(strip_ul(i))],
            o => vec![o],
        })
        .collect()
}

pub fn read(data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let head = data.iter().take(32).copied().collect::<Vec<u8>>();
    if !head.windows(5).any(|w| w == b"{\\rtf") {
        return Err(Error::new(ErrorCode::CorruptFile, "this is not an RTF file"));
    }
    let base = St {
        bold: false,
        italic: false,
        ul: false,
        strike: false,
        sup: false,
        sub: false,
        font: 0,
        uc: 1,
        dest: Dest::Normal,
    };
    let mut p = P {
        b: data,
        i: 0,
        stack: Vec::new(),
        cur: base,
        fonts: HashMap::new(),
        font_def: None,
        style_def: None,
        styles: HashMap::new(),
        cp: oc_text::Encoding::Windows1252,
        cp_num: 1252,
        pending: Vec::new(),
        doc: Document::default(),
        builder: Builder::default(),
        par: Par::default(),
        inl: Vec::new(),
        mono_only: true,
        any_text: false,
        rows: Vec::new(),
        cells: Vec::new(),
        field_url: None,
        field_start: Vec::new(),
        pict: Pict::default(),
        info_text: String::new(),
        notes,
        pict_loss: 0,
        after: Vec::new(),
        max_depth: (limits.max_depth as usize).clamp(8, 64),
        hi: None,
    };
    p.run();
    if !p.inl.is_empty() {
        p.end_paragraph();
    }
    p.end_table();
    let pict_loss = p.pict_loss;
    let b = std::mem::take(&mut p.builder);
    p.doc.blocks = b.finish();
    if pict_loss > 0 {
        p.notes.push(Note::new(
            Severity::Loss,
            "rtf.images_skipped",
            format!("{pict_loss} picture(s) in a format other than PNG or JPEG were left out"),
        ));
    }
    Ok(p.doc)
}
