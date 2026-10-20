//! XLSX (Office Open XML SpreadsheetML): shared and inline strings, number formats that mean
//! dates (1900 and 1904 systems), several sheets, merged cells (read), formula text next to
//! the cached value, and on write a bold frozen header row, date styles and column widths.
// Parsers here walk byte and cell positions they have just bounds-checked.
#![allow(clippy::indexing_slicing)]

use crate::book::{
    classify, col_name, format_datetime, format_number, parse_ref, serial_to_datetime,
    unique_sheet_names, xml_clean, Kind, Sheet, Workbook,
};
use crate::Table;
use oc_archive::{format, Archive, Context, Entry, Format, WriteOpts};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};
use oc_xml::{Event, Reader, Writer};
use std::collections::{BTreeMap, HashMap};

const MAX_ROWS: usize = 1_048_576;
const MAX_COLS: usize = 16_384;

fn corrupt(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::CorruptFile, msg)
}

pub(crate) fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

pub(crate) fn attr<'a>(attrs: &'a [oc_xml::Attribute], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| local(&a.name) == key)
        .map(|a| a.value.as_str())
}

/// Decodes an XML part: UTF-8, or UTF-16 with a byte-order mark.
pub(crate) fn xml_text(data: &[u8]) -> Result<String> {
    let found = oc_text::detect(data);
    let enc = if found.certain {
        found.encoding
    } else {
        oc_text::Encoding::Utf8
    };
    let body = data.get(found.bom_len..).unwrap_or_default();
    Ok(oc_text::decode(body, enc, oc_text::Policy::Strict)
        .map_err(|_| corrupt("a part of the file is not valid text"))?
        .0)
}

fn find<'a>(a: &'a Archive, path: &str) -> Option<&'a [u8]> {
    a.entries
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case(path) && e.kind == oc_archive::EntryKind::File)
        .map(|e| e.data.as_slice())
}

fn is_date_format(id: u32, code: Option<&str>) -> (bool, bool) {
    // (has date part, has time part)
    match id {
        14..=17 | 27..=31 | 34..=36 | 50..=58 => return (true, false),
        18..=21 | 32 | 33 | 45..=47 => return (false, true),
        22 => return (true, true),
        _ => {}
    }
    let Some(code) = code else {
        return (false, false);
    };
    // Drop quoted text, [colour] and [$-locale] sections and escaped characters.
    let mut clean = String::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                for d in chars.by_ref() {
                    if d == '"' {
                        break;
                    }
                }
            }
            '[' => {
                let mut inner = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    inner.push(d);
                }
                // [h], [mm], [ss] are elapsed-time fields.
                let l = inner.to_ascii_lowercase();
                if l.chars().all(|x| matches!(x, 'h' | 'm' | 's')) && !l.is_empty() {
                    clean.push_str(&l);
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            _ => clean.push(c.to_ascii_lowercase()),
        }
    }
    let first = clean.split(';').next().unwrap_or("");
    let date = first.contains('y')
        || first.contains('d')
        || (first.contains('m') && !first.contains('h') && !first.contains('s'));
    let time = first.contains('h') || first.contains('s');
    (date, time)
}

#[derive(Clone, Copy, Default)]
struct DateStyle {
    date: bool,
    time: bool,
}

