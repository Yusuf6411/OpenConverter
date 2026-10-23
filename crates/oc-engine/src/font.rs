//! Font files: TrueType, OpenType, WOFF and collections, converted by rewrapping tables or
//! by rebuilding the outlines, and shown as a specimen picture.
use oc_core::limits::Limits;
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Note, Notes, Ownership,
    Result, Severity, Sniffer,
};
use oc_font::{build, convert, sfnt, Font};

pub const TTF: FormatId = FormatId("ttf");
pub const OTF: FormatId = FormatId("otf");
pub const WOFF: FormatId = FormatId("woff");
pub const TTC: FormatId = FormatId("ttc");
pub const WOFF2: FormatId = FormatId("woff2");

pub const FORMATS: [FormatId; 5] = [TTF, OTF, WOFF, TTC, WOFF2];

pub fn is_font(id: FormatId) -> bool {
    FORMATS.contains(&id)
}

pub fn read_only(id: FormatId) -> bool {
    id == TTC
}

/// WOFF2 can be written (Brotli encoder) but not read (that needs Brotli's static dictionary).
pub fn write_only(id: FormatId) -> bool {
    id == WOFF2
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

fn options_for(id: FormatId) -> OptionSchema {
    let mut specs = vec![spec(
        "font_index",
        "opt.font.index",
        "Font number",
        "Which font of a collection (.ttc) to convert; 0 is the first",
        OptionKind::Int {
            min: 0,
            max: 255,
            default: 0,
        },
    )];
    if id == TTF {
        specs.push(spec(
            "curve_tolerance",
            "opt.font.curve_tolerance",
            "Curve tolerance",
            "Largest allowed deviation, in 1/1000 em, when cubic curves become quadratic",
            OptionKind::Float {
                min: 0.01,
                max: 10.0,
                default: 0.5,
            },
        ));
    }
    if id == WOFF || id == WOFF2 {
        specs.push(spec(
            "level",
            "opt.font.level",
            "Compression level",
            "1 is fastest, 9 is smallest",
            OptionKind::Int {
                min: 1,
                max: 9,
                default: if id == WOFF2 { 5 } else { 9 },
            },
        ));
    }
    OptionSchema { specs }
}

pub fn register(registry: &mut Registry) {
    let rows = [
        (
            TTF,
            "TrueType",
            &["ttf"][..],
            &["font/ttf", "application/x-font-ttf"][..],
            "TrueType outlines in an sfnt (OpenType 1.9)",
            vec![Sniffer {
                offset: 0,
                magic: &[0, 1, 0, 0],
                confidence: 60,
            }],
        ),
        (
            OTF,
            "OpenType (CFF)",
            &["otf"][..],
            &["font/otf", "application/x-font-opentype"][..],
            "OpenType with CFF outlines (Type 2 charstrings)",
            vec![Sniffer {
                offset: 0,
                magic: b"OTTO",
                confidence: 100,
            }],
        ),
        (
            WOFF,
            "WOFF",
            &["woff"][..],
            &["font/woff"][..],
            "W3C WOFF 1.0 (zlib-compressed tables)",
            vec![Sniffer {
                offset: 0,
                magic: b"wOFF",
                confidence: 100,
            }],
        ),
        (
            WOFF2,
            "WOFF2",
            &["woff2"][..],
            &["font/woff2"][..],
            "W3C WOFF 2.0 (Brotli; tables stored without the glyph transform; write only)",
            vec![Sniffer {
                offset: 0,
                magic: b"wOF2",
                confidence: 100,
            }],
        ),
        (
            TTC,
            "Font collection",
            &["ttc", "otc"][..],
            &["font/collection"][..],
            "TrueType/OpenType collection (one font is read)",
            vec![Sniffer {
                offset: 0,
                magic: b"ttcf",
                confidence: 100,
            }],
        ),
    ];
    for (id, name, extensions, mime, spec_text, sniffers) in rows {
        registry.add_format(FormatDescriptor {
            id,
            name,
            category: Category::Font,
            extensions,
            mime,
            sniffers,
            capabilities: Capabilities {
                read: !write_only(id),
                write: !read_only(id),
                ..Capabilities::default()
            },
            writer_options: options_for(id),
            reader_options: Default::default(),
            ownership: Ownership::Own,
            spec: spec_text,
        });
    }
    let desc = |from: FormatId, to: FormatId| -> &'static str {
        match (from == TTF || from == TTC, to) {
            (_, t) if t == WOFF => "Compress the font tables as WOFF",
            (_, t) if t == WOFF2 => "Compress the font tables as WOFF2 (Brotli)",
            (true, t) if t == OTF => "Rebuild the outlines as CFF",
            (false, t) if t == TTF && from == OTF => "Rebuild the outlines as TrueType quadratics",
            _ => "Unpack the font tables",
        }
    };
    for from in FORMATS {
        if write_only(from) {
            continue;
        }
        for to in [TTF, OTF, WOFF, WOFF2] {
            if from == to {
                continue;
            }
            // Changing the outline type rebuilds every glyph, so it counts as lossy.
            let rebuild = (matches!(from, f if f == TTF || f == TTC) && to == OTF)
                || (from == OTF && to == TTF);
            registry.add_edge(Edge {
                from,
                to,
                kind: EdgeKind::Transcode,
                loss: u32::from(rebuild),
                ownership: Ownership::Own,
                time: 2,
                description: desc(from, to),
            });
        }
        for img in crate::image::FORMATS {
            if matches!(
                img,
                crate::image::PNG
                    | crate::image::JPEG
                    | crate::image::WEBP
                    | crate::image::BMP
                    | crate::image::TIFF
                    | crate::image::GIF
                    | crate::image::QOI
            ) {
                registry.add_edge(Edge {
                    from,
                    to: img,
                    kind: EdgeKind::Transcode,
                    loss: 1,
                    ownership: Ownership::Own,
                    time: 3,
                    description: "Draw a specimen sheet of the font",
                });
            }
        }
    }
}

