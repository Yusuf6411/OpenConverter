//! The document model every format reads into and writes from.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Meta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub language: Option<String>,
    pub date: Option<String>,
}

/// A picture carried inside the document; inlines refer to it by `name`.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Document {
    pub meta: Meta,
    pub blocks: Vec<Block>,
    pub images: Vec<Image>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    /// Task list state (`[ ]` / `[x]`).
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub header: bool,
    pub align: Align,
    pub content: Vec<Inline>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    List {
        ordered: bool,
        start: u32,
        tight: bool,
        items: Vec<ListItem>,
    },
    Quote(Vec<Block>),
    Code {
        /// The whole info string after the opening fence.
        info: String,
        text: String,
    },
    Table {
        rows: Vec<Vec<Cell>>,
    },
    Rule,
    /// HTML written in the source, kept only for HTML and Markdown output.
    Html(String),
    /// A forced page break (DOCX, ODT, RTF, PDF).
    PageBreak,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Underline(Vec<Inline>),
    Sup(Vec<Inline>),
    Sub(Vec<Inline>),
    Code(String),
    Link {
        url: String,
        title: String,
        content: Vec<Inline>,
    },
    Image {
        src: String,
        title: String,
        alt: Vec<Inline>,
    },
    SoftBreak,
    LineBreak,
    Html(String),
}

impl Document {
    pub fn image(&self, src: &str) -> Option<&Image> {
        self.images.iter().find(|i| i.name == src)
    }

    /// Add a picture, returning the name inlines use to refer to it.
    pub fn add_image(&mut self, mime: &str, data: Vec<u8>) -> String {
        let ext = match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            "image/bmp" => "bmp",
            _ => "bin",
        };
        let name = format!("image{}.{ext}", self.images.len() + 1);
        self.images.push(Image {
            name: name.clone(),
            mime: mime.into(),
            data,
        });
        name
    }
}

/// The text of inlines with all markup removed (image alt text included).
pub fn plain(inlines: &[Inline]) -> String {
    let mut s = String::new();
    push_plain(inlines, &mut s);
    s
}

fn push_plain(inlines: &[Inline], s: &mut String) {
    for i in inlines {
        match i {
            Inline::Text(t) | Inline::Code(t) => s.push_str(t),
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strike(c)
            | Inline::Underline(c)
            | Inline::Sup(c)
            | Inline::Sub(c) => push_plain(c, s),
            Inline::Link { content, .. } => push_plain(content, s),
            Inline::Image { alt, .. } => push_plain(alt, s),
            Inline::SoftBreak => s.push('\n'),
            Inline::LineBreak => s.push('\n'),
            Inline::Html(_) => {}
        }
    }
}

/// Visit every picture inline in the blocks, mutably.
pub fn visit_images(blocks: &mut [Block], f: &mut dyn FnMut(&mut Inline)) {
    for b in blocks {
        match b {
            Block::Heading { content, .. } | Block::Paragraph(content) => {
                visit_inline_images(content, f)
            }
            Block::List { items, .. } => {
                for it in items {
                    visit_images(&mut it.blocks, f);
                }
            }
            Block::Quote(c) => visit_images(c, f),
            Block::Table { rows } => {
                for r in rows {
                    for c in r {
                        visit_inline_images(&mut c.content, f);
                    }
                }
            }
            _ => {}
        }
    }
}

fn visit_inline_images(inl: &mut [Inline], f: &mut dyn FnMut(&mut Inline)) {
    for i in inl {
        match i {
            Inline::Image { .. } => f(i),
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strike(c)
            | Inline::Underline(c)
            | Inline::Sup(c)
            | Inline::Sub(c)
            | Inline::Link { content: c, .. } => visit_inline_images(c, f),
            _ => {}
        }
    }
}

/// Remove raw HTML (blocks and inlines), for formats that need well-formed XML.
pub fn strip_html(blocks: &mut Vec<Block>) {
    blocks.retain(|b| !matches!(b, Block::Html(_)));
    for b in blocks.iter_mut() {
        match b {
            Block::Heading { content, .. } | Block::Paragraph(content) => {
                strip_inline_html(content)
            }
            Block::List { items, .. } => {
                for it in items {
                    strip_html(&mut it.blocks);
                }
            }
            Block::Quote(c) => strip_html(c),
            Block::Table { rows } => {
                for r in rows {
                    for c in r {
                        strip_inline_html(&mut c.content);
                    }
                }
            }
            _ => {}
        }
    }
}

fn strip_inline_html(inl: &mut Vec<Inline>) {
    inl.retain(|i| !matches!(i, Inline::Html(_)));
    for i in inl.iter_mut() {
        match i {
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strike(c)
            | Inline::Underline(c)
            | Inline::Sup(c)
            | Inline::Sub(c)
            | Inline::Link { content: c, .. }
            | Inline::Image { alt: c, .. } => strip_inline_html(c),
            _ => {}
        }
    }
}
