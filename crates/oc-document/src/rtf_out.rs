//! Document to RTF 1.9 (Word-compatible subset): styles, lists, tables, links, PNG and JPEG pictures.
use crate::imgsize;
use crate::model::{plain, Align, Block, Document, Inline, ListItem};
use oc_core::{Note, Notes, Severity};
use std::fmt::Write;

struct Out<'a> {
    doc: &'a Document,
    s: String,
    list_id: i32,
    lists: Vec<(i32, bool, u32)>,
    dropped: u32,
}

fn esc(t: &str, out: &mut String) {
    for c in t.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\t' => out.push_str("\\tab "),
            '\n' => out.push_str("\\line "),
            '\r' => {}
            c if (c as u32) < 0x80 && (c as u32) >= 0x20 => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => {
                let mut b = [0u16; 2];
                for u in c.encode_utf16(&mut b) {
                    let _ = write!(out, "\\u{}?", *u as i16);
                }
            }
        }
    }
}

#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    ul: bool,
    strike: bool,
    sup: bool,
    sub: bool,
    code: bool,
}

impl Out<'_> {
    fn inlines(&mut self, c: &[Inline], st: &Style) {
        for i in c {
            self.inline(i, st);
        }
    }

    fn run(&mut self, t: &str, st: &Style) {
        let mut open = String::new();
        if st.bold {
            open.push_str("\\b");
        }
        if st.italic {
            open.push_str("\\i");
        }
        if st.ul {
            open.push_str("\\ul");
        }
        if st.strike {
            open.push_str("\\strike");
        }
        if st.sup {
            open.push_str("\\super");
        }
        if st.sub {
            open.push_str("\\sub");
        }
        if st.code {
            open.push_str("\\f1");
        }
        if open.is_empty() {
            esc(t, &mut self.s);
        } else {
            self.s.push('{');
            self.s.push_str(&open);
            self.s.push(' ');
            esc(t, &mut self.s);
            self.s.push('}');
        }
    }

    fn inline(&mut self, i: &Inline, st: &Style) {
        match i {
            Inline::Text(t) => self.run(t, st),
            Inline::Emph(c) => self.inlines(c, &Style { italic: true, ..st.clone() }),
            Inline::Strong(c) => self.inlines(c, &Style { bold: true, ..st.clone() }),
            Inline::Strike(c) => self.inlines(c, &Style { strike: true, ..st.clone() }),
            Inline::Underline(c) => self.inlines(c, &Style { ul: true, ..st.clone() }),
            Inline::Sup(c) => self.inlines(c, &Style { sup: true, sub: false, ..st.clone() }),
            Inline::Sub(c) => self.inlines(c, &Style { sub: true, sup: false, ..st.clone() }),
            Inline::Code(t) => self.run(t, &Style { code: true, ..st.clone() }),
            Inline::Link { url, content, .. } => {
                self.s.push_str("{\\field{\\*\\fldinst{HYPERLINK \"");
                esc(&url.replace('"', "%22"), &mut self.s);
                self.s.push_str("\"}}{\\fldrslt{\\cf1\\ul ");
                self.inlines(content, st);
                self.s.push_str("}}}");
            }
            Inline::Image { src, alt, .. } => {
                let pic = self.doc.image(src).and_then(|img| {
                    let kind = match imgsize::mime_of(&img.data) {
                        Some("image/png") => "pngblip",
                        Some("image/jpeg") => "jpegblip",
                        _ => return None,
                    };
                    Some((img, kind))
                });
                match pic {
                    Some((img, kind)) => {
                        let (w, h) = imgsize::size(&img.data).unwrap_or((100, 100));
                        let (mut gw, mut gh) = (i64::from(w) * 15, i64::from(h) * 15);
                        if gw > 8640 {
                            gh = gh * 8640 / gw.max(1);
                            gw = 8640;
                        }
                        let _ = write!(
                            self.s,
                            "{{\\pict\\{kind}\\picw{w}\\pich{h}\\picwgoal{gw}\\pichgoal{gh}\n"
                        );
                        for (n, b) in img.data.iter().enumerate() {
                            let _ = write!(self.s, "{b:02x}");
                            if n % 64 == 63 {
                                self.s.push('\n');
                            }
                        }
                        self.s.push_str("}");
                    }
                    None => {
                        self.dropped += 1;
                        let a = plain(alt);
                        if !a.is_empty() {
                            self.run(&a, st);
                        }
                    }
                }
            }
            Inline::SoftBreak => self.s.push(' '),
            Inline::LineBreak => self.s.push_str("\\line "),
            Inline::Html(_) => {}
        }
    }

    fn block(&mut self, b: &Block, indent: i32) {
        match b {
            Block::Heading { level, content } => {
                let l = (*level).clamp(1, 6);
                let fs = [40, 34, 30, 26, 24, 22][usize::from(l) - 1];
                let _ = write!(
                    self.s,
                    "\\pard\\keepn\\sb280\\sa120\\outlinelevel{}\\f2\\fs{fs}{{\\b ",
                    l - 1
                );
                self.inlines(content, &Style { bold: false, ..Style::default() });
                self.s.push_str("}\\par\n");
            }
            Block::Paragraph(c) => {
                let _ = write!(self.s, "\\pard\\li{indent}\\sa160\\sl276\\slmult1\\f0\\fs24 ");
                self.inlines(c, &Style::default());
                self.s.push_str("\\par\n");
            }
            Block::Quote(c) => {
                for b in c {
                    self.block(b, indent + 720);
                }
            }
            Block::List { ordered, start, items, .. } => {
                self.list_id += 1;
                let id = self.list_id;
                self.list(*ordered, *start, items, 0, id, indent);
            }
            Block::Code { text, .. } => {
                for l in text.trim_end_matches('\n').split('\n') {
                    let _ = write!(self.s, "\\pard\\li{indent}\\sa0\\f1\\fs20 ");
                    esc(l, &mut self.s);
                    self.s.push_str("\\par\n");
                }
                self.s.push_str("\\pard\\sa120\\par\n");
            }
            Block::Rule => {
                self.s.push_str("\\pard\\sa120\\brdrb\\brdrs\\brdrw10\\brsp20 \\par\n");
            }
            Block::PageBreak => self.s.push_str("\\pard\\page\\par\n"),
            Block::Html(_) => {}
            Block::Table { rows } => {
                let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1) as i32;
                let w = 9000 / cols;
                for r in rows {
                    self.s.push_str("\\trowd\\trgaph108\\trleft0");
                    for k in 0..cols {
                        let _ = write!(
                            self.s,
                            "\\clbrdrt\\brdrs\\brdrw10\\clbrdrl\\brdrs\\brdrw10\\clbrdrb\\brdrs\\brdrw10\\clbrdrr\\brdrs\\brdrw10\\cellx{}",
                            w * (k + 1)
                        );
                    }
                    self.s.push('\n');
                    for k in 0..cols as usize {
                        let cell = r.get(k);
                        let al = match cell.map(|c| &c.align) {
                            Some(Align::Center) => "\\qc",
                            Some(Align::Right) => "\\qr",
                            _ => "\\ql",
                        };
                        let _ = write!(self.s, "\\pard\\intbl{al}\\f0\\fs22 ");
                        if let Some(c) = cell {
                            let st = Style { bold: c.header, ..Style::default() };
                            self.inlines(&c.content, &st);
                        }
                        self.s.push_str("\\cell\n");
                    }
                    self.s.push_str("\\row\n");
                }
                self.s.push_str("\\pard\\sa120\\par\n");
            }
        }
    }

    fn list(&mut self, ordered: bool, start: u32, items: &[ListItem], lvl: i32, id: i32, indent: i32) {
        self.lists.push((id, ordered, start));
        for (n, it) in items.iter().enumerate() {
            let marker = if ordered {
                format!("{}.", start as usize + n)
            } else if lvl % 2 == 0 {
                "\u{2022}".to_string()
            } else {
                "\u{25e6}".to_string()
            };
            let mut first = true;
            for b in &it.blocks {
                match b {
                    Block::List { ordered, start, items, .. } => {
                        self.list_id += 1;
                        let nid = self.list_id;
                        self.list(*ordered, *start, items, lvl + 1, nid, indent);
                    }
                    Block::Paragraph(c) | Block::Heading { content: c, .. } => {
                        let li = indent + 720 * (lvl + 1);
                        let _ = write!(
                            self.s,
                            "\\pard\\li{li}\\fi-360\\sa60\\f0\\fs24\\ls{id}\\ilvl{lvl} "
                        );
                        if first {
                            self.s.push_str("{\\listtext ");
                            let mut m = String::new();
                            esc(&marker, &mut m);
                            self.s.push_str(&m);
                            self.s.push_str("\\tab}");
                            let check = match it.checked {
                                Some(true) => "\u{2611} ",
                                Some(false) => "\u{2610} ",
                                None => "",
                            };
                            esc(check, &mut self.s);
                        }
                        self.inlines(c, &Style::default());
                        self.s.push_str("\\par\n");
                        first = false;
                    }
                    other => self.block(other, indent + 720 * (lvl + 1)),
                }
            }
        }
    }
}

