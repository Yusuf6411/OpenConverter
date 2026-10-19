//! Tabular formats: CSV, TSV and flat JSON, converted through a text-cell table.
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, FormatDescriptor, FormatId, Limits, Note, Notes, Ownership,
    Progress, Result, Severity,
};
use oc_sheet::{delimited, json, Table, TableLimits};
use oc_text::{Encoding, Policy};

pub const CSV: FormatId = FormatId("csv");
pub const TSV: FormatId = FormatId("tsv");
pub const JSON: FormatId = FormatId("json");
pub const TABLE_IR: FormatId = FormatId("table-ir");

pub const FORMATS: [FormatId; 3] = [CSV, TSV, JSON];

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
    if id == JSON {
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
            description: if id == JSON {
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
            description: if id == JSON {
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
    let table = read(from, data, opts, &tl, progress, &mut notes)?;
    progress.check_cancelled()?;
    let out = write(to, &table, opts, &tl, progress, &mut notes)?;
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
