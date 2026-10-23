//! A tolerant HTML reader: tokenizer, a tree builder with the usual implied end tags, and a
//! conversion of the tree to the document model. It is not the full WHATWG state machine
//! (no foster parenting, no adoption agency), which matters only for badly broken markup.
use crate::entities;
use crate::model::{Align, Block, Cell, Document, Inline, ListItem, Meta};
use oc_core::Notes;

#[derive(Debug, Clone)]
pub enum Node {
    Text(String),
    Element(Element),
}

#[derive(Debug, Clone, Default)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Element {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr", "basefont", "frame",
];
const RAW_TEXT: &[&str] = &[
    "script", "style", "textarea", "title", "xmp", "noembed", "noframes",
];

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'&' {
            let ch = s[i..].chars().next().unwrap_or('\u{FFFD}');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let rest = &s[i + 1..];
        if let Some(num) = rest.strip_prefix('#') {
            let (digits, radix, pre) = match num.strip_prefix(['x', 'X']) {
                Some(h) => (h, 16, 3),
                None => (num, 10, 2),
            };
            let n = digits
                .bytes()
                .take_while(|c| {
                    if radix == 16 {
                        c.is_ascii_hexdigit()
                    } else {
                        c.is_ascii_digit()
                    }
                })
                .count();
            if n > 0 {
                let v = u32::from_str_radix(&digits[..n], radix).unwrap_or(0xFFFD);
                let v = match v {
                    0x80..=0x9F => WIN1252[(v - 0x80) as usize],
                    0 | 0xD800..=0xDFFF | 0x110000.. => 0xFFFD,
                    v => v,
                };
                out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                i += pre + n + usize::from(digits.as_bytes().get(n) == Some(&b';'));
                continue;
            }
        } else {
            let n = rest.bytes().take_while(u8::is_ascii_alphanumeric).count();
            if n > 0 {
                let name = &rest[..n];
                if rest.as_bytes().get(n) == Some(&b';') {
                    if let Ok(k) = entities::NAMED.binary_search_by(|(k, _)| (*k).cmp(name)) {
                        out.push_str(entities::NAMED[k].1);
                        i += n + 2;
                        continue;
                    }
                }
                // legacy names work without a semicolon (longest prefix)
                let mut done = false;
                for len in (2..=n.min(6)).rev() {
                    let cand = &name[..len];
                    if entities::LEGACY.binary_search(&cand).is_ok() {
                        if let Ok(k) = entities::NAMED.binary_search_by(|(k, _)| (*k).cmp(cand)) {
                            out.push_str(entities::NAMED[k].1);
                            i += 1 + len;
                            done = true;
                            break;
                        }
                    }
                }
                if done {
                    continue;
                }
            }
        }
        out.push('&');
        i += 1;
    }
    out
}

const WIN1252: [u32; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
];

/// Elements that close an open `<p>` when they start.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "div",
    "dl",
    "fieldset",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
    "figure",
    "details",
    "menu",
];

