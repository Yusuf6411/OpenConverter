//! DOCX to the document model: styles, numbering, runs, fields, tables, pictures, footnotes.
use crate::imgsize;
use crate::build::Builder;
use crate::model::{Align, Block, Cell, Document, Inline, ListItem, Meta};
use crate::xml_tree::{self, El};
use crate::zipio::Package;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};
use std::collections::HashMap;

#[derive(Clone, Default)]
struct RStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    sup: bool,
    sub: bool,
    code: bool,
}

#[derive(Clone, Default)]
struct Style {
    name: String,
    based_on: Option<String>,
    outline: Option<u8>,
    num: Option<(String, u32)>,
    run: RStyle,
}

struct Level {
    bullet: bool,
    start: u32,
}

#[derive(Default)]
struct Numbering {
    abstracts: HashMap<String, HashMap<u32, Level>>,
    nums: HashMap<String, (String, HashMap<u32, u32>)>,
}

impl Numbering {
    fn level(&self, num: &str, ilvl: u32) -> (bool, u32) {
        let Some((abs, over)) = self.nums.get(num) else {
            return (true, 1);
        };
        let lvl = self.abstracts.get(abs).and_then(|a| a.get(&ilvl));
        let bullet = lvl.is_none_or(|l| l.bullet);
        let start = over.get(&ilvl).copied().or(lvl.map(|l| l.start)).unwrap_or(1);
        (bullet, start)
    }
}

fn on(e: &El) -> bool {
    !matches!(e.attr("val"), Some("0" | "false" | "off" | "none"))
}

fn run_props(rpr: &El, base: &RStyle, styles: &HashMap<String, Style>) -> RStyle {
    let mut s = base.clone();
    for c in rpr.children() {
        match c.name.as_str() {
            "rStyle" => {
                if let Some(id) = c.attr("val") {
                    let st = resolve_run(id, styles);
                    s.bold |= st.bold;
                    s.italic |= st.italic;
                    s.strike |= st.strike;
                    s.underline |= st.underline;
                    s.code |= st.code;
                }
            }
            "b" | "bCs" => s.bold = on(c),
            "i" | "iCs" => s.italic = on(c),
            "strike" | "dstrike" => s.strike = on(c),
            "u" => s.underline = on(c),
            "vertAlign" => match c.attr("val") {
                Some("superscript") => s.sup = true,
                Some("subscript") => s.sub = true,
                _ => {}
            },
            "rFonts" => {
                let f = c.attr("ascii").or(c.attr("hAnsi")).unwrap_or("");
                if is_mono(f) {
                    s.code = true;
                }
            }
            _ => {}
        }
    }
    s
}

fn is_mono(f: &str) -> bool {
    let l = f.to_ascii_lowercase();
    [
        "consolas", "courier", "mono", "menlo", "monaco", "lucida console", "source code", "fira code",
        "cascadia", "inconsolata",
    ]
    .iter()
    .any(|m| l.contains(m))
}

fn resolve_run(id: &str, styles: &HashMap<String, Style>) -> RStyle {
    let mut chain = Vec::new();
    let mut cur = Some(id.to_string());
    while let Some(c) = cur {
        if chain.len() > 12 {
            break;
        }
        let Some(s) = styles.get(&c) else { break };
        chain.push(s.run.clone());
        cur = s.based_on.clone();
    }
    let mut out = RStyle::default();
    for r in chain.into_iter().rev() {
        out.bold |= r.bold;
        out.italic |= r.italic;
        out.strike |= r.strike;
        out.underline |= r.underline;
        out.code |= r.code;
    }
    out
}

struct Ctx<'a> {
    pkg: &'a Package,
    rels: HashMap<String, (String, bool)>,
    styles: HashMap<String, Style>,
    numbering: Numbering,
    footnotes: HashMap<String, Vec<Inline>>,
    doc: Document,
    notes: &'a mut Notes,
    image_names: HashMap<String, String>,
    unsupported_images: usize,
    spans: bool,
    textboxes: usize,
    footnote_refs: Vec<(String, usize)>,
}

fn parse_rels(xml: &str, limits: &Limits) -> HashMap<String, (String, bool)> {
    let mut out = HashMap::new();
    if let Ok(root) = xml_tree::parse(xml, limits) {
        for r in root.children() {
            if let (Some(id), Some(t)) = (r.attr("Id"), r.attr("Target")) {
                out.insert(
                    id.to_string(),
                    (t.to_string(), r.attr("TargetMode") == Some("External")),
                );
            }
        }
    }
    out
}

