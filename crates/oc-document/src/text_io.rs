//! Plain text: reading paragraphs, and writing a document without markup.
use crate::model::{plain, Block, Cell, Document, Inline, ListItem};

/// Blank-line separated paragraphs; line breaks inside a paragraph are kept.
pub fn read(text: &str) -> Document {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut doc = Document::default();
    let mut cur: Vec<&str> = Vec::new();
    let flush = |cur: &mut Vec<&str>, doc: &mut Document| {
        if cur.is_empty() {
            return;
        }
        let mut inl = Vec::new();
        for (i, l) in cur.iter().enumerate() {
            if i > 0 {
                inl.push(Inline::LineBreak);
            }
            inl.push(Inline::Text(l.trim_end().to_string()));
        }
        doc.blocks.push(Block::Paragraph(inl));
        cur.clear();
    };
    for l in t.split('\n') {
        if l.trim().is_empty() {
            flush(&mut cur, &mut doc);
        } else {
            cur.push(l);
        }
    }
    flush(&mut cur, &mut doc);
    doc
}

pub fn write(doc: &Document) -> String {
    let mut out = String::new();
    if let Some(t) = &doc.meta.title {
        let has_h1 = matches!(doc.blocks.first(), Some(Block::Heading { .. }));
        if !has_h1 {
            out.push_str(t);
            out.push_str("\n\n");
        }
    }
    out.push_str(blocks(&doc.blocks, "").trim_end_matches('\n'));
    out.push('\n');
    out
}

fn blocks(bs: &[Block], indent: &str) -> String {
    let mut parts = Vec::new();
    for b in bs {
        let s = match b {
            Block::Heading { content, .. } => inline_text(content),
            Block::Paragraph(c) => inline_text(c),
            Block::Quote(c) => blocks(c, "")
                .trim_end_matches('\n')
                .lines()
                .map(|l| format!("> {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
            Block::List {
                ordered,
                start,
                tight,
                items,
            } => list(*ordered, *start, *tight, items),
            Block::Code { text, .. } => text
                .trim_end_matches('\n')
                .lines()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Rule => "-----".to_string(),
            Block::Html(_) | Block::PageBreak => continue,
            Block::Table { rows } => table(rows),
        };
        parts.push(s);
    }
    let body = parts.join("\n\n");
    if indent.is_empty() {
        body
    } else {
        body.lines()
            .map(|l| format!("{indent}{l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn list(ordered: bool, start: u32, tight: bool, items: &[ListItem]) -> String {
    let mut out = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let mk = if ordered {
            format!("{}. ", start.saturating_add(i as u32))
        } else {
            "- ".to_string()
        };
        let task = match it.checked {
            Some(true) => "[x] ",
            Some(false) => "[ ] ",
            None => "",
        };
        let body = blocks(&it.blocks, "");
        let pad = " ".repeat(mk.len());
        let mut s = String::new();
        for (k, l) in body.lines().enumerate() {
            if k == 0 {
                s.push_str(&mk);
                s.push_str(task);
            } else if !l.is_empty() {
                s.push_str(&pad);
            }
            s.push_str(l);
            s.push('\n');
        }
        if body.is_empty() {
            s = format!("{}\n", mk.trim_end());
        }
        out.push(s.trim_end_matches('\n').to_string());
    }
    out.join(if tight { "\n" } else { "\n\n" })
}

fn table(rows: &[Vec<Cell>]) -> String {
    let text: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|c| inline_text(&c.content).replace('\n', " "))
                .collect()
        })
        .collect();
    let cols = text.iter().map(Vec::len).max().unwrap_or(0);
    let mut width = vec![0usize; cols];
    for r in &text {
        for (i, c) in r.iter().enumerate() {
            width[i] = width[i].max(c.chars().count());
        }
    }
    let mut lines = Vec::new();
    for (n, r) in text.iter().enumerate() {
        let cells: Vec<String> = (0..cols)
            .map(|i| {
                let c = r.get(i).map(String::as_str).unwrap_or("");
                format!("{c}{}", " ".repeat(width[i] - c.chars().count()))
            })
            .collect();
        lines.push(cells.join(" | ").trim_end().to_string());
        if n == 0 && rows.first().is_some_and(|r| r.iter().any(|c| c.header)) {
            lines.push(
                width
                    .iter()
                    .map(|w| "-".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("-+-"),
            );
        }
    }
    lines.join("\n")
}

pub fn inline_text(is: &[Inline]) -> String {
    let mut s = String::new();
    for i in is {
        match i {
            Inline::Text(t) | Inline::Code(t) => s.push_str(t),
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strike(c)
            | Inline::Underline(c)
            | Inline::Sup(c)
            | Inline::Sub(c) => s.push_str(&inline_text(c)),
            Inline::Link { url, content, .. } => {
                let t = inline_text(content);
                if t == *url || url.starts_with('#') || url.is_empty() {
                    s.push_str(&t);
                } else {
                    s.push_str(&format!("{t} ({url})"));
                }
            }
            Inline::Image { alt, .. } => {
                let a = plain(alt);
                s.push_str(&if a.is_empty() {
                    "[image]".to_string()
                } else {
                    format!("[{a}]")
                });
            }
            Inline::SoftBreak => s.push(' '),
            Inline::LineBreak => s.push('\n'),
            Inline::Html(_) => {}
        }
    }
    s
}