pub fn parse_tree(html: &str, max_depth: usize) -> Vec<Node> {
    let b = html.as_bytes();
    let mut i = 0;
    // stack of open elements; index 0 is a virtual root
    let mut stack: Vec<Element> = vec![Element::default()];
    let flush_text = |stack: &mut Vec<Element>, t: &str| {
        if t.is_empty() {
            return;
        }
        if let Some(top) = stack.last_mut() {
            top.children.push(Node::Text(t.to_string()));
        }
    };
    let pop = |stack: &mut Vec<Element>| {
        if stack.len() > 1 {
            if let Some(e) = stack.pop() {
                if let Some(top) = stack.last_mut() {
                    top.children.push(Node::Element(e));
                }
            }
        }
    };
    let close_until = |stack: &mut Vec<Element>, names: &[&str], scope_stop: &[&str]| {
        // close up to and including the nearest open element with one of `names`
        let mut depth = None;
        for (k, e) in stack.iter().enumerate().skip(1).rev() {
            if names.contains(&e.tag.as_str()) {
                depth = Some(k);
                break;
            }
            if scope_stop.contains(&e.tag.as_str()) {
                break;
            }
        }
        if let Some(k) = depth {
            while stack.len() > k {
                if let Some(e) = stack.pop() {
                    if let Some(top) = stack.last_mut() {
                        top.children.push(Node::Element(e));
                    }
                }
            }
        }
    };
    let mut text_start = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &html[i..];
        // comment
        if rest.starts_with("<!--") {
            flush_text(&mut stack, &decode_entities(&html[text_start..i]));
            let end = rest[4..].find("-->").map_or(html.len(), |p| i + 4 + p + 3);
            i = end;
            text_start = i;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            flush_text(&mut stack, &decode_entities(&html[text_start..i]));
            let end = rest.find('>').map_or(html.len(), |p| i + p + 1);
            i = end;
            text_start = i;
            continue;
        }
        let closing = rest.starts_with("</");
        let name_start = if closing { 2 } else { 1 };
        let name: String = rest[name_start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() || !name.as_bytes()[0].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        flush_text(&mut stack, &decode_entities(&html[text_start..i]));
        let mut j = i + name_start + name.len();
        // attributes
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_close = false;
        loop {
            while j < b.len() && (b[j].is_ascii_whitespace() || b[j] == b'/') {
                if b[j] == b'/' {
                    self_close = true;
                }
                j += 1;
            }
            if j >= b.len() {
                break;
            }
            if b[j] == b'>' {
                j += 1;
                break;
            }
            self_close = false;
            let ns = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && !matches!(b[j], b'=' | b'>' | b'/')
            {
                j += 1;
            }
            let an = html[ns..j].to_ascii_lowercase();
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut val = String::new();
            if j < b.len() && b[j] == b'=' {
                j += 1;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                    let q = b[j];
                    j += 1;
                    let vs = j;
                    while j < b.len() && b[j] != q {
                        j += 1;
                    }
                    val = decode_entities(&html[vs..j]);
                    j = (j + 1).min(b.len());
                } else {
                    let vs = j;
                    while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                        j += 1;
                    }
                    val = decode_entities(&html[vs..j]);
                }
            }
            if !an.is_empty() && !attrs.iter().any(|(k, _)| *k == an) {
                attrs.push((an, val));
            }
        }
        i = j;
        text_start = i;
        if closing {
            let scope: &[&str] = match name.as_str() {
                "li" => &["ul", "ol"],
                "td" | "th" | "tr" => &["table"],
                "p" => &[],
                _ => &[],
            };
            close_until(&mut stack, &[name.as_str()], scope);
            continue;
        }
        // implied end tags
        if CLOSES_P.contains(&name.as_str()) {
            close_until(
                &mut stack,
                &["p"],
                &["table", "td", "th", "li", "div", "blockquote", "body"],
            );
        }
        match name.as_str() {
            "li" => close_until(&mut stack, &["li"], &["ul", "ol"]),
            "dt" | "dd" => close_until(&mut stack, &["dt", "dd"], &["dl"]),
            "td" | "th" => close_until(&mut stack, &["td", "th"], &["tr", "table"]),
            "tr" => close_until(&mut stack, &["tr"], &["table", "thead", "tbody", "tfoot"]),
            "thead" | "tbody" | "tfoot" => {
                close_until(&mut stack, &["thead", "tbody", "tfoot"], &["table"])
            }
            "option" => close_until(&mut stack, &["option"], &["select"]),
            _ => {}
        }
        let el = Element {
            tag: name.clone(),
            attrs,
            children: Vec::new(),
        };
        if VOID.contains(&name.as_str()) || self_close && !RAW_TEXT.contains(&name.as_str()) {
            if let Some(top) = stack.last_mut() {
                top.children.push(Node::Element(el));
            }
            continue;
        }
        if RAW_TEXT.contains(&name.as_str()) {
            // everything up to the matching end tag is text
            let lower = html[i..].to_ascii_lowercase();
            let close = format!("</{name}");
            let end = lower.find(&close).map_or(html.len(), |p| i + p);
            let mut el = el;
            let raw = &html[i..end];
            let t = if name == "title" || name == "textarea" {
                decode_entities(raw)
            } else {
                raw.to_string()
            };
            el.children.push(Node::Text(t));
            if let Some(top) = stack.last_mut() {
                top.children.push(Node::Element(el));
            }
            let after = html[end..].find('>').map_or(html.len(), |p| end + p + 1);
            i = after;
            text_start = i;
            continue;
        }
        if stack.len() >= max_depth {
            // too deep: treat as a flat element
            if let Some(top) = stack.last_mut() {
                top.children.push(Node::Element(el));
            }
            continue;
        }
        stack.push(el);
    }
    flush_text(
        &mut stack,
        &decode_entities(&html[text_start.min(html.len())..]),
    );
    while stack.len() > 1 {
        pop(&mut stack);
    }
    stack.pop().map(|r| r.children).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Tree to document