fn list_tables(lists: &[(i32, bool, u32)]) -> String {
    let mut t = String::from("{\\*\\listtable");
    for (id, ordered, start) in lists {
        let _ = write!(t, "{{\\list\\listtemplateid{id}\\listsimple0");
        for lvl in 0..9 {
            let li = 720 * (lvl + 1);
            if *ordered {
                let _ = write!(
                    t,
                    "{{\\listlevel\\levelnfc0\\leveljc0\\levelstartat{start}\\levelfollow0\\levelspace0\\levelindent0{{\\leveltext\\'02\\'0{lvl}.;}}{{\\levelnumbers\\'01;}}\\fi-360\\li{li}}}"
                );
            } else {
                let _ = write!(
                    t,
                    "{{\\listlevel\\levelnfc23\\leveljc0\\levelstartat1\\levelfollow0\\levelspace0\\levelindent0{{\\leveltext\\'01\\u8226 ?;}}{{\\levelnumbers;}}\\f0\\fi-360\\li{li}}}"
                );
            }
        }
        let _ = write!(t, "\\listid{id}}}");
    }
    t.push_str("}\n{\\*\\listoverridetable");
    for (id, _, _) in lists {
        let _ = write!(t, "{{\\listoverride\\listid{id}\\listoverridecount0\\ls{id}}}");
    }
    t.push_str("}\n");
    t
}

