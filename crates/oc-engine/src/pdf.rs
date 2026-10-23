//! PDF: read as pages (drawn as pictures, extracted as text, split into files), written from
//! pictures, and rewritten page by page.
use crate::image;
use oc_core::limits::Limits;
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Note, Notes, Ownership,
    Progress, Result, Severity, Sniffer,
};
use oc_pdf::doc::Permission;
use oc_pdf::interp::{interpret_page, Settings};
use oc_pdf::render::{render, RenderOptions};

pub const PDF: FormatId = FormatId("pdf");
pub const PAGES: FormatId = FormatId("pages.zip");
pub const FORMATS: [FormatId; 2] = [PDF, PAGES];

pub fn is_pdf(id: FormatId) -> bool {
    id == PDF
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
        i18n_key: "opt.pdf",
        kind,
    }
}

fn reader_options() -> OptionSchema {
    OptionSchema {
        specs: vec![
            spec(
                "pages",
                "Pages",
                "Which pages to use, such as 1-3,5,last or odd; empty means all (a picture uses the first one chosen)",
                OptionKind::Text {
                    default: String::new(),
                },
            ),
            spec(
                "dpi",
                "Resolution",
                "Pixels per inch when a page is drawn as a picture (72 points make one inch)",
                OptionKind::Float {
                    min: 10.0,
                    max: 1200.0,
                    default: 150.0,
                },
            ),
            spec(
                "annotations",
                "Draw annotations",
                "Include stamps, form field values and other annotations when drawing pages",
                OptionKind::Bool { default: true },
            ),
            spec(
                "source_password",
                "Password of the PDF",
                "Needed to open an encrypted PDF",
                OptionKind::Text {
                    default: String::new(),
                },
            ),
        ],
    }
}

fn writer_options() -> OptionSchema {
    OptionSchema {
        specs: vec![
            spec(
                "page_size",
                "Page size",
                "For pictures: fit makes each page as large as its picture, a4 and letter centre it on a standard page",
                OptionKind::Enum {
                    choices: vec!["fit", "a4", "letter"],
                    default: "fit",
                },
            ),
            spec(
                "margin",
                "Margin",
                "Margin in points around a picture or text on an a4 or letter page (documents use 56.7, 20 mm, when this is 0)",
                OptionKind::Float {
                    min: 0.0,
                    max: 200.0,
                    default: 0.0,
                },
            ),
            spec(
                "font_size",
                "Font size",
                "For documents and text: the size of body text in points",
                OptionKind::Float {
                    min: 6.0,
                    max: 48.0,
                    default: 11.0,
                },
            ),
            spec(
                "font",
                "Font",
                "For documents and text: serif, sans, mono, or the name of an installed font family",
                OptionKind::Text {
                    default: "serif".into(),
                },
            ),
            spec(
                "image_dpi",
                "Picture resolution",
                "Pixels per inch used to size a page (0 uses what the picture says, or 96)",
                OptionKind::Float {
                    min: 0.0,
                    max: 2400.0,
                    default: 0.0,
                },
            ),
            spec(
                "rotate",
                "Rotate pages",
                "Clockwise rotation added to every page, in degrees",
                OptionKind::Enum {
                    choices: vec!["0", "90", "180", "270"],
                    default: "0",
                },
            ),
            spec(
                "compress",
                "Recompress streams",
                "Deflate content that is stored raw or lightly compressed, which makes some files smaller",
                OptionKind::Bool { default: false },
            ),
            spec(
                "strip_metadata",
                "Remove metadata",
                "Leave out the title, author and other document information and the XMP packet",
                OptionKind::Bool { default: false },
            ),
            spec(
                "password",
                "Password for the new PDF",
                "Encrypts the output with AES-256; the same password opens it and grants full rights",
                OptionKind::Text {
                    default: String::new(),
                },
            ),
        ],
    }
}

fn pages_options() -> OptionSchema {
    OptionSchema {
        specs: vec![
            spec(
                "page_format",
                "File type of each page",
                "png or jpeg draw the page, pdf cuts it out as a one-page PDF, txt extracts its text",
                OptionKind::Enum {
                    choices: vec!["png", "jpeg", "pdf", "txt"],
                    default: "png",
                },
            ),
            spec(
                "quality",
                "JPEG quality",
                "Quality of the pictures when page_format is jpeg",
                OptionKind::Int {
                    min: 1,
                    max: 100,
                    default: 90,
                },
            ),
        ],
    }
}