struct Conv<'a> {
    doc: Document,
    notes: &'a mut Notes,
    external_images: usize,
}

pub fn read(html: &str, notes: &mut Notes, max_depth: usize) -> Document {
    let tree = parse_tree(html, max_depth);
    let mut c = Conv {
        doc: Document::default(),
        notes,
        external_images: 0,
    };
    // head metadata
    find_meta(&tree, &mut c.doc.meta);
    let blocks = c.blocks(&tree);
    c.doc.blocks = blocks;
    if c.external_images > 0 {
        let n = c.external_images;
        c.notes.push(oc_core::Note::new(
            oc_core::Severity::Warning,
            "document.external_images",
            format!("{n} picture(s) refer to files or addresses that were not loaded; only embedded (data:) pictures are carried"),
        ));
    }
    c.doc
}

fn text_of(nodes: &[Node]) -> String {
    let mut s = String::new();
    for n in nodes {
        match n {
            Node::Text(t) => s.push_str(t),
            Node::Element(e) => s.push_str(&text_of(&e.children)),
        }
    }
    s
}

fn find_meta(nodes: &[Node], m: &mut Meta) {
    for n in nodes {
        if let Node::Element(e) = n {
            match e.tag.as_str() {
                "title" if m.title.is_none() => {
                    let t = collapse(&text_of(&e.children));
                    if !t.is_empty() {
                        m.title = Some(t);
                    }
                }
                "html" => {
                    if let Some(l) = e.attr("lang") {
                        m.language = Some(l.to_string());
                    }
                }
                "meta" => {
                    let name = e.attr("name").unwrap_or("").to_ascii_lowercase();
                    let content = e.attr("content").unwrap_or("").to_string();
                    if !content.is_empty() {
                        match name.as_str() {
                            "author" => m.author = Some(content),
                            "description" => m.subject = Some(content),
                            "date" | "dcterms.created" => m.date = Some(content),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            find_meta(&e.children, m);
        }
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const BLOCK_CONTAINERS: &[&str] = &[
    "div",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "main",
    "nav",
    "figure",
    "figcaption",
    "body",
    "html",
    "form",
    "fieldset",
    "center",
    "details",
    "summary",
    "address",
    "hgroup",
    "dl",
    "menu",
    "dir",
];
const SKIP: &[&str] = &[
    "script", "style", "head", "template", "noscript", "title", "meta", "link", "svg", "iframe",
    "object", "select", "button",
];

fn is_block(tag: &str) -> bool {
    BLOCK_CONTAINERS.contains(&tag)
        || matches!(
            tag,
            "p" | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "ul"
                | "ol"
                | "li"
                | "blockquote"
                | "pre"
                | "table"
                | "hr"
                | "dt"
                | "dd"
                | "tr"
                | "td"
                | "th"
                | "thead"
                | "tbody"
                | "tfoot"
                | "caption"
        )
}

impl Conv<'_> {
    fn blocks(&mut self, nodes: &[Node]) -> Vec<Block> {
        let mut out = Vec::new();
        let mut run: Vec<Inline> = Vec::new();
        self.blocks_into(nodes, &mut out, &mut run);
        flush_run(&mut run, &mut out);
        out
    }

    fn blocks_into(&mut self, nodes: &[Node], out: &mut Vec<Block>, run: &mut Vec<Inline>) {
        for n in nodes {
            match n {
                Node::Text(t) => push_text(run, t),
                Node::Element(e) => {
                    let tag = e.tag.as_str();
                    if SKIP.contains(&tag) {
                        continue;
                    }
                    if !is_block(tag) {
                        self.inline_el(e, run, &Style::default());
                        continue;
                    }
                    match tag {
                        "html" | "body" | "div" | "section" | "article" | "aside" | "header"
                        | "footer" | "main" | "nav" | "figure" | "figcaption" | "form"
                        | "fieldset" | "center" | "details" | "summary" | "address" | "hgroup"
                        | "dl" | "menu" | "dir" => {
                            flush_run(run, out);
                            self.blocks_into(&e.children, out, run);
                            flush_run(run, out);
                        }
                        "p" => {
                            flush_run(run, out);
                            let mut inl = Vec::new();
                            self.inlines(&e.children, &mut inl, &Style::default());
                            // a page break style on the paragraph
                            if e.attr("style").is_some_and(|s| {
                                s.contains("page-break-before") && s.contains("always")
                            }) {
                                out.push(Block::PageBreak);
                            }
                            let inl = trim_inlines(inl);
                            if !inl.is_empty() {
                                out.push(Block::Paragraph(inl));
                            }
                        }
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            flush_run(run, out);
                            let mut inl = Vec::new();
                            self.inlines(&e.children, &mut inl, &Style::default());
                            out.push(Block::Heading {
                                level: tag.as_bytes()[1] - b'0',
                                content: trim_inlines(inl),
                            });
                        }
                        "blockquote" => {
                            flush_run(run, out);
                            let inner = self.blocks(&e.children);
                            out.push(Block::Quote(inner));
                        }
                        "pre" => {
                            flush_run(run, out);
                            let (info, text) = pre_text(e);
                            out.push(Block::Code { info, text });
                        }
                        "hr" => {
                            flush_run(run, out);
                            out.push(Block::Rule);
                        }
                        "ul" | "ol" => {
                            flush_run(run, out);
                            out.push(self.list(e));
                        }
                        "li" | "dt" | "dd" => {
                            // stray item outside a list: keep its text
                            flush_run(run, out);
                            let inner = self.blocks(&e.children);
                            out.extend(inner);
                        }
                        "table" => {
                            flush_run(run, out);
                            if let Some(t) = self.table(e) {
                                out.push(t);
                            }
                        }
                        _ => {
                            flush_run(run, out);
                            self.blocks_into(&e.children, out, run);
                            flush_run(run, out);
                        }
                    }
                }
            }
        }
    }

    fn list(&mut self, e: &Element) -> Block {
        let ordered = e.tag == "ol";
        let start = e
            .attr("start")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        let mut items = Vec::new();
        let mut tight = true;
        for n in &e.children {
            if let Node::Element(li) = n {
                if li.tag == "li" {
                    if li
                        .children
                        .iter()
                        .any(|c| matches!(c, Node::Element(p) if p.tag == "p"))
                    {
                        tight = false;
                    }
                    let mut checked = None;
                    // task list checkbox
                    let mut kids = li.children.clone();
                    if let Some(Node::Element(first)) = kids
                        .iter()
                        .find(|c| !matches!(c, Node::Text(t) if t.trim().is_empty()))
                    {
                        if first.tag == "input" && first.attr("type") == Some("checkbox") {
                            checked = Some(first.attr("checked").is_some());
                            let pos = kids
                                .iter()
                                .position(|c| matches!(c, Node::Element(x) if x.tag == "input"))
                                .unwrap_or(0);
                            kids.remove(pos);
                        }
                    }
                    let blocks = self.blocks(&kids);
                    items.push(ListItem { checked, blocks });
                } else if matches!(li.tag.as_str(), "ul" | "ol") {
                    // a list directly inside a list: attach to the previous item
                    let nested = self.list(li);
                    if let Some(last) = items.last_mut() {
                        let last: &mut ListItem = last;
                        last.blocks.push(nested);
                    } else {
                        items.push(ListItem {
                            checked: None,
                            blocks: vec![nested],
                        });
                    }
                }
            }
        }
        Block::List {
            ordered,
            start,
            tight,
            items,
        }
    }

    fn table(&mut self, e: &Element) -> Option<Block> {
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let mut spans = false;
        fn collect<'a>(e: &'a Element, trs: &mut Vec<&'a Element>) {
            for n in &e.children {
                if let Node::Element(c) = n {
                    match c.tag.as_str() {
                        "tr" => trs.push(c),
                        "thead" | "tbody" | "tfoot" => collect(c, trs),
                        _ => {}
                    }
                }
            }
        }
        let mut trs = Vec::new();
        collect(e, &mut trs);
        for tr in trs {
            let mut row = Vec::new();
            for n in &tr.children {
                if let Node::Element(td) = n {
                    if td.tag == "td" || td.tag == "th" {
                        if td.attr("colspan").is_some_and(|c| c != "1")
                            || td.attr("rowspan").is_some_and(|c| c != "1")
                        {
                            spans = true;
                        }
                        let blocks = self.blocks(&td.children);
                        let mut content = Vec::new();
                        for (k, b) in blocks.iter().enumerate() {
                            if k > 0 {
                                content.push(Inline::LineBreak);
                            }
                            block_inlines(b, &mut content);
                        }
                        let align = match td.attr("align").map(str::to_ascii_lowercase).as_deref() {
                            Some("left") => Align::Left,
                            Some("center") => Align::Center,
                            Some("right") => Align::Right,
                            _ => Align::None,
                        };
                        row.push(Cell {
                            header: td.tag == "th",
                            align,
                            content,
                        });
                    }
                }
            }
            if !row.is_empty() {
                rows.push(row);
            }
        }
        if spans {
            self.notes.push(oc_core::Note::new(
                oc_core::Severity::Warning,
                "document.table_spans",
                "merged table cells (colspan, rowspan) were not kept; each cell stands alone",
            ));
        }
        if rows.is_empty() {
            None
        } else {
            Some(Block::Table { rows })
        }
    }

    fn inlines(&mut self, nodes: &[Node], out: &mut Vec<Inline>, st: &Style) {
        for n in nodes {
            match n {
                Node::Text(t) => push_text_styled(out, t, st),
                Node::Element(e) => {
                    if SKIP.contains(&e.tag.as_str()) {
                        continue;
                    }
                    self.inline_el(e, out, st);
                }
            }
        }
    }

    fn inline_el(&mut self, e: &Element, out: &mut Vec<Inline>, st: &Style) {
        let mut inner_style = st.clone();
        if let Some(s) = e.attr("style") {
            inner_style.apply_css(s);
        }
        let mut kids = Vec::new();
        match e.tag.as_str() {
            "br" => out.push(Inline::LineBreak),
            "img" => {
                let src = e.attr("src").unwrap_or("");
                let alt = e.attr("alt").unwrap_or("").to_string();
                let title = e.attr("title").unwrap_or("").to_string();
                let alt = if alt.is_empty() {
                    vec![]
                } else {
                    vec![Inline::Text(alt)]
                };
                if let Some((mime, data)) = crate::base64::parse_data_uri(src) {
                    if mime.starts_with("image/") {
                        let name = self.doc.add_image(&mime, data);
                        out.push(Inline::Image {
                            src: name,
                            title,
                            alt,
                        });
                        return;
                    }
                }
                if !src.is_empty() {
                    self.external_images += 1;
                    out.push(Inline::Image {
                        src: src.to_string(),
                        title,
                        alt,
                    });
                }
            }
            "a" => {
                self.inlines(&e.children, &mut kids, &inner_style);
                match e.attr("href") {
                    Some(h) if !h.trim().is_empty() => out.push(Inline::Link {
                        url: h.trim().to_string(),
                        title: e.attr("title").unwrap_or("").to_string(),
                        content: kids,
                    }),
                    _ => out.extend(kids),
                }
            }
            "em" | "i" | "cite" | "var" | "dfn" => {
                self.inlines(&e.children, &mut kids, &inner_style);
                push_wrapped(out, kids, Inline::Emph);
            }
            "strong" | "b" => {
                self.inlines(&e.children, &mut kids, &inner_style);
                push_wrapped(out, kids, Inline::Strong);
            }
            "del" | "s" | "strike" => {
                self.inlines(&e.children, &mut kids, &inner_style);
                push_wrapped(out, kids, Inline::Strike);
            }
            "u" | "ins" => {
                self.inlines(&e.children, &mut kids, &inner_style);
                push_wrapped(out, kids, Inline::Underline);
            }
            "sup" => {
                self.inlines(&e.children, &mut kids, &inner_style);
                push_wrapped(out, kids, Inline::Sup);
            }
            "sub" => {
                self.inlines(&e.children, &mut kids, &inner_style);
                push_wrapped(out, kids, Inline::Sub);
            }
            "code" | "kbd" | "samp" | "tt" => {
                let t = text_of(&e.children);
                if !t.is_empty() {
                    out.push(Inline::Code(collapse_keep(&t)));
                }
            }
            "input" => {
                if e.attr("type") == Some("checkbox") {
                    out.push(Inline::Text(
                        if e.attr("checked").is_some() {
                            "[x] "
                        } else {
                            "[ ] "
                        }
                        .into(),
                    ));
                }
            }
            _ => {
                // span, font, small, mark and the rest: just their content, with CSS emphasis
                self.inlines(&e.children, &mut kids, &inner_style);
                let mut node = kids;
                if inner_style.bold && !st.bold {
                    node = vec![Inline::Strong(node)];
                }
                if inner_style.italic && !st.italic {
                    node = vec![Inline::Emph(node)];
                }
                if inner_style.underline && !st.underline {
                    node = vec![Inline::Underline(node)];
                }
                if inner_style.strike && !st.strike {
                    node = vec![Inline::Strike(node)];
                }
                if !node.is_empty() {
                    out.extend(node);
                }
            }
        }
    }
}

#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

impl Style {
    fn apply_css(&mut self, css: &str) {
        for decl in css.split(';') {
            let Some((k, v)) = decl.split_once(':') else {
                continue;
            };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase());
            match k.as_str() {
                "font-weight" => {
                    self.bold =
                        v == "bold" || v == "bolder" || v.parse::<u32>().is_ok_and(|n| n >= 600)
                }
                "font-style" => self.italic = v == "italic" || v == "oblique",
                "text-decoration" | "text-decoration-line" => {
                    if v.contains("underline") {
                        self.underline = true;
                    }
                    if v.contains("line-through") {
                        self.strike = true;
                    }
                }
                _ => {}
            }
        }
    }
}

fn push_wrapped(out: &mut Vec<Inline>, kids: Vec<Inline>, f: fn(Vec<Inline>) -> Inline) {
    if !kids.is_empty() {
        out.push(f(kids));
    }
}

fn collapse_keep(s: &str) -> String {
    s.replace('\n', " ")
}

/// Whitespace collapsing as in HTML: runs of whitespace become one space.
fn push_text(run: &mut Vec<Inline>, t: &str) {
    push_text_styled(run, t, &Style::default());
}

fn push_text_styled(out: &mut Vec<Inline>, t: &str, st: &Style) {
    let mut s = String::new();
    let mut ws = false;
    for c in t.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            ws = true;
        } else {
            if ws {
                s.push(' ');
                ws = false;
            }
            s.push(c);
        }
    }
    if ws {
        s.push(' ');
    }
    if s.is_empty() {
        return;
    }
    let mut node = Inline::Text(s);
    if st.strike {
        node = Inline::Strike(vec![node]);
    }
    if st.underline {
        node = Inline::Underline(vec![node]);
    }
    if st.italic {
        node = Inline::Emph(vec![node]);
    }
    if st.bold {
        node = Inline::Strong(vec![node]);
    }
    // merge adjacent plain text
    if let (Some(Inline::Text(prev)), Inline::Text(new)) = (out.last_mut(), &node) {
        if !(prev.ends_with(' ') && new.starts_with(' ')) {
            prev.push_str(new);
        } else {
            prev.push_str(&new[1..]);
        }
        return;
    }
    // collapse a space following a space across node boundaries
    if let Inline::Text(new) = &mut node {
        if new.starts_with(' ') && ends_with_space(out.last()) {
            new.remove(0);
            if new.is_empty() {
                return;
            }
        }
    }
    out.push(node);
}

