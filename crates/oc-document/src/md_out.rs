//! Document to Markdown (CommonMark with GitHub tables, strikethrough and task lists).
use crate::model::{plain, Align, Block, Document, Inline};

pub struct WriteOptions<'a> {
    /// Maps a document picture name to what goes inside `![](...)`.
    pub image_src: &'a dyn Fn(&str) -> String,
}

pub fn write(doc: &Document, opts: &WriteOptions) -> String {
    let mut out = String::new();
    if doc.meta.title.is_some() || doc.meta.author.is_some() {
        out.push_str("---\n");
        for (k, v) in [
            ("title", &doc.meta.title),
            ("author", &doc.meta.author),
            ("date", &doc.meta.date),
            ("lang", &doc.meta.language),
        ] {
            if let Some(v) = v {
                out.push_str(&format!(
                    "{k}: \"{}\"\n",
                    v.replace('\\', "\\\\").replace('"', "\\\"")
                ));
            }
        }
        out.push_str("---\n\n");
    }
    let body = blocks(&doc.blocks, opts, 0);
    out.push_str(body.trim_end_matches('\n'));
    out.push('\n');
    out
}

fn blocks(bs: &[Block], o: &WriteOptions, depth: usize) -> String {
    blocks_sep(bs, o, depth, "\n\n")
}