pub fn register(registry: &mut Registry) {
    registry.add_format(FormatDescriptor {
        id: PDF,
        name: "PDF",
        category: Category::Document,
        extensions: &["pdf"],
        mime: &["application/pdf"],
        sniffers: vec![Sniffer {
            offset: 0,
            magic: b"%PDF-",
            confidence: 95,
        }],
        capabilities: Capabilities {
            read: true,
            write: true,
            ..Capabilities::default()
        },
        writer_options: writer_options(),
        reader_options: reader_options(),
        ownership: Ownership::Own,
        spec: "ISO 32000-1/-2 (own parser, renderer and writer; AES/RC4 via RustCrypto)",
    });
    registry.add_format(FormatDescriptor {
        id: PAGES,
        name: "PDF pages (ZIP)",
        category: Category::Document,
        extensions: &["pages.zip"],
        mime: &["application/zip"],
        sniffers: Vec::new(),
        capabilities: Capabilities {
            read: false,
            write: true,
            ..Capabilities::default()
        },
        writer_options: pages_options(),
        reader_options: Default::default(),
        ownership: Ownership::Own,
        spec: "ZIP with one numbered file per page",
    });
    registry.add_edge(Edge {
        from: PDF,
        to: PDF,
        kind: EdgeKind::Transcode,
        loss: 0,
        ownership: Ownership::Own,
        time: 2,
        description: "Select, rotate, recompress or encrypt pages",
    });
    registry.add_edge(Edge {
        from: PDF,
        to: PAGES,
        kind: EdgeKind::Transcode,
        loss: 1,
        ownership: Ownership::Own,
        time: 5,
        description: "Write each page as its own file",
    });
    registry.add_edge(Edge {
        from: PDF,
        to: crate::TXT,
        kind: EdgeKind::Transcode,
        loss: 3,
        ownership: Ownership::Own,
        time: 3,
        description: "Extract the text of the pages",
    });
    for img in image::FORMATS {
        if matches!(
            img,
            image::PNG
                | image::JPEG
                | image::WEBP
                | image::BMP
                | image::TIFF
                | image::GIF
                | image::QOI
        ) {
            registry.add_edge(Edge {
                from: PDF,
                to: img,
                kind: EdgeKind::Transcode,
                loss: 2,
                ownership: Ownership::Own,
                time: 4,
                description: "Draw the first page",
            });
            registry.add_edge(Edge {
                from: img,
                to: PDF,
                kind: EdgeKind::Transcode,
                loss: u32::from(img != image::JPEG && img != image::PNG),
                ownership: Ownership::Own,
                time: 2,
                description: "Place the picture on a page",
            });
        }
    }
}

/// Open a PDF with the password the options carry.
pub fn open(data: &[u8], opts: &Options, limits: &Limits) -> Result<oc_pdf::Pdf> {
    limits.check_alloc(data.len() as u64)?;
    let mut pdf = oc_pdf::Pdf::open(data.to_vec())?;
    if pdf.needs_password() {
        let pw = opts.text("source_password").unwrap_or("");
        if pw.is_empty() {
            return Err(Error::new(
                ErrorCode::EncryptedNeedPassword,
                "this PDF is protected by a password; set source_password",
            ));
        }
        if !pdf.authenticate(pw.as_bytes()) {
            return Err(Error::new(
                ErrorCode::WrongPassword,
                "the password does not open this PDF",
            ));
        }
    }
    Ok(pdf)
}

pub(crate) fn selection(
    pdf: &oc_pdf::Pdf,
    opts: &Options,
    default_first: bool,
) -> Result<Vec<usize>> {
    let total = pdf.pages().len();
    if total == 0 {
        return Err(Error::new(ErrorCode::CorruptFile, "the PDF has no pages"));
    }
    let spec = opts.text("pages").unwrap_or("").trim();
    if spec.is_empty() {
        return Ok(if default_first {
            vec![0]
        } else {
            (0..total).collect()
        });
    }
    oc_pdf::edit::parse_pages(spec, total)
}

