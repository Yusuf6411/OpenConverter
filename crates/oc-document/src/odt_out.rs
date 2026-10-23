//! Document to ODT (OpenDocument Text 1.2).
use crate::imgsize;
use crate::model::{plain, Align, Block, Document, Inline, ListItem};
use oc_core::{Limits, Result};
use oc_xml::{escape_attr, escape_text};

const NS: &str = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\" xmlns:fo=\"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\" xmlns:svg=\"urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0\"";

#[derive(Default, Clone, PartialEq)]
struct RunStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    sup: bool,
    sub: bool,
    code: bool,
}

impl RunStyle {
    fn plain(&self) -> bool {
        *self == RunStyle::default()
    }
}

struct Out<'a> {
    doc: &'a Document,
    styles: Vec<RunStyle>,
    media: Vec<(String, Vec<u8>)>,
    frame: u32,
}

impl Out<'_> {
    fn style_name(&mut self, s: &RunStyle) -> String {
        let n = match self.styles.iter().position(|x| x == s) {
            Some(n) => n,
            None => {
                self.styles.push(s.clone());
                self.styles.len() - 1
            }
        };
        format!("T{}", n + 1)
    }

    fn text(&mut self, t: &str, s: &RunStyle, out: &mut String) {
        let mut body = String::new();
        let mut prev_space = false;
        for c in t.chars() {
            match c {
                ' ' => {
                    if prev_space {
                        body.push_str("<text:s/>");
                    } else {
                        body.push(' ');
                    }
                    prev_space = true;
                    continue;
                }
                '\t' => body.push_str("<text:tab/>"),
                '\n' => body.push_str("<text:line-break/>"),
                c if oc_xml::is_xml_char(c) => body.push_str(&escape_text(&c.to_string())),
                _ => {}
            }
            prev_space = false;
        }
        // leading spaces of a paragraph are significant to ODF only when written as text:s
        if body.starts_with(' ') {
            body = format!("<text:s/>{}", &body[1..]);
        }
        if s.plain() {
            out.push_str(&body);
        } else {
            let n = self.style_name(s);
            out.push_str(&format!(
                "<text:span text:style-name=\"{n}\">{body}</text:span>"
            ));
        }
    }

    fn inlines(&mut self, is: &[Inline], s: &RunStyle, out: &mut String) {
        for i in is {
            match i {
                Inline::Text(t) => self.text(t, s, out),
                Inline::SoftBreak => self.text(" ", s, out),
                Inline::LineBreak => out.push_str("<text:line-break/>"),
                Inline::Code(c) => {
                    let st = RunStyle {
                        code: true,
                        ..s.clone()
                    };
                    self.text(c, &st, out);
                }
                Inline::Emph(c) => self.inlines(
                    c,
                    &RunStyle {
                        italic: true,
                        ..s.clone()
                    },
                    out,
                ),
                Inline::Strong(c) => self.inlines(
                    c,
                    &RunStyle {
                        bold: true,
                        ..s.clone()
                    },
                    out,
                ),
                Inline::Strike(c) => self.inlines(
                    c,
                    &RunStyle {
                        strike: true,
                        ..s.clone()
                    },
                    out,
                ),
                Inline::Underline(c) => self.inlines(
                    c,
                    &RunStyle {
                        underline: true,
                        ..s.clone()
                    },
                    out,
                ),
                Inline::Sup(c) => self.inlines(
                    c,
                    &RunStyle {
                        sup: true,
                        ..s.clone()
                    },
                    out,
                ),
                Inline::Sub(c) => self.inlines(
                    c,
                    &RunStyle {
                        sub: true,
                        ..s.clone()
                    },
                    out,
                ),
                Inline::Link { url, content, .. } => {
                    let mut inner = String::new();
                    self.inlines(content, s, &mut inner);
                    if url.is_empty() {
                        out.push_str(&inner);
                    } else {
                        out.push_str(&format!(
                            "<text:a xlink:type=\"simple\" xlink:href=\"{}\">{inner}</text:a>",
                            escape_attr(url)
                        ));
                    }
                }
                Inline::Image { src, alt, title } => {
                    if let Some(img) = self.doc.image(src) {
                        let (w, h) = imgsize::size(&img.data).unwrap_or((400, 300));
                        // 96 dpi pixels to centimetres, at most the text width
                        let (mut cw, mut ch) = (
                            f64::from(w.max(1)) * 2.54 / 96.0,
                            f64::from(h.max(1)) * 2.54 / 96.0,
                        );
                        if cw > 16.0 {
                            ch = ch * 16.0 / cw;
                            cw = 16.0;
                        }
                        self.frame += 1;
                        let name = format!("Pictures/{}", img.name);
                        if !self.media.iter().any(|(n, _)| *n == name) {
                            self.media.push((name.clone(), img.data.clone()));
                        }
                        let a = plain(alt);
                        let desc = if a.is_empty() { title.clone() } else { a };
                        out.push_str(&format!(
                            "<draw:frame draw:style-name=\"fr1\" draw:name=\"Image{}\" text:anchor-type=\"as-char\" svg:width=\"{cw:.3}cm\" svg:height=\"{ch:.3}cm\"><draw:image xlink:href=\"{}\" xlink:type=\"simple\" xlink:show=\"embed\" xlink:actuate=\"onLoad\"/><svg:desc>{}</svg:desc></draw:frame>",
                            self.frame,
                            escape_attr(&name),
                            escape_text(&desc)
                        ));
                    } else {
                        let a = plain(alt);
                        if !a.is_empty() {
                            self.text(&format!("[{a}]"), s, out);
                        }
                    }
                }
                Inline::Html(_) => {}
            }
        }
    }

    fn blocks(&mut self, bs: &[Block], quote: bool, out: &mut String) {
        for b in bs {
            self.block(b, quote, out);
        }
    }

    fn block(&mut self, b: &Block, quote: bool, out: &mut String) {
        let pstyle = if quote { "Quotations" } else { "Text_20_body" };
        match b {
            Block::Heading { level, content } => {
                let l = (*level).clamp(1, 6);
                out.push_str(&format!(
                    "<text:h text:style-name=\"Heading_20_{l}\" text:outline-level=\"{l}\">"
                ));
                self.inlines(content, &RunStyle::default(), out);
                out.push_str("</text:h>");
            }
            Block::Paragraph(c) => {
                out.push_str(&format!("<text:p text:style-name=\"{pstyle}\">"));
                self.inlines(c, &RunStyle::default(), out);
                out.push_str("</text:p>");
            }
            Block::Quote(c) => self.blocks(c, true, out),
            Block::List {
                ordered,
                start,
                items,
                ..
            } => self.list(*ordered, *start, items, quote, out),
            Block::Code { text, .. } => {
                for l in text.trim_end_matches('\n').split('\n') {
                    out.push_str("<text:p text:style-name=\"Preformatted_20_Text\">");
                    self.text(l, &RunStyle::default(), out);
                    out.push_str("</text:p>");
                }
            }
            Block::Rule => out.push_str("<text:p text:style-name=\"Horizontal_20_Line\"/>"),
            Block::PageBreak => out.push_str("<text:p text:style-name=\"PageBreak\"/>"),
            Block::Html(_) => {}
            Block::Table { rows } => {
                let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
                out.push_str("<table:table table:name=\"Table\" table:style-name=\"Table1\">");
                out.push_str(&format!(
                    "<table:table-column table:number-columns-repeated=\"{cols}\"/>"
                ));
                for (n, r) in rows.iter().enumerate() {
                    let header = n == 0 && r.iter().any(|c| c.header);
                    if header {
                        out.push_str("<table:table-header-rows>");
                    }
                    out.push_str("<table:table-row>");
                    for i in 0..cols {
                        out.push_str("<table:table-cell table:style-name=\"Cell\" office:value-type=\"string\">");
                        let c = r.get(i);
                        let st = if c.is_some_and(|c| c.header) {
                            RunStyle {
                                bold: true,
                                ..RunStyle::default()
                            }
                        } else {
                            RunStyle::default()
                        };
                        let ps = match c.map(|c| &c.align) {
                            Some(Align::Center) => "Table_20_Center",
                            Some(Align::Right) => "Table_20_Right",
                            _ => "Table_20_Contents",
                        };
                        out.push_str(&format!("<text:p text:style-name=\"{ps}\">"));
                        if let Some(c) = c {
                            self.inlines(&c.content, &st, out);
                        }
                        out.push_str("</text:p></table:table-cell>");
                    }
                    out.push_str("</table:table-row>");
                    if header {
                        out.push_str("</table:table-header-rows>");
                    }
                }
                out.push_str("</table:table>");
            }
        }
    }

    fn list(
        &mut self,
        ordered: bool,
        start: u32,
        items: &[ListItem],
        quote: bool,
        out: &mut String,
    ) {
        out.push_str(&format!(
            "<text:list text:style-name=\"{}\">",
            if ordered {
                "Numbering_20_123"
            } else {
                "List_20_Bullet"
            }
        ));
        for (i, it) in items.iter().enumerate() {
            if ordered && i == 0 && start != 1 {
                out.push_str(&format!("<text:list-item text:start-value=\"{start}\">"));
            } else {
                out.push_str("<text:list-item>");
            }
            let mut first = true;
            for b in &it.blocks {
                match b {
                    Block::Paragraph(c) if first => {
                        out.push_str("<text:p text:style-name=\"List_20_Contents\">");
                        if let Some(done) = it.checked {
                            out.push_str(if done { "\u{2611} " } else { "\u{2610} " });
                        }
                        self.inlines(c, &RunStyle::default(), out);
                        out.push_str("</text:p>");
                    }
                    other => {
                        if first && !matches!(other, Block::List { .. }) {
                            // fallthrough: block content directly
                        }
                        self.block(other, quote, out);
                    }
                }
                first = false;
            }
            if it.blocks.is_empty() {
                out.push_str("<text:p text:style-name=\"List_20_Contents\"/>");
            }
            out.push_str("</text:list-item>");
        }
        out.push_str("</text:list>");
    }
}

