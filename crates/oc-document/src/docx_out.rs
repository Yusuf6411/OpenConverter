//! Document to DOCX (WordprocessingML in an OPC package).
use crate::imgsize;
use crate::model::{plain, Align, Block, Cell, Document, Inline, ListItem};
use oc_core::{Limits, Result};
use oc_xml::{escape_attr, escape_text};

const NS: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"";

/// Text width of an A4 page with 1 inch margins, in EMU.
const MAX_WIDTH_EMU: i64 = 5_731_510;

#[derive(Default, Clone)]
struct RunStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    sup: bool,
    sub: bool,
    code: bool,
    link: bool,
}

struct Out<'a> {
    doc: &'a Document,
    rels: Vec<(String, String, bool)>, // id, target, external
    media: Vec<(String, Vec<u8>)>,
    image_ids: Vec<(String, String, i64, i64)>, // name, rel id, cx, cy
    next_rel: usize,
    nums: Vec<(u32, u32)>, // numId, start
    next_pic: u32,
    notes_images: usize,
    page_width_emu: i64,
}

impl Out<'_> {
    fn rel(&mut self, target: &str, external: bool) -> String {
        if let Some((id, _, _)) = self.rels.iter().find(|(_, t, e)| t == target && *e == external) {
            return id.clone();
        }
        self.next_rel += 1;
        let id = format!("rId{}", self.next_rel + 10);
        self.rels.push((id.clone(), target.to_string(), external));
        id
    }
}

fn rpr(s: &RunStyle) -> String {
    let mut x = String::new();
    if s.link {
        x.push_str("<w:rStyle w:val=\"Hyperlink\"/>");
    }
    if s.code {
        x.push_str("<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:cs=\"Consolas\" w:eastAsia=\"Consolas\"/>");
    }
    if s.bold {
        x.push_str("<w:b/><w:bCs/>");
    }
    if s.italic {
        x.push_str("<w:i/><w:iCs/>");
    }
    if s.strike {
        x.push_str("<w:strike/>");
    }
    if s.underline {
        x.push_str("<w:u w:val=\"single\"/>");
    }
    if s.sup {
        x.push_str("<w:vertAlign w:val=\"superscript\"/>");
    }
    if s.sub {
        x.push_str("<w:vertAlign w:val=\"subscript\"/>");
    }
    if x.is_empty() {
        String::new()
    } else {
        format!("<w:rPr>{x}</w:rPr>")
    }
}

