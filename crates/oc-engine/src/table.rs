//! Tabular formats: CSV, TSV and flat JSON, converted through a text-cell table.
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, FormatDescriptor, FormatId, Limits, Note, Notes, Ownership,
    Progress, Result, Severity,
};
use oc_sheet::{delimited, json, ods, xlsx, Table, TableLimits, Workbook};
use oc_text::{Encoding, Policy};

pub const CSV: FormatId = FormatId("csv");
pub const TSV: FormatId = FormatId("tsv");
pub const JSON: FormatId = FormatId("json");
pub const XLSX: FormatId = FormatId("xlsx");
pub const ODS: FormatId = FormatId("ods");
pub const TABLE_IR: FormatId = FormatId("table-ir");

pub const FORMATS: [FormatId; 5] = [CSV, TSV, JSON, XLSX, ODS];

fn is_workbook(id: FormatId) -> bool {
    id == XLSX || id == ODS
}

pub fn is_table(id: FormatId) -> bool {
    FORMATS.contains(&id)
}

fn spec(
    key: &'static str,
    i18n_key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: OptionKind,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        help,
        i18n_key,
        kind,
    }
}

/// Options that steer how the input is read; every table output accepts them.
fn read_options() -> Vec<OptionSpec> {
    vec![
        spec(
            "source_delimiter",
            "opt.table.source_delimiter",
            "Input delimiter",
            "auto detects it; otherwise comma, semicolon, tab or pipe (CSV and TSV input)",
            OptionKind::Enum {
                choices: vec!["auto", "comma", "semicolon", "tab", "pipe"],
                default: "auto",
            },
        ),
        spec(
            "has_header",
            "opt.table.has_header",
            "First row is a header",
            "Turn off when the first row is data (CSV and TSV input)",
            OptionKind::Bool { default: true },
        ),
        spec(
            "source_encoding",
            "opt.table.source_encoding",
            "Fallback input encoding",
            "Used when the file has no byte-order mark and is not valid UTF-8",
            OptionKind::Text {
                default: "windows-1252".into(),
            },
        ),
        spec(
            "sheet",
            "opt.table.sheet",
            "Sheet to convert",
            "Name or number (from 1) of the sheet when a workbook goes to CSV, TSV or JSON; empty means the first visible one",
            OptionKind::Text { default: String::new() },
        ),
    ]
}

fn newline_bom() -> Vec<OptionSpec> {
    vec![
        spec(
            "newline",
            "opt.table.newline",
            "Line endings",
            "crlf (RFC 4180) or lf",
            OptionKind::Enum {
                choices: vec!["crlf", "lf"],
                default: "crlf",
            },
        ),
        spec(
            "bom",
            "opt.table.bom",
            "Write UTF-8 byte-order mark",
            "Helps Excel recognise UTF-8",
            OptionKind::Bool { default: false },
        ),
    ]
}

fn options_for(id: FormatId) -> OptionSchema {
    let mut specs = read_options();
    if id == CSV {
        specs.push(spec(
            "delimiter",
            "opt.table.delimiter",
            "Output delimiter",
            "comma, semicolon, tab or pipe",
            OptionKind::Enum {
                choices: vec!["comma", "semicolon", "tab", "pipe"],
                default: "comma",
            },
        ));
    }
    if is_workbook(id) {
        specs.push(spec(
            "sheet_name",
            "opt.table.sheet_name",
            "Sheet name",
            "Name of the sheet when the input has no sheets of its own",
            OptionKind::Text {
                default: "Sheet1".into(),
            },
        ));
        specs.push(spec(
            "detect_types",
            "opt.table.detect_types",
            "Store numbers and booleans as such",
            "Off keeps every cell as text",
            OptionKind::Bool { default: true },
        ));
        specs.push(spec(
            "detect_dates",
            "opt.table.detect_dates",
            "Store YYYY-MM-DD dates as dates",
            "Needs the option above",
            OptionKind::Bool { default: true },
        ));
        specs.push(spec(
            "style_header",
            "opt.table.style_header",
            "Bold header row",
            "Also freezes the header row in XLSX",
            OptionKind::Bool { default: true },
        ));
    } else if id == JSON {
        specs.push(spec(
            "indent",
            "opt.table.indent",
            "Indent",
            "Spaces per level; 0 writes compact JSON",
            OptionKind::Int {
                min: 0,
                max: 8,
                default: 2,
            },
        ));
        specs.push(spec(
            "ensure_ascii",
            "opt.table.ensure_ascii",
            "Escape non-ASCII characters",
            "Write \\uXXXX escapes instead of the characters themselves",
            OptionKind::Bool { default: false },
        ));
    } else {
        specs.extend(newline_bom());
    }
    OptionSchema { specs }
}