fn num_val(e: Option<&El>) -> Option<u32> {
    e.and_then(|e| e.attr("val")).and_then(|v| v.parse().ok())
}

fn load_styles(root: &El, out: &mut HashMap<String, Style>) {
    // document defaults are not needed: only differences from Normal matter
    for s in root.children().filter(|c| c.name == "style") {
        let Some(id) = s.attr("styleId") else { continue };
        let mut st = Style {
            name: s.child("name").and_then(|n| n.attr("val")).unwrap_or("").to_ascii_lowercase(),
            based_on: s.child("basedOn").and_then(|b| b.attr("val")).map(String::from),
            ..Style::default()
        };
        if let Some(ppr) = s.child("pPr") {
            st.outline = num_val(ppr.child("outlineLvl")).map(|n| (n + 1).min(6) as u8);
            if let Some(np) = ppr.child("numPr") {
                if let Some(n) = np.child("numId").and_then(|n| n.attr("val")) {
                    st.num = Some((n.to_string(), num_val(np.child("ilvl")).unwrap_or(0)));
                }
            }
        }
        if let Some(rpr) = s.child("rPr") {
            let dummy = HashMap::new();
            st.run = run_props(rpr, &RStyle::default(), &dummy);
        }
        // the look of links is not content
        if st.name == "hyperlink" || st.name == "followed hyperlink" {
            st.run = RStyle::default();
        }
        out.insert(id.to_string(), st);
    }
}

fn load_numbering(root: &El) -> Numbering {
    let mut n = Numbering::default();
    for a in root.children().filter(|c| c.name == "abstractNum") {
        let Some(id) = a.attr("abstractNumId") else { continue };
        let mut levels = HashMap::new();
        for l in a.children().filter(|c| c.name == "lvl") {
            let ilvl = l.attr("ilvl").and_then(|v| v.parse().ok()).unwrap_or(0);
            let fmt = l.child("numFmt").and_then(|f| f.attr("val")).unwrap_or("decimal");
            levels.insert(
                ilvl,
                Level {
                    bullet: fmt == "bullet" || fmt == "none",
                    start: num_val(l.child("start")).unwrap_or(1),
                },
            );
        }
        n.abstracts.insert(id.to_string(), levels);
    }
    for m in root.children().filter(|c| c.name == "num") {
        let (Some(id), Some(abs)) = (m.attr("numId"), m.child("abstractNumId").and_then(|a| a.attr("val"))) else {
            continue;
        };
        let mut over = HashMap::new();
        for o in m.children().filter(|c| c.name == "lvlOverride") {
            if let (Some(l), Some(s)) = (
                o.attr("ilvl").and_then(|v| v.parse().ok()),
                num_val(o.child("startOverride")),
            ) {
                over.insert(l, s);
            }
        }
        n.nums.insert(id.to_string(), (abs.to_string(), over));
    }
    n
}

