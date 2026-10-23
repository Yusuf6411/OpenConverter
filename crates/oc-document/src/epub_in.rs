//! EPUB to the document model: the reading order of the package, chapter by chapter.
use crate::html_in;
use crate::model::{visit_images, Block, Document, Inline};
use crate::xml_tree::{self, El};
use crate::zipio::Package;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};
use std::collections::HashMap;

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|c| (*c as char).to_digit(16)),
                b.get(i + 2).and_then(|c| (*c as char).to_digit(16)),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve `href` against the directory of `base` inside the package.
fn resolve(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or("");
    let href = percent_decode(href);
    let mut parts: Vec<&str> = if href.starts_with('/') {
        Vec::new()
    } else {
        let mut p: Vec<&str> = base.split('/').collect();
        p.pop();
        p
    };
    for seg in href.split('/') {
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

fn text_of(e: Option<&El>) -> Option<String> {
    e.map(|e| e.text().trim().to_string())
        .filter(|t| !t.is_empty())
}

pub fn read(data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let pkg = Package::read(data, limits)?;
    let container = pkg.text("META-INF/container.xml").ok_or_else(|| {
        Error::new(
            ErrorCode::CorruptFile,
            "this EPUB has no META-INF/container.xml",
        )
    })?;
    let c = xml_tree::parse(&container, limits)?;
    let opf_path = c
        .child("rootfiles")
        .and_then(|r| r.child("rootfile"))
        .and_then(|r| r.attr("full-path"))
        .ok_or_else(|| Error::new(ErrorCode::CorruptFile, "this EPUB names no package file"))?
        .to_string();
    let opf_text = pkg.text(&opf_path).ok_or_else(|| {
        Error::new(
            ErrorCode::CorruptFile,
            "the package file is missing from this EPUB",
        )
    })?;
    let opf = xml_tree::parse(&opf_text, limits)?;
    let mut doc = Document::default();
    if let Some(md) = opf.child("metadata") {
        doc.meta.title = text_of(md.child("title"));
        doc.meta.author = text_of(md.child("creator"));
        doc.meta.subject = text_of(md.child("description"));
        doc.meta.language = text_of(md.child("language"));
        doc.meta.date =
            text_of(md.child("date")).map(|d| d.split('T').next().unwrap_or(&d).to_string());
    }
    let mut manifest: HashMap<String, (String, String)> = HashMap::new();
    if let Some(m) = opf.child("manifest") {
        for it in m.children().filter(|c| c.name == "item") {
            if let (Some(id), Some(href)) = (it.attr("id"), it.attr("href")) {
                manifest.insert(
                    id.to_string(),
                    (
                        href.to_string(),
                        it.attr("media-type").unwrap_or("").to_string(),
                    ),
                );
            }
        }
    }
    let spine: Vec<String> = opf
        .child("spine")
        .map(|s| {
            s.children()
                .filter(|c| c.name == "itemref" && c.attr("linear") != Some("no"))
                .filter_map(|c| c.attr("idref").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if spine.is_empty() {
        return Err(Error::new(
            ErrorCode::CorruptFile,
            "this EPUB has an empty reading order",
        ));
    }
    let mut image_map: HashMap<String, String> = HashMap::new();
    let mut missing = 0u32;
    let mut chapters = 0;
    for idref in spine {
        let Some((href, mt)) = manifest.get(&idref) else {
            continue;
        };
        if !(mt.contains("html") || mt.contains("xml")) {
            continue;
        }
        let path = resolve(&opf_path, href);
        let Some(bytes) = pkg.get(&path) else {
            continue;
        };
        let html = String::from_utf8_lossy(bytes).into_owned();
        let mut scratch = Notes::new();
        let mut ch = html_in::read(&html, &mut scratch, limits.max_depth as usize);
        for n in scratch.iter() {
            if n.code != "document.external_images" {
                notes.push(n.clone());
            }
        }
        // pictures: embedded ones move into the book, packaged ones are loaded
        let chapter_images = std::mem::take(&mut ch.images);
        let mut blocks = ch.blocks;
        visit_images(&mut blocks, &mut |i| {
            if let Inline::Image { src, .. } = i {
                if let Some(img) = chapter_images.iter().find(|x| &x.name == src) {
                    *src = doc.add_image(&img.mime, img.data.clone());
                    return;
                }
                let p = resolve(&path, src);
                if let Some(n) = image_map.get(&p) {
                    *src = n.clone();
                } else if let Some(d) = pkg.get(&p) {
                    match crate::imgsize::mime_of(d) {
                        Some(m) => {
                            let n = doc.add_image(m, d.to_vec());
                            image_map.insert(p, n.clone());
                            *src = n;
                        }
                        None => missing += 1,
                    }
                } else {
                    missing += 1;
                }
            }
        });
        if chapters > 0 && !blocks.is_empty() {
            doc.blocks.push(Block::PageBreak);
        }
        if !blocks.is_empty() {
            chapters += 1;
        }
        doc.blocks.extend(blocks);
        if doc.meta.title.is_none() {
            doc.meta.title = ch.meta.title;
        }
    }
    if missing > 0 {
        notes.push(Note::new(
            Severity::Loss,
            "epub.images_missing",
            format!("{missing} picture(s) were missing or in a format that is not carried"),
        ));
    }
    Ok(doc)
}