fn refuse(what: &str) -> Error {
    Error::new(
        ErrorCode::DrmProtected,
        format!(
            "the PDF's permissions do not allow {what}; open it with the owner password (source_password) if you are entitled to"
        ),
    )
}

fn render_notes(strings: &[String], notes: &mut Notes) {
    for s in strings {
        let (sev, code) = if s.contains("not embedded") || s.contains("was used instead") {
            (Severity::Info, "pdf.font_substituted")
        } else {
            (Severity::Warning, "pdf.render")
        };
        let already = notes.iter().any(|n| n.message == *s);
        if !already {
            notes.push(Note::new(sev, code, s.clone()));
        }
    }
}

/// Draw one page at `scale` device pixels per point.
fn draw(
    pdf: &oc_pdf::Pdf,
    index: usize,
    scale: f64,
    opts: &Options,
    fonts: &oc_font::db::FontDb,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<oc_image::Image> {
    let pages = pdf.pages();
    let info = pages
        .get(index)
        .ok_or_else(|| Error::new(ErrorCode::InvalidOptions, "no such page"))?;
    let set = Settings {
        fonts: Some(fonts),
        annotations: opts.bool("annotations").unwrap_or(true),
    };
    let page = interpret_page(pdf, info, &set);
    render_notes(&page.notes, notes);
    let bg = opts.color("background").unwrap_or([255, 255, 255, 255]);
    let ropts = RenderOptions {
        scale,
        background: (bg[3] > 0).then(|| {
            [
                f32::from(bg[0]) / 255.0,
                f32::from(bg[1]) / 255.0,
                f32::from(bg[2]) / 255.0,
            ]
        }),
    };
    let canvas = render(&page, &ropts, limits)?;
    let rgba = canvas.to_rgba8(None);
    oc_image::Image::new(
        canvas.width as u32,
        canvas.height as u32,
        oc_image::Layout::Rgba,
        oc_image::Depth::Eight,
        rgba,
    )
    .map_err(|e| Error::new(ErrorCode::InternalError, e.to_string()))
}

fn scale_for(pdf: &oc_pdf::Pdf, index: usize, opts: &Options) -> f64 {
    let pages = pdf.pages();
    let (w, h, _) = pages
        .get(index)
        .map_or((612.0, 792.0, oc_raster::Transform::IDENTITY), |p| {
            oc_pdf::interp::page_geometry(pdf, p)
        });
    let width = opts.int("width").filter(|v| *v > 0);
    let height = opts.int("height").filter(|v| *v > 0);
    match (width, height) {
        (Some(wp), _) if w > 0.0 => wp as f64 / w,
        (None, Some(hp)) if h > 0.0 => hp as f64 / h,
        _ => opts.float("dpi").unwrap_or(150.0) / 72.0,
    }
}

/// The first selected page as a picture.
pub fn render_first(
    data: &[u8],
    opts: &Options,
    limits: &Limits,
) -> Result<(oc_image::Image, Notes)> {
    let mut notes = Notes::new();
    let pdf = open(data, opts, limits)?;
    let pages = selection(&pdf, opts, true)?;
    let index = pages.first().copied().unwrap_or(0);
    if pages.len() > 1 {
        notes.push(Note::new(
            Severity::Info,
            "pdf.first_page_only",
            "a picture holds one page; the first chosen page was drawn (use a pages.zip output for all of them)",
        ));
    } else if pdf.pages().len() > 1 && opts.text("pages").unwrap_or("").is_empty() {
        notes.push(Note::new(
            Severity::Info,
            "pdf.first_page_only",
            "the PDF has several pages; the first was drawn (set pages, or write a pages.zip, for others)",
        ));
    }
    let fonts = oc_font::db::FontDb::system();
    let scale = scale_for(&pdf, index, opts);
    let img = draw(&pdf, index, scale, opts, &fonts, limits, &mut notes)?;
    Ok((img, notes))
}

pub(crate) fn page_text(
    pdf: &oc_pdf::Pdf,
    indices: &[usize],
    fonts: &oc_font::db::FontDb,
    notes: &mut Notes,
) -> Vec<oc_pdf::text::PageText> {
    let infos = pdf.pages();
    let set = Settings {
        fonts: Some(fonts),
        annotations: false,
    };
    let mut out = Vec::new();
    for &i in indices {
        let Some(info) = infos.get(i) else { continue };
        let page = interpret_page(pdf, info, &set);
        let pt = oc_pdf::text::extract_page(&page);
        if pt.image_only {
            notes.push(Note::new(
                Severity::Warning,
                "pdf.no_text_layer",
                format!(
                    "page {} has no text, only pictures; text recognition (OCR) is not available",
                    i + 1
                ),
            ));
        }
        out.push(pt);
    }
    out
}

/// The text of the selected pages, UTF-8.
pub fn extract_text(data: &[u8], opts: &Options, limits: &Limits) -> Result<(String, Notes)> {
    let mut notes = Notes::new();
    let pdf = open(data, opts, limits)?;
    if !pdf.permission(Permission::Copy) {
        return Err(refuse("copying its text"));
    }
    let pages = selection(&pdf, opts, false)?;
    let fonts = oc_font::db::FontDb::system();
    let texts = page_text(&pdf, &pages, &fonts, &mut notes);
    let text = oc_pdf::text::to_text(&texts);
    if text.trim().is_empty() {
        notes.push(Note::new(
            Severity::Warning,
            "pdf.no_text",
            "no text was found in the chosen pages",
        ));
    }
    Ok((text, notes))
}

fn zip(
    entries: Vec<oc_archive::Entry>,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Vec<u8>> {
    let cx = oc_archive::Context {
        limits,
        progress,
        password: None,
        list_only: false,
    };
    oc_archive::format::write(
        oc_archive::Format::Zip,
        &oc_archive::Archive {
            entries,
            comment: None,
        },
        &oc_archive::format::WriteOpts::default(),
        &cx,
    )
}

/// Every selected page as its own file inside a ZIP.
pub fn pages_zip(
    data: &[u8],
    opts: &Options,
    source_opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let pdf = open(data, source_opts, limits)?;
    let pages = selection(&pdf, source_opts, false)?;
    let format = opts.text("page_format").unwrap_or("png").to_string();
    if format == "txt" && !pdf.permission(Permission::Copy) {
        return Err(refuse("copying its text"));
    }
    if format == "pdf" && !pdf.permission(Permission::Assemble) {
        return Err(refuse("taking pages out"));
    }
    let fonts = oc_font::db::FontDb::system();
    let mut entries = Vec::with_capacity(pages.len());
    for (n, &i) in pages.iter().enumerate() {
        progress.check_cancelled()?;
        progress.report(n as u64, pages.len() as u64, "pages");
        let name = format!("page_{:04}", i + 1);
        match format.as_str() {
            "png" | "jpeg" => {
                let scale = scale_for(&pdf, i, source_opts);
                let img = draw(&pdf, i, scale, source_opts, &fonts, limits, notes)?;
                let (bytes, ext) = if format == "jpeg" {
                    let schema = oc_core::options::OptionSchema {
                        specs: vec![spec(
                            "quality",
                            "quality",
                            "",
                            OptionKind::Int {
                                min: 1,
                                max: 100,
                                default: 90,
                            },
                        )],
                    };
                    let q = schema.validate(&[(
                        "quality".to_string(),
                        oc_core::options::OptionValue::Int(opts.int("quality").unwrap_or(90)),
                    )])?;
                    // the encoder wants the image schema; the defaults fill the rest
                    let mut o = Options::default();
                    o.set(
                        "quality",
                        oc_core::options::OptionValue::Int(q.int("quality").unwrap_or(90)),
                    );
                    o.set(
                        "background",
                        oc_core::options::OptionValue::Color([255, 255, 255, 255]),
                    );
                    (image::encode(image::JPEG, &img, &o, "jpg")?.0, "jpg")
                } else {
                    (
                        image::encode(image::PNG, &img, &Options::default(), "png")?.0,
                        "png",
                    )
                };
                entries.push(oc_archive::Entry::file(format!("{name}.{ext}"), bytes));
            }
            "pdf" => {
                let out = oc_pdf::edit::assemble(
                    &[oc_pdf::edit::Part {
                        pdf: &pdf,
                        pages: vec![i],
                    }],
                    &oc_pdf::edit::Edit::default(),
                )?;
                entries.push(oc_archive::Entry::file(format!("{name}.pdf"), out.data));
            }
            _ => {
                let t = page_text(&pdf, &[i], &fonts, notes);
                entries.push(oc_archive::Entry::file(
                    format!("{name}.txt"),
                    oc_pdf::text::to_text(&t).into_bytes(),
                ));
            }
        }
    }
    zip(entries, limits, progress)
}

/// PDF to PDF: the chosen pages, rotated, recompressed, stripped or encrypted as asked.
pub fn rewrite(
    data: &[u8],
    opts: &Options,
    source_opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let pdf = open(data, source_opts, limits)?;
    let total = pdf.pages().len();
    let pages = selection(&pdf, source_opts, false)?;
    // The output carries no encryption of the input, so its restrictions would be lifted.
    if !pdf.permission(Permission::Assemble) || !pdf.permission(Permission::Modify) {
        return Err(refuse("changing it or taking pages out"));
    }
    if pdf.is_encrypted() && opts.text("password").unwrap_or("").is_empty() {
        notes.push(Note::new(
            Severity::Info,
            "pdf.decrypted",
            "the input was encrypted; the output is not (set password to encrypt it)",
        ));
    }
    let rotate: i64 = opts
        .text("rotate")
        .and_then(|r| r.parse().ok())
        .unwrap_or(0);
    let pw = opts.text("password").unwrap_or("").as_bytes().to_vec();
    let edit = oc_pdf::edit::Edit {
        rotate,
        compress: opts.bool("compress").unwrap_or(false),
        strip_metadata: opts.bool("strip_metadata").unwrap_or(false),
        encrypt: (!pw.is_empty()).then(|| (pw.clone(), pw)),
    };
    let out = oc_pdf::edit::assemble(
        &[oc_pdf::edit::Part {
            pdf: &pdf,
            pages: pages.clone(),
        }],
        &edit,
    )?;
    for n in out.notes {
        notes.push(Note::new(Severity::Info, "pdf.structure_dropped", n));
    }
    if pages.len() != total {
        notes.push(Note::new(
            Severity::Info,
            "pdf.pages_selected",
            format!("{} of {} pages kept", pages.len(), total),
        ));
    }
    Ok(out.data)
}

/// Pictures (a GIF or WebP animation gives one page per frame) to a PDF.
pub fn from_image(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    use oc_pdf::writer::{ImageOptions, PageSize, Source};
    let page_size = match opts.text("page_size") {
        Some("a4") => PageSize::A4,
        Some("letter") => PageSize::Letter,
        _ => PageSize::Fit,
    };
    let wopts = ImageOptions {
        page_size,
        margin: opts.float("margin").unwrap_or(0.0),
        dpi: opts.float("image_dpi").unwrap_or(0.0),
        title: None,
        rotate: opts
            .text("rotate")
            .and_then(|r| r.parse().ok())
            .unwrap_or(0),
        encrypt: opts
            .text("password")
            .filter(|p| !p.is_empty())
            .map(|p| (p.as_bytes().to_vec(), p.as_bytes().to_vec())),
    };
    let mut sources = Vec::new();
    if from == image::JPEG {
        let (img, n) = image::decode(from, data, limits, progress)?;
        // the file itself goes in untouched; decoding proved it is readable
        notes.extend(n);
        let dpi = img.metadata.dpi.map(|d| d.0);
        sources.push(Source::Jpeg(data.to_vec(), dpi));
        notes.push(Note::new(
            Severity::Info,
            "pdf.jpeg_passthrough",
            "the JPEG data was stored as it is, without re-encoding",
        ));
    } else if let Some((anim, n)) = image::decode_animation(from, data, limits, progress)? {
        notes.extend(n);
        for f in anim.frames {
            sources.push(Source::Raster(f.image));
        }
        notes.push(Note::new(
            Severity::Info,
            "pdf.animation_pages",
            "each frame of the animation became a page",
        ));
    } else {
        let (img, n) = image::decode(from, data, limits, progress)?;
        notes.extend(n);
        sources.push(Source::Raster(img));
    }
    oc_pdf::writer::images_to_pdf(sources, &wopts)
}
