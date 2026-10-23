//! Markdown reading (CommonMark with GitHub tables, strikethrough, task lists and autolinks).
use crate::model::{Block, Document, Meta};

pub struct ReadOptions {
    pub github: bool,
}

pub fn read(text: &str, opts: &ReadOptions) -> Document {
    let (meta, body) = front_matter(text);
    Document {
        meta,
        blocks: crate::md_block::parse(body, opts.github),
        images: Vec::new(),
    }
}

/// A leading `---` block of `key: value` lines gives title, author, date and language.
fn front_matter(text: &str) -> (Meta, &str) {
    let mut meta = Meta::default();
    let t = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let Some(rest) = t.strip_prefix("---\n").or_else(|| t.strip_prefix("---\r\n")) else {
        return (meta, text);
    };
    let mut pos = 0;
    let mut found = None;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" || line.trim_end() == "..." {
            found = Some(pos + line.len());
            break;
        }
        pos += line.len();
    }
    let Some(end) = found else { return (meta, text) };
    let head = &rest[..pos];
    // only plain `key: value` lines count as front matter
    if head.trim().is_empty()
        || !head
            .lines()
        .all(|l| l.trim().is_empty() || l.trim_start().starts_with('#') || (l.contains(": ") || l.ends_with(':')))
    {
        return (meta, text);
    }
    for l in head.lines() {
        let Some((k, v)) = l.split_once(':') else { continue };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        if v.is_empty() {
            continue;
        }
        match k.trim().to_ascii_lowercase().as_str() {
            "title" => meta.title = Some(v),
            "author" => meta.author = Some(v),
            "subject" | "description" => meta.subject = Some(v),
            "lang" | "language" => meta.language = Some(v),
            "date" => meta.date = Some(v),
            _ => {}
        }
    }
    (meta, &rest[end..])
}

pub fn blocks_only(text: &str, github: bool) -> Vec<Block> {
    crate::md_block::parse(text, github)
}
