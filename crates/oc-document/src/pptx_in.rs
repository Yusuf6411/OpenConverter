//! PPTX slide decks to the document model: each slide becomes a titled section with its text,
//! lists, tables and pictures, in slide order.
use crate::imgsize;
use crate::model::{Block, Cell, Document, Inline, ListItem};
use crate::xml_tree::{self, El};
use crate::zipio::Package;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};
use std::collections::HashMap;

fn rels(pkg: &Package, path: &str, limits: &Limits) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let Some(x) = pkg.text(path) else { return m };
    let Ok(r) = xml_tree::parse(&x, limits) else {
        return m;
    };
    for c in r.children() {
        if let (Some(id), Some(t)) = (c.attr("Id"), c.attr("Target")) {
            m.insert(id.to_string(), t.to_string());
        }
    }
    m
}

fn join(dir: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') {
        return t.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

struct Cx<'a> {
    doc: Document,
    pkg: &'a Package,
    rels: HashMap<String, String>,
    images: HashMap<String, String>,
    skipped: u32,
}

fn runs(p: &El) -> Vec<Inline> {
    let mut out = Vec::new();
    for r in p.children() {
        match r.name.as_str() {
            "r" => {
                let t = r.child("t").map(El::text).unwrap_or_default();
                if t.is_empty() {
                    continue;
                }
                let pr = r.child("rPr");
                let flag = |n: &str| {
                    pr.and_then(|p| p.attr(n))
                        .is_some_and(|v| v == "1" || v == "true")
                };
                let mut inl = vec![Inline::Text(t)];
                if let Some(link) = pr.and_then(|p| p.child("hlinkClick")) {
                    let _ = link;
                }
                if flag("i") {
                    inl = vec![Inline::Emph(inl)];
                }
                if flag("b") {
                    inl = vec![Inline::Strong(inl)];
                }
                out.extend(inl);
            }
            "br" => out.push(Inline::LineBreak),
            "fld" => {
                if let Some(t) = r.child("t") {
                    out.push(Inline::Text(t.text()));
                }
            }
            _ => {}
        }
    }
    out
}