fn blocks_sep(bs: &[Block], o: &WriteOptions, depth: usize, sep: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut last_list_marker = 0u8;
    for (n, b) in bs.iter().enumerate() {
        let s = match b {
            Block::Heading { level, content } => {
                let t = inlines(content, o).replace('\n', " ");
                format!("{} {}", "#".repeat((*level).clamp(1, 6) as usize), t.trim())
            }
            Block::Paragraph(c) => paragraph(c, o),
            Block::Quote(c) => {
                let inner = blocks(c, o, depth + 1);
                inner
                    .trim_end_matches('\n')
                    .lines()
                    .map(|l| {
                        if l.is_empty() {
                            ">".to_string()
                        } else {
                            format!("> {l}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Block::List {
                ordered,
                start,
                tight,
                items,
            } => {
                // adjacent lists need different markers or they would merge
                let follows_list = n > 0 && matches!(bs[n - 1], Block::List { .. });
                let marker = if *ordered {
                    if follows_list && last_list_marker == b'.' {
                        b')'
                    } else {
                        b'.'
                    }
                } else if follows_list && last_list_marker == b'-' {
                    b'*'
                } else {
                    b'-'
                };
                last_list_marker = marker;
                let mut lines = Vec::new();
                for (i, it) in items.iter().enumerate() {
                    let mk = if *ordered {
                        let d = marker as char;
                        format!("{}{d} ", start.saturating_add(i as u32))
                    } else {
                        format!("{} ", marker as char)
                    };
                    let mut body =
                        blocks_sep(&it.blocks, o, depth + 1, if *tight { "\n" } else { "\n\n" });
                    if let Some(c) = it.checked {
                        body = format!("{} {body}", if c { "[x]" } else { "[ ]" });
                    }
                    let body = body.trim_end_matches('\n');
                    let pad = " ".repeat(mk.len());
                    let mut text = String::new();
                    for (k, l) in body.lines().enumerate() {
                        if k == 0 {
                            text.push_str(&mk);
                            text.push_str(l);
                        } else if l.is_empty() {
                        } else {
                            text.push_str(&pad);
                            text.push_str(l);
                        }
                        text.push('\n');
                        if l.is_empty() && k > 0 {
                            // keep the blank line inside the item
                            let len = text.len();
                            text.truncate(len);
                        }
                    }
                    if body.is_empty() {
                        text = format!("{}\n", mk.trim_end());
                    }
                    lines.push(text.trim_end_matches('\n').to_string());
                }
                lines.join(if *tight { "\n" } else { "\n\n" })
            }
            Block::Code { info, text } => {
                let mut fence = "```".to_string();
                while text.contains(&fence) {
                    fence.push('`');
                }
                let body = text.trim_end_matches('\n');
                format!("{fence}{info}\n{body}\n{fence}")
            }
            Block::Rule => "---".to_string(),
            Block::Html(h) => h.clone(),
            Block::PageBreak => continue,
            Block::Table { rows } => table(rows, o),
        };
        parts.push(s);
    }
    let mut out = parts.join(sep);
    out.push('\n');
    out
}

fn table(rows: &[Vec<crate::model::Cell>], o: &WriteOptions) -> String {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return String::new();
    }
    let cell = |c: &crate::model::Cell| {
        inlines(&c.content, o)
            .replace('|', "\\|")
            .replace('\n', " ")
            .trim()
            .to_string()
    };
    let mut lines = Vec::new();
    let header: Vec<String> = (0..cols)
        .map(|i| rows[0].get(i).map(cell).unwrap_or_default())
        .collect();
    lines.push(format!("| {} |", header.join(" | ")));
    let delim: Vec<&str> = (0..cols)
        .map(|i| match rows[0].get(i).map(|c| &c.align) {
            Some(Align::Left) => ":---",
            Some(Align::Center) => ":---:",
            Some(Align::Right) => "---:",
            _ => "---",
        })
        .collect();
    lines.push(format!("| {} |", delim.join(" | ")));
    for r in &rows[1..] {
        let cells: Vec<String> = (0..cols)
            .map(|i| r.get(i).map(cell).unwrap_or_default())
            .collect();
        lines.push(format!("| {} |", cells.join(" | ")));
    }
    lines.join("\n")
}

fn paragraph(c: &[Inline], o: &WriteOptions) -> String {
    let s = inlines(c, o);
    // a paragraph must not look like another block when it starts a line
    let mut out = Vec::new();
    for (i, line) in s.split('\n').enumerate() {
        let l = escape_line_start(line);
        let _ = i;
        out.push(l);
    }
    out.join("\n")
}

fn escape_line_start(line: &str) -> String {
    let t = line.trim_start();
    let lead = &line[..line.len() - t.len()];
    let b = t.as_bytes();
    let first = b.first().copied().unwrap_or(0);
    let needs = matches!(first, b'#' | b'>' | b'+' | b'-' | b'=' | b'~')
        && (first != b'#' || t.chars().take_while(|c| *c == '#').count() <= 6)
        || {
            let d = t.bytes().take_while(u8::is_ascii_digit).count();
            d > 0 && d < 10 && matches!(b.get(d), Some(b'.') | Some(b')'))
        }
        || t.starts_with("```");
    if needs && !lead.is_empty() || needs {
        // escape the marker character itself
        let d = t.bytes().take_while(u8::is_ascii_digit).count();
        if d > 0 {
            return format!("{lead}{}\\{}", &t[..d], &t[d..]);
        }
        return format!("{lead}\\{t}");
    }
    if lead.len() >= 4 {
        return line.trim_start().to_string();
    }
    line.to_string()
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev = ' ';
    for c in s.chars() {
        match c {
            '\\' | '`' | '*' | '[' | ']' | '<' | '>' => {
                out.push('\\');
                out.push(c);
            }
            '_' => {
                // intraword underscores are literal, others are escaped
                out.push('\\');
                out.push('_');
            }
            '~' => out.push_str("\\~"),
            '!' => out.push('!'),
            '&' => out.push('&'),
            '|' => out.push('|'),
            _ => out.push(c),
        }
        prev = c;
    }
    let _ = prev;
    out
}

fn wrap(mark: &str, inner: &str) -> String {
    let t = inner.trim_matches(' ');
    if t.is_empty() {
        return inner.to_string();
    }
    let lead = &inner[..inner.len() - inner.trim_start_matches(' ').len()];
    let trail = &inner[inner.trim_end_matches(' ').len()..];
    format!("{lead}{mark}{t}{mark}{trail}")
}

pub fn inlines(is: &[Inline], o: &WriteOptions) -> String {
    let mut out = String::new();
    for i in is {
        match i {
            Inline::Text(t) => out.push_str(&esc(t)),
            Inline::SoftBreak => out.push('\n'),
            Inline::LineBreak => out.push_str("\\\n"),
            Inline::Code(c) => {
                let mut n = 1;
                while c.contains(&"`".repeat(n)) {
                    n += 1;
                }
                let fence = "`".repeat(n);
                let pad = if c.starts_with('`')
                    || c.ends_with('`')
                    || (c.starts_with(' ') && c.ends_with(' ') && !c.trim().is_empty())
                {
                    " "
                } else {
                    ""
                };
                out.push_str(&format!("{fence}{pad}{c}{pad}{fence}"));
            }
            Inline::Emph(c) => out.push_str(&wrap("*", &inlines(c, o))),
            Inline::Strong(c) => out.push_str(&wrap("**", &inlines(c, o))),
            Inline::Strike(c) => out.push_str(&wrap("~~", &inlines(c, o))),
            Inline::Underline(c) => out.push_str(&format!("<u>{}</u>", inlines(c, o))),
            Inline::Sup(c) => out.push_str(&format!("<sup>{}</sup>", inlines(c, o))),
            Inline::Sub(c) => out.push_str(&format!("<sub>{}</sub>", inlines(c, o))),
            Inline::Link {
                url,
                title,
                content,
            } => {
                let text = inlines(content, o);
                if title.is_empty() && plain(content) == *url && !url.contains(' ') {
                    out.push_str(&format!("<{url}>"));
                } else {
                    out.push_str(&format!("[{text}]({}{})", dest(url), title_part(title)));
                }
            }
            Inline::Image { src, title, alt } => {
                let s = (o.image_src)(src);
                out.push_str(&format!(
                    "![{}]({}{})",
                    esc(&plain(alt)),
                    dest(&s),
                    title_part(title)
                ));
            }
            Inline::Html(h) => out.push_str(h),
        }
    }
    out
}

fn dest(u: &str) -> String {
    if u.is_empty()
        || u.contains(|c: char| c.is_whitespace() || c == '<' || c == '>')
        || !balanced(u)
    {
        format!(
            "<{}>",
            u.replace('<', "%3C")
                .replace('>', "%3E")
                .replace(' ', "%20")
        )
    } else {
        u.replace('\\', "\\\\")
    }
}

fn balanced(u: &str) -> bool {
    let mut d = 0i32;
    for c in u.chars() {
        match c {
            '(' => d += 1,
            ')' => {
                d -= 1;
                if d < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    d == 0
}

fn title_part(t: &str) -> String {
    if t.is_empty() {
        String::new()
    } else {
        format!(" \"{}\"", t.replace('\\', "\\\\").replace('"', "\\\""))
    }
}
