//! ODT (OpenDocument Text) to the document model: styles, lists, tables, pictures, notes.
use crate::imgsize;
use crate::model::{Align, Block, Cell, Document, Inline, ListItem, Meta};
use crate::xml_tree::{self, El};
use crate::zipio::Package;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};
use std::collections::HashMap;

#[derive(Clone, Default)]
struct Props {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    sup: bool,
    sub: bool,
    code: bool,
    page_break: bool,
    heading: Option<u8>,
    quote: bool,
    pre: bool,
    rule: bool,
    align: Align,
}

#[derive(Default)]
struct Styles {
    /// name -> (parent, own props)
    map: HashMap<String, (Option<String>, Props)>,
    /// list style name -> level -> (ordered, start)
    lists: HashMap<String, HashMap<usize, (bool, u32)>>,
}

fn is_mono(font: &str) -> bool {
    let f = font.to_ascii_lowercase();
    f.contains("mono") || f.contains("courier") || f.contains("consolas") || f.contains("code")
}

fn name_flags(name: &str, p: &mut Props) {
    let n = name.to_ascii_lowercase().replace("_20_", " ");
    if n.contains("preformatted") || n.contains("source code") || n == "code" {
        p.pre = true;
    }
    if n.contains("quotation") || n == "quote" || n.contains("block quote") {
        p.quote = true;
    }
    if n.contains("horizontal line") {
        p.rule = true;
    }
    if n == "pagebreak" {
        p.page_break = true;
    }
    if let Some(r) = n.strip_prefix("heading ") {
        if let Ok(l) = r.trim().parse::<u8>() {
            p.heading = Some(l.clamp(1, 6));
        }
    }
    if n == "title" {
        p.heading = Some(1);
    }
}