pub fn read(data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let pkg = Package::read(data, limits)?;
    let main = main_part(&pkg, limits).unwrap_or_else(|| "word/document.xml".into());
    let Some(doc_xml) = pkg.text(&main) else {
        return Err(Error::new(ErrorCode::CorruptFile, "the DOCX has no main document part"));
    };
    let dir = main.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let rels_path = format!("{dir}/_rels/{}.rels", main.rsplit('/').next().unwrap_or("document.xml"));
    let rels = pkg.text(&rels_path).map(|x| parse_rels(&x, limits)).unwrap_or_default();
    let mut styles = HashMap::new();
    let mut numbering = Numbering::default();
    for (id, (target, ext)) in &rels {
        let _ = id;
        if *ext {
            continue;
        }
        let p = join(&dir, target);
        if p.ends_with("styles.xml") {
            if let Some(x) = pkg.text(&p) {
                if let Ok(r) = xml_tree::parse(&x, limits) {
                    load_styles(&r, &mut styles);
                }
            }
        } else if p.ends_with("numbering.xml") {
            if let Some(x) = pkg.text(&p) {
                if let Ok(r) = xml_tree::parse(&x, limits) {
                    numbering = load_numbering(&r);
                }
            }
        }
    }
    let mut cx = Ctx {
        pkg: &pkg,
        rels,
        styles,
        numbering,
        footnotes: HashMap::new(),
        doc: Document::default(),
        notes,
        image_names: HashMap::new(),
        unsupported_images: 0,
        spans: false,
        textboxes: 0,
        footnote_refs: Vec::new(),
    };
    cx.doc.meta = core_meta(&pkg, limits);
    // footnotes
    let fpath = format!("{dir}/footnotes.xml");
    if let Some(x) = pkg.text(&fpath) {
        if let Ok(r) = xml_tree::parse(&x, limits) {
            for f in r.children().filter(|c| c.name == "footnote") {
                let id = f.attr("id").unwrap_or("").to_string();
                if f.attr("type").is_some() {
                    continue;
                }
                let mut inl = Vec::new();
                for p in f.children().filter(|c| c.name == "p") {
                    if !inl.is_empty() {
                        inl.push(Inline::Text(" ".into()));
                    }
                    let mut para = Para::default();
                    cx.paragraph_runs(p, &dir, &mut para, &RStyle::default());
                    inl.extend(para.inlines);
                }
                cx.footnotes.insert(id, inl);
            }
        }
    }
    let root = xml_tree::parse(&doc_xml, limits)?;
    let body = root.child("body").unwrap_or(&root);
    let mut b = Builder::default();
    cx.body(body, &dir, &mut b);
    let mut blocks = b.finish();
    if !cx.footnote_refs.is_empty() {
        blocks.push(Block::Rule);
        let refs = std::mem::take(&mut cx.footnote_refs);
        for (id, n) in refs {
            let mut inl = vec![Inline::Text(format!("{n}. "))];
            if let Some(t) = cx.footnotes.get(&id) {
                inl.extend(t.clone());
            }
            blocks.push(Block::Paragraph(inl));
        }
    }
    cx.doc.blocks = blocks;
    if cx.doc.meta.title.is_none() {
        if let Some(Block::Heading { content, .. }) = cx.doc.blocks.first() {
            let t = crate::model::plain(content);
            if !t.is_empty() {
                cx.doc.meta.title = Some(t);
            }
        }
    }
    if cx.spans {
        cx.notes.push(Note::new(
            Severity::Warning,
            "document.table_spans",
            "merged table cells were not kept; each cell stands alone",
        ));
    }
    if cx.unsupported_images > 0 {
        let n = cx.unsupported_images;
        cx.notes.push(Note::new(
            Severity::Warning,
            "document.images_dropped",
            format!("{n} picture(s) in a format that cannot be read (for example EMF or WMF) were left out"),
        ));
    }
    if cx.textboxes > 0 {
        let n = cx.textboxes;
        cx.notes.push(Note::new(
            Severity::Info,
            "document.text_boxes",
            format!("{n} text box(es) were read as ordinary paragraphs after the text around them"),
        ));
    }
    Ok(cx.doc)
}

fn join(dir: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') {
        return t.to_string();
    }
    let mut parts: Vec<&str> = if dir.is_empty() { vec![] } else { dir.split('/').collect() };
    for seg in target.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn main_part(pkg: &Package, limits: &Limits) -> Option<String> {
    let x = pkg.text("_rels/.rels")?;
    let r = xml_tree::parse(&x, limits).ok()?;
    for rel in r.children() {
        if rel.attr("Type").is_some_and(|t| t.ends_with("/officeDocument")) {
            return rel.attr("Target").map(|t| t.trim_start_matches('/').to_string());
        }
    }
    None
}

fn core_meta(pkg: &Package, limits: &Limits) -> Meta {
    let mut m = Meta::default();
    let Some(x) = pkg.text("docProps/core.xml") else { return m };
    let Ok(r) = xml_tree::parse(&x, limits) else { return m };
    for c in r.children() {
        let t = c.text().trim().to_string();
        if t.is_empty() {
            continue;
        }
        match c.name.as_str() {
            "title" => m.title = Some(t),
            "creator" => m.author = Some(t),
            "subject" | "description" => m.subject = Some(t),
            "language" => m.language = Some(t),
            "created" => m.date = Some(t.split('T').next().unwrap_or(&t).to_string()),
            _ => {}
        }
    }
    m
}

