//! ODS (OpenDocument Spreadsheet): sheets, typed cells (float, percentage, currency, date,
//! time, boolean, string), repeated rows and columns, merged cells and formulas on read; on
//! write the same scope as XLSX.
// Parsers here walk byte and cell positions they have just bounds-checked.
#![allow(clippy::indexing_slicing)]

/// Column, text, formula, and (column repeat, row span, column span).
type RowCell = (usize, String, Option<String>, (usize, usize, usize));

use crate::book::{
    a1_formula_to_odf, classify, odf_formula_to_a1, unique_sheet_names, xml_clean, Kind, Workbook,
};
use crate::xlsx::{assemble, attr, local, xml_text};
use oc_archive::{format, Archive, Context, Entry, Format, WriteOpts};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};
use oc_xml::{Event, Reader, Writer};
use std::collections::BTreeMap;

const NS: [(&str, &str); 11] = [
    (
        "xmlns:office",
        "urn:oasis:names:tc:opendocument:xmlns:office:1.0",
    ),
    (
        "xmlns:style",
        "urn:oasis:names:tc:opendocument:xmlns:style:1.0",
    ),
    (
        "xmlns:text",
        "urn:oasis:names:tc:opendocument:xmlns:text:1.0",
    ),
    (
        "xmlns:table",
        "urn:oasis:names:tc:opendocument:xmlns:table:1.0",
    ),
    (
        "xmlns:fo",
        "urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0",
    ),
    (
        "xmlns:number",
        "urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0",
    ),
    ("xmlns:of", "urn:oasis:names:tc:opendocument:xmlns:of:1.2"),
    ("xmlns:xlink", "http://www.w3.org/1999/xlink"),
    ("xmlns:dc", "http://purl.org/dc/elements/1.1/"),
    (
        "xmlns:meta",
        "urn:oasis:names:tc:opendocument:xmlns:meta:1.0",
    ),
    (
        "xmlns:manifest",
        "urn:oasis:names:tc:opendocument:xmlns:manifest:1.0",
    ),
];
const MIME: &str = "application/vnd.oasis.opendocument.spreadsheet";

fn corrupt(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::CorruptFile, msg)
}

fn find<'a>(a: &'a Archive, path: &str) -> Option<&'a [u8]> {
    a.entries
        .iter()
        .find(|e| e.path == path && e.kind == oc_archive::EntryKind::File)
        .map(|e| e.data.as_slice())
}

/// `2024-01-05T10:30:00.5` or `...Z` to `2024-01-05 10:30:00`; midnight values are date-only.
fn odf_date(v: &str) -> String {
    let v = v.trim();
    let Some((d, t)) = v.split_once('T') else {
        return v.to_string();
    };
    let t = t.split(['.', 'Z', '+']).next().unwrap_or(t);
    let t = t.trim_end_matches('-');
    if t == "00:00:00" || t.is_empty() {
        d.to_string()
    } else {
        format!("{d} {t}")
    }
}

/// `PT01H30M05S` to `01:30:05` (hours may exceed 24).
fn odf_time(v: &str) -> String {
    let mut h = 0u64;
    let mut m = 0u64;
    let mut s = 0u64;
    let mut num = String::new();
    for c in v.trim().trim_start_matches(['P', 'T']).chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'H' | 'M' | 'S' | 'D' => {
                let n = num
                    .split('.')
                    .next()
                    .unwrap_or("0")
                    .parse::<u64>()
                    .unwrap_or(0);
                match c {
                    'H' => h += n,
                    'M' => m += n,
                    'S' => s += n,
                    _ => h += n * 24,
                }
                num.clear();
            }
            _ => {}
        }
    }
    format!("{h:02}:{m:02}:{s:02}")
}

struct CellState {
    value_type: String,
    value: Option<String>,
    string_value: Option<String>,
    formula: Option<String>,
    col_rep: usize,
    row_span: usize,
    col_span: usize,
    text: String,
    paras: usize,
}

