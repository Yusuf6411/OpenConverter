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
pub const DOCX: FormatId = FormatId("docx");
pub const ODT: FormatId = FormatId("odt");
pub const RTF: FormatId = FormatId("rtf");
pub const EPUB: FormatId = FormatId("epub");
pub const FB2: FormatId = FormatId("fb2");
pub const PPTX: FormatId = FormatId("pptx");
pub const ODP: FormatId = FormatId("odp");
pub const DOC: FormatId = FormatId("doc");
pub const PPT: FormatId = FormatId("ppt");
pub const FORMATS: [FormatId; 11] = [MD, HTML, DOCX, ODT, RTF, EPUB, FB2, PPTX, ODP, DOC, PPT];

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
        Info {
            id: DOCX,
            name: "Word document (DOCX)",
            exts: &["docx"],
            mime: &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"],
            sniff: vec![],
            read: true,
            write: true,
            spec: "ISO/IEC 29500 WordprocessingML subset (own reader and writer)",
        },
        Info {
            id: ODT,
            name: "OpenDocument text (ODT)",
            exts: &["odt", "fodt"],
            mime: &["application/vnd.oasis.opendocument.text"],
            sniff: vec![],
            read: true,
            write: true,
            spec: "ISO/IEC 26300 OpenDocument 1.2 text subset (own reader and writer)",
        },
        Info {
            id: RTF,
            name: "Rich Text Format (RTF)",
            exts: &["rtf"],
            mime: &["application/rtf", "text/rtf"],
            sniff: vec![s(b"{\\rtf", 90)],
            read: true,
            write: true,
            spec: "RTF 1.9 subset: runs, paragraphs, lists, tables, hyperlinks, PNG and JPEG pictures (own reader and writer)",
        },
        Info {
            id: EPUB,
            name: "EPUB e-book",
            exts: &["epub"],
            mime: &["application/epub+zip"],
            sniff: vec![],
            read: true,
            write: true,
            spec: "EPUB 3.3 (with an EPUB 2 navigation file): own reader and writer, chapters split at the top heading",
        },
        Info {
            id: FB2,
            name: "FictionBook (FB2)",
            exts: &["fb2"],
            mime: &["application/x-fictionbook+xml"],
            sniff: vec![],
            read: true,
            write: false,
            spec: "FictionBook 2.1 (own reader; read only)",
        },
        Info {
            id: PPTX,
            name: "PowerPoint presentation (PPTX)",
            exts: &["pptx"],
            mime: &["application/vnd.openxmlformats-officedocument.presentationml.presentation"],
            sniff: vec![],
            read: true,
            write: false,
            spec: "ISO/IEC 29500 PresentationML: slide text, lists, tables and pictures (own reader; read only)",
        },
        Info {
            id: DOC,
            name: "Word 97-2003 document (DOC)",
            exts: &["doc"],
            mime: &["application/msword"],
            sniff: vec![],
            read: true,
            write: false,
            spec: "Word binary format: text, tables and links from the piece table in a compound file (own reader; read only)",
        },
        Info {
            id: PPT,
            name: "PowerPoint 97-2003 presentation (PPT)",
            exts: &["ppt"],
            mime: &["application/vnd.ms-powerpoint"],
            sniff: vec![],
            read: true,
            write: false,
            spec: "PowerPoint binary format: slide text from the document stream in a compound file (own reader; read only)",
        },
        Info {
            id: ODP,
            name: "OpenDocument presentation (ODP)",
            exts: &["odp"],
            mime: &["application/vnd.oasis.opendocument.presentation"],
            sniff: vec![],
            read: true,
            write: false,
            spec: "ISO/IEC 26300 presentation: slide text, lists, tables and pictures (own reader; read only)",
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
    for a in &all {
        if a.read {
            edge(
                registry,
                a.id,
                crate::pdf::PDF,
                2,
                4,
                "Lay out the text on pages and embed fonts",
            );
        }
    }
    for a in &all {
        if a.read {
            for img in [
                crate::image::PNG,
                crate::image::JPEG,
                crate::image::WEBP,
                crate::image::BMP,
                crate::image::GIF,
            ] {
                edge(
                    registry,
                    a.id,
                    img,
                    3,
                    5,
                    "Draw the first page of the laid-out document",
                );
            }
        }
    }
    for img in [
        crate::image::PNG,
        crate::image::JPEG,
        crate::image::WEBP,
        crate::image::BMP,
        crate::image::GIF,
    ] {
        edge(
            registry,
            crate::TXT,
            img,
            3,
            5,
            "Draw the first page of the laid-out text",
        );
    }
    edge(
        registry,
        crate::TXT,
        crate::pdf::PDF,
        2,
        3,
        "Lay out the text on pages and embed fonts",
    );
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
    if !d.certain && from == FB2 {
        let head = String::from_utf8_lossy(data.get(..200).unwrap_or(data)).to_ascii_lowercase();
        if let Some(p) = head.find("encoding=") {
            let name: String = head
                .get(p + 9..)
                .unwrap_or("")
                .trim_start_matches(['"', '\''])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if let Some(e) = oc_text::Encoding::from_name(&name) {
                enc = e;
            }
        }
    }
    if !d.certain && from == HTML {
        let head = String::from_utf8_lossy(data.get(..2048).unwrap_or(data)).to_ascii_lowercase();
        if let Some(p) = head.find("charset=") {
            let name: String = head
                .get(p + 8..)
                .unwrap_or("")
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
        DOCX => oc_document::docx_in::read(data, limits, notes),
        ODT => oc_document::odt_in::read(data, limits, notes),
        RTF => oc_document::rtf_in::read(data, limits, notes),
        EPUB => oc_document::epub_in::read(data, limits, notes),
        PPTX => oc_document::pptx_in::read(data, limits, notes),
        DOC => oc_document::doc_in::read(data, limits, notes),
        PPT => oc_document::ppt_in::read(data, limits, notes),
        ODP => oc_document::odt_in::read_presentation(data, limits, notes),
        FB2 => {
            let t = decode_text(data, from, notes)?;
            oc_document::fb2_in::read(&t, limits, notes)
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
    limits: &Limits,
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
        DOCX => oc_document::docx_out::write(doc, limits),
        ODT => oc_document::odt_out::write(doc, limits),
        RTF => Ok(oc_document::rtf_out::write(doc, notes)),
        EPUB => oc_document::epub_out::write(doc, limits),
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

/// A document format into a PDF with real text, laid out on pages.
pub fn to_pdf(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let doc = read(from, data, opts, limits, notes)?;
    crate::docpdf::write(&doc, opts, limits, notes)
}