/// The font format of `data`, judged by content.
pub fn detect(data: &[u8]) -> Option<FormatId> {
    if build::is_woff(data) {
        return Some(WOFF);
    }
    if build::is_woff2(data) {
        return Some(WOFF2);
    }
    if data.get(..4) == Some(b"ttcf") {
        return Some(TTC);
    }
    if !sfnt::is_sfnt(data) {
        return None;
    }
    Some(if data.get(..4) == Some(b"OTTO") {
        OTF
    } else {
        TTF
    })
}

/// The sfnt bytes of a font file, unpacking WOFF and picking one font of a collection.
fn plain_sfnt(from: FormatId, data: &[u8], index: usize, limits: &Limits) -> Result<Vec<u8>> {
    let bytes = if build::is_woff(data) {
        build::woff_to_sfnt(data, limits)?
    } else if build::is_woff2(data) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "WOFF2 needs a Brotli decoder, which is not built yet",
        ));
    } else if from == TTC || data.get(..4) == Some(b"ttcf") {
        let font = Font::parse_index(data, index)?;
        let (_, mut tabs) = build::tables_of_font(&font);
        let flavor = if font.is_cff { *b"OTTO" } else { [0, 1, 0, 0] };
        build::build_sfnt(flavor, &mut tabs)
    } else {
        data.to_vec()
    };
    if bytes.len() as u64 > limits.max_alloc {
        return Err(Error::limit("max_alloc", "font is too large"));
    }
    Ok(bytes)
}

pub fn load(from: FormatId, data: &[u8], opts: &Options, limits: &Limits) -> Result<Font> {
    let index = opts.int("font_index").unwrap_or(0).max(0) as usize;
    Font::parse(&plain_sfnt(from, data, index, limits)?)
}

pub fn convert(
    from: FormatId,
    to: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let index = opts.int("font_index").unwrap_or(0).max(0) as usize;
    if from == TTC && index > 0 {
        notes.push(Note::new(
            Severity::Info,
            "font.collection_member",
            format!("font number {index} of the collection was used"),
        ));
    }
    let sf = plain_sfnt(from, data, index, limits)?;
    let font = Font::parse(&sf)?;
    for tag in [b"fvar", b"gvar", b"CFF2"] {
        if font.has_table(tag) {
            notes.push(Note::new(
                Severity::Warning,
                "font.variable_default_instance",
                "this is a variable font; only the default instance is converted",
            ));
            break;
        }
    }
    let out = match to {
        t if t == WOFF => build::sfnt_to_woff(&sf, opts.int("level").unwrap_or(9) as u32)?,
        t if t == WOFF2 => build::sfnt_to_woff2(&sf, opts.int("level").unwrap_or(5) as u32)?,
        t if t == TTF => {
            if font.is_cff {
                let tol = opts.float("curve_tolerance").unwrap_or(0.5)
                    * f64::from(font.units_per_em)
                    / 1000.0;
                notes.push(Note::new(
                    Severity::Warning,
                    "font.outlines_rebuilt",
                    "cubic curves were approximated by quadratic ones within the tolerance",
                ));
                convert::cff_to_truetype(&font, tol)?
            } else {
                sf
            }
        }
        _ => {
            if font.is_cff {
                sf
            } else {
                if font.has_table(b"fpgm") || font.has_table(b"prep") || font.has_table(b"cvt ") {
                    notes.push(Note::new(
                        Severity::Warning,
                        "font.hinting_dropped",
                        "TrueType hinting instructions have no CFF equivalent and were dropped",
                    ));
                }
                convert::truetype_to_cff(&font)?
            }
        }
    };
    Ok((out, notes))
}

/// A specimen sheet of the font as a picture.
pub fn specimen(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
) -> Result<(oc_image::Image, Notes)> {
    let font = load(from, data, opts, limits)?;
    let mut notes = Notes::new();
    notes.push(Note::new(
        Severity::Info,
        "font.specimen",
        "a picture of a font is a specimen sheet; kerning from the kern table is applied, other layout features are not",
    ));
    let width = opts.int("width").filter(|w| *w > 0).unwrap_or(1000) as usize;
    let canvas = oc_font::render::specimen(&font, width)
        .ok_or_else(|| Error::new(ErrorCode::LimitExceeded, "specimen sheet is too large"))?;
    let rgba = canvas.to_rgba8(Some([1.0, 1.0, 1.0]));
    let img = oc_image::Image::new(
        canvas.width as u32,
        canvas.height as u32,
        oc_image::Layout::Rgba,
        oc_image::Depth::Eight,
        rgba,
    )?;
    Ok((img, notes))
}