fn text_run(t: &str, s: &RunStyle) -> String {
    let mut out = String::new();
    // tabs and newlines inside text become their own elements
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut String| {
        if !cur.is_empty() {
            out.push_str(&format!(
                "<w:r>{}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
                rpr(s),
                escape_text(&clean(cur))
            ));
            cur.clear();
        }
    };
    for c in t.chars() {
        match c {
            '\t' => {
                flush(&mut cur, &mut out);
                out.push_str(&format!("<w:r>{}<w:tab/></w:r>", rpr(s)));
            }
            '\n' => {
                flush(&mut cur, &mut out);
                out.push_str(&format!("<w:r>{}<w:br/></w:r>", rpr(s)));
            }
            c => cur.push(c),
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Characters XML 1.0 cannot carry are dropped.
fn clean(s: &str) -> String {
    s.chars().filter(|c| oc_xml::is_xml_char(*c)).collect()
}

impl Out<'_> {
    fn inlines(&mut self, is: &[Inline], s: &RunStyle, out: &mut String) {
        for i in is {
            match i {
                Inline::Text(t) => out.push_str(&text_run(t, s)),
                Inline::SoftBreak => out.push_str(&text_run(" ", s)),
                Inline::LineBreak => out.push_str(&format!("<w:r>{}<w:br/></w:r>", rpr(s))),
                Inline::Code(c) => {
                    let mut st = s.clone();
                    st.code = true;
                    out.push_str(&text_run(c, &st));
                }
                Inline::Emph(c) => {
                    let mut st = s.clone();
                    st.italic = true;
                    self.inlines(c, &st, out);
                }
                Inline::Strong(c) => {
                    let mut st = s.clone();
                    st.bold = true;
                    self.inlines(c, &st, out);
                }
                Inline::Strike(c) => {
                    let mut st = s.clone();
                    st.strike = true;
                    self.inlines(c, &st, out);
                }
                Inline::Underline(c) => {
                    let mut st = s.clone();
                    st.underline = true;
                    self.inlines(c, &st, out);
                }
                Inline::Sup(c) => {
                    let mut st = s.clone();
                    st.sup = true;
                    self.inlines(c, &st, out);
                }
                Inline::Sub(c) => {
                    let mut st = s.clone();
                    st.sub = true;
                    self.inlines(c, &st, out);
                }
                Inline::Link { url, content, .. } => {
                    let mut st = s.clone();
                    st.link = true;
                    let mut inner = String::new();
                    self.inlines(content, &st, &mut inner);
                    if let Some(a) = url.strip_prefix('#') {
                        out.push_str(&format!(
                            "<w:hyperlink w:anchor=\"{}\" w:history=\"1\">{inner}</w:hyperlink>",
                            escape_attr(a)
                        ));
                    } else if url.is_empty() {
                        out.push_str(&inner);
                    } else {
                        let id = self.rel(url, true);
                        out.push_str(&format!("<w:hyperlink r:id=\"{id}\" w:history=\"1\">{inner}</w:hyperlink>"));
                    }
                }
                Inline::Image { src, title, alt } => {
                    match self.drawing(src, &plain(alt), title) {
                        Some(d) => out.push_str(&format!("<w:r>{d}</w:r>")),
                        None => {
                            let a = plain(alt);
                            if !a.is_empty() {
                                out.push_str(&text_run(&format!("[{a}]"), s));
                            }
                        }
                    }
                }
                Inline::Html(_) => {}
            }
        }
    }

    fn drawing(&mut self, src: &str, alt: &str, title: &str) -> Option<String> {
        let img = self.doc.image(src)?;
        if img.mime == "image/svg+xml" || img.mime == "image/webp" {
            self.notes_images += 1;
            return None;
        }
        let (w, h) = imgsize::size(&img.data).unwrap_or((400, 300));
        let (mut cx, mut cy) = (i64::from(w.max(1)) * 9525, i64::from(h.max(1)) * 9525);
        if cx > self.page_width_emu {
            cy = cy * self.page_width_emu / cx;
            cx = self.page_width_emu;
        }
        let name = format!("media/{}", img.name);
        let id = match self.image_ids.iter().find(|(n, _, _, _)| *n == img.name) {
            Some((_, id, _, _)) => id.clone(),
            None => {
                let id = self.rel(&name, false);
                self.media.push((format!("word/{name}"), img.data.clone()));
                self.image_ids.push((img.name.clone(), id.clone(), cx, cy));
                id
            }
        };
        self.next_pic += 1;
        let n = self.next_pic;
        let descr = if alt.is_empty() { title } else { alt };
        Some(format!(
            "<w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{n}\" name=\"Picture {n}\" descr=\"{}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic><pic:nvPicPr><pic:cNvPr id=\"{n}\" name=\"{}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{id}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>",
            escape_attr(descr),
            escape_attr(&img.name)
        ))
    }

    fn para(&mut self, ppr: &str, is: &[Inline], s: &RunStyle, out: &mut String) {
        out.push_str("<w:p>");
        if !ppr.is_empty() {
            out.push_str(&format!("<w:pPr>{ppr}</w:pPr>"));
        }
        self.inlines(is, s, out);
        out.push_str("</w:p>");
    }

    fn blocks(&mut self, bs: &[Block], ctx: &Ctx, out: &mut String) {
        for b in bs {
            self.block(b, ctx, out);
        }
    }

    fn block(&mut self, b: &Block, ctx: &Ctx, out: &mut String) {
        let ind = if ctx.quote > 0 {
            format!("<w:ind w:left=\"{}\"/>", ctx.quote * 567)
        } else {
            String::new()
        };
        match b {
            Block::Heading { level, content } => {
                let l = (*level).clamp(1, 6);
                self.para(&format!("<w:pStyle w:val=\"Heading{l}\"/>"), content, &RunStyle::default(), out);
            }
            Block::Paragraph(c) => {
                let style = if ctx.quote > 0 { "<w:pStyle w:val=\"Quote\"/>" } else { "" };
                self.para(&format!("{style}{ind}"), c, &RunStyle::default(), out);
            }
            Block::Quote(c) => {
                let inner = Ctx { quote: ctx.quote + 1, ..*ctx };
                self.blocks(c, &inner, out);
            }
            Block::List {
                ordered,
                start,
                items,
                ..
            } => self.list(*ordered, *start, items, ctx, out),
            Block::Code { text, .. } => {
                let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
                for l in lines {
                    let mut p = String::from("<w:p><w:pPr><w:pStyle w:val=\"Code\"/>");
                    p.push_str(&ind);
                    p.push_str("</w:pPr>");
                    p.push_str(&text_run(l, &RunStyle::default()));
                    p.push_str("</w:p>");
                    out.push_str(&p);
                }
            }
            Block::Rule => out.push_str(
                "<w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"1\" w:color=\"808080\"/></w:pBdr></w:pPr></w:p>",
            ),
            Block::Html(_) => {}
            Block::PageBreak => out.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>"),
            Block::Table { rows } => self.table(rows, out),
        }
    }

    fn list(&mut self, ordered: bool, start: u32, items: &[ListItem], ctx: &Ctx, out: &mut String) {
        let num_id = if ordered {
            let id = 10 + self.nums.len() as u32;
            self.nums.push((id, start));
            id
        } else {
            1
        };
        let lvl = ctx.level.min(8);
        for it in items {
            let mut first = true;
            for b in &it.blocks {
                match b {
                    Block::Paragraph(c) if first => {
                        let mut c = c.clone();
                        if let Some(done) = it.checked {
                            c.insert(0, Inline::Text(if done { "\u{2611} " } else { "\u{2610} " }.into()));
                        }
                        self.para(
                            &format!("<w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"{lvl}\"/><w:numId w:val=\"{num_id}\"/></w:numPr>"),
                            &c,
                            &RunStyle::default(),
                            out,
                        );
                    }
                    Block::List { ordered, start, items, .. } => {
                        if first {
                            // an item that starts with a nested list: an empty numbered paragraph first
                            self.para(
                                &format!("<w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"{lvl}\"/><w:numId w:val=\"{num_id}\"/></w:numPr>"),
                                &[],
                                &RunStyle::default(),
                                out,
                            );
                        }
                        let inner = Ctx { level: ctx.level + 1, ..*ctx };
                        self.list(*ordered, *start, items, &inner, out);
                    }
                    other => {
                        if first {
                            self.para(
                                &format!("<w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"{lvl}\"/><w:numId w:val=\"{num_id}\"/></w:numPr>"),
                                &[],
                                &RunStyle::default(),
                                out,
                            );
                        }
                        let inner = Ctx { quote: ctx.quote + 1 + i64::from(ctx.level), ..*ctx };
                        self.block(other, &inner, out);
                    }
                }
                first = false;
            }
            if it.blocks.is_empty() {
                self.para(
                    &format!("<w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"{lvl}\"/><w:numId w:val=\"{num_id}\"/></w:numPr>"),
                    &[],
                    &RunStyle::default(),
                    out,
                );
            }
        }
    }

    fn table(&mut self, rows: &[Vec<Cell>], out: &mut String) {
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let w = 9000 / cols as i64;
        out.push_str("<w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblLook w:val=\"04A0\"/></w:tblPr><w:tblGrid>");
        for _ in 0..cols {
            out.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
        }
        out.push_str("</w:tblGrid>");
        for (n, r) in rows.iter().enumerate() {
            out.push_str("<w:tr>");
            if n == 0 && r.iter().any(|c| c.header) {
                out.push_str("<w:trPr><w:tblHeader/></w:trPr>");
            }
            for i in 0..cols {
                let empty = Cell {
                    header: false,
                    align: Align::None,
                    content: vec![],
                };
                let c = r.get(i).unwrap_or(&empty);
                out.push_str(&format!("<w:tc><w:tcPr><w:tcW w:w=\"{w}\" w:type=\"dxa\"/></w:tcPr>"));
                let jc = match c.align {
                    Align::Center => "<w:jc w:val=\"center\"/>",
                    Align::Right => "<w:jc w:val=\"right\"/>",
                    _ => "",
                };
                let st = RunStyle {
                    bold: c.header,
                    ..RunStyle::default()
                };
                self.para(jc, &c.content, &st, out);
                out.push_str("</w:tc>");
            }
            out.push_str("</w:tr>");
        }
        out.push_str("</w:tbl><w:p/>");
    }
}

#[derive(Clone, Copy)]
struct Ctx {
    quote: i64,
    level: u32,
}

const STYLES: &str = include_str!("docx_styles.xml");

fn abstract_num(id: u32, bullet: bool) -> String {
    let mut x = format!("<w:abstractNum w:abstractNumId=\"{id}\"><w:multiLevelType w:val=\"hybridMultilevel\"/>");
    for l in 0..9u32 {
        let (fmt, text) = if bullet {
            ("bullet".to_string(), "\u{2022}".to_string())
        } else {
            (
                ["decimal", "lowerLetter", "lowerRoman"][(l % 3) as usize].to_string(),
                format!("%{}.", l + 1),
            )
        };
        x.push_str(&format!(
            "<w:lvl w:ilvl=\"{l}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr>{}</w:lvl>",
            720 + 360 * l,
            if bullet { "<w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\" w:hint=\"default\"/></w:rPr>" } else { "" }
        ));
    }
    x.push_str("</w:abstractNum>");
    x
}

pub fn write(doc: &Document, limits: &Limits) -> Result<Vec<u8>> {
    let mut o = Out {
        doc,
        rels: Vec::new(),
        media: Vec::new(),
        image_ids: Vec::new(),
        next_rel: 0,
        nums: Vec::new(),
        next_pic: 0,
        notes_images: 0,
        page_width_emu: MAX_WIDTH_EMU,
    };
    let mut body = String::new();
    if let Some(t) = &doc.meta.title {
        if !matches!(doc.blocks.first(), Some(Block::Heading { .. })) {
            o.para(
                "<w:pStyle w:val=\"Title\"/>",
                &[Inline::Text(t.clone())],
                &RunStyle::default(),
                &mut body,
            );
        }
    }
    o.blocks(&doc.blocks, &Ctx { quote: 0, level: 0 }, &mut body);
    let sect = "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr>";
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document {NS}><w:body>{body}{sect}</w:body></w:document>"
    );
    let mut numbering = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:numbering {NS}>{}{}<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>",
        abstract_num(0, true),
        abstract_num(1, false)
    );
    for (id, start) in &o.nums {
        numbering.push_str(&format!(
            "<w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"1\"/><w:lvlOverride w:ilvl=\"0\"><w:startOverride w:val=\"{start}\"/></w:lvlOverride></w:num>"
        ));
    }
    numbering.push_str("</w:numbering>");
    let mut rels = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>");
    for (id, target, external) in &o.rels {
        if *external {
            rels.push_str(&format!(
                "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"{}\" TargetMode=\"External\"/>",
                escape_attr(target)
            ));
        } else {
            rels.push_str(&format!(
                "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"{}\"/>",
                escape_attr(target)
            ));
        }
    }
    rels.push_str("</Relationships>");
    let mut exts = String::new();
    for e in ["png", "jpg", "jpeg", "gif", "bmp"] {
        if o.media.iter().any(|(n, _)| n.ends_with(&format!(".{e}"))) {
            let ct = match e {
                "png" => "image/png",
                "gif" => "image/gif",
                "bmp" => "image/bmp",
                _ => "image/jpeg",
            };
            exts.push_str(&format!("<Default Extension=\"{e}\" ContentType=\"{ct}\"/>"));
        }
    }
    let content_types = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/>{exts}<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/></Types>"
    );
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>";
    let m = &doc.meta;
    let mut core = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">");
    if let Some(t) = &m.title {
        core.push_str(&format!("<dc:title>{}</dc:title>", escape_text(&clean(t))));
    }
    if let Some(t) = &m.subject {
        core.push_str(&format!("<dc:subject>{}</dc:subject>", escape_text(&clean(t))));
    }
    if let Some(t) = &m.author {
        core.push_str(&format!("<dc:creator>{}</dc:creator>", escape_text(&clean(t))));
    }
    if let Some(t) = &m.language {
        core.push_str(&format!("<dc:language>{}</dc:language>", escape_text(&clean(t))));
    }
    if let Some(d) = &m.date {
        if d.len() >= 10 && d.as_bytes().get(4) == Some(&b'-') {
            let full = if d.len() == 10 { format!("{d}T00:00:00Z") } else { d.clone() };
            core.push_str(&format!("<dcterms:created xsi:type=\"dcterms:W3CDTF\">{}</dcterms:created>", escape_text(&full)));
        }
    }
    core.push_str("</cp:coreProperties>");
    let mut entries = vec![
        ("[Content_Types].xml".to_string(), content_types.into_bytes()),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        ("word/document.xml".to_string(), document.into_bytes()),
        ("word/styles.xml".to_string(), STYLES.as_bytes().to_vec()),
        ("word/numbering.xml".to_string(), numbering.into_bytes()),
        ("word/_rels/document.xml.rels".to_string(), rels.into_bytes()),
        ("docProps/core.xml".to_string(), core.into_bytes()),
    ];
    entries.extend(o.media);
    crate::zipio::write(entries, limits)
}