pub fn register(registry: &mut Registry) {
    let rows = [
        (
            CSV,
            "CSV",
            &["csv"][..],
            &["text/csv"][..],
            "RFC 4180 (lenient reader)",
        ),
        (
            TSV,
            "TSV",
            &["tsv", "tab"][..],
            &["text/tab-separated-values"][..],
            "IANA text/tab-separated-values, RFC 4180 quoting",
        ),
        (
            XLSX,
            "XLSX",
            &["xlsx", "xlsm"][..],
            &["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"][..],
            "ECMA-376 SpreadsheetML (cells, shared strings, dates)",
        ),
        (
            ODS,
            "ODS",
            &["ods"][..],
            &["application/vnd.oasis.opendocument.spreadsheet"][..],
            "OASIS ODF 1.3 spreadsheet (cells, repeats, dates)",
        ),
        (
            JSON,
            "JSON table",
            &["json"][..],
            &["application/json"][..],
            "RFC 8259; arrays of flat objects or arrays",
        ),
    ];
    for (id, name, extensions, mime, spec) in rows {
        registry.add_format(FormatDescriptor {
            id,
            name,
            category: Category::Sheet,
            extensions,
            mime,
            sniffers: Vec::new(),
            capabilities: Capabilities {
                read: true,
                write: true,
                ..Capabilities::default()
            },
            writer_options: options_for(id),
            reader_options: Default::default(),
            ownership: Ownership::Own,
            spec,
        });
        registry.add_edge(Edge {
            from: id,
            to: TABLE_IR,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 1,
            description: if is_workbook(id) {
                "Read spreadsheet"
            } else if id == JSON {
                "Parse JSON table"
            } else {
                "Parse delimited text"
            },
        });
        registry.add_edge(Edge {
            from: TABLE_IR,
            to: id,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 1,
            description: if is_workbook(id) {
                "Write spreadsheet"
            } else if id == JSON {
                "Write JSON table"
            } else {
                "Write delimited text"
            },
        });
    }
}

fn decode_source(data: &[u8], opts: &Options, notes: &mut Notes) -> Result<String> {
    let name = opts.text("source_encoding").unwrap_or("windows-1252");
    let fallback = Encoding::from_name(name).ok_or_else(|| {
        Error::invalid_option("source_encoding", format!("unknown encoding '{name}'"))
    })?;
    let found = oc_text::detect(data);
    let mut enc = found.encoding;
    if !found.certain && enc == Encoding::Windows1252 {
        enc = fallback;
    }
    let body = data.get(found.bom_len..).unwrap_or_default();
    let decoded = match oc_text::decode(body, enc, Policy::Strict) {
        // windows-1252 leaves five bytes undefined; ISO-8859-1 maps them to controls, which
        // is what spreadsheet programs do too.
        Err(_) if enc == Encoding::Windows1252 => {
            enc = Encoding::Latin1;
            oc_text::decode(body, enc, Policy::Strict)?
        }
        other => other?,
    };
    if enc != Encoding::Utf8 {
        notes.push(
            Note::new(
                Severity::Info,
                "source_encoding",
                format!("the input was read as {}", enc.name()),
            )
            .with_detail(enc.name()),
        );
    }
    Ok(decoded.0)
}

