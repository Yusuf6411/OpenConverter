//! Document to HTML, in the shape the CommonMark reference renderer produces.
use crate::model::{plain, Align, Block, Document, Inline};

pub fn escape_html(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

pub fn escape_url(s: &str, out: &mut String) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let keep = c.is_ascii_alphanumeric()
            || b"-_.+!*'(),%#@?=;:/&$~".contains(&c);
        if c == b'&' {
            out.push_str("&amp;");
        } else if c == b'\'' {
            out.push_str("&#x27;");
        } else if c == b'%'
            && !(i + 2 < b.len() + 0 && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit())
        {
            out.push_str("%25");
        } else if keep {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
        i += 1;
    }
}

fn cr(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

pub struct Ctx<'a> {
    pub doc: &'a Document,
    /// Rewrite picture sources (the EPUB writer maps document images to package paths).
    pub image_src: &'a dyn Fn(&str) -> String,
}

pub fn fragment(doc: &Document) -> String {
    let id = |s: &str| s.to_string();
    let cx = Ctx {
        doc,
        image_src: &id,
    };
    let mut out = String::new();
    blocks(&cx, &doc.blocks, false, &mut out);
    out
}

pub fn fragment_with(cx: &Ctx) -> String {
    let mut out = String::new();
    blocks(cx, &cx.doc.blocks, false, &mut out);
    out
}

pub fn blocks(cx: &Ctx, bs: &[Block], tight: bool, out: &mut String) {
    for b in bs {
        block(cx, b, tight, out);
    }
}

fn block(cx: &Ctx, b: &Block, tight: bool, out: &mut String) {
    match b {
        Block::Paragraph(c) => {
            if tight {
                inlines(cx, c, out);
            } else {
                cr(out);
                out.push_str("<p>");
                inlines(cx, c, out);
                out.push_str("</p>\n");
            }
        }
        Block::Heading { level, content } => {
            cr(out);
            out.push_str(&format!("<h{level}>"));
            inlines(cx, content, out);
            out.push_str(&format!("</h{level}>\n"));
        }
        Block::Quote(c) => {
            cr(out);
            out.push_str("<blockquote>\n");
            blocks(cx, c, false, out);
            cr(out);
            out.push_str("</blockquote>\n");
        }
        Block::List {
            ordered,
            start,
            tight,
            items,
        } => {
            cr(out);
            if *ordered {
                if *start == 1 {
                    out.push_str("<ol>\n");
                } else {
                    out.push_str(&format!("<ol start=\"{start}\">\n"));
                }
            } else {
                out.push_str("<ul>\n");
            }
            for it in items {
                out.push_str("<li>");
                if let Some(c) = it.checked {
                    out.push_str(if c {
                        "<input type=\"checkbox\" checked=\"\" disabled=\"\" /> "
                    } else {
                        "<input type=\"checkbox\" disabled=\"\" /> "
                    });
                }
                blocks(cx, &it.blocks, *tight, out);
                if !it.blocks.is_empty() && (!*tight || !matches!(it.blocks.last(), Some(Block::Paragraph(_)))) {
                    cr(out);
                }
                out.push_str("</li>\n");
            }
            out.push_str(if *ordered { "</ol>\n" } else { "</ul>\n" });
        }
        Block::Code { info, text } => {
            cr(out);
            out.push_str("<pre><code");
            let lang = info.split_whitespace().next().unwrap_or("");
            if !lang.is_empty() {
                out.push_str(" class=\"language-");
                escape_html(lang, out);
                out.push('"');
            }
            out.push('>');
            escape_html(text, out);
            out.push_str("</code></pre>\n");
        }
        Block::Rule => {
            cr(out);
            out.push_str("<hr />\n");
        }
        Block::Html(h) => {
            cr(out);
            out.push_str(h);
            out.push('\n');
        }
        Block::PageBreak => {}
        Block::Table { rows } => {
            cr(out);
            out.push_str("<table>\n");
            for (n, row) in rows.iter().enumerate() {
                if n == 0 {
                    out.push_str("<thead>\n");
                } else if n == 1 {
                    out.push_str("<tbody>\n");
                }
                out.push_str("<tr>\n");
                for c in row {
                    let tag = if c.header { "th" } else { "td" };
                    out.push('<');
                    out.push_str(tag);
                    match c.align {
                        Align::Left => out.push_str(" align=\"left\""),
                        Align::Center => out.push_str(" align=\"center\""),
                        Align::Right => out.push_str(" align=\"right\""),
                        Align::None => {}
                    }
                    out.push('>');
                    inlines(cx, &c.content, out);
                    out.push_str(&format!("</{tag}>\n"));
                }
                out.push_str("</tr>\n");
                if n == 0 {
                    out.push_str("</thead>\n");
                }
            }
            if rows.len() > 1 {
                out.push_str("</tbody>\n");
            }
            out.push_str("</table>\n");
        }
    }
}

pub fn inlines(cx: &Ctx, is: &[Inline], out: &mut String) {
    for i in is {
        inline(cx, i, out);
    }
}

fn inline(cx: &Ctx, i: &Inline, out: &mut String) {
    match i {
        Inline::Text(t) => escape_html(t, out),
        Inline::SoftBreak => out.push('\n'),
        Inline::LineBreak => out.push_str("<br />\n"),
        Inline::Code(c) => {
            out.push_str("<code>");
            escape_html(c, out);
            out.push_str("</code>");
        }
        Inline::Emph(c) => wrap(cx, "em", c, out),
        Inline::Strong(c) => wrap(cx, "strong", c, out),
        Inline::Strike(c) => wrap(cx, "del", c, out),
        Inline::Underline(c) => wrap(cx, "u", c, out),
        Inline::Sup(c) => wrap(cx, "sup", c, out),
        Inline::Sub(c) => wrap(cx, "sub", c, out),
        Inline::Link { url, title, content } => {
            out.push_str("<a href=\"");
            escape_url(url, out);
            out.push('"');
            if !title.is_empty() {
                out.push_str(" title=\"");
                escape_html(title, out);
                out.push('"');
            }
            out.push('>');
            inlines(cx, content, out);
            out.push_str("</a>");
        }
        Inline::Image { src, title, alt } => {
            out.push_str("<img src=\"");
            escape_url(&(cx.image_src)(src), out);
            out.push_str("\" alt=\"");
            escape_html(&plain(alt), out);
            out.push('"');
            if !title.is_empty() {
                out.push_str(" title=\"");
                escape_html(title, out);
                out.push('"');
            }
            out.push_str(" />");
        }
        Inline::Html(h) => raw_html(h, out),
    }
}

/// GitHub escapes the opening `<` of tags that could swallow the rest of the page.
fn raw_html(h: &str, out: &mut String) {
    let t = h.trim_start_matches('<').trim_start_matches('/').to_ascii_lowercase();
    let name: String = t.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    let unsafe_tag = matches!(
        name.as_str(),
        "title" | "textarea" | "style" | "xmp" | "iframe" | "noembed" | "noframes" | "script" | "plaintext"
    );
    if unsafe_tag && h.starts_with('<') {
        out.push_str("&lt;");
        out.push_str(&h[1..]);
    } else {
        out.push_str(h);
    }
}

fn wrap(cx: &Ctx, tag: &str, c: &[Inline], out: &mut String) {
    out.push('<');
    out.push_str(tag);
    out.push('>');
    inlines(cx, c, out);
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}