fn ends_with_space(i: Option<&Inline>) -> bool {
    match i {
        Some(Inline::Text(t)) => t.ends_with(' '),
        Some(Inline::Emph(c) | Inline::Strong(c) | Inline::Strike(c) | Inline::Underline(c)) => {
            ends_with_space(c.last())
        }
        Some(Inline::LineBreak | Inline::SoftBreak) => true,
        _ => false,
    }
}

/// Remove leading and trailing spaces of an inline run.
fn trim_inlines(mut v: Vec<Inline>) -> Vec<Inline> {
    fn trim_front(v: &mut Vec<Inline>) {
        loop {
            match v.first_mut() {
                Some(Inline::Text(t)) => {
                    let n = t.trim_start_matches(' ').to_string();
                    if n.is_empty() {
                        v.remove(0);
                        continue;
                    }
                    *t = n;
                    break;
                }
                Some(
                    Inline::Emph(c) | Inline::Strong(c) | Inline::Strike(c) | Inline::Underline(c),
                ) => {
                    trim_front(c);
                    if c.is_empty() {
                        v.remove(0);
                        continue;
                    }
                    break;
                }
                Some(Inline::LineBreak) => {
                    // a break at the start of a paragraph is meaningless
                    v.remove(0);
                }
                _ => break,
            }
        }
    }
    fn trim_back(v: &mut Vec<Inline>) {
        loop {
            match v.last_mut() {
                Some(Inline::Text(t)) => {
                    let n = t.trim_end_matches(' ').to_string();
                    if n.is_empty() {
                        v.pop();
                        continue;
                    }
                    *t = n;
                    break;
                }
                Some(
                    Inline::Emph(c) | Inline::Strong(c) | Inline::Strike(c) | Inline::Underline(c),
                ) => {
                    trim_back(c);
                    if c.is_empty() {
                        v.pop();
                        continue;
                    }
                    break;
                }
                Some(Inline::LineBreak) => {
                    v.pop();
                }
                _ => break,
            }
        }
    }
    trim_front(&mut v);
    trim_back(&mut v);
    v
}