impl Styles {
    fn load(&mut self, root: &El) {
        for sec in root.children() {
            if !matches!(sec.name.as_str(), "styles" | "automatic-styles") {
                continue;
            }
            for s in sec.children() {
                match s.name.as_str() {
                    "style" => {
                        let Some(name) = s.attr("name") else { continue };
                        let mut p = Props::default();
                        name_flags(name, &mut p);
                        if let Some(l) = s
                            .attr("default-outline-level")
                            .and_then(|v| v.parse::<u8>().ok())
                        {
                            p.heading = Some(l.clamp(1, 6));
                        }
                        for c in s.children() {
                            match c.name.as_str() {
                                "text-properties" => {
                                    if matches!(
                                        c.attr("font-weight"),
                                        Some("bold" | "700" | "800" | "900")
                                    ) {
                                        p.bold = true;
                                    }
                                    if matches!(c.attr("font-style"), Some("italic" | "oblique")) {
                                        p.italic = true;
                                    }
                                    if c.attr("text-line-through-style")
                                        .is_some_and(|v| v != "none")
                                    {
                                        p.strike = true;
                                    }
                                    if c.attr("text-underline-style").is_some_and(|v| v != "none") {
                                        p.underline = true;
                                    }
                                    if let Some(pos) = c.attr("text-position") {
                                        let first = pos.split_whitespace().next().unwrap_or("");
                                        if first.starts_with("super") {
                                            p.sup = true;
                                        } else if first.starts_with("sub") {
                                            p.sub = true;
                                        } else if let Ok(v) =
                                            first.trim_end_matches('%').parse::<f64>()
                                        {
                                            if v > 0.0 {
                                                p.sup = true;
                                            } else if v < 0.0 {
                                                p.sub = true;
                                            }
                                        }
                                    }
                                    if c.attr("font-name").is_some_and(is_mono)
                                        || c.attr("font-family").is_some_and(is_mono)
                                    {
                                        p.code = true;
                                    }
                                }
                                "paragraph-properties" => {
                                    if c.attr("break-before") == Some("page") {
                                        p.page_break = true;
                                    }
                                    p.align = match c.attr("text-align") {
                                        Some("center") => Align::Center,
                                        Some("end" | "right") => Align::Right,
                                        Some("start" | "left") => Align::Left,
                                        _ => Align::None,
                                    };
                                }
                                _ => {}
                            }
                        }
                        let parent = s.attr("parent-style-name").map(str::to_string);
                        self.map.insert(name.to_string(), (parent, p));
                    }
                    "list-style" => {
                        let Some(name) = s.attr("name") else { continue };
                        let mut lv = HashMap::new();
                        for l in s.children() {
                            let level: usize =
                                l.attr("level").and_then(|v| v.parse().ok()).unwrap_or(1);
                            let ordered = l.name == "list-level-style-number";
                            let start = l
                                .attr("start-value")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1);
                            lv.insert(level.saturating_sub(1), (ordered, start));
                        }
                        self.lists.insert(name.to_string(), lv);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Properties of a style with its parents merged in (child wins).
    fn resolve(&self, name: &str) -> Props {
        let mut chain = Vec::new();
        let mut cur = Some(name.to_string());
        while let Some(n) = cur {
            if chain.len() > 16 {
                break;
            }
            let Some((parent, p)) = self.map.get(&n) else {
                break;
            };
            chain.push(p.clone());
            cur = parent.clone();
        }
        let mut out = Props::default();
        for p in chain.iter().rev() {
            out.bold |= p.bold;
            out.italic |= p.italic;
            out.strike |= p.strike;
            out.underline |= p.underline;
            out.sup |= p.sup;
            out.sub |= p.sub;
            out.code |= p.code;
            out.page_break |= p.page_break;
            out.quote |= p.quote;
            out.pre |= p.pre;
            out.rule |= p.rule;
            if p.heading.is_some() {
                out.heading = p.heading;
            }
            if p.align != Align::None {
                out.align = p.align.clone();
            }
        }
        out
    }
}

struct Cx<'a> {
    doc: Document,
    styles: Styles,
    pkg: &'a Package,
    notes: &'a mut Notes,
    footnotes: Vec<Vec<Block>>,
    images: HashMap<String, String>,
    skipped_images: u32,
    depth: usize,
    max_depth: usize,
}

fn wrap(p: &Props, mut inner: Vec<Inline>) -> Vec<Inline> {
    if inner.is_empty() {
        return inner;
    }
    if p.code {
        let t = crate::model::plain(&inner);
        inner = vec![Inline::Code(t)];
    }
    if p.sup {
        inner = vec![Inline::Sup(inner)];
    }
    if p.sub {
        inner = vec![Inline::Sub(inner)];
    }
    if p.underline {
        inner = vec![Inline::Underline(inner)];
    }
    if p.strike {
        inner = vec![Inline::Strike(inner)];
    }
    if p.italic {
        inner = vec![Inline::Emph(inner)];
    }
    if p.bold {
        inner = vec![Inline::Strong(inner)];
    }
    inner
}

/// Push text, merging with a previous text node.
fn push_text(out: &mut Vec<Inline>, t: &str) {
    if t.is_empty() {
        return;
    }
    if let Some(Inline::Text(p)) = out.last_mut() {
        p.push_str(t);
    } else {
        out.push(Inline::Text(t.to_string()));
    }
}

impl Cx<'_> {
    fn inlines(&mut self, el: &El, out: &mut Vec<Inline>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for k in &el.kids {
            match k {
                xml_tree::Kid::Text(t) => {
                    // ODF collapses whitespace runs; <text:s/> adds explicit spaces.
                    let mut prev_space =
                        matches!(out.last(), Some(Inline::Text(p)) if p.ends_with(' '));
                    let mut s = String::new();
                    for c in t.chars() {
                        if c.is_whitespace() {
                            if !prev_space {
                                s.push(' ');
                            }
                            prev_space = true;
                        } else {
                            s.push(c);
                            prev_space = false;
                        }
                    }
                    push_text(out, &s);
                }
                xml_tree::Kid::El(e) => self.inline_el(e, out, depth),
            }
        }
    }

    fn inline_el(&mut self, e: &El, out: &mut Vec<Inline>, depth: usize) {
        match e.name.as_str() {
            "s" => {
                let n: usize = e.attr("c").and_then(|v| v.parse().ok()).unwrap_or(1);
                push_text(out, &" ".repeat(n.min(1000)));
            }
            "tab" => push_text(out, "\t"),
            "line-break" => out.push(Inline::LineBreak),
            "span" => {
                let p = e
                    .attr("style-name")
                    .map(|n| self.styles.resolve(n))
                    .unwrap_or_default();
                let mut inner = Vec::new();
                self.inlines(e, &mut inner, depth + 1);
                out.extend(wrap(&p, inner));
            }
            "a" => {
                let mut inner = Vec::new();
                self.inlines(e, &mut inner, depth + 1);
                let inner = strip_underline(inner);
                match e.attr("href") {
                    Some(u) if !inner.is_empty() => out.push(Inline::Link {
                        url: u.to_string(),
                        title: String::new(),
                        content: inner,
                    }),
                    _ => out.extend(inner),
                }
            }
            "frame" => self.frame(e, out),
            "note" => {
                // Footnotes become a marker in the text and a list at the end.
                let body = e.child("note-body");
                let mut blocks = Vec::new();
                if let Some(b) = body {
                    self.blocks(b, &mut blocks, depth + 1);
                }
                self.footnotes.push(blocks);
                let n = self.footnotes.len();
                out.push(Inline::Sup(vec![Inline::Text(format!("[{n}]"))]));
            }
            "bookmark" | "bookmark-start" | "bookmark-end" | "soft-page-break" | "annotation"
            | "note-citation" | "sequence-decls" | "tracked-changes" | "title" | "desc" => {}
            _ => self.inlines(e, out, depth + 1),
        }
    }

    pub(crate) fn frame(&mut self, e: &El, out: &mut Vec<Inline>) {
        let alt = e
            .child("desc")
            .or_else(|| e.child("title"))
            .map(|d| d.text())
            .unwrap_or_default();
        let Some(img) = e.child("image") else {
            // a text box: keep its text
            if let Some(tb) = e.child("text-box") {
                let mut blocks = Vec::new();
                self.blocks(tb, &mut blocks, 1);
                for b in blocks {
                    if let Block::Paragraph(c) = b {
                        out.push(Inline::LineBreak);
                        out.extend(c);
                    }
                }
            }
            return;
        };
        let href = img.attr("href").unwrap_or("").to_string();
        if let Some(n) = self.images.get(&href) {
            out.push(Inline::Image {
                src: n.clone(),
                title: String::new(),
                alt: vec![Inline::Text(alt)],
            });
            return;
        }
        let data = self.pkg.get(&href).map(|d| d.to_vec()).or_else(|| {
            img.child("binary-data")
                .and_then(|b| crate::base64::decode(&b.text()))
        });
        match data
            .as_deref()
            .and_then(|d| imgsize::mime_of(d).map(|m| (m, d)))
        {
            Some((m, d)) => {
                let name = self.doc.add_image(m, d.to_vec());
                self.images.insert(href, name.clone());
                out.push(Inline::Image {
                    src: name,
                    title: String::new(),
                    alt: vec![Inline::Text(alt)],
                });
            }
            None => {
                self.skipped_images += 1;
                if !alt.is_empty() {
                    push_text(out, &alt);
                }
            }
        }
    }

    fn blocks(&mut self, parent: &El, out: &mut Vec<Block>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for e in parent.children() {
            match e.name.as_str() {
                "p" | "h" => self.paragraph(e, out, depth),
                "list" => {
                    let b = self.list(e, None, 0, depth);
                    out.extend(b);
                }
                "table" => {
                    if let Some(t) = self.table(e, depth) {
                        out.push(t);
                    }
                }
                "section" | "index-body" | "table-of-content" | "index-title" | "text-box" => {
                    self.blocks(e, out, depth + 1)
                }
                _ => {}
            }
        }
    }

    fn paragraph(&mut self, e: &El, out: &mut Vec<Block>, depth: usize) {
        let mut p = e
            .attr("style-name")
            .map(|n| self.styles.resolve(n))
            .unwrap_or_default();
        if e.name == "h" {
            let l = e
                .attr("outline-level")
                .and_then(|v| v.parse::<u8>().ok())
                .unwrap_or(1);
            p.heading = Some(l.clamp(1, 6));
        }
        if p.page_break {
            out.push(Block::PageBreak);
            if e.kids.is_empty() {
                return;
            }
        }
        if p.rule && e.kids.is_empty() {
            out.push(Block::Rule);
            return;
        }
        let mut inl = Vec::new();
        self.inlines(e, &mut inl, depth + 1);
        // trim leading/trailing collapsed spaces
        if let Some(Inline::Text(t)) = inl.first_mut() {
            *t = t.trim_start().to_string();
        }
        if let Some(Inline::Text(t)) = inl.last_mut() {
            *t = t.trim_end().to_string();
        }
        if p.pre {
            let text = crate::model::plain(&inl);
            if let Some(Block::Code { text: prev, info }) = out.last_mut() {
                if info == "\u{1}" {
                    prev.push('\n');
                    prev.push_str(&text);
                    return;
                }
            }
            // the marker info string is cleared once the block is finished (see `finish_code`)
            out.push(Block::Code {
                info: "\u{1}".into(),
                text,
            });
            return;
        }
        if inl.is_empty() {
            return;
        }
        let block = match p.heading {
            Some(level) => Block::Heading {
                level,
                content: inl,
            },
            None => Block::Paragraph(inl),
        };
        if p.quote && p.heading.is_none() {
            if let Some(Block::Quote(q)) = out.last_mut() {
                q.push(block);
                return;
            }
            out.push(Block::Quote(vec![block]));
        } else {
            out.push(block);
        }
    }

    fn list(&mut self, e: &El, inherited: Option<&str>, level: usize, depth: usize) -> Vec<Block> {
        if depth > self.max_depth {
            return vec![];
        }
        let style = e.attr("style-name").or(inherited).map(str::to_string);
        let (ordered, mut start) = style
            .as_deref()
            .and_then(|s| self.styles.lists.get(s))
            .and_then(|l| l.get(&level))
            .copied()
            .unwrap_or((false, 1));
        let mut items = Vec::new();
        let mut first = true;
        for it in e
            .children()
            .filter(|c| c.name == "list-item" || c.name == "list-header")
        {
            if first {
                if let Some(s) = it.attr("start-value").and_then(|v| v.parse().ok()) {
                    start = s;
                }
                first = false;
            }
            let mut blocks = Vec::new();
            for c in it.children() {
                match c.name.as_str() {
                    "list" => blocks.extend(self.list(c, style.as_deref(), level + 1, depth + 1)),
                    "p" | "h" => self.paragraph(c, &mut blocks, depth + 1),
                    _ => {}
                }
            }
            finish_code(&mut blocks);
            if !items.is_empty() && blocks.iter().all(|b| matches!(b, Block::List { .. })) {
                // a bare nested list belongs to the item before it
                if let Some(last) = items.last_mut() {
                    let last: &mut ListItem = last;
                    last.blocks.extend(blocks);
                    continue;
                }
            }
            items.push(ListItem {
                checked: None,
                blocks,
            });
        }
        if items.is_empty() {
            return vec![];
        }
        if items.len() == 1
            && items
                .iter()
                .all(|i| i.blocks.iter().all(|b| matches!(b, Block::List { .. })))
        {
            // a list whose only item is another list adds nothing
            return items.pop().map(|i| i.blocks).unwrap_or_default();
        }
        vec![Block::List {
            ordered,
            start,
            tight: true,
            items,
        }]
    }

    fn table(&mut self, e: &El, depth: usize) -> Option<Block> {
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let collect = |this: &mut Self, parent: &El, header: bool, rows: &mut Vec<Vec<Cell>>| {
            for r in parent.children().filter(|c| c.name == "table-row") {
                let mut cells = Vec::new();
                for c in r
                    .children()
                    .filter(|c| c.name == "table-cell" || c.name == "covered-table-cell")
                {
                    if c.name == "covered-table-cell" {
                        continue;
                    }
                    let mut blocks = Vec::new();
                    this.blocks(c, &mut blocks, depth + 1);
                    finish_code(&mut blocks);
                    let mut content = Vec::new();
                    let mut align = Align::None;
                    for b in blocks {
                        if !content.is_empty() {
                            content.push(Inline::LineBreak);
                        }
                        match b {
                            Block::Paragraph(i) | Block::Heading { content: i, .. } => {
                                content.extend(i)
                            }
                            Block::Code { text, .. } => content.push(Inline::Code(text)),
                            Block::List { items, .. } => {
                                for it in items {
                                    for b in it.blocks {
                                        if let Block::Paragraph(i) = b {
                                            content.extend(i);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(p) = c.child("p") {
                        if let Some(n) = p.attr("style-name") {
                            align = this.styles.resolve(n).align;
                        }
                    }
                    let repeat: usize = c
                        .attr("number-columns-repeated")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1)
                        .min(64);
                    let content = if header { unbold(content) } else { content };
                    for _ in 0..repeat {
                        cells.push(Cell {
                            header,
                            align: align.clone(),
                            content: content.clone(),
                        });
                    }
                }
                rows.push(cells);
            }
        };
        for c in e.children() {
            match c.name.as_str() {
                "table-header-rows" => collect(self, c, true, &mut rows),
                "table-rows" => collect(self, c, false, &mut rows),
                "table-row" => {
                    // rows directly under the table
                    let holder = El {
                        name: "x".into(),
                        attrs: vec![],
                        kids: vec![xml_tree::Kid::El(c.clone())],
                    };
                    collect(self, &holder, false, &mut rows);
                }
                _ => {}
            }
        }
        rows.retain(|r| !r.is_empty());
        if rows.is_empty() {
            return None;
        }
        // Markdown-style tables need a header row; promote the first row when none is marked.
        if !rows.iter().any(|r| r.iter().any(|c| c.header)) {
            if let Some(r) = rows.first_mut() {
                for c in r {
                    c.header = true;
                }
            }
        }
        Some(Block::Table { rows })
    }
}

fn strip_underline(inl: Vec<Inline>) -> Vec<Inline> {
    inl.into_iter()
        .flat_map(|i| match i {
            Inline::Underline(c) => strip_underline(c),
            Inline::Strong(c) => vec![Inline::Strong(strip_underline(c))],
            Inline::Emph(c) => vec![Inline::Emph(strip_underline(c))],
            o => vec![o],
        })
        .collect()
}

/// Unwrap the bold a header-cell style adds to the whole cell.
fn unbold(c: Vec<Inline>) -> Vec<Inline> {
    match <[Inline; 1]>::try_from(c) {
        Ok([Inline::Strong(inner)]) => inner,
        Ok([one]) => vec![one],
        Err(c) => c,
    }
}

/// Clear the marker that lets consecutive preformatted paragraphs join into one block.
fn finish_code(blocks: &mut [Block]) {
    for b in blocks {
        if let Block::Code { info, .. } = b {
            if info == "\u{1}" {
                info.clear();
            }
        }
    }
}

fn meta(pkg: &Package, limits: &Limits) -> Meta {
    let mut m = Meta::default();
    let Some(x) = pkg.text("meta.xml") else {
        return m;
    };
    let Ok(r) = xml_tree::parse(&x, limits) else {
        return m;
    };
    let Some(md) = r.child("meta") else { return m };
    for c in md.children() {
        let t = c.text().trim().to_string();
        if t.is_empty() {
            continue;
        }
        match c.name.as_str() {
            "title" => m.title = Some(t),
            "creator" | "initial-creator" => {
                if m.author.is_none() || c.name == "creator" {
                    m.author = Some(t)
                }
            }
            "subject" | "description" => m.subject = Some(t),
            "language" => m.language = Some(t),
            "date" | "creation-date" => {
                m.date
                    .get_or_insert_with(|| t.split('T').next().unwrap_or(&t).to_string());
            }
            _ => {}
        }
    }
    m
}

pub fn read(data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let pkg = Package::read(data, limits)?;
    let content = pkg
        .text("content.xml")
        .ok_or_else(|| Error::new(ErrorCode::CorruptFile, "this ODT has no content.xml"))?;
    let root = xml_tree::parse(&content, limits)?;
    let mut styles = Styles::default();
    if let Some(s) = pkg.text("styles.xml") {
        if let Ok(sr) = xml_tree::parse(&s, limits) {
            styles.load(&sr);
        }
    }
    styles.load(&root);
    let mut cx = Cx {
        doc: Document::default(),
        styles,
        pkg: &pkg,
        notes,
        footnotes: Vec::new(),
        images: HashMap::new(),
        skipped_images: 0,
        depth: 0,
        max_depth: (limits.max_depth as usize).clamp(8, 64),
    };
    cx.depth = 0;
    cx.doc.meta = meta(&pkg, limits);
    let body = root
        .child("body")
        .and_then(|b| b.child("text"))
        .ok_or_else(|| Error::new(ErrorCode::CorruptFile, "this ODT has no text body"))?;
    let mut blocks = Vec::new();
    cx.blocks(body, &mut blocks, 0);
    finish_code(&mut blocks);
    if !cx.footnotes.is_empty() {
        blocks.push(Block::Rule);
        let items = std::mem::take(&mut cx.footnotes)
            .into_iter()
            .map(|mut b| {
                finish_code(&mut b);
                ListItem {
                    checked: None,
                    blocks: b,
                }
            })
            .collect();
        blocks.push(Block::List {
            ordered: true,
            start: 1,
            tight: true,
            items,
        });
        cx.notes.push(Note::new(
            Severity::Info,
            "odt.footnotes",
            "footnotes were moved to a numbered list at the end",
        ));
    }
    if cx.skipped_images > 0 {
        cx.notes.push(Note::new(
            Severity::Loss,
            "odt.images_skipped",
            format!(
                "{} picture(s) could not be read and were left out",
                cx.skipped_images
            ),
        ));
    }
    cx.doc.blocks = blocks;
    Ok(cx.doc)
}

/// ODP slide decks: each slide a titled section, like the PPTX reader.
pub fn read_presentation(data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let pkg = Package::read(data, limits)?;
    let content = pkg
        .text("content.xml")
        .ok_or_else(|| Error::new(ErrorCode::CorruptFile, "this ODP has no content.xml"))?;
    let root = xml_tree::parse(&content, limits)?;
    let mut styles = Styles::default();
    if let Some(s) = pkg.text("styles.xml") {
        if let Ok(sr) = xml_tree::parse(&s, limits) {
            styles.load(&sr);
        }
    }
    styles.load(&root);
    let mut cx = Cx {
        doc: Document::default(),
        styles,
        pkg: &pkg,
        notes,
        footnotes: Vec::new(),
        images: HashMap::new(),
        skipped_images: 0,
        depth: 0,
        max_depth: (limits.max_depth as usize).clamp(8, 64),
    };
    cx.doc.meta = meta(&pkg, limits);
    let pres = root
        .child("body")
        .and_then(|b| b.child("presentation"))
        .ok_or_else(|| Error::new(ErrorCode::CorruptFile, "this ODP has no presentation body"))?;
    let mut blocks: Vec<Block> = Vec::new();
    for (n, page) in pres.children().filter(|c| c.name == "page").enumerate() {
        let mut title: Option<Vec<Inline>> = None;
        let mut body: Vec<Block> = Vec::new();
        for fr in page.children().filter(|c| c.name == "frame") {
            let class = fr.attr("class").unwrap_or("");
            let tb = fr.child("text-box");
            if class == "notes" {
                continue;
            }
            match tb {
                Some(tb) if class == "title" => {
                    let mut t = Vec::new();
                    let mut tmp = Vec::new();
                    cx.blocks(tb, &mut tmp, 1);
                    for b in tmp {
                        if let Block::Paragraph(i) | Block::Heading { content: i, .. } = b {
                            if !t.is_empty() {
                                t.push(Inline::Text(" ".into()));
                            }
                            t.extend(i);
                        }
                    }
                    if title.is_none() && !t.is_empty() {
                        title = Some(t);
                    }
                }
                Some(tb) => cx.blocks(tb, &mut body, 1),
                None if fr.child("table").is_some() => {
                    if let Some(t) = fr.child("table").and_then(|t| cx.table(t, 1)) {
                        body.push(t);
                    }
                }
                None => {
                    let mut inl = Vec::new();
                    cx.frame(fr, &mut inl);
                    if !inl.is_empty() {
                        body.push(Block::Paragraph(inl));
                    }
                }
            }
        }
        finish_code(&mut body);
        if n > 0 {
            blocks.push(Block::PageBreak);
        }
        blocks.push(Block::Heading {
            level: 1,
            content: title.unwrap_or_else(|| vec![Inline::Text(format!("Slide {}", n + 1))]),
        });
        blocks.extend(body);
    }
    cx.notes.push(Note::new(
        Severity::Info,
        "odp.text_only",
        "slides were read as text, lists, tables and pictures; layout, shapes, animations and speaker notes are not carried",
    ));
    cx.doc.blocks = blocks;
    Ok(cx.doc)
}
