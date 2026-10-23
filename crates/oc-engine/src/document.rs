//! Documents (Markdown, HTML, and the office and e-book formats) read into one model and
//! written back out; plain text and PDF text join in through the same model.
use oc_core::limits::Limits;
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Note, Notes, Ownership,
    Result, Severity, Sniffer,
};
use oc_document::model::Document;

pub const MD: FormatId = FormatId("md");
pub const HTML: FormatId = FormatId("html");
pub const FORMATS: [FormatId; 2] = [MD, HTML];

pub fn is_document(id: FormatId) -> bool {
    FORMATS.contains(&id)
}

fn spec(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: OptionKind,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        help,
        i18n_key: "opt.document",
        kind,
    }
}

fn embed_option() -> OptionSpec {
    spec(
        "embed_images",
        "Embed pictures",
        "Keep pictures inside the file as data: addresses (the file gets larger); off leaves them out",
        OptionKind::Bool { default: true },
    )
}

fn writer_options(id: FormatId) -> OptionSchema {
    let mut specs = vec![embed_option()];
    if id == HTML {
        specs.push(spec(
            "style",
            "Add a style sheet",
            "Include a small built-in style sheet so the page reads well in a browser",
            OptionKind::Bool { default: true },
        ));
    }
    OptionSchema { specs }
}

fn reader_options(id: FormatId) -> OptionSchema {
    if id == MD {
        OptionSchema {
            specs: vec![spec(
                "github",
                "GitHub extensions",
                "Read tables, strikethrough, task lists and bare web addresses as GitHub does",
                OptionKind::Bool { default: true },
            )],
        }
    } else {
        OptionSchema::default()
    }
}

struct Info {
    id: FormatId,
    name: &'static str,
    exts: &'static [&'static str],
    mime: &'static [&'static str],
    sniff: Vec<Sniffer>,
    read: bool,
    write: bool,
    spec: &'static str,
}

fn infos() -> Vec<Info> {
    let s = |magic: &'static [u8], confidence| Sniffer {
        offset: 0,
        magic,
        confidence,
    };
    vec![
        Info {
            id: MD,
            name: "Markdown",
            exts: &["md", "markdown", "mdown"],
            mime: &["text/markdown"],
            sniff: vec![],
            read: true,
            write: true,
            spec: "CommonMark 0.31 with GitHub tables, strikethrough, task lists and autolinks (own parser)",
        },
        Info {
            id: HTML,
            name: "HTML",
            exts: &["html", "htm", "xhtml"],
            mime: &["text/html", "application/xhtml+xml"],
            sniff: vec![s(b"<!DOCTYPE html", 80), s(b"<!doctype html", 80), s(b"<html", 70), s(b"<HTML", 70)],
            read: true,
            write: true,
            spec: "HTML (own tolerant reader; not the full WHATWG tree builder) and writer",
        },
    ]
}

pub fn register(registry: &mut Registry) {
    let all = infos();
    for i in &all {
        registry.add_format(FormatDescriptor {
            id: i.id,
            name: i.name,
            category: Category::Document,
            extensions: i.exts,
            mime: i.mime,
            sniffers: i.sniff.clone(),
            capabilities: Capabilities {
                read: i.read,
                write: i.write,
                metadata: true,
                multi_page: false,
                ..Capabilities::default()
            },
            writer_options: writer_options(i.id),
            reader_options: reader_options(i.id),
            ownership: Ownership::Own,
            spec: i.spec,
        });
    }
    let edge = |registry: &mut Registry, from, to, loss, time, description| {
        registry.add_edge(Edge {
            from,
            to,
            kind: EdgeKind::Transcode,
            loss,
            ownership: Ownership::Own,
            time,
            description,
        });
    };
    for a in &all {
        for b in &all {
            if a.id != b.id && a.read && b.write {
                let loss = if (a.id == MD && b.id == HTML) || (a.id == HTML && b.id == MD) {
                    1
                } else {
                    2
                };
                edge(
                    registry,
                    a.id,
                    b.id,
                    loss,
                    2,
                    "Read the document, write it in the other format",
                );
            }
        }
        if a.write {
            edge(
                registry,
                crate::TXT,
                a.id,
                2,
                1,
                "Make paragraphs of the text",
            );
        }
        if a.read {
            edge(
                registry,
                a.id,
                crate::TXT,
                3,
                1,
                "Write the text without markup",
            );
        }
    }
    edge(
        registry,
        crate::pdf::PDF,
        MD,
        3,
        5,
        "Rebuild headings, lists and paragraphs from the page text",
    );
    edge(
        registry,
        crate::pdf::PDF,
        HTML,
        3,
        5,
        "Rebuild headings, lists and paragraphs from the page text",
    );
}

fn bad(msg: &str) -> Error {
    Error::new(ErrorCode::InvalidOptions, msg.to_string())
}