fn flush_run(run: &mut Vec<Inline>, out: &mut Vec<Block>) {
    let v = trim_inlines(std::mem::take(run));
    if !v.is_empty() {
        out.push(Block::Paragraph(v));
    }
}

fn block_inlines(b: &Block, out: &mut Vec<Inline>) {
    match b {
        Block::Paragraph(c) | Block::Heading { content: c, .. } => out.extend(c.iter().cloned()),
        Block::Code { text, .. } => out.push(Inline::Text(text.trim_end().to_string())),
        Block::Quote(bs) => {
            for (k, x) in bs.iter().enumerate() {
                if k > 0 {
                    out.push(Inline::LineBreak);
                }
                block_inlines(x, out);
            }
        }
        Block::List { items, .. } => {
            for (k, it) in items.iter().enumerate() {
                if k > 0 {
                    out.push(Inline::LineBreak);
                }
                out.push(Inline::Text("• ".into()));
                for x in &it.blocks {
                    block_inlines(x, out);
                }
            }
        }
        _ => {}
    }
}

fn pre_text(e: &Element) -> (String, String) {
    let mut info = String::new();
    let mut inner = e;
    if let [Node::Element(c)] = e.children.as_slice() {
        if c.tag == "code" {
            if let Some(cls) = c.attr("class") {
                for w in cls.split_whitespace() {
                    if let Some(l) = w.strip_prefix("language-") {
                        info = l.to_string();
                    }
                }
            }
            inner = c;
        }
    }
    let mut t = text_of(&inner.children);
    // a newline right after <pre> is dropped by HTML
    if let Some(r) = t.strip_prefix('\n') {
        t = r.to_string();
    }
    if !t.ends_with('\n') {
        t.push('\n');
    }
    (info, t)
}