fn read(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &TableLimits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Table> {
    let text = decode_source(data, opts, notes)?;
    if from == JSON {
        return json::read(&text, limits);
    }
    let delim = match opts.text("source_delimiter").unwrap_or("auto") {
        "auto" if from == TSV => b'\t',
        "auto" => {
            let d = delimited::detect_delimiter(&text, b',');
            if d != b',' {
                notes.push(
                    Note::new(
                        Severity::Info,
                        "delimiter_detected",
                        format!(
                            "the input delimiter was detected as {}",
                            delimited::delimiter_name(d)
                        ),
                    )
                    .with_detail(delimited::delimiter_name(d)),
                );
            }
            d
        }
        name => delimited::delimiter_from_name(name)
            .ok_or_else(|| Error::invalid_option("source_delimiter", "unknown delimiter"))?,
    };
    let rows = delimited::parse(&text, delim, limits, progress)?;
    Ok(delimited::to_table(
        rows,
        opts.bool("has_header") != Some(false),
    ))
}

fn write(
    to: FormatId,
    table: &Table,
    opts: &Options,
    limits: &TableLimits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    table.check_padded_size(limits)?;
    if table.is_ragged() {
        notes.push(Note::new(
            Severity::Info,
            "rows_padded",
            "rows have different numbers of fields; short rows were padded with empty values",
        ));
    }
    if to == JSON {
        let indent = usize::try_from(opts.int("indent").unwrap_or(2)).unwrap_or(2);
        let text = json::write(
            table,
            indent,
            opts.bool("ensure_ascii") == Some(true),
            notes,
        );
        return Ok(text.into_bytes());
    }
    let delim = if to == TSV {
        b'\t'
    } else {
        delimited::delimiter_from_name(opts.text("delimiter").unwrap_or("comma"))
            .ok_or_else(|| Error::invalid_option("delimiter", "unknown delimiter"))?
    };
    let text = delimited::write(table, delim, opts.text("newline") != Some("lf"), progress)?;
    let mut out = Vec::with_capacity(text.len() + 3);
    if opts.bool("bom") == Some(true) {
        out.extend_from_slice(oc_text::bom(Encoding::Utf8));
    }
    out.extend_from_slice(text.as_bytes());
    Ok(out)
}

/// Any table format as a workbook (text formats become a single sheet).
fn read_book(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    tl: &TableLimits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Workbook> {
    let has_header = opts.bool("has_header") != Some(false);
    if from == XLSX {
        return xlsx::read(data, limits, progress, has_header, notes);
    }
    if from == ODS {
        return ods::read(data, limits, progress, has_header, notes);
    }
    let table = read(from, data, opts, tl, progress, notes)?;
    let mut b = Workbook::single(table, opts.text("sheet_name").unwrap_or("Sheet1"));
    if let Some(s) = b.sheets.first_mut() {
        if s.name.is_empty() {
            s.name = "Sheet1".into();
        }
    }
    Ok(b)
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn json_row(row: &[String], max_cols: usize) -> String {
    let cells: Vec<String> = row
        .iter()
        .take(max_cols)
        .map(|c| {
            if c.chars().count() > 200 {
                json_str(&format!("{}…", c.chars().take(200).collect::<String>()))
            } else {
                json_str(c)
            }
        })
        .collect();
    format!("[{}]", cells.join(","))
}

/// The start of a table (one sheet of a workbook) as JSON for the web preview:
/// `{"format","sheets":[{"name","rows","cols","hidden"}],"sheet","header","rows","total_rows","total_cols","truncated"}`.
pub fn preview_json(
    from: FormatId,
    data: &[u8],
    sheet: &str,
    max_rows: usize,
    max_cols: usize,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<String> {
    let mut notes = Notes::new();
    let tl = TableLimits::from(limits);
    let book = read_book(
        from,
        data,
        &Options::default(),
        limits,
        &tl,
        progress,
        &mut notes,
    )?;
    let chosen = if let Some(n) = sheet.parse::<usize>().ok().filter(|n| *n >= 1) {
        book.sheets.get(n - 1)
    } else if sheet.is_empty() {
        book.sheets
            .iter()
            .find(|s| !s.hidden)
            .or_else(|| book.sheets.first())
    } else {
        book.sheets.iter().find(|s| s.name == sheet)
    }
    .ok_or_else(|| Error::invalid_option("sheet", "no such sheet"))?;
    let t = &chosen.table;
    let sheets: Vec<String> = book
        .sheets
        .iter()
        .map(|s| {
            format!(
                "{{\"name\":{},\"rows\":{},\"cols\":{},\"hidden\":{}}}",
                json_str(&s.name),
                s.table.rows.len() + usize::from(s.table.header.is_some()),
                s.table.width(),
                s.hidden
            )
        })
        .collect();
    let rows: Vec<String> = t
        .rows
        .iter()
        .take(max_rows)
        .map(|r| json_row(r, max_cols))
        .collect();
    Ok(format!(
        "{{\"format\":{},\"sheets\":[{}],\"sheet\":{},\"header\":{},\"rows\":[{}],\"total_rows\":{},\"total_cols\":{},\"truncated\":{}}}",
        json_str(from.0),
        sheets.join(","),
        json_str(&chosen.name),
        t.header.as_ref().map_or("null".to_string(), |h| json_row(h, max_cols)),
        rows.join(","),
        t.rows.len(),
        t.width(),
        t.rows.len() > max_rows || t.width() > max_cols
    ))
}

/// The sheet a single-table format receives from a workbook.
fn pick_sheet<'a>(
    book: &'a Workbook,
    opts: &Options,
    notes: &mut Notes,
) -> Result<&'a oc_sheet::Sheet> {
    let want = opts.text("sheet").unwrap_or("").trim();
    let chosen = if want.is_empty() {
        book.sheets
            .iter()
            .find(|s| !s.hidden)
            .or_else(|| book.sheets.first())
    } else if let Some(n) = want.parse::<usize>().ok().filter(|n| *n >= 1) {
        book.sheets.get(n - 1)
    } else {
        book.sheets
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(want))
    };
    let sheet = chosen.ok_or_else(|| {
        let names: Vec<&str> = book.sheets.iter().map(|s| s.name.as_str()).collect();
        Error::invalid_option(
            "sheet",
            format!("no such sheet; the workbook has: {}", names.join(", ")),
        )
    })?;
    if book.sheets.len() > 1 {
        notes.push(
            Note::new(
                Severity::Warning,
                "other_sheets_dropped",
                format!(
                    "only the sheet '{}' was converted; the workbook has {} sheets",
                    sheet.name,
                    book.sheets.len()
                ),
            )
            .with_detail(sheet.name.clone()),
        );
    }
    if !sheet.formulas.is_empty() {
        notes.push(Note::new(
            Severity::Info,
            "formulas_values_only",
            "formulas were written as their calculated values",
        ));
    }
    Ok(sheet)
}

/// Converts between the table formats.
pub fn convert(
    from: FormatId,
    to: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let tl = TableLimits::from(limits);
    let book = read_book(from, data, opts, limits, &tl, progress, &mut notes)?;
    progress.check_cancelled()?;
    let table_ref;
    let out = if is_workbook(to) {
        let mut book = book;
        if !is_workbook(from) {
            // A single sheet made from text: it takes the name the user chose.
            if let (Some(s), Some(n)) = (book.sheets.first_mut(), opts.text("sheet_name")) {
                if !n.trim().is_empty() {
                    s.name = n.to_string();
                }
            }
        }
        for s in &book.sheets {
            s.table.check_padded_size(&tl)?;
        }
        let wo = xlsx::WriteOptions {
            detect_types: opts.bool("detect_types") != Some(false),
            detect_dates: opts.bool("detect_dates") != Some(false),
            style_header: opts.bool("style_header") != Some(false),
        };
        table_ref = book
            .sheets
            .first()
            .map(|s| s.table.clone())
            .unwrap_or_default();
        if to == XLSX {
            xlsx::write(&book, &wo, limits, progress, &mut notes)?
        } else {
            ods::write(
                &book,
                &ods::WriteOptions {
                    detect_types: wo.detect_types,
                    detect_dates: wo.detect_dates,
                    style_header: wo.style_header,
                },
                limits,
                progress,
                &mut notes,
            )?
        }
    } else {
        let sheet = pick_sheet(&book, opts, &mut notes)?;
        table_ref = sheet.table.clone();
        write(to, &sheet.table, opts, &tl, progress, &mut notes)?
    };
    let table = table_ref;
    if table.coerced_scalars {
        notes.push(Note::new(
            Severity::Warning,
            "json_scalars_as_text",
            "JSON numbers, booleans and null were written as text (null becomes an empty value)",
        ));
    }
    if out == data {
        notes.push(Note::new(
            Severity::Info,
            "no_change",
            "the output is identical to the input",
        ));
    }
    Ok((out, notes))
}