fn read_styles(xml: &str, limits: &Limits) -> Result<Vec<DateStyle>> {
    let mut r = Reader::new(xml, limits);
    let mut codes: HashMap<u32, String> = HashMap::new();
    let mut xfs: Vec<DateStyle> = Vec::new();
    let mut in_cell_xfs = false;
    loop {
        match r.read_event()? {
            Event::Start { name, attrs, .. } => match local(&name) {
                "numFmt" => {
                    if let (Some(id), Some(code)) = (
                        attr(&attrs, "numFmtId").and_then(|v| v.parse().ok()),
                        attr(&attrs, "formatCode"),
                    ) {
                        codes.insert(id, code.to_string());
                    }
                }
                "cellXfs" => in_cell_xfs = true,
                "xf" if in_cell_xfs => {
                    let id: u32 = attr(&attrs, "numFmtId")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let (date, time) = is_date_format(id, codes.get(&id).map(String::as_str));
                    if xfs.len() < 100_000 {
                        xfs.push(DateStyle { date, time });
                    }
                }
                _ => {}
            },
            Event::End { name, .. } if local(&name) == "cellXfs" => in_cell_xfs = false,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(xfs)
}

fn read_shared_strings(xml: &str, limits: &Limits, max: u64) -> Result<Vec<String>> {
    let mut r = Reader::new(xml, limits);
    let mut out = Vec::new();
    let (mut cur, mut in_si, mut in_t, mut skip) = (String::new(), false, false, 0u32);
    let mut total = 0u64;
    loop {
        match r.read_event()? {
            Event::Start { name, .. } => match local(&name) {
                "si" => {
                    in_si = true;
                    cur.clear();
                }
                "t" if in_si && skip == 0 => in_t = true,
                "rPh" | "phoneticPr" => skip += 1,
                _ => {}
            },
            Event::End { name, .. } => match local(&name) {
                "t" => in_t = false,
                "rPh" | "phoneticPr" => skip = skip.saturating_sub(1),
                "si" => {
                    in_si = false;
                    total += cur.len() as u64;
                    if total > max {
                        return Err(Error::limit(
                            "strings",
                            "the shared strings are larger than the allowed maximum",
                        ));
                    }
                    out.push(std::mem::take(&mut cur));
                }
                _ => {}
            },
            Event::Text(t) | Event::CData(t) if in_t => cur.push_str(&t),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

struct SheetData {
    cells: BTreeMap<(usize, usize), String>,
    formulas: BTreeMap<(usize, usize), String>,
    merges: Vec<(usize, usize, usize, usize)>,
}

fn parse_range(r: &str) -> Option<(usize, usize, usize, usize)> {
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    let (r1, c1) = parse_ref(a)?;
    let (r2, c2) = parse_ref(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

fn read_sheet(
    xml: &str,
    shared: &[String],
    styles: &[DateStyle],
    date1904: bool,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<SheetData> {
    let mut r = Reader::new(xml, limits);
    let mut d = SheetData {
        cells: BTreeMap::new(),
        formulas: BTreeMap::new(),
        merges: Vec::new(),
    };
    let (mut row, mut next_col) = (0usize, 0usize);
    let mut cell: Option<((usize, usize), String, usize)> = None; // position, t attribute, style index
    let (mut v, mut f, mut inline) = (String::new(), String::new(), String::new());
    let (mut in_v, mut in_f, mut in_t, mut in_is) = (false, false, false, false);
    let mut count = 0u64;
    loop {
        match r.read_event()? {
            Event::Start { name, attrs, .. } => match local(&name) {
                "row" => {
                    row = attr(&attrs, "r")
                        .and_then(|x| x.parse::<usize>().ok())
                        .map_or(row, |x| x.saturating_sub(1));
                    next_col = 0;
                    if row >= MAX_ROWS {
                        return Err(corrupt("a row number is beyond the worksheet limit"));
                    }
                    if count.is_multiple_of(64) {
                        progress.check_cancelled()?;
                    }
                }
                "c" => {
                    let pos = match attr(&attrs, "r").and_then(parse_ref) {
                        Some(p) => p,
                        None => (row, next_col),
                    };
                    if pos.0 >= MAX_ROWS || pos.1 >= MAX_COLS {
                        return Err(corrupt("a cell lies outside the worksheet limits"));
                    }
                    next_col = pos.1 + 1;
                    let t = attr(&attrs, "t").unwrap_or("n").to_string();
                    let s = attr(&attrs, "s").and_then(|x| x.parse().ok()).unwrap_or(0);
                    cell = Some((pos, t, s));
                    v.clear();
                    f.clear();
                    inline.clear();
                }
                "v" if cell.is_some() => in_v = true,
                "f" if cell.is_some() => in_f = true,
                "is" if cell.is_some() => in_is = true,
                "t" if in_is => in_t = true,
                "mergeCell" => {
                    if let Some(m) = attr(&attrs, "ref").and_then(parse_range) {
                        if d.merges.len() < 100_000 {
                            d.merges.push(m);
                        }
                    }
                }
                _ => {}
            },
            Event::End { name, .. } => match local(&name) {
                "v" => in_v = false,
                "f" => in_f = false,
                "is" => in_is = false,
                "t" => in_t = false,
                "c" => {
                    if let Some((pos, t, s)) = cell.take() {
                        let text = match t.as_str() {
                            "s" => v
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| shared.get(i))
                                .cloned()
                                .unwrap_or_default(),
                            "inlineStr" => std::mem::take(&mut inline),
                            "b" => if v.trim() == "1" { "TRUE" } else { "FALSE" }.to_string(),
                            "d" => v.trim().replace('T', " "),
                            "str" | "e" => v.clone(),
                            _ => {
                                let t = v.trim();
                                match t.parse::<f64>() {
                                    Ok(x) => {
                                        let st = styles.get(s).copied().unwrap_or_default();
                                        if st.date || st.time {
                                            match serial_to_datetime(x, date1904) {
                                                Some(p) => format_datetime(
                                                    p,
                                                    st.date,
                                                    st.time && (x.fract() != 0.0 || !st.date),
                                                ),
                                                None => t.to_string(),
                                            }
                                        } else if t.contains(['E', 'e']) {
                                            format_number(x)
                                        } else {
                                            t.to_string()
                                        }
                                    }
                                    Err(_) => t.to_string(),
                                }
                            }
                        };
                        count += 1;
                        if count > limits.max_entries {
                            return Err(Error::limit(
                                "cells",
                                "the sheet has more cells than the allowed maximum",
                            ));
                        }
                        if !f.trim().is_empty() {
                            d.formulas.insert(pos, f.trim().to_string());
                        }
                        if !text.is_empty() || d.formulas.contains_key(&pos) {
                            d.cells.insert(pos, text);
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) | Event::CData(t) => {
                if in_v {
                    v.push_str(&t);
                } else if in_f {
                    f.push_str(&t);
                } else if in_t {
                    inline.push_str(&t);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(d)
}

fn resolve_target(t: &str) -> String {
    let t = t.trim();
    if let Some(abs) = t.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = vec!["xl"];
    for p in t.split('/') {
        match p {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Builds a [`Table`] (and side information) from a sparse cell map.
pub(crate) fn assemble(
    name: String,
    hidden: bool,
    cells: BTreeMap<(usize, usize), String>,
    formulas: BTreeMap<(usize, usize), String>,
    merges: Vec<(usize, usize, usize, usize)>,
    has_header: bool,
    limits: &Limits,
) -> Result<Sheet> {
    let Some(((min_r, _), _)) = cells.iter().next() else {
        return Ok(Sheet {
            name,
            hidden,
            ..Sheet::default()
        });
    };
    let min_r = *min_r;
    let max_r = cells.keys().map(|k| k.0).max().unwrap_or(min_r);
    let min_c = cells.keys().map(|k| k.1).min().unwrap_or(0);
    let max_c = cells.keys().map(|k| k.1).max().unwrap_or(0);
    let (h, w) = (max_r - min_r + 1, max_c - min_c + 1);
    if (h as u64).saturating_mul(w as u64) > limits.max_entries {
        return Err(Error::limit(
            "table",
            "the used area of the sheet is larger than the allowed maximum",
        ));
    }
    let mut grid: Vec<Vec<String>> = vec![vec![String::new(); w]; h];
    for ((r, c), v) in cells {
        if let Some(slot) = grid
            .get_mut(r - min_r)
            .and_then(|row| row.get_mut(c - min_c))
        {
            *slot = v;
        }
    }
    let shift = |(r, c): (usize, usize)| (r.saturating_sub(min_r), c.saturating_sub(min_c));
    let mut table = Table::default();
    if has_header && !grid.is_empty() {
        table.header = Some(grid.remove(0));
    }
    table.rows = grid;
    Ok(Sheet {
        name,
        table,
        hidden,
        formulas: formulas.into_iter().map(|(k, v)| (shift(k), v)).collect(),
        merges: merges
            .into_iter()
            .map(|(a, b, c, d)| {
                let (r1, c1) = shift((a, b));
                let (r2, c2) = shift((c, d));
                (r1, c1, r2, c2)
            })
            .collect(),
    })
}

pub fn read(
    data: &[u8],
    limits: &Limits,
    progress: &dyn Progress,
    has_header: bool,
    notes: &mut Notes,
) -> Result<Workbook> {
    if data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "this is an old binary Excel file or a password-protected workbook, which cannot be read",
        ));
    }
    let cx = Context {
        limits,
        progress,
        password: None,
        list_only: false,
    };
    let zip = format::read(Format::Zip, data, &cx, None)?;
    let wb_xml = xml_text(
        find(&zip, "xl/workbook.xml")
            .ok_or_else(|| corrupt("not an XLSX file: xl/workbook.xml is missing"))?,
    )?;
    let mut rels: HashMap<String, String> = HashMap::new();
    if let Some(r) = find(&zip, "xl/_rels/workbook.xml.rels") {
        let text = xml_text(r)?;
        let mut rd = Reader::new(&text, limits);
        loop {
            match rd.read_event()? {
                Event::Start { name, attrs, .. } if local(&name) == "Relationship" => {
                    if let (Some(id), Some(t)) = (attr(&attrs, "Id"), attr(&attrs, "Target")) {
                        rels.insert(id.to_string(), resolve_target(t));
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }
    let mut date1904 = false;
    let mut sheets_meta: Vec<(String, Option<String>, bool)> = Vec::new();
    let mut rd = Reader::new(&wb_xml, limits);
    loop {
        match rd.read_event()? {
            Event::Start { name, attrs, .. } => match local(&name) {
                "workbookPr" => date1904 = matches!(attr(&attrs, "date1904"), Some("1" | "true")),
                "sheet" => {
                    let rid = attrs
                        .iter()
                        .find(|a| local(&a.name) == "id")
                        .map(|a| a.value.clone());
                    let hidden = matches!(attr(&attrs, "state"), Some("hidden" | "veryHidden"));
                    sheets_meta.push((attr(&attrs, "name").unwrap_or("").to_string(), rid, hidden));
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    if sheets_meta.is_empty() {
        return Err(corrupt("the workbook has no sheets"));
    }
    let styles = match find(&zip, "xl/styles.xml") {
        Some(b) => read_styles(&xml_text(b)?, limits)?,
        None => Vec::new(),
    };
    let shared = match find(&zip, "xl/sharedStrings.xml") {
        Some(b) => read_shared_strings(&xml_text(b)?, limits, limits.max_alloc.min(1 << 28))?,
        None => Vec::new(),
    };
    let mut book = Workbook {
        sheets: Vec::new(),
        date1904,
    };
    let mut merged = false;
    let mut formulas = false;
    for (i, (name, rid, hidden)) in sheets_meta.into_iter().enumerate() {
        progress.check_cancelled()?;
        let path = rid
            .and_then(|r| rels.get(&r).cloned())
            .unwrap_or_else(|| format!("xl/worksheets/sheet{}.xml", i + 1));
        let Some(part) = find(&zip, &path) else {
            // Chart sheets and the like have no cell data.
            notes.push(Note::new(
                Severity::Info,
                "sheet_skipped",
                format!("sheet '{name}' has no cell data and was skipped"),
            ));
            continue;
        };
        let d = read_sheet(
            &xml_text(part)?,
            &shared,
            &styles,
            date1904,
            limits,
            progress,
        )?;
        merged |= !d.merges.is_empty();
        formulas |= !d.formulas.is_empty();
        book.sheets.push(assemble(
            name, hidden, d.cells, d.formulas, d.merges, has_header, limits,
        )?);
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

#[derive(Debug, Clone, Copy)]
pub struct WriteOptions {
    pub detect_types: bool,
    pub detect_dates: bool,
    pub style_header: bool,
}

const NS_MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const NS_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_PKG_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

fn styles_xml() -> String {
    let mut w = Writer::new();
    w.declaration();
    w.start("styleSheet");
    w.attr("xmlns", NS_MAIN);
    w.start("numFmts");
    w.attr("count", "2");
    for (id, code) in [(164, "yyyy\\-mm\\-dd"), (165, "yyyy\\-mm\\-dd\\ hh:mm:ss")] {
        w.start("numFmt");
        w.attr("numFmtId", &id.to_string());
        w.attr("formatCode", code);
        w.end();
    }
    w.end();
    w.start("fonts");
    w.attr("count", "2");
    for bold in [false, true] {
        w.start("font");
        if bold {
            w.start("b");
            w.end();
        }
        w.start("sz");
        w.attr("val", "11");
        w.end();
        w.start("name");
        w.attr("val", "Calibri");
        w.end();
        w.end();
    }
    w.end();
    w.start("fills");
    w.attr("count", "2");
    for pat in ["none", "gray125"] {
        w.start("fill");
        w.start("patternFill");
        w.attr("patternType", pat);
        w.end();
        w.end();
    }
    w.end();
    w.start("borders");
    w.attr("count", "1");
    w.start("border");
    for side in ["left", "right", "top", "bottom", "diagonal"] {
        w.start(side);
        w.end();
    }
    w.end();
    w.end();
    w.start("cellStyleXfs");
    w.attr("count", "1");
    w.start("xf");
    w.attr("numFmtId", "0");
    w.attr("fontId", "0");
    w.attr("fillId", "0");
    w.attr("borderId", "0");
    w.end();
    w.end();
    w.start("cellXfs");
    w.attr("count", "4");
    // 0 plain, 1 bold header, 2 date, 3 date and time
    for (num, font) in [(0, 0), (0, 1), (164, 0), (165, 0)] {
        w.start("xf");
        w.attr("numFmtId", &num.to_string());
        w.attr("fontId", &font.to_string());
        w.attr("fillId", "0");
        w.attr("borderId", "0");
        w.attr("xfId", "0");
        if num != 0 {
            w.attr("applyNumberFormat", "1");
        }
        if font != 0 {
            w.attr("applyFont", "1");
        }
        w.end();
    }
    w.end();
    w.start("cellStyles");
    w.attr("count", "1");
    w.start("cellStyle");
    w.attr("name", "Normal");
    w.attr("xfId", "0");
    w.attr("builtinId", "0");
    w.end();
    w.end();
    w.finish()
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
    if names.iter().zip(&book.sheets).any(|(n, s)| *n != s.name) {
        notes.push(Note::new(
            Severity::Info,
            "sheet_renamed",
            "some sheet names were changed to fit Excel's rules (31 characters, no []:*?/\\)",
        ));
    }
    let mut shared: Vec<String> = Vec::new();
    let mut shared_index: HashMap<String, usize> = HashMap::new();
    let mut dropped_ctrl = false;
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();

    for (si, sheet) in book.sheets.iter().enumerate() {
        progress.check_cancelled()?;
        let t = &sheet.table;
        let width = t.width();
        let total_rows = t.rows.len() + usize::from(t.header.is_some());
        if width > MAX_COLS || total_rows > MAX_ROWS {
            return Err(Error::limit("worksheet", "the table is larger than an Excel worksheet can hold (1,048,576 rows, 16,384 columns)"));
        }
        let mut col_w = vec![0usize; width];
        let mut w = Writer::new();
        w.declaration();
        w.start("worksheet");
        w.attr("xmlns", NS_MAIN);
        w.attr("xmlns:r", NS_REL);
        if total_rows > 0 && width > 0 {
            w.start("dimension");
            w.attr("ref", &format!("A1:{}{}", col_name(width - 1), total_rows));
            w.end();
        }
        w.start("sheetViews");
        w.start("sheetView");
        w.attr("workbookViewId", "0");
        if si == 0 {
            w.attr("tabSelected", "1");
        }
        if t.header.is_some() && opts.style_header {
            w.start("pane");
            w.attr("ySplit", "1");
            w.attr("topLeftCell", "A2");
            w.attr("activePane", "bottomLeft");
            w.attr("state", "frozen");
            w.end();
        }
        w.end();
        w.end();
        w.start("sheetFormatPr");
        w.attr("defaultRowHeight", "15");
        w.end();
        // Column widths need the contents, so the sheet data goes into its own writer first.
        let mut data = Writer::new();
        let header = t.header.iter().map(|h| (true, h));
        let body = t.rows.iter().map(|r| (false, r));
        for (ri, (is_header, row)) in header.chain(body).enumerate() {
            if ri % 512 == 0 {
                progress.check_cancelled()?;
            }
            let mut row_started = false;
            for (ci, text) in row.iter().enumerate() {
                let formula = sheet.formulas.get(&(ri, ci));
                if text.is_empty() && formula.is_none() {
                    continue;
                }
                if !row_started {
                    data.start("row");
                    data.attr("r", &(ri + 1).to_string());
                    row_started = true;
                }
                let (clean, dropped) = xml_clean(text);
                dropped_ctrl |= dropped;
                if let Some(cw) = col_w.get_mut(ci) {
                    let len = clean.lines().map(|l| l.chars().count()).max().unwrap_or(0);
                    if ri < 1000 {
                        *cw = (*cw).max(len);
                    }
                }
                let cell_ref = format!("{}{}", col_name(ci), ri + 1);
                let kind = if is_header || !opts.detect_types {
                    if text.is_empty() {
                        Kind::Empty
                    } else {
                        Kind::Text
                    }
                } else {
                    classify(&clean, opts.detect_dates, false)
                };
                data.start("c");
                data.attr("r", &cell_ref);
                let write_formula = |data: &mut Writer| {
                    if let Some(f) = formula {
                        let (f, _) = xml_clean(f.strip_prefix('=').unwrap_or(f));
                        data.start("f");
                        data.text(&f);
                        data.end();
                    }
                };
                match kind {
                    Kind::Empty => {
                        write_formula(&mut data);
                    }
                    Kind::Number(n) => {
                        write_formula(&mut data);
                        data.start("v");
                        data.text(n);
                        data.end();
                    }
                    Kind::Bool(b) => {
                        data.attr("t", "b");
                        write_formula(&mut data);
                        data.start("v");
                        data.text(if b { "1" } else { "0" });
                        data.end();
                    }
                    Kind::Date(serial, time) => {
                        data.attr("s", if time { "3" } else { "2" });
                        write_formula(&mut data);
                        data.start("v");
                        data.text(&format!("{serial}"));
                        data.end();
                    }
                    Kind::Text => {
                        let idx = match shared_index.get(clean.as_ref()) {
                            Some(i) => *i,
                            None => {
                                shared.push(clean.to_string());
                                shared_index.insert(clean.to_string(), shared.len() - 1);
                                shared.len() - 1
                            }
                        };
                        if is_header && opts.style_header {
                            data.attr("s", "1");
                        }
                        if formula.is_some() {
                            data.attr("t", "str");
                            write_formula(&mut data);
                            data.start("v");
                            data.text(&clean);
                            data.end();
                        } else {
                            data.attr("t", "s");
                            data.start("v");
                            data.text(&idx.to_string());
                            data.end();
                        }
                    }
                }
                data.end();
            }
            if row_started {
                data.end();
            }
        }
        if width > 0 {
            w.start("cols");
            for (i, cw) in col_w.iter().enumerate() {
                w.start("col");
                w.attr("min", &(i + 1).to_string());
                w.attr("max", &(i + 1).to_string());
                w.attr(
                    "width",
                    &format!("{}", (*cw as f64 + 2.0).clamp(8.43, 60.0)),
                );
                w.attr("customWidth", "1");
                w.end();
            }
            w.end();
        }
        // `Writer` has no way to splice documents, so the data element is assembled by hand.
        let sheet_data = data.finish();
        let mut head = w.finish();
        // `finish` closed <worksheet>; reopen by cutting its end tag.
        if let Some(cut) = head.rfind("</worksheet>") {
            head.truncate(cut);
        } else if head.ends_with("/>") {
            head.truncate(head.len() - 2);
            head.push('>');
        }
        head.push_str("<sheetData>");
        head.push_str(&sheet_data);
        head.push_str("</sheetData>");
        let merges: Vec<_> = sheet
            .merges
            .iter()
            .filter(|(r1, c1, r2, c2)| (r1, c1) != (r2, c2) && *r2 < total_rows && *c2 < width)
            .collect();
        if !merges.is_empty() {
            head.push_str(&format!("<mergeCells count=\"{}\">", merges.len()));
            for (r1, c1, r2, c2) in merges {
                head.push_str(&format!(
                    "<mergeCell ref=\"{}{}:{}{}\"/>",
                    col_name(*c1),
                    r1 + 1,
                    col_name(*c2),
                    r2 + 1
                ));
            }
            head.push_str("</mergeCells>");
        }
        head.push_str("</worksheet>");
        parts.push((
            format!("xl/worksheets/sheet{}.xml", si + 1),
            head.into_bytes(),
        ));
    }
    if dropped_ctrl {
        notes.push(Note::new(
            Severity::Warning,
            "control_characters_dropped",
            "control characters that XML cannot hold were removed from some cells",
        ));
    }

    // Workbook, relationships, content types, shared strings.
    let mut wb = Writer::new();
    wb.declaration();
    wb.start("workbook");
    wb.attr("xmlns", NS_MAIN);
    wb.attr("xmlns:r", NS_REL);
    wb.start("bookViews");
    wb.start("workbookView");
    wb.end();
    wb.end();
    wb.start("sheets");
    for (i, n) in names.iter().enumerate() {
        wb.start("sheet");
        wb.attr("name", n);
        wb.attr("sheetId", &(i + 1).to_string());
        if book.sheets.get(i).is_some_and(|s| s.hidden) && i > 0 {
            wb.attr("state", "hidden");
        }
        wb.attr("r:id", &format!("rId{}", i + 1));
        wb.end();
    }
    wb.end();
    let wb_xml = wb.finish();

    let mut rels = Writer::new();
    rels.declaration();
    rels.start("Relationships");
    rels.attr("xmlns", NS_PKG_REL);
    for i in 0..names.len() {
        rels.start("Relationship");
        rels.attr("Id", &format!("rId{}", i + 1));
        rels.attr("Type", &format!("{NS_REL}/worksheet"));
        rels.attr("Target", &format!("worksheets/sheet{}.xml", i + 1));
        rels.end();
    }
    for (k, (ty, target)) in [
        ("styles", "styles.xml"),
        ("sharedStrings", "sharedStrings.xml"),
    ]
    .into_iter()
    .enumerate()
    {
        rels.start("Relationship");
        rels.attr("Id", &format!("rId{}", names.len() + 1 + k));
        rels.attr("Type", &format!("{NS_REL}/{ty}"));
        rels.attr("Target", target);
        rels.end();
    }
    let rels_xml = rels.finish();

    let mut ct = Writer::new();
    ct.declaration();
    ct.start("Types");
    ct.attr(
        "xmlns",
        "http://schemas.openxmlformats.org/package/2006/content-types",
    );
    for (ext, ty) in [
        (
            "rels",
            "application/vnd.openxmlformats-package.relationships+xml",
        ),
        ("xml", "application/xml"),
    ] {
        ct.start("Default");
        ct.attr("Extension", ext);
        ct.attr("ContentType", ty);
        ct.end();
    }
    let main = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let mut overrides = vec![
        (
            "/xl/workbook.xml".to_string(),
            format!("{main}.sheet.main+xml"),
        ),
        ("/xl/styles.xml".to_string(), format!("{main}.styles+xml")),
        (
            "/xl/sharedStrings.xml".to_string(),
            format!("{main}.sharedStrings+xml"),
        ),
    ];
    for i in 0..names.len() {
        overrides.push((
            format!("/xl/worksheets/sheet{}.xml", i + 1),
            format!("{main}.worksheet+xml"),
        ));
    }
    for (part, ty) in overrides {
        ct.start("Override");
        ct.attr("PartName", &part);
        ct.attr("ContentType", &ty);
        ct.end();
    }
    let ct_xml = ct.finish();

    let mut top = Writer::new();
    top.declaration();
    top.start("Relationships");
    top.attr("xmlns", NS_PKG_REL);
    top.start("Relationship");
    top.attr("Id", "rId1");
    top.attr("Type", &format!("{NS_REL}/officeDocument"));
    top.attr("Target", "xl/workbook.xml");
    top.end();
    let top_xml = top.finish();

    let mut ss = Writer::new();
    ss.declaration();
    ss.start("sst");
    ss.attr("xmlns", NS_MAIN);
    ss.attr("count", &shared.len().to_string());
    ss.attr("uniqueCount", &shared.len().to_string());
    for s in &shared {
        ss.start("si");
        ss.start("t");
        if s.starts_with(char::is_whitespace)
            || s.ends_with(char::is_whitespace)
            || s.contains('\n')
        {
            ss.attr("xml:space", "preserve");
        }
        ss.text(s);
        ss.end();
        ss.end();
    }
    let ss_xml = ss.finish();

    let mut archive = Archive::default();
    let mut add = |path: &str, data: Vec<u8>| archive.entries.push(Entry::file(path, data));
    add("[Content_Types].xml", ct_xml.into_bytes());
    add("_rels/.rels", top_xml.into_bytes());
    add("xl/workbook.xml", wb_xml.into_bytes());
    add("xl/_rels/workbook.xml.rels", rels_xml.into_bytes());
    add("xl/styles.xml", styles_xml().into_bytes());
    add("xl/sharedStrings.xml", ss_xml.into_bytes());
    for (p, d) in parts {
        add(&p, d);
    }
    let cx = Context {
        limits,
        progress,
        password: None,
        list_only: false,
    };
    format::write(Format::Zip, &archive, &WriteOpts::default(), &cx)
}