impl Cx<'_> {
    fn text_body(&mut self, tx: &El, bullets: bool, out: &mut Vec<Block>) {
        let mut items: Vec<ListItem> = Vec::new();
        let flush = |items: &mut Vec<ListItem>, out: &mut Vec<Block>| {
            if !items.is_empty() {
                out.push(Block::List {
                    ordered: false,
                    start: 1,
                    tight: true,
                    items: std::mem::take(items),
                });
            }
        };
        for p in tx.children().filter(|c| c.name == "p") {
            let inl = runs(p);
            if inl.is_empty() {
                continue;
            }
            let ppr = p.child("pPr");
            let none = ppr.is_some_and(|x| x.child("buNone").is_some());
            let has_bullet =
                ppr.is_some_and(|x| x.child("buChar").is_some() || x.child("buAutoNum").is_some());
            if (bullets && !none) || has_bullet {
                items.push(ListItem {
                    checked: None,
                    blocks: vec![Block::Paragraph(inl)],
                });
            } else {
                flush(&mut items, out);
                out.push(Block::Paragraph(inl));
            }
        }
        flush(&mut items, out);
    }

    fn picture(&mut self, pic: &El, base: &str, out: &mut Vec<Block>) {
        let rid = pic
            .child("blipFill")
            .and_then(|b| b.child("blip"))
            .and_then(|b| {
                b.attrs
                    .iter()
                    .find(|(k, _)| k == "embed")
                    .map(|(_, v)| v.clone())
            });
        let Some(rid) = rid else { return };
        let Some(target) = self.rels.get(&rid).cloned() else {
            return;
        };
        let path = join(base, &target);
        let name = if let Some(n) = self.images.get(&path) {
            n.clone()
        } else {
            match self
                .pkg
                .get(&path)
                .and_then(|d| imgsize::mime_of(d).map(|m| (m, d)))
            {
                Some((m, d)) => {
                    let n = self.doc.add_image(m, d.to_vec());
                    self.images.insert(path, n.clone());
                    n
                }
                None => {
                    self.skipped += 1;
                    return;
                }
            }
        };
        out.push(Block::Paragraph(vec![Inline::Image {
            src: name,
            title: String::new(),
            alt: vec![],
        }]));
    }

    fn shapes(
        &mut self,
        parent: &El,
        base: &str,
        title: &mut Option<Vec<Inline>>,
        out: &mut Vec<Block>,
        depth: usize,
    ) {
        if depth > 8 {
            return;
        }
        for sh in parent.children() {
            match sh.name.as_str() {
                "sp" => {
                    let ph = sh
                        .child("nvSpPr")
                        .and_then(|n| n.child("nvPr"))
                        .and_then(|n| n.child("ph"));
                    let kind = ph.and_then(|p| p.attr("type")).unwrap_or("");
                    let Some(tx) = sh.child("txBody") else {
                        continue;
                    };
                    if matches!(kind, "title" | "ctrTitle") {
                        let mut t = Vec::new();
                        for p in tx.children().filter(|c| c.name == "p") {
                            if !t.is_empty() {
                                t.push(Inline::Text(" ".into()));
                            }
                            t.extend(runs(p));
                        }
                        if title.is_none() && !t.is_empty() {
                            *title = Some(t);
                        }
                    } else if matches!(kind, "sldNum" | "dt" | "ftr") {
                    } else {
                        let bullets = ph.is_some() && matches!(kind, "" | "body" | "obj");
                        self.text_body(tx, bullets, out);
                    }
                }
                "pic" => self.picture(sh, base, out),
                "grpSp" => self.shapes(sh, base, title, out, depth + 1),
                "graphicFrame" => {
                    let tbl = sh
                        .child("graphic")
                        .and_then(|g| g.child("graphicData"))
                        .and_then(|g| g.child("tbl"));
                    if let Some(t) = tbl {
                        let mut rows: Vec<Vec<Cell>> = Vec::new();
                        for (ri, tr) in t.children().filter(|c| c.name == "tr").enumerate() {
                            let mut cells = Vec::new();
                            for tc in tr.children().filter(|c| c.name == "tc") {
                                let mut content = Vec::new();
                                if let Some(tx) = tc.child("txBody") {
                                    for p in tx.children().filter(|c| c.name == "p") {
                                        if !content.is_empty() {
                                            content.push(Inline::LineBreak);
                                        }
                                        content.extend(runs(p));
                                    }
                                }
                                cells.push(Cell {
                                    header: ri == 0,
                                    align: Default::default(),
                                    content,
                                });
                            }
                            rows.push(cells);
                        }
                        if !rows.is_empty() {
                            out.push(Block::Table { rows });
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

pub fn read(data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let pkg = Package::read(data, limits)?;
    let pres = pkg
        .text("ppt/presentation.xml")
        .ok_or_else(|| Error::new(ErrorCode::CorruptFile, "this PPTX has no presentation.xml"))?;
    let pres = xml_tree::parse(&pres, limits)?;
    let prels = rels(&pkg, "ppt/_rels/presentation.xml.rels", limits);
    let mut slides: Vec<String> = Vec::new();
    if let Some(l) = pres.child("sldIdLst") {
        for s in l.children().filter(|c| c.name == "sldId") {
            // the relationship id is the last `id` attribute (`r:id`)
            let rid = s
                .attrs
                .iter()
                .rev()
                .find(|(k, _)| k == "id")
                .map(|(_, v)| v.clone());
            if let Some(t) = rid.and_then(|r| prels.get(&r)) {
                slides.push(join("ppt", t));
            }
        }
    }
    if slides.is_empty() {
        let mut v: Vec<(u32, String)> = pkg
            .entries
            .iter()
            .filter_map(|(n, _)| {
                let k = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
                Some((k.parse().ok()?, n.clone()))
            })
            .collect();
        v.sort();
        slides = v.into_iter().map(|(_, n)| n).collect();
    }
    if slides.is_empty() {
        return Err(Error::new(
            ErrorCode::CorruptFile,
            "this PPTX has no slides",
        ));
    }
    let mut cx = Cx {
        doc: Document::default(),
        pkg: &pkg,
        rels: HashMap::new(),
        images: HashMap::new(),
        skipped: 0,
    };
    if let Some(core) = pkg
        .text("docProps/core.xml")
        .and_then(|x| xml_tree::parse(&x, limits).ok())
    {
        for c in core.children() {
            let t = c.text().trim().to_string();
            match c.name.as_str() {
                "title" if !t.is_empty() => cx.doc.meta.title = Some(t),
                "creator" if !t.is_empty() => cx.doc.meta.author = Some(t),
                _ => {}
            }
        }
    }
    let mut blocks: Vec<Block> = Vec::new();
    for (n, path) in slides.iter().enumerate() {
        let Some(x) = pkg.text(path) else { continue };
        let Ok(root) = xml_tree::parse(&x, limits) else {
            continue;
        };
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
        let file = path.rsplit('/').next().unwrap_or("");
        cx.rels = rels(&pkg, &format!("{dir}/_rels/{file}.rels"), limits);
        let mut title = None;
        let mut body = Vec::new();
        if let Some(tree) = root.child("cSld").and_then(|c| c.child("spTree")) {
            cx.shapes(tree, &dir, &mut title, &mut body, 0);
        }
        if n > 0 {
            blocks.push(Block::PageBreak);
        }
        blocks.push(Block::Heading {
            level: 1,
            content: title.unwrap_or_else(|| vec![Inline::Text(format!("Slide {}", n + 1))]),
        });
        blocks.extend(body);
    }
    notes.push(Note::new(
        Severity::Info,
        "pptx.text_only",
        "slides were read as text, lists, tables and pictures; layout, shapes, charts, animations and speaker notes are not carried",
    ));
    if cx.skipped > 0 {
        notes.push(Note::new(
            Severity::Loss,
            "pptx.images_skipped",
            format!(
                "{} picture(s) were in a format that is not carried",
                cx.skipped
            ),
        ));
    }
    cx.doc.blocks = blocks;
    Ok(cx.doc)
}
