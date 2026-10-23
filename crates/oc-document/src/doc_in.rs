//! Legacy Word documents (`.doc`, Word 97-2003): the text of the main document from the piece
//! table, with paragraphs, tables, hyperlink fields, line and page breaks.
//! Not read: styles (so no headings or lists), pictures, footnotes, headers, encrypted files,
//! Word 6/95 files.
#![allow(clippy::chunks_exact_to_as_chunks)]
use crate::model::{Block, Cell, Document, Inline};
use oc_archive::cfb::Cfb;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

fn corrupt(m: &str) -> Error {
    Error::new(ErrorCode::CorruptFile, m.to_string())
}

fn u16at(d: &[u8], p: usize) -> Option<usize> {
    d.get(p..p + 2)
        .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
}

fn u32at(d: &[u8], p: usize) -> Option<usize> {
    d.get(p..p + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

const HI: [u16; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
];

/// The characters of the main text, in order.
fn main_text(
    wd: &[u8],
    table: &[u8],
    fc_clx: usize,
    lcb_clx: usize,
    ccp: usize,
) -> Result<Vec<char>> {
    let clx = table
        .get(
            fc_clx
                ..fc_clx
                    .checked_add(lcb_clx)
                    .ok_or_else(|| corrupt("bad piece table"))?,
        )
        .ok_or_else(|| corrupt("the piece table lies outside the table stream"))?;
    let mut p = 0;
    while clx.get(p) == Some(&1) {
        p += 3 + u16at(clx, p + 1).ok_or_else(|| corrupt("bad piece table"))?;
    }
    if clx.get(p) != Some(&2) {
        return Err(corrupt("the document has no piece table"));
    }
    let lcb = u32at(clx, p + 1).ok_or_else(|| corrupt("bad piece table"))?;
    let plc = clx
        .get(p + 5..p + 5 + lcb)
        .ok_or_else(|| corrupt("bad piece table"))?;
    let n = lcb.saturating_sub(4) / 12;
    let mut out = Vec::new();
    for i in 0..n {
        let cp0 = u32at(plc, i * 4).unwrap_or(0);
        let cp1 = u32at(plc, (i + 1) * 4).unwrap_or(0);
        if cp0 >= ccp {
            break;
        }
        let cp1 = cp1.min(ccp);
        let pcd = (n + 1) * 4 + i * 8;
        let fc = u32at(plc, pcd + 2).ok_or_else(|| corrupt("bad piece descriptor"))?;
        let compressed = fc & 0x4000_0000 != 0;
        let fc = fc & 0x3FFF_FFFF;
        let count = cp1.saturating_sub(cp0);
        if compressed {
            let start = fc / 2;
            let bytes = wd
                .get(start..start.saturating_add(count).min(wd.len()))
                .unwrap_or(&[]);
            for b in bytes {
                out.push(if (0x80..0xA0).contains(b) {
                    char::from_u32(u32::from(HI[usize::from(*b - 0x80)])).unwrap_or('?')
                } else {
                    char::from(*b)
                });
            }
        } else {
            let bytes = wd
                .get(fc..fc.saturating_add(count * 2).min(wd.len()))
                .unwrap_or(&[]);
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            out.extend(char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')));
        }
    }
    Ok(out)
}

/// Text with Word's control characters into blocks.
fn blocks(text: &[char]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut para: Vec<Inline> = Vec::new();
    let mut buf = String::new();
    // field state: 0 none, 1 instruction, 2 result
    let mut field_depth = 0usize;
    let mut instr = String::new();
    let mut in_instr = false;
    let mut link_url: Vec<Option<String>> = Vec::new();
    let mut link_start: Vec<usize> = Vec::new();
    let mut cells: Vec<Cell> = Vec::new();
    let mut rows: Vec<Vec<Cell>> = Vec::new();

    fn flush(buf: &mut String, para: &mut Vec<Inline>) {
        if !buf.is_empty() {
            para.push(Inline::Text(std::mem::take(buf)));
        }
    }

    for &c in text {
        if in_instr {
            match c {
                '\u{14}' => {
                    in_instr = false;
                    let t = instr.trim();
                    let url = t
                        .strip_prefix("HYPERLINK")
                        .map(|r| {
                            r.trim()
                                .trim_matches('"')
                                .split('"')
                                .next()
                                .unwrap_or("")
                                .to_string()
                        })
                        .filter(|u| !u.is_empty() && !u.starts_with("\\l"));
                    link_url.push(url);
                    link_start.push(para.len() + usize::from(!buf.is_empty()));
                    flush(&mut buf, &mut para);
                    let _ = link_start.pop();
                    link_start.push(para.len());
                    instr.clear();
                }
                '\u{15}' => {
                    in_instr = false;
                    instr.clear();
                    field_depth = field_depth.saturating_sub(1);
                }
                '\u{13}' => instr.clear(),
                c => instr.push(c),
            }
            continue;
        }
        match c {
            '\u{13}' => {
                field_depth += 1;
                in_instr = true;
                instr.clear();
            }
            '\u{15}' => {
                field_depth = field_depth.saturating_sub(1);
                flush(&mut buf, &mut para);
                if let (Some(Some(url)), Some(start)) = (link_url.pop(), link_start.pop()) {
                    let content: Vec<Inline> = para.drain(start.min(para.len())..).collect();
                    if !content.is_empty() {
                        para.push(Inline::Link {
                            url,
                            title: String::new(),
                            content,
                        });
                    }
                }
            }
            '\u{14}' => {}
            '\r' | '\u{7}' => {
                flush(&mut buf, &mut para);
                if c == '\u{7}' {
                    if para.is_empty() && !cells.is_empty() {
                        // the row mark after the last cell
                        rows.push(std::mem::take(&mut cells));
                    } else {
                        cells.push(Cell {
                            header: false,
                            align: Default::default(),
                            content: std::mem::take(&mut para),
                        });
                    }
                    continue;
                }
                if !rows.is_empty() || !cells.is_empty() {
                    if !cells.is_empty() {
                        rows.push(std::mem::take(&mut cells));
                    }
                    if let Some(first) = rows.first_mut() {
                        for c in first.iter_mut() {
                            c.header = true;
                        }
                    }
                    out.push(Block::Table {
                        rows: std::mem::take(&mut rows),
                    });
                }
                let p = std::mem::take(&mut para);
                if !p.is_empty() {
                    out.push(Block::Paragraph(p));
                }
            }
            '\u{b}' => {
                flush(&mut buf, &mut para);
                para.push(Inline::LineBreak);
            }
            '\u{c}' => {
                flush(&mut buf, &mut para);
                let p = std::mem::take(&mut para);
                if !p.is_empty() {
                    out.push(Block::Paragraph(p));
                }
                out.push(Block::PageBreak);
            }
            '\u{1e}' => buf.push('-'),
            '\u{1f}' | '\u{1}' | '\u{2}' | '\u{8}' => {}
            '\t' => buf.push('\t'),
            c if (c as u32) < 0x20 => {}
            c => buf.push(c),
        }
    }
    flush(&mut buf, &mut para);
    if !para.is_empty() {
        out.push(Block::Paragraph(para));
    }
    if !rows.is_empty() {
        out.push(Block::Table { rows });
    }
    let _ = field_depth;
    out
}

pub fn read(data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Document> {
    let cfb = Cfb::open(data, limits)?;
    let wd = cfb
        .stream("WordDocument")
        .ok_or_else(|| corrupt("this compound file holds no Word document"))?;
    if u16at(&wd, 0) != Some(0xA5EC) {
        return Err(corrupt("the Word document header is not valid"));
    }
    let nfib = u16at(&wd, 2).unwrap_or(0);
    if nfib < 0x00C1 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "only Word 97-2003 documents can be read (this is Word 95 or older)",
        ));
    }
    let flags = u16at(&wd, 0x0A).unwrap_or(0);
    if flags & 0x0100 != 0 {
        return Err(Error::new(
            ErrorCode::EncryptedNeedPassword,
            "this document is password protected",
        ));
    }
    let table = cfb
        .stream(if flags & 0x0200 != 0 {
            "1Table"
        } else {
            "0Table"
        })
        .ok_or_else(|| corrupt("the table stream of the document is missing"))?;
    let ccp = u32at(&wd, 0x4C).ok_or_else(|| corrupt("the document header is cut short"))?;
    let fc = u32at(&wd, 0x1A2).ok_or_else(|| corrupt("the document header is cut short"))?;
    let lcb = u32at(&wd, 0x1A6).ok_or_else(|| corrupt("the document header is cut short"))?;
    let text = main_text(&wd, &table, fc, lcb, ccp.min(wd.len().saturating_mul(2)))?;
    let mut doc = Document {
        blocks: blocks(&text),
        ..Document::default()
    };
    // the document properties live in the summary information stream
    if let Some(si) = cfb.stream("\u{5}SummaryInformation") {
        read_summary(&si, &mut doc);
    }
    notes.push(Note::new(
        Severity::Info,
        "doc.text_only",
        "the text, tables and links were read; styles (headings, lists), pictures, footnotes, headers and formatting are not carried",
    ));
    Ok(doc)
}

/// Title (property 2), subject (3) and author (4) of a property set stream.
fn read_summary(d: &[u8], doc: &mut Document) {
    let Some(off) = u32at(d, 44) else { return };
    let Some(n) = u32at(d, off + 4) else { return };
    for i in 0..n.min(64) {
        let (Some(id), Some(po)) = (u32at(d, off + 8 + i * 8), u32at(d, off + 12 + i * 8)) else {
            continue;
        };
        let p = off + po;
        if u16at(d, p) != Some(0x1E) {
            continue;
        }
        let Some(len) = u32at(d, p + 4) else { continue };
        let Some(b) = d.get(p + 8..p + 8 + len.min(4096)) else {
            continue;
        };
        let s: String = b
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| char::from(*c))
            .collect();
        let s = s.trim().to_string();
        if s.is_empty() {
            continue;
        }
        match id {
            2 => doc.meta.title = Some(s),
            3 => doc.meta.subject = Some(s),
            4 => doc.meta.author = Some(s),
            _ => {}
        }
    }
}