#[derive(Default)]
struct Para {
    inlines: Vec<Inline>,
    /// Blocks that must follow this paragraph (text box contents, page breaks).
    trailing: Vec<Block>,
    /// A page break occurred before the text that follows it.
    breaks: Vec<(usize, Block)>,
    all_code: bool,
    has_text: bool,
}

impl Ctx<'_> {
    fn style_chain(&self, id: &str) -> Vec<&Style> {
        let mut v = Vec::new();
        let mut cur = Some(id.to_string());
        while let Some(c) = cur {
            if v.len() > 12 {
                break;
            }
            let Some(s) = self.styles.get(&c) else { break };
            v.push(s);
            cur = s.based_on.clone();
        }
        v
    }

    fn body(&mut self, body: &El, dir: &str, b: &mut Builder) {
        for c in body.children() {
            match c.name.as_str() {
                "p" => self.paragraph(c, dir, b),
                "tbl" => {
                    if let Some(t) = self.table(c, dir) {
                        b.push(t);
                    }
                }
                "sdt" => {
                    if let Some(content) = c.child("sdtContent") {
                        self.body(content, dir, b);
                    }
                }
                "AlternateContent" => {
                    if let Some(ch) = c.child("Choice") {
                        self.body(ch, dir, b);
                    }
                }
                "ins" | "customXml" | "smartTag" => self.body(c, dir, b),
                _ => {}
            }
        }
    }

    fn paragraph(&mut self, p: &El, dir: &str, b: &mut Builder) {
        let ppr = p.child("pPr");
        let style_id = ppr.and_then(|x| x.child("pStyle")).and_then(|s| s.attr("val")).unwrap_or("Normal");
        let chain = self.style_chain(style_id);
        let names: Vec<String> = chain.iter().map(|s| s.name.clone()).collect();
        let name0 = names.first().cloned().unwrap_or_default();
        let mut level = None;
        for n in &names {
            if let Some(rest) = n.strip_prefix("heading ") {
                level = rest.trim().parse::<u8>().ok().map(|l| l.clamp(1, 6));
                break;
            }
            if n == "title" {
                level = Some(1);
                break;
            }
            if n == "subtitle" {
                level = Some(2);
                break;
            }
        }
        if level.is_none() {
            level = chain.iter().find_map(|s| s.outline);
        }
        if let Some(l) = ppr.and_then(|x| x.child("outlineLvl")).and_then(|o| num_val(Some(o))) {
            level = Some((l + 1).min(6) as u8);
        }
        let is_code_style = names.iter().any(|n| {
            matches!(n.as_str(), "code" | "source code" | "html preformatted" | "preformatted text" | "macro text" | "plain text")
                || n.starts_with("code ")
        });
        let is_quote_style = names.iter().any(|n| matches!(n.as_str(), "quote" | "block text" | "intense quote" | "block quotation"));
        let mut num = ppr
            .and_then(|x| x.child("numPr"))
            .and_then(|n| n.child("numId").and_then(|i| i.attr("val")).map(|v| (v.to_string(), num_val(n.child("ilvl")))));
        if let Some((n, il)) = &mut num {
            if il.is_none() {
                *il = chain.iter().find_map(|s| s.num.as_ref().map(|x| x.1)).or(Some(0));
            }
            let _ = n;
        }
        if num.is_none() {
            if let Some(s) = chain.iter().find_map(|s| s.num.clone()) {
                num = Some((s.0, Some(s.1)));
            }
        }
        let base_run = chain.iter().rev().fold(RStyle::default(), |mut acc, s| {
            acc.bold |= s.run.bold;
            acc.italic |= s.run.italic;
            acc.strike |= s.run.strike;
            acc.underline |= s.run.underline;
            acc.code |= s.run.code;
            acc
        });
        let mut para = Para::default();
        // headings are bold by style: do not repeat that in the text
        let mut base = base_run.clone();
        if level.is_some() {
            base.bold = false;
            base.italic = false;
        }
        if is_code_style {
            base.code = false;
        }
        if is_quote_style {
            base.italic = false;
        }
        self.paragraph_runs(p, dir, &mut para, &base);
        let page_before = ppr.is_some_and(|x| x.child("pageBreakBefore").is_some_and(on));
        let border_rule = ppr.is_some_and(|x| x.child("pBdr").is_some_and(|d| d.child("bottom").is_some()));
        let _ = name0;
        // trim inline edges
        trim_edges(&mut para.inlines);
        let text_empty = para.inlines.iter().all(|i| matches!(i, Inline::Text(t) if t.trim().is_empty()) || matches!(i, Inline::LineBreak));
        if page_before {
            b.push(Block::PageBreak);
        }
        let jc_center = false;
        let _ = jc_center;
        // emit
        let breaks = std::mem::take(&mut para.breaks);
        if text_empty && para.trailing.is_empty() && breaks.is_empty() {
            if border_rule {
                b.push(Block::Rule);
            } else if num.is_none() {
                // a blank paragraph ends a code or quote run
                b.flush_code();
                b.flush_quote();
                b.close_lists();
            }
            return;
        }
        if !breaks.is_empty() && !para.inlines.is_empty() {
            // split the paragraph at page breaks
            let mut parts: Vec<Vec<Inline>> = vec![Vec::new()];
            let mut at = 0;
            let mut bs: Vec<Block> = Vec::new();
            for (i, inl) in para.inlines.clone().into_iter().enumerate() {
                while at < breaks.len() && breaks[at].0 == i {
                    bs.push(breaks[at].1.clone());
                    parts.push(Vec::new());
                    at += 1;
                }
                if let Some(last) = parts.last_mut() {
                    last.push(inl);
                }
            }
            while at < breaks.len() {
                bs.push(breaks[at].1.clone());
                parts.push(Vec::new());
                at += 1;
            }
            for (k, part) in parts.into_iter().enumerate() {
                if k > 0 {
                    if let Some(x) = bs.get(k - 1) {
                        b.push(x.clone());
                    }
                }
                if !part.is_empty() {
                    b.push(Block::Paragraph(part));
                }
            }
            for t in para.trailing {
                b.push(t);
            }
            return;
        }
        if let Some((numid, ilvl)) = num.filter(|(n, _)| n != "0") {
            b.flush_code();
            b.flush_quote();
            let ilvl = ilvl.unwrap_or(0) as usize;
            let (bullet, start) = self.numbering.level(&numid, ilvl as u32);
            let mut inl = para.inlines;
            let mut checked = None;
            if let Some(Inline::Text(t)) = inl.first_mut() {
                for (m, v) in [("\u{2610} ", false), ("\u{2611} ", true), ("\u{2612} ", true)] {
                    if let Some(r) = t.strip_prefix(m) {
                        checked = Some(v);
                        *t = r.to_string();
                        break;
                    }
                }
            }
            b.list_item(
                ilvl,
                &numid,
                !bullet,
                start,
                ListItem {
                    checked,
                    blocks: if inl.is_empty() { vec![] } else { vec![Block::Paragraph(inl)] },
                },
            );
        } else if let Some(l) = level {
            b.push(Block::Heading {
                level: l,
                content: para.inlines,
            });
        } else if is_code_style || para.all_code {
            b.close_lists();
            b.flush_quote();
            let t = crate::model::plain(&para.inlines);
            match &mut b.code {
                Some(c) => {
                    c.push_str(&t);
                    c.push('\n');
                }
                None => b.code = Some(format!("{t}\n")),
            }
        } else if is_quote_style {
            b.close_lists();
            b.flush_code();
            b.quote.push(Block::Paragraph(para.inlines));
        } else {
            b.push(Block::Paragraph(para.inlines));
        }
        for t in para.trailing {
            b.push(t);
        }
    }

    fn paragraph_runs(&mut self, p: &El, dir: &str, para: &mut Para, base: &RStyle) {
        let mut fields: Vec<FieldState> = Vec::new();
        let mut link: Option<(String, String)> = None; // (url, collected text so far unused)
        let mut collected: Vec<Inline> = Vec::new();
        self.runs_in(p, dir, para, base, &mut fields, &mut link, &mut collected);
        para.inlines.extend(collected);
        // all runs monospace and some text: a code line even without a code style
        para.all_code = para.has_text && para.inlines.iter().all(|i| matches!(i, Inline::Code(_)));
    }

    #[allow(clippy::too_many_arguments)]
    fn runs_in(
        &mut self,
        parent: &El,
        dir: &str,
        para: &mut Para,
        base: &RStyle,
        fields: &mut Vec<FieldState>,
        link: &mut Option<(String, String)>,
        out: &mut Vec<Inline>,
    ) {
        for c in parent.children() {
            match c.name.as_str() {
                "r" => self.run(c, dir, para, base, fields, link, out),
                "hyperlink" => {
                    let url = c
                        .attr("id")
                        .and_then(|id| self.rels.get(id))
                        .map(|(t, _)| t.clone())
                        .or_else(|| c.attr("anchor").map(|a| format!("#{a}")));
                    let mut inner = Vec::new();
                    let mut none = None;
                    self.runs_in(c, dir, para, base, fields, &mut none, &mut inner);
                    match url {
                        Some(u) if !inner.is_empty() => out.push(Inline::Link {
                            url: u,
                            title: String::new(),
                            content: inner,
                        }),
                        _ => out.extend(inner),
                    }
                }
                "fldSimple" => {
                    let instr = c.attr("instr").unwrap_or("").to_string();
                    let mut inner = Vec::new();
                    let mut none = None;
                    self.runs_in(c, dir, para, base, fields, &mut none, &mut inner);
                    match hyperlink_target(&instr) {
                        Some(u) if !inner.is_empty() => out.push(Inline::Link {
                            url: u,
                            title: String::new(),
                            content: inner,
                        }),
                        _ => out.extend(inner),
                    }
                }
                "ins" | "smartTag" | "sdt" | "sdtContent" | "customXml" | "moveTo" => {
                    self.runs_in(c, dir, para, base, fields, link, out)
                }
                "AlternateContent" => {
                    if let Some(ch) = c.child("Choice") {
                        self.runs_in(ch, dir, para, base, fields, link, out);
                    }
                }
                _ => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        r: &El,
        dir: &str,
        para: &mut Para,
        base: &RStyle,
        fields: &mut Vec<FieldState>,
        link: &mut Option<(String, String)>,
        out: &mut Vec<Inline>,
    ) {
        let st = match r.child("rPr") {
            Some(p) => run_props(p, base, &self.styles),
            None => base.clone(),
        };
        let mut buf = String::new();
        let wrap = |inl: Inline, st: &RStyle| -> Inline { style_wrap(inl, st) };
        let flush = |buf: &mut String, out: &mut Vec<Inline>, para: &mut Para| {
            if !buf.is_empty() {
                let t = std::mem::take(buf);
                if t.chars().any(|c| !c.is_whitespace()) {
                    para.has_text = true;
                }
                let inl = if st.code {
                    Inline::Code(t)
                } else {
                    wrap(Inline::Text(t), &st)
                };
                let inl = if st.code { wrap_code(inl, &st) } else { inl };
                push_inline(out, inl);
            }
        };
        for c in r.children() {
            match c.name.as_str() {
                "t" => {
                    if let Some(f) = fields.last_mut() {
                        if f.in_instr {
                            continue;
                        }
                    }
                    buf.push_str(&c.text());
                }
                "instrText" => {
                    if let Some(f) = fields.last_mut() {
                        f.instr.push_str(&c.text());
                    }
                }
                "tab" => buf.push('\t'),
                "noBreakHyphen" => buf.push('\u{2011}'),
                "cr" => {
                    flush(&mut buf, out, para);
                    out.push(Inline::LineBreak);
                }
                "br" => match c.attr("type") {
                    Some("page") => {
                        flush(&mut buf, out, para);
                        let at = para.inlines.len() + out.len();
                        para.breaks.push((at, Block::PageBreak));
                    }
                    Some("column") => {}
                    _ => {
                        flush(&mut buf, out, para);
                        out.push(Inline::LineBreak);
                    }
                },
                "fldChar" => {
                    flush(&mut buf, out, para);
                    match c.attr("fldCharType") {
                        Some("begin") => fields.push(FieldState {
                            instr: String::new(),
                            in_instr: true,
                            start: out.len(),
                        }),
                        Some("separate") => {
                            if let Some(f) = fields.last_mut() {
                                f.in_instr = false;
                                f.start = out.len();
                            }
                        }
                        Some("end") => {
                            if let Some(f) = fields.pop() {
                                if let Some(u) = hyperlink_target(&f.instr) {
                                    let inner: Vec<Inline> = out.drain(f.start.min(out.len())..).collect();
                                    if !inner.is_empty() {
                                        out.push(Inline::Link {
                                            url: u,
                                            title: String::new(),
                                            content: inner,
                                        });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                "footnoteReference" => {
                    flush(&mut buf, out, para);
                    if let Some(id) = c.attr("id") {
                        let n = self.footnote_refs.len() + 1;
                        self.footnote_refs.push((id.to_string(), n));
                        out.push(Inline::Sup(vec![Inline::Text(format!("[{n}]"))]));
                    }
                }
                "drawing" | "pict" | "object" => {
                    flush(&mut buf, out, para);
                    self.picture(c, dir, para, out);
                }
                "AlternateContent" => {
                    if let Some(ch) = c.child("Choice") {
                        for d in ch.children() {
                            if d.name == "drawing" || d.name == "pict" {
                                flush(&mut buf, out, para);
                                self.picture(d, dir, para, out);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        flush(&mut buf, out, para);
        let _ = link;
    }

    fn picture(&mut self, d: &El, dir: &str, para: &mut Para, out: &mut Vec<Inline>) {
        // text boxes
        fn find<'a>(e: &'a El, name: &str, acc: &mut Vec<&'a El>) {
            for c in e.children() {
                if c.name == name {
                    acc.push(c);
                } else {
                    find(c, name, acc);
                }
            }
        }
        let mut boxes = Vec::new();
        find(d, "txbxContent", &mut boxes);
        for bx in boxes {
            self.textboxes += 1;
            let mut b = Builder::default();
            self.body(bx, dir, &mut b);
            para.trailing.extend(b.finish());
        }
        let mut blips = Vec::new();
        find(d, "blip", &mut blips);
        let mut ids: Vec<String> = blips.iter().filter_map(|b| b.attr("embed").map(String::from)).collect();
        let mut imagedata = Vec::new();
        find(d, "imagedata", &mut imagedata);
        ids.extend(imagedata.iter().filter_map(|b| b.attr("id").map(String::from)));
        let mut docpr = Vec::new();
        find(d, "docPr", &mut docpr);
        let alt = docpr.first().and_then(|p| p.attr("descr")).unwrap_or("").to_string();
        for id in ids {
            let Some((target, ext)) = self.rels.get(&id).cloned() else { continue };
            if ext {
                continue;
            }
            let path = join(dir, &target);
            if let Some(name) = self.image_names.get(&path) {
                out.push(Inline::Image {
                    src: name.clone(),
                    title: String::new(),
                    alt: if alt.is_empty() { vec![] } else { vec![Inline::Text(alt.clone())] },
                });
                continue;
            }
            let Some(data) = self.pkg.get(&path) else { continue };
            match imgsize::mime_of(data) {
                Some(m) => {
                    let name = self.doc.add_image(m, data.to_vec());
                    self.image_names.insert(path, name.clone());
                    out.push(Inline::Image {
                        src: name,
                        title: String::new(),
                        alt: if alt.is_empty() { vec![] } else { vec![Inline::Text(alt.clone())] },
                    });
                }
                None => self.unsupported_images += 1,
            }
        }
    }

    fn table(&mut self, t: &El, dir: &str) -> Option<Block> {
        let mut rows = Vec::new();
        for (n, tr) in t.children().filter(|c| c.name == "tr").enumerate() {
            let header = tr.child("trPr").is_some_and(|p| p.child("tblHeader").is_some());
            let mut row = Vec::new();
            for tc in tr.children().filter(|c| c.name == "tc") {
                if tc.child("tcPr").is_some_and(|p| {
                    p.child("vMerge").is_some() || p.child("gridSpan").is_some_and(|g| g.attr("val") != Some("1"))
                }) {
                    self.spans = true;
                }
                let mut b = Builder::default();
                self.body(tc, dir, &mut b);
                let blocks = b.finish();
                let mut content = Vec::new();
                for (k, blk) in blocks.iter().enumerate() {
                    if k > 0 {
                        content.push(Inline::LineBreak);
                    }
                    cell_text(blk, &mut content);
                }
                let align = tc
                    .child("p")
                    .and_then(|p| p.child("pPr"))
                    .and_then(|p| p.child("jc"))
                    .and_then(|j| j.attr("val"))
                    .map(|v| match v {
                        "center" => Align::Center,
                        "right" | "end" => Align::Right,
                        "left" | "start" => Align::Left,
                        _ => Align::None,
                    })
                    .unwrap_or_default();
                row.push(Cell {
                    header: header || n == 0 && rows.is_empty() && false,
                    align,
                    content,
                });
            }
            if !row.is_empty() {
                rows.push(row);
            }
        }
        if rows.is_empty() {
            return None;
        }
        // the first row is the header row for Markdown and HTML; its bold is the style's doing
        if let Some(first) = rows.first_mut() {
            for c in first {
                c.header = true;
                if let [Inline::Strong(inner)] = c.content.as_slice() {
                    c.content = inner.clone();
                }
            }
        }
        Some(Block::Table { rows })
    }
}

struct FieldState {
    instr: String,
    in_instr: bool,
    start: usize,
}

fn hyperlink_target(instr: &str) -> Option<String> {
    let t = instr.trim();
    let rest = t.strip_prefix("HYPERLINK")?.trim();
    if let Some(r) = rest.strip_prefix('"') {
        return r.split('"').next().map(String::from);
    }
    let first = rest.split_whitespace().next()?;
    if first == "\\l" {
        let anchor = rest.split('"').nth(1)?;
        return Some(format!("#{anchor}"));
    }
    Some(first.to_string())
}

fn wrap_code(i: Inline, _st: &RStyle) -> Inline {
    i
}

fn style_wrap(inl: Inline, st: &RStyle) -> Inline {
    let mut n = inl;
    if st.sup {
        n = Inline::Sup(vec![n]);
    }
    if st.sub {
        n = Inline::Sub(vec![n]);
    }
    if st.strike {
        n = Inline::Strike(vec![n]);
    }
    if st.underline {
        n = Inline::Underline(vec![n]);
    }
    if st.italic {
        n = Inline::Emph(vec![n]);
    }
    if st.bold {
        n = Inline::Strong(vec![n]);
    }
    n
}

/// Add an inline, merging neighbouring runs with the same formatting.
fn push_inline(out: &mut Vec<Inline>, i: Inline) {
    match (out.last_mut(), &i) {
        (Some(Inline::Text(a)), Inline::Text(b)) => a.push_str(b),
        (Some(Inline::Code(a)), Inline::Code(b)) => a.push_str(b),
        (Some(Inline::Strong(a)), Inline::Strong(b)) => merge_into(a, b.clone()),
        (Some(Inline::Emph(a)), Inline::Emph(b)) => merge_into(a, b.clone()),
        (Some(Inline::Strike(a)), Inline::Strike(b)) => merge_into(a, b.clone()),
        (Some(Inline::Underline(a)), Inline::Underline(b)) => merge_into(a, b.clone()),
        (Some(Inline::Sup(a)), Inline::Sup(b)) => merge_into(a, b.clone()),
        (Some(Inline::Sub(a)), Inline::Sub(b)) => merge_into(a, b.clone()),
        _ => out.push(i),
    }
}

fn merge_into(a: &mut Vec<Inline>, b: Vec<Inline>) {
    for x in b {
        push_inline(a, x);
    }
}

fn trim_edges(v: &mut Vec<Inline>) {
    while matches!(v.first(), Some(Inline::LineBreak)) {
        v.remove(0);
    }
    while matches!(v.last(), Some(Inline::LineBreak)) {
        v.pop();
    }
}

fn cell_text(b: &Block, out: &mut Vec<Inline>) {
    match b {
        Block::Paragraph(c) | Block::Heading { content: c, .. } => out.extend(c.iter().cloned()),
        Block::Code { text, .. } => out.push(Inline::Code(text.trim_end().to_string())),
        Block::Quote(bs) => {
            for (k, x) in bs.iter().enumerate() {
                if k > 0 {
                    out.push(Inline::LineBreak);
                }
                cell_text(x, out);
            }
        }
        Block::List { items, .. } => {
            for (k, it) in items.iter().enumerate() {
                if k > 0 {
                    out.push(Inline::LineBreak);
                }
                out.push(Inline::Text("\u{2022} ".into()));
                for x in &it.blocks {
                    cell_text(x, out);
                }
            }
        }
        Block::Table { rows } => {
            for r in rows {
                for c in r {
                    out.extend(c.content.iter().cloned());
                    out.push(Inline::Text(" ".into()));
                }
            }
        }
        _ => {}
    }
}