pub fn write(doc: &Document, notes: &mut Notes) -> Vec<u8> {
    let mut o = Out {
        doc,
        s: String::new(),
        list_id: 0,
        lists: Vec::new(),
        dropped: 0,
    };
    for b in &doc.blocks {
        o.block(b, 0);
    }
    let mut head = String::new();
    head.push_str("{\\rtf1\\ansi\\ansicpg1252\\deff0\\uc1\n");
    head.push_str("{\\fonttbl{\\f0\\froman\\fcharset0 Times New Roman;}{\\f1\\fmodern\\fcharset0 Courier New;}{\\f2\\fswiss\\fcharset0 Arial;}}\n");
    head.push_str("{\\colortbl;\\red5\\green99\\blue193;}\n");
    if !o.lists.is_empty() {
        head.push_str(&list_tables(&o.lists));
    }
    let m = &doc.meta;
    if m.title.is_some() || m.author.is_some() || m.subject.is_some() {
        head.push_str("{\\info");
        for (k, v) in [("title", &m.title), ("author", &m.author), ("subject", &m.subject)] {
            if let Some(v) = v {
                let _ = write!(head, "{{\\{k} ");
                esc(v, &mut head);
                head.push('}');
            }
        }
        if let Some(d) = &m.date {
            let p: Vec<u32> = d.split('-').filter_map(|x| x.parse().ok()).collect();
            if let [y, mo, dy] = p.as_slice() {
                let _ = write!(head, "{{\\creatim\\yr{y}\\mo{mo}\\dy{dy}}}");
            }
        }
        head.push_str("}\n");
    }
    head.push_str("\\paperw11906\\paperh16838\\margl1417\\margr1417\\margt1417\\margb1417\\widowctrl\n");
    head.push_str(&o.s);
    head.push_str("}\n");
    if o.dropped > 0 {
        notes.push(Note::new(
            Severity::Loss,
            "rtf.images_dropped",
            format!("{} picture(s) other than PNG or JPEG were left out", o.dropped),
        ));
    }
    head.into_bytes()
}