const STYLES: &str = include_str!("odt_styles.xml");

pub fn write(doc: &Document, limits: &Limits) -> Result<Vec<u8>> {
    let mut o = Out {
        doc,
        styles: Vec::new(),
        media: Vec::new(),
        frame: 0,
    };
    let mut body = String::new();
    if let Some(t) = &doc.meta.title {
        if !matches!(doc.blocks.first(), Some(Block::Heading { .. })) {
            body.push_str(&format!(
                "<text:p text:style-name=\"Title\">{}</text:p>",
                escape_text(t)
            ));
        }
    }
    o.blocks(&doc.blocks, false, &mut body);
    let mut auto = String::new();
    for (i, s) in o.styles.iter().enumerate() {
        let mut props = String::new();
        if s.bold {
            props.push_str(" fo:font-weight=\"bold\" style:font-weight-asian=\"bold\" style:font-weight-complex=\"bold\"");
        }
        if s.italic {
            props.push_str(" fo:font-style=\"italic\" style:font-style-asian=\"italic\" style:font-style-complex=\"italic\"");
        }
        if s.strike {
            props.push_str(
                " style:text-line-through-style=\"solid\" style:text-line-through-type=\"single\"",
            );
        }
        if s.underline {
            props.push_str(" style:text-underline-style=\"solid\" style:text-underline-width=\"auto\" style:text-underline-color=\"font-color\"");
        }
        if s.sup {
            props.push_str(" style:text-position=\"super 58%\"");
        }
        if s.sub {
            props.push_str(" style:text-position=\"sub 58%\"");
        }
        if s.code {
            props.push_str(" style:font-name=\"Liberation Mono\" fo:font-family=\"'Liberation Mono'\" style:font-family-generic=\"modern\" style:font-pitch=\"fixed\"");
        }
        auto.push_str(&format!(
            "<style:style style:name=\"T{}\" style:family=\"text\"><style:text-properties{props}/></style:style>",
            i + 1
        ));
    }
    let content = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-content {NS} office:version=\"1.2\"><office:font-face-decls><style:font-face style:name=\"Liberation Mono\" svg:font-family=\"'Liberation Mono'\" style:font-family-generic=\"modern\" style:font-pitch=\"fixed\"/></office:font-face-decls><office:automatic-styles>{auto}<style:style style:name=\"fr1\" style:family=\"graphic\" style:parent-style-name=\"Graphics\"><style:graphic-properties style:vertical-pos=\"top\" style:vertical-rel=\"baseline\"/></style:style><style:style style:name=\"Table1\" style:family=\"table\"><style:table-properties style:width=\"17cm\" table:align=\"margins\"/></style:style><style:style style:name=\"Cell\" style:family=\"table-cell\"><style:table-cell-properties fo:padding=\"0.1cm\" fo:border=\"0.5pt solid #000000\"/></style:style></office:automatic-styles><office:body><office:text>{body}</office:text></office:body></office:document-content>"
    );
    let m = &doc.meta;
    let mut meta = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-meta {NS} office:version=\"1.2\"><office:meta><meta:generator>OpenConverter</meta:generator>");
    if let Some(t) = &m.title {
        meta.push_str(&format!("<dc:title>{}</dc:title>", escape_text(t)));
    }
    if let Some(t) = &m.subject {
        meta.push_str(&format!("<dc:subject>{}</dc:subject>", escape_text(t)));
    }
    if let Some(t) = &m.author {
        meta.push_str(&format!(
            "<meta:initial-creator>{0}</meta:initial-creator><dc:creator>{0}</dc:creator>",
            escape_text(t)
        ));
    }
    if let Some(t) = &m.language {
        meta.push_str(&format!("<dc:language>{}</dc:language>", escape_text(t)));
    }
    if let Some(d) = &m.date {
        let full = if d.len() == 10 {
            format!("{d}T00:00:00")
        } else {
            d.clone()
        };
        meta.push_str(&format!(
            "<meta:creation-date>{}</meta:creation-date>",
            escape_text(&full)
        ));
    }
    meta.push_str("</office:meta></office:document-meta>");
    let mut manifest = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\" manifest:version=\"1.2\"><manifest:file-entry manifest:full-path=\"/\" manifest:version=\"1.2\" manifest:media-type=\"application/vnd.oasis.opendocument.text\"/><manifest:file-entry manifest:full-path=\"content.xml\" manifest:media-type=\"text/xml\"/><manifest:file-entry manifest:full-path=\"styles.xml\" manifest:media-type=\"text/xml\"/><manifest:file-entry manifest:full-path=\"meta.xml\" manifest:media-type=\"text/xml\"/>");
    for (n, d) in &o.media {
        let mt = imgsize::mime_of(d).unwrap_or("application/octet-stream");
        manifest.push_str(&format!(
            "<manifest:file-entry manifest:full-path=\"{}\" manifest:media-type=\"{mt}\"/>",
            escape_attr(n)
        ));
    }
    manifest.push_str("</manifest:manifest>");
    let mut entries = vec![
        (
            "mimetype".to_string(),
            b"application/vnd.oasis.opendocument.text".to_vec(),
        ),
        ("META-INF/manifest.xml".to_string(), manifest.into_bytes()),
        ("content.xml".to_string(), content.into_bytes()),
        (
            "styles.xml".to_string(),
            STYLES.replace("{NS}", NS).into_bytes(),
        ),
        ("meta.xml".to_string(), meta.into_bytes()),
    ];
    entries.extend(o.media);
    crate::zipio::write(entries, limits)
}