pub fn read(
    data: &[u8],
    limits: &Limits,
    progress: &dyn Progress,
    has_header: bool,
    notes: &mut Notes,
) -> Result<Workbook> {
    let cx = Context {
        limits,
        progress,
        password: None,
        list_only: false,
    };
    let zip = format::read(Format::Zip, data, &cx, None)?;
    let xml = xml_text(
        find(&zip, "content.xml")
            .ok_or_else(|| corrupt("not an ODS file: content.xml is missing"))?,
    )?;
    let mut r = Reader::new(&xml, limits);
    let mut book = Workbook::default();
    let mut merged = false;
    let mut formulas = false;

    // Current sheet.
    let mut name = String::new();
    let mut hidden = false;
    let mut in_table = false;
    let mut cells: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let mut fmls: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let mut merges: Vec<(usize, usize, usize, usize)> = Vec::new();
    let (mut row, mut col) = (0usize, 0usize);
    let mut row_rep = 1usize;
    let mut row_cells: Vec<RowCell> = Vec::new();
    let mut cell: Option<CellState> = None;
    let mut depth_in_cell_tables = 0u32;
    let mut in_p = false;
    let mut count = 0u64;

    loop {
        match r.read_event()? {
            Event::Start { name: n, attrs, .. } => match local(&n) {
                "table" if cell.is_none() && !in_table => {
                    in_table = true;
                    name = attr(&attrs, "name").unwrap_or("").to_string();
                    hidden = attr(&attrs, "display") == Some("false");
                    cells.clear();
                    fmls.clear();
                    merges.clear();
                    row = 0;
                }
                "table" if cell.is_some() => depth_in_cell_tables += 1,
                "table-row" if in_table && cell.is_none() => {
                    row_rep = attr(&attrs, "number-rows-repeated")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1)
                        .max(1);
                    col = 0;
                    row_cells.clear();
                    progress.check_cancelled()?;
                }
                "table-cell" | "covered-table-cell"
                    if in_table && depth_in_cell_tables == 0 && cell.is_none() =>
                {
                    let get = |k: &str| attr(&attrs, k).map(str::to_string);
                    cell = Some(CellState {
                        value_type: get("value-type").unwrap_or_default(),
                        value: get("value"),
                        string_value: get("string-value")
                            .or_else(|| get("date-value"))
                            .or_else(|| get("time-value"))
                            .or_else(|| get("boolean-value")),
                        formula: get("formula"),
                        col_rep: get("number-columns-repeated")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1)
                            .max(1),
                        row_span: get("number-rows-spanned")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1)
                            .max(1),
                        col_span: get("number-columns-spanned")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1)
                            .max(1),
                        text: String::new(),
                        paras: 0,
                    });
                    if local(&n) == "covered-table-cell" {
                        if let Some(c) = cell.as_mut() {
                            c.value_type.clear();
                        }
                    }
                }
                "p" | "h" if cell.is_some() && depth_in_cell_tables == 0 => {
                    in_p = true;
                    if let Some(c) = cell.as_mut() {
                        if c.paras > 0 {
                            c.text.push('\n');
                        }
                        c.paras += 1;
                    }
                }
                "s" if in_p => {
                    let n: usize = attr(&attrs, "c")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1)
                        .min(10_000);
                    if let Some(c) = cell.as_mut() {
                        c.text.extend(std::iter::repeat_n(' ', n));
                    }
                }
                "tab" if in_p => {
                    if let Some(c) = cell.as_mut() {
                        c.text.push('\t');
                    }
                }
                "line-break" if in_p => {
                    if let Some(c) = cell.as_mut() {
                        c.text.push('\n');
                    }
                }
                _ => {}
            },
            Event::Text(t) | Event::CData(t) if in_p => {
                if let Some(c) = cell.as_mut() {
                    c.text.push_str(&t);
                }
            }
            Event::End { name: n, .. } => match local(&n) {
                "p" | "h" => in_p = false,
                "table" if depth_in_cell_tables > 0 => depth_in_cell_tables -= 1,
                "table-cell" | "covered-table-cell" if depth_in_cell_tables == 0 => {
                    if let Some(c) = cell.take() {
                        let text = match c.value_type.as_str() {
                            "float" | "percentage" | "currency" => {
                                c.value.clone().unwrap_or_else(|| c.text.clone())
                            }
                            "date" => {
                                odf_date(&c.string_value.clone().unwrap_or_else(|| c.text.clone()))
                            }
                            "time" => {
                                odf_time(&c.string_value.clone().unwrap_or_else(|| c.text.clone()))
                            }
                            "boolean" => if c.string_value.as_deref() == Some("true") {
                                "TRUE"
                            } else {
                                "FALSE"
                            }
                            .to_string(),
                            "string" => c.string_value.clone().unwrap_or_else(|| c.text.clone()),
                            _ => c.text.clone(),
                        };
                        let formula = c
                            .formula
                            .as_deref()
                            .map(odf_formula_to_a1)
                            .filter(|f| !f.is_empty());
                        if !text.is_empty() || formula.is_some() {
                            if (c.col_rep as u64).saturating_mul(row_rep as u64)
                                > limits.max_entries
                            {
                                return Err(Error::limit(
                                    "cells",
                                    "a repeated cell expands beyond the allowed maximum",
                                ));
                            }
                            row_cells.push((
                                col,
                                text,
                                formula,
                                (c.col_rep, c.row_span, c.col_span),
                            ));
                        }
                        col += c.col_rep;
                    }
                }
                "table-row" if in_table && cell.is_none() => {
                    for (c0, text, formula, (crep, rspan, cspan)) in row_cells.drain(..) {
                        for rr in 0..row_rep {
                            for cc in 0..crep {
                                count += 1;
                                if count > limits.max_entries {
                                    return Err(Error::limit(
                                        "cells",
                                        "the sheet has more cells than the allowed maximum",
                                    ));
                                }
                                let pos = (row + rr, c0 + cc);
                                if let Some(f) = &formula {
                                    fmls.insert(pos, f.clone());
                                }
                                cells.insert(pos, text.clone());
                                if (rspan > 1 || cspan > 1) && merges.len() < 100_000 {
                                    merges.push((
                                        pos.0,
                                        pos.1,
                                        pos.0 + rspan - 1,
                                        pos.1 + cspan - 1,
                                    ));
                                }
                            }
                        }
                    }
                    row += row_rep;
                    if row > 1_048_576 + 4096 {
                        return Err(corrupt("a sheet has more rows than the format allows"));
                    }
                }
                "table" if in_table => {
                    in_table = false;
                    merged |= !merges.is_empty();
                    formulas |= !fmls.is_empty();
                    book.sheets.push(assemble(
                        std::mem::take(&mut name),
                        hidden,
                        std::mem::take(&mut cells),
                        std::mem::take(&mut fmls),
                        std::mem::take(&mut merges),
                        has_header,
                        limits,
                    )?);
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    if book.sheets.is_empty() {
        return Err(corrupt("the spreadsheet has no sheets"));
    }
    if merged {
        notes.push(Note::new(
            Severity::Info,
            "merged_cells",
            "merged cells keep their value in the top-left cell; the others are empty",
        ));
    }
    if formulas {
        notes.push(Note::new(
            Severity::Info,
            "formulas_cached",
            "cells with formulas were read as their last calculated values; nothing is recalculated",
        ));
    }
    Ok(book)
}

// ---------------------------------------------------------------- writing

fn paragraphs(w: &mut Writer, text: &str) {
    for line in text.split('\n') {
        w.start("text:p");
        let mut run = String::new();
        let mut chars = line.chars().peekable();
        let mut prev_space = true; // a leading space also needs <text:s/>
        while let Some(c) = chars.next() {
            match c {
                ' ' if prev_space || chars.peek() == Some(&' ') || chars.peek().is_none() => {
                    if !run.is_empty() {
                        w.text(&run);
                        run.clear();
                    }
                    let mut n = 1;
                    while chars.peek() == Some(&' ') {
                        chars.next();
                        n += 1;
                    }
                    w.start("text:s");
                    if n > 1 {
                        w.attr("text:c", &n.to_string());
                    }
                    w.end();
                    prev_space = true;
                    continue;
                }
                '\t' => {
                    if !run.is_empty() {
                        w.text(&run);
                        run.clear();
                    }
                    w.start("text:tab");
                    w.end();
                }
                '\r' => {}
                _ => run.push(c),
            }
            prev_space = c == ' ';
        }
        if !run.is_empty() {
            w.text(&run);
        }
        w.end();
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WriteOptions {
    pub detect_types: bool,
    pub detect_dates: bool,
    pub style_header: bool,
}

fn root(w: &mut Writer, name: &str) {
    w.start(name);
    for (k, v) in NS {
        w.attr(k, v);
    }
    w.attr("office:version", "1.3");
}

pub fn write(
    book: &Workbook,
    opts: &WriteOptions,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let names = unique_sheet_names(
        &book
            .sheets
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>(),
    );
    let mut dropped_ctrl = false;
    let mut c = Writer::new();
    c.declaration();
    root(&mut c, "office:document-content");
    c.start("office:automatic-styles");
    for (name, with_time) in [("N84", false), ("N85", true)] {
        c.start("number:date-style");
        c.attr("style:name", name);
        let mut parts: Vec<(&str, Option<&str>)> = vec![
            ("year", Some("long")),
            ("-", None),
            ("month", Some("long")),
            ("-", None),
            ("day", Some("long")),
        ];
        if with_time {
            parts.extend([
                (" ", None),
                ("hours", Some("long")),
                (":", None),
                ("minutes", Some("long")),
                (":", None),
                ("seconds", Some("long")),
            ]);
        }
        for (part, style) in parts {
            match style {
                Some(st) => {
                    c.start(&format!("number:{part}"));
                    c.attr("number:style", st);
                    c.end();
                }
                None => {
                    c.start("number:text");
                    c.text(part);
                    c.end();
                }
            }
        }
        c.end();
    }
    for (name, data_style) in [("ce1", "N84"), ("ce2", "N85")] {
        c.start("style:style");
        c.attr("style:name", name);
        c.attr("style:family", "table-cell");
        c.attr("style:data-style-name", data_style);
        c.end();
    }
    c.start("style:style");
    c.attr("style:name", "ceh");
    c.attr("style:family", "table-cell");
    c.start("style:text-properties");
    c.attr("fo:font-weight", "bold");
    c.attr("style:font-weight-asian", "bold");
    c.attr("style:font-weight-complex", "bold");
    c.end();
    c.end();
    // Column styles: one per distinct width in tenths of a centimetre.
    let mut widths_seen: Vec<usize> = Vec::new();
    let mut sheet_widths: Vec<Vec<usize>> = Vec::new();
    for sheet in &book.sheets {
        let t = &sheet.table;
        let mut cw = vec![0usize; t.width()];
        for row in t.header.iter().chain(t.rows.iter().take(1000)) {
            for (i, cell) in row.iter().enumerate() {
                if let Some(w) = cw.get_mut(i) {
                    *w = (*w).max(cell.lines().map(|l| l.chars().count()).max().unwrap_or(0));
                }
            }
        }
        let tenths: Vec<usize> = cw
            .iter()
            .map(|w| ((*w as f64 + 2.0).clamp(8.0, 60.0) * 2.0) as usize)
            .collect();
        for t in &tenths {
            if !widths_seen.contains(t) {
                widths_seen.push(*t);
            }
        }
        sheet_widths.push(tenths);
    }
    for t in &widths_seen {
        c.start("style:style");
        c.attr("style:name", &format!("co{t}"));
        c.attr("style:family", "table-column");
        c.start("style:table-column-properties");
        c.attr("style:column-width", &format!("{:.2}cm", *t as f64 / 10.0));
        c.end();
        c.end();
    }
    c.end(); // automatic-styles
    c.start("office:body");
    c.start("office:spreadsheet");
    for (si, sheet) in book.sheets.iter().enumerate() {
        progress.check_cancelled()?;
        let t = &sheet.table;
        let width = t.width();
        if width > 16_384 || t.rows.len() + usize::from(t.header.is_some()) > 1_048_576 {
            return Err(Error::limit(
                "worksheet",
                "the table is larger than a spreadsheet can hold (1,048,576 rows, 16,384 columns)",
            ));
        }
        c.start("table:table");
        c.attr("table:name", names.get(si).map_or("Sheet", String::as_str));
        for tenths in sheet_widths.get(si).into_iter().flatten() {
            c.start("table:table-column");
            c.attr("table:style-name", &format!("co{tenths}"));
            c.end();
        }
        let total_rows = t.rows.len() + usize::from(t.header.is_some());
        let mut top_left: std::collections::HashMap<(usize, usize), (usize, usize)> =
            std::collections::HashMap::new();
        let mut covered: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();
        for &(r1, c1, r2, c2) in &sheet.merges {
            if (r1, c1) == (r2, c2)
                || r2 >= total_rows
                || c2 >= width
                || (r2 - r1 + 1) * (c2 - c1 + 1) > 100_000
            {
                continue;
            }
            top_left.insert((r1, c1), (r2 - r1 + 1, c2 - c1 + 1));
            for r in r1..=r2 {
                for cc in c1..=c2 {
                    if (r, cc) != (r1, c1) {
                        covered.insert((r, cc));
                    }
                }
            }
        }
        let header = t.header.iter().map(|h| (true, h));
        let body = t.rows.iter().map(|r| (false, r));
        for (ri, (is_header, row)) in header.chain(body).enumerate() {
            if ri % 512 == 0 {
                progress.check_cancelled()?;
            }
            c.start("table:table-row");
            let special =
                |ci: usize| top_left.contains_key(&(ri, ci)) || covered.contains(&(ri, ci));
            let last = row
                .iter()
                .enumerate()
                .rposition(|(ci, v)| {
                    !v.is_empty() || sheet.formulas.contains_key(&(ri, ci)) || special(ci)
                })
                .map_or(0, |p| p + 1);
            let mut empty_run = 0usize;
            let flush = |c: &mut Writer, n: &mut usize| {
                if *n > 0 {
                    c.start("table:table-cell");
                    if *n > 1 {
                        c.attr("table:number-columns-repeated", &n.to_string());
                    }
                    c.end();
                    *n = 0;
                }
            };
            for (ci, text) in row.iter().enumerate().take(last) {
                let formula = sheet.formulas.get(&(ri, ci));
                if text.is_empty() && formula.is_none() && !special(ci) {
                    empty_run += 1;
                    continue;
                }
                flush(&mut c, &mut empty_run);
                if covered.contains(&(ri, ci)) {
                    c.start("table:covered-table-cell");
                    c.end();
                    continue;
                }
                let (clean, dropped) = xml_clean(text);
                dropped_ctrl |= dropped;
                let kind = if is_header || !opts.detect_types {
                    if text.is_empty() {
                        Kind::Empty
                    } else {
                        Kind::Text
                    }
                } else {
                    classify(&clean, opts.detect_dates, false)
                };
                c.start("table:table-cell");
                if let Some((rs, cs)) = top_left.get(&(ri, ci)) {
                    if *cs > 1 {
                        c.attr("table:number-columns-spanned", &cs.to_string());
                    }
                    if *rs > 1 {
                        c.attr("table:number-rows-spanned", &rs.to_string());
                    }
                }
                if let Some(f) = formula {
                    c.attr("table:formula", &a1_formula_to_odf(f));
                }
                match kind {
                    Kind::Number(n) => {
                        c.attr("office:value-type", "float");
                        c.attr("office:value", n);
                    }
                    Kind::Bool(b) => {
                        c.attr("office:value-type", "boolean");
                        c.attr("office:boolean-value", if b { "true" } else { "false" });
                    }
                    Kind::Date(_, time) => {
                        c.attr("table:style-name", if time { "ce2" } else { "ce1" });
                        c.attr("office:value-type", "date");
                        c.attr("office:date-value", &clean.replace(' ', "T"));
                    }
                    Kind::Text | Kind::Empty => {
                        c.attr("office:value-type", "string");
                        if is_header && opts.style_header {
                            c.attr("table:style-name", "ceh");
                        }
                    }
                }
                if !clean.is_empty() {
                    paragraphs(&mut c, &clean);
                }
                c.end();
            }
            c.end();
        }
        c.end();
    }
    if dropped_ctrl {
        notes.push(Note::new(
            Severity::Warning,
            "control_characters_dropped",
            "control characters that XML cannot hold were removed from some cells",
        ));
    }
    let content = c.finish();

    let mut m = Writer::new();
    m.declaration();
    m.start("manifest:manifest");
    m.attr(
        "xmlns:manifest",
        "urn:oasis:names:tc:opendocument:xmlns:manifest:1.0",
    );
    m.attr("manifest:version", "1.3");
    for (path, ty) in [
        ("/", MIME),
        ("content.xml", "text/xml"),
        ("styles.xml", "text/xml"),
        ("meta.xml", "text/xml"),
    ] {
        m.start("manifest:file-entry");
        m.attr("manifest:full-path", path);
        m.attr("manifest:media-type", ty);
        if path == "/" {
            m.attr("manifest:version", "1.3");
        }
        m.end();
    }
    let manifest = m.finish();

    let mut s = Writer::new();
    s.declaration();
    root(&mut s, "office:document-styles");
    let styles = s.finish();

    let mut meta = Writer::new();
    meta.declaration();
    root(&mut meta, "office:document-meta");
    meta.start("office:meta");
    meta.start("meta:generator");
    meta.text("OpenConverter");
    meta.end();
    let meta = meta.finish();

    let mut archive = Archive::default();
    archive
        .entries
        .push(Entry::file("mimetype", MIME.as_bytes().to_vec()));
    archive
        .entries
        .push(Entry::file("META-INF/manifest.xml", manifest.into_bytes()));
    archive
        .entries
        .push(Entry::file("content.xml", content.into_bytes()));
    archive
        .entries
        .push(Entry::file("styles.xml", styles.into_bytes()));
    archive
        .entries
        .push(Entry::file("meta.xml", meta.into_bytes()));
    let cx = Context {
        limits,
        progress,
        password: None,
        list_only: false,
    };
    format::write(Format::Zip, &archive, &WriteOpts::default(), &cx)
}
