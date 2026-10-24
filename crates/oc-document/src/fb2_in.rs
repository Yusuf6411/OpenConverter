//! FictionBook 2 (FB2) to the document model.
use crate::model::{Block, Document, Inline, ListItem};
use crate::xml_tree::{self, El, Kid};
use oc_core::{Limits, Note, Notes, Result, Severity};
use std::collections::HashMap;

struct Cx<'a> {
    doc: Document,
    binaries: HashMap<String, (String, Vec<u8>)>,
    used: HashMap<String, String>,
    notes: &'a mut Notes,
    max_depth: usize,
}

impl Cx<'_> {
    fn inlines(&mut self, e: &El, out: &mut Vec<Inline>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for k in &e.kids {
            match k {
                Kid::Text(t) => {
                    let s: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
                    if s.is_empty() {
                        if !t.is_empty()
                            && !matches!(out.last(), None | Some(Inline::Text(_)) if false)
                        {
                            if let Some(Inline::Text(p)) = out.last_mut() {
                                if !p.ends_with(' ') {
                                    p.push(' ');
                                }
                            }
                        }
                        continue;
                    }
                    let lead = t.starts_with(char::is_whitespace);
                    let trail = t.ends_with(char::is_whitespace);
                    let mut s = s;
                    if lead && !out.is_empty() {
                        s.insert(0, ' ');
                    }
                    if trail {
                        s.push(' ');
                    }
                    match out.last_mut() {
                        Some(Inline::Text(p)) => {
                            if p.ends_with(' ') && s.starts_with(' ') {
                                s.remove(0);
                            }
                            p.push_str(&s)
                        }
                        _ => out.push(Inline::Text(s)),
                    }
                }
                Kid::El(c) => self.inline_el(c, out, depth),
            }
        }
    }

    fn inline_el(&mut self, e: &El, out: &mut Vec<Inline>, depth: usize) {
        let mut kids = Vec::new();
        match e.name.as_str() {
            "emphasis" => {
                self.inlines(e, &mut kids, depth + 1);
                out.push(Inline::Emph(kids));
            }
            "strong" => {
                self.inlines(e, &mut kids, depth + 1);
                out.push(Inline::Strong(kids));
            }
            "strikethrough" => {
                self.inlines(e, &mut kids, depth + 1);
                out.push(Inline::Strike(kids));
            }
            "sub" => {
                self.inlines(e, &mut kids, depth + 1);
                out.push(Inline::Sub(kids));
            }
            "sup" => {
                self.inlines(e, &mut kids, depth + 1);
                out.push(Inline::Sup(kids));
            }
            "code" => out.push(Inline::Code(e.text())),
            "a" => {
                self.inlines(e, &mut kids, depth + 1);
                let href = e.attr("href").unwrap_or("");
                if e.attr("type") == Some("note") || href.starts_with('#') {
                    out.push(Inline::Sup(kids));
                } else if !href.is_empty() {
                    out.push(Inline::Link {
                        url: href.to_string(),
                        title: String::new(),
                        content: kids,
                    });
                } else {
                    out.extend(kids);
                }
            }
            "image" => self.image(e, out),
            _ => self.inlines(e, out, depth + 1),
        }
    }

    fn image(&mut self, e: &El, out: &mut Vec<Inline>) {
        let id = e
            .attr("href")
            .unwrap_or("")
            .trim_start_matches('#')
            .to_string();
        if let Some(n) = self.used.get(&id) {
            out.push(Inline::Image {
                src: n.clone(),
                title: String::new(),
                alt: vec![],
            });
            return;
        }
        if let Some((_, data)) = self.binaries.get(&id) {
            if let Some(m) = crate::imgsize::mime_of(data) {
                let n = self.doc.add_image(m, data.clone());
                self.used.insert(id, n.clone());
                out.push(Inline::Image {
                    src: n,
                    title: String::new(),
                    alt: vec![],
                });
            }
        }
    }

    fn block_children(&mut self, e: &El, level: u8, out: &mut Vec<Block>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for c in e.children() {
            match c.name.as_str() {
                "title" => {
                    let mut t = Vec::new();
                    for p in c.children() {
                        if !t.is_empty() {
                            t.push(Inline::LineBreak);
                        }
                        self.inlines(p, &mut t, depth + 1);
                    }
                    if !t.is_empty() {
                        out.push(Block::Heading {
                            level: level.clamp(1, 6),
                            content: t,
                        });
                    }
                }
                "subtitle" => {
                    let mut t = Vec::new();
                    self.inlines(c, &mut t, depth + 1);
                    out.push(Block::Heading {
                        level: (level + 1).clamp(1, 6),
                        content: t,
                    });
                }
                "p" => {
                    let mut t = Vec::new();
                    self.inlines(c, &mut t, depth + 1);
                    if let Some(Inline::Text(s)) = t.first_mut() {
                        *s = s.trim_start().to_string();
                    }
                    if let Some(Inline::Text(s)) = t.last_mut() {
                        *s = s.trim_end().to_string();
                    }
                    if !t.is_empty() {
                        out.push(Block::Paragraph(t));
                    }
                }
                "empty-line" => {}
                "image" => {
                    let mut t = Vec::new();
                    self.image(c, &mut t);
                    if !t.is_empty() {
                        out.push(Block::Paragraph(t));
                    }
                }
                "epigraph" | "cite" | "annotation" => {
                    let mut inner = Vec::new();
                    self.block_children(c, level + 1, &mut inner, depth + 1);
                    out.push(Block::Quote(inner));
                }
                "poem" => {
                    let mut lines = Vec::new();
                    for st in c.children() {
                        match st.name.as_str() {
                            "title" => {
                                let mut inner = Vec::new();
                                self.block_children(
                                    &El {
                                        name: "x".into(),
                                        attrs: vec![],
                                        kids: vec![Kid::El(st.clone())],
                                    },
                                    level + 1,
                                    &mut inner,
                                    depth + 1,
                                );
                                out.extend(inner);
                            }
                            "stanza" => {
                                if !lines.is_empty() {
                                    lines.push(Inline::LineBreak);
                                    lines.push(Inline::LineBreak);
                                }
                                let mut first = true;
                                for v in st.children().filter(|v| v.name == "v") {
                                    if !first {
                                        lines.push(Inline::LineBreak);
                                    }
                                    first = false;
                                    self.inlines(v, &mut lines, depth + 1);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !lines.is_empty() {
                        out.push(Block::Quote(vec![Block::Paragraph(lines)]));
                    }
                }
                "section" => {
                    let before = out.len();
                    self.block_children(c, level + 1, out, depth + 1);
                    let _ = before;
                }
                "table" => {}
                _ => {}
            }
        }
    }
}

pub fn read(xml: &str, limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let root = xml_tree::parse(xml, limits)?;
    let mut cx = Cx {
        doc: Document::default(),
        binaries: HashMap::new(),
        used: HashMap::new(),
        notes,
        max_depth: (limits.max_depth as usize).clamp(8, 64),
    };
    for b in root.children().filter(|c| c.name == "binary") {
        if let Some(id) = b.attr("id") {
            if let Some(d) = crate::base64::decode(&b.text().split_whitespace().collect::<String>())
            {
                cx.binaries.insert(
                    id.to_string(),
                    (b.attr("content-type").unwrap_or("").to_string(), d),
                );
            }
        }
    }
    if let Some(ti) = root
        .child("description")
        .and_then(|d| d.child("title-info"))
    {
        let t = |n: &str| {
            ti.child(n)
                .map(|e| e.text().split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|s| !s.is_empty())
        };
        cx.doc.meta.title = t("book-title");
        cx.doc.meta.language = t("lang");
        cx.doc.meta.date = t("date");
        if let Some(a) = ti.child("author") {
            let parts: Vec<String> = ["first-name", "middle-name", "last-name"]
                .iter()
                .filter_map(|n| a.child(n))
                .map(|e| e.text().trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !parts.is_empty() {
                cx.doc.meta.author = Some(parts.join(" "));
            }
        }
        if let Some(an) = ti.child("annotation") {
            let s = an.text().split_whitespace().collect::<Vec<_>>().join(" ");
            if !s.is_empty() {
                cx.doc.meta.subject = Some(s);
            }
        }
    }
    let mut blocks = Vec::new();
    let mut note_items: Vec<ListItem> = Vec::new();
    for body in root.children().filter(|c| c.name == "body") {
        if body.attr("name") == Some("notes") {
            for sec in body.children().filter(|c| c.name == "section") {
                let mut inner = Vec::new();
                cx.block_children(sec, 2, &mut inner, 0);
                inner.retain(|b| !matches!(b, Block::Heading { .. }));
                note_items.push(ListItem {
                    checked: None,
                    blocks: inner,
                });
            }
        } else {
            cx.block_children(body, 1, &mut blocks, 0);
        }
    }
    if !note_items.is_empty() {
        blocks.push(Block::Rule);
        blocks.push(Block::List {
            ordered: true,
            start: 1,
            tight: true,
            items: note_items,
        });
        cx.notes.push(Note::new(
            Severity::Info,
            "fb2.notes",
            "notes were moved to a numbered list at the end",
        ));
    }
    cx.doc.blocks = blocks;
    Ok(cx.doc)
}
