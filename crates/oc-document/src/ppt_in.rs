//! Legacy PowerPoint presentations (`.ppt`, 97-2003): the text of each slide in order, titles as
//! headings and body text as lists. Not read: layout, pictures, tables, notes, encrypted files.
#![allow(clippy::chunks_exact_to_as_chunks)]
use crate::model::{Block, Document, Inline, ListItem};
use oc_archive::cfb::Cfb;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

fn corrupt(m: &str) -> Error {
    Error::new(ErrorCode::CorruptFile, m.to_string())
}

struct Atom {
    kind: u16,
    inst: u16,
    ver: u8,
    start: usize,
    end: usize,
}

fn atoms(d: &[u8], from: usize, to: usize, out: &mut Vec<(usize, Atom)>, depth: usize) {
    let mut p = from;
    while p + 8 <= to && depth < 24 {
        let vi = u16::from_le_bytes([d[p], d[p + 1]]);
        let kind = u16::from_le_bytes([d[p + 2], d[p + 3]]);
        let len = u32::from_le_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]) as usize;
        let end = (p + 8).saturating_add(len).min(to);
        let a = Atom {
            kind,
            inst: vi >> 4,
            ver: (vi & 0xF) as u8,
            start: p + 8,
            end,
        };
        let container = a.ver == 0xF;
        out.push((depth, a));
        if container {
            atoms(d, p + 8, end, out, depth + 1);
        }
        p = end;
    }
}

fn text_of(d: &[u8], a: &Atom) -> String {
    let b = &d[a.start..a.end];
    let s: String = if a.kind == 0x0FA0 {
        let units: Vec<u16> = b
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        b.iter().map(|c| char::from(*c)).collect()
    };
    s
}

pub fn read(data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let cfb = Cfb::open(data, limits)?;
    let ppt = cfb
        .stream("PowerPoint Document")
        .ok_or_else(|| corrupt("this compound file holds no PowerPoint document"))?;
    if cfb.has_stream("EncryptedSummary") {
        return Err(Error::new(
            ErrorCode::EncryptedNeedPassword,
            "this presentation is password protected",
        ));
    }
    let mut all = Vec::new();
    atoms(&ppt, 0, ppt.len(), &mut all, 0);
    // slides: SlideListWithText (instance 0) holds a SlidePersistAtom per slide, then its text
    let mut slides: Vec<Vec<(u32, String)>> = Vec::new();
    let mut in_slides = false;
    let mut slide_depth = 0;
    let mut cur_type: u32 = 1;
    for (depth, a) in &all {
        match a.kind {
            0x0FF0 => {
                in_slides = a.inst == 0;
                slide_depth = *depth;
            }
            _ if in_slides && *depth <= slide_depth => in_slides = false,
            _ => {}
        }
        if !in_slides {
            continue;
        }
        match a.kind {
            0x03F3 => slides.push(Vec::new()),
            0x0F9F => {
                cur_type = ppt
                    .get(a.start..a.start + 4)
                    .map_or(1, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            }
            0x0FA0 | 0x0FA8 => {
                let t = text_of(&ppt, a);
                if let Some(s) = slides.last_mut() {
                    s.push((cur_type, t));
                }
            }
            _ => {}
        }
    }
    if slides.iter().all(Vec::is_empty) {
        // writers that keep the text inside each Slide container (RT_Slide)
        slides.clear();
        let mut in_slide = false;
        let mut sd = 0;
        let mut ty: u32 = 1;
        for (depth, a) in &all {
            if a.kind == 0x03EE {
                in_slide = true;
                sd = *depth;
                slides.push(Vec::new());
                continue;
            }
            if in_slide && *depth <= sd {
                in_slide = false;
            }
            if !in_slide {
                continue;
            }
            if a.kind == 0x0F9F {
                ty = ppt
                    .get(a.start..a.start + 4)
                    .map_or(1, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            } else if a.kind == 0x0FA0 || a.kind == 0x0FA8 {
                if let Some(s) = slides.last_mut() {
                    s.push((ty, text_of(&ppt, a)));
                }
            }
        }
    }
    if slides.iter().all(Vec::is_empty) {
        // no slide list: take every text atom, one section for the lot
        let mut s = Vec::new();
        for (_, a) in &all {
            if a.kind == 0x0FA0 || a.kind == 0x0FA8 {
                s.push((1, text_of(&ppt, a)));
            }
        }
        slides = vec![s];
    }
    let mut blocks = Vec::new();
    for (n, s) in slides.iter().enumerate() {
        if n > 0 {
            blocks.push(Block::PageBreak);
        }
        let mut title: Option<String> = None;
        let mut body: Vec<Block> = Vec::new();
        for (kind, t) in s {
            let paras: Vec<String> = t
                .split(['\r', '\n'])
                .map(|p| p.replace('\u{b}', " ").trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            match kind {
                0 | 5 => {
                    if title.is_none() {
                        title = Some(paras.join(" "));
                    }
                }
                1 | 4 | 6 | 7 => body.push(Block::List {
                    ordered: false,
                    start: 1,
                    tight: true,
                    items: paras
                        .into_iter()
                        .map(|p| ListItem {
                            checked: None,
                            blocks: vec![Block::Paragraph(vec![Inline::Text(p)])],
                        })
                        .collect(),
                }),
                2 => {}
                _ => {
                    for p in paras {
                        body.push(Block::Paragraph(vec![Inline::Text(p)]));
                    }
                }
            }
        }
        blocks.push(Block::Heading {
            level: 1,
            content: vec![Inline::Text(
                title
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| format!("Slide {}", n + 1)),
            )],
        });
        blocks.extend(body);
    }
    notes.push(Note::new(
        Severity::Info,
        "ppt.text_only",
        "slide text was read; layout, pictures, tables, shapes, animations and speaker notes are not carried",
    ));
    Ok(Document {
        blocks,
        ..Document::default()
    })
}