/// The text of a file, decoded by BOM, a declared charset (HTML), or by guessing.
fn decode_text(data: &[u8], from: FormatId, notes: &mut Notes) -> Result<String> {
    let d = oc_text::detect(data);
    let mut enc = d.encoding;
    if !d.certain && from == HTML {
        let head = String::from_utf8_lossy(data.get(..2048).unwrap_or(data)).to_ascii_lowercase();
        if let Some(p) = head.find("charset=") {
            let name: String = head.get(p + 8..).unwrap_or("")
                .trim_start_matches(['"', '\''])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if let Some(e) = oc_text::Encoding::from_name(&name) {
                enc = e;
            }
        }
    }
    if !d.certain && enc == oc_text::Encoding::Windows1252 && d.encoding == enc {
        notes.push(Note::new(
            Severity::Warning,
            "encoding_guessed",
            "input is not valid UTF-8; assumed windows-1252",
        ));
    }
    let (t, n) = oc_text::decode(
        data.get(d.bom_len..).unwrap_or(&[]),
        enc,
        oc_text::Policy::Replace,
    )?;
    notes.extend(n);
    Ok(t)
}

pub fn read(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Document> {
    limits.check_alloc(data.len() as u64)?;
    match from {
        MD => {
            let t = decode_text(data, from, notes)?;
            Ok(oc_document::markdown::read(
                &t,
                &oc_document::markdown::ReadOptions {
                    github: opts.bool("github").unwrap_or(true),
                },
            ))
        }
        HTML => {
            let t = decode_text(data, from, notes)?;
            let mut n = Notes::new();
            let doc = oc_document::html_in::read(&t, &mut n, limits.max_depth as usize);
            notes.extend(n);
            Ok(doc)
        }
        _ => Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "this document format cannot be read",
        )),
    }
}

fn md_image_src(doc: &Document, embed: bool) -> impl Fn(&str) -> String + '_ {
    move |s: &str| match doc.image(s) {
        Some(img) if embed => format!(
            "data:{};base64,{}",
            img.mime,
            oc_document::base64::encode(&img.data)
        ),
        Some(_) => String::new(),
        None => s.to_string(),
    }
}

pub fn write(
    to: FormatId,
    doc: &Document,
    opts: &Options,
    _limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let embed = opts.bool("embed_images").unwrap_or(true);
    if !embed && !doc.images.is_empty() {
        notes.push(Note::new(
            Severity::Loss,
            "document.images_dropped",
            format!(
                "{} picture(s) were left out (embedding is off)",
                doc.images.len()
            ),
        ));
    }
    match to {
        MD => {
            let f = md_image_src(doc, embed);
            let s = oc_document::md_out::write(
                doc,
                &oc_document::md_out::WriteOptions { image_src: &f },
            );
            Ok(s.into_bytes())
        }
        HTML => Ok(oc_document::html_out::page(
            doc,
            &oc_document::html_out::PageOptions {
                embed_images: embed,
                style: opts.bool("style").unwrap_or(true),
                xhtml: false,
            },
        )
        .into_bytes()),
        _ => Err(bad("this document format cannot be written")),
    }
}

/// Document to document.
pub fn convert(
    from: FormatId,
    to: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let doc = read(from, data, opts, limits, notes)?;
    write(to, &doc, opts, limits, notes)
}

/// Plain text (already UTF-8) into a document format.
pub fn from_text(
    to: FormatId,
    text: &str,
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let doc = oc_document::text_io::read(text);
    write(to, &doc, opts, limits, notes)
}

/// A document format into UTF-8 text.
pub fn to_text(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<String> {
    let doc = read(from, data, opts, limits, notes)?;
    Ok(oc_document::text_io::write(&doc))
}

/// PDF text into a document format, through Markdown rebuilt from the page layout.
pub fn from_pdf(
    to: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    use oc_pdf::doc::Permission;
    let pdf = crate::pdf::open(data, opts, limits)?;
    if !pdf.permission(Permission::Copy) {
        return Err(Error::new(
            ErrorCode::EncryptedNeedPassword,
            "this PDF's owner does not allow copying its text",
        ));
    }
    let pages = crate::pdf::selection(&pdf, opts, false)?;
    let fonts = oc_font::db::FontDb::system();
    let texts = crate::pdf::page_text(&pdf, &pages, &fonts, notes);
    let md = oc_pdf::text::to_markdown(&texts);
    let doc =
        oc_document::markdown::read(&md, &oc_document::markdown::ReadOptions { github: true });
    if doc.blocks.is_empty() {
        notes.push(Note::new(
            Severity::Warning,
            "pdf.no_text",
            "no text was found in the chosen pages",
        ));
    }
    write(to, &doc, opts, limits, notes)
}
