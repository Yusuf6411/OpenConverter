//! Raster image formats: registration, decoding, transforms and encoding.
//!
//! Every image format connects to an internal hub node (`image-ir`): one edge in from the
//! decoder and one edge out to the encoder. A conversion between two image formats is
//! therefore always "Decode X -> Encode Y", and adding a format adds exactly two edges.
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Limits, Note, Notes,
    Ownership, Progress, Result, Severity, Sniffer,
};
use oc_image::{bmp, gif, ico, jpeg, netpbm, ops, png, qoi, tga, tiff, webp, Image};

pub const PNG: FormatId = FormatId("png");
pub const BMP: FormatId = FormatId("bmp");
pub const TGA: FormatId = FormatId("tga");
pub const QOI: FormatId = FormatId("qoi");
pub const PNM: FormatId = FormatId("pnm");
pub const GIF: FormatId = FormatId("gif");
pub const JPEG: FormatId = FormatId("jpeg");
pub const ICO: FormatId = FormatId("ico");
pub const TIFF: FormatId = FormatId("tiff");
pub const WEBP: FormatId = FormatId("webp");
/// Internal hub; never shown to users.
pub const IMAGE_IR: FormatId = FormatId("image-ir");

pub const FORMATS: [FormatId; 10] = [PNG, JPEG, BMP, TGA, QOI, PNM, GIF, ICO, TIFF, WEBP];

pub fn is_image(id: FormatId) -> bool {
    FORMATS.contains(&id)
}

/// Options shared by every image output: orientation, size and the colour that replaces
/// transparency where the target cannot hold it.
fn common_options() -> Vec<OptionSpec> {
    vec![
        OptionSpec {
            key: "background",
            label: "Background colour",
            help: "Replaces transparency when the output cannot store it, as #rrggbb",
            i18n_key: "opt.image.background",
            kind: OptionKind::Color {
                default: [255, 255, 255, 255],
            },
        },
        OptionSpec {
            key: "width",
            label: "Width",
            help: "Resize to this width in pixels; 0 keeps the size (the other side keeps the aspect ratio)",
            i18n_key: "opt.image.width",
            kind: OptionKind::Int {
                min: 0,
                max: 65535,
                default: 0,
            },
        },
        OptionSpec {
            key: "height",
            label: "Height",
            help: "Resize to this height in pixels; 0 keeps the size",
            i18n_key: "opt.image.height",
            kind: OptionKind::Int {
                min: 0,
                max: 65535,
                default: 0,
            },
        },
        OptionSpec {
            key: "rotate",
            label: "Rotate",
            help: "Clockwise rotation in degrees",
            i18n_key: "opt.image.rotate",
            kind: OptionKind::Enum {
                choices: vec!["0", "90", "180", "270"],
                default: "0",
            },
        },
        OptionSpec {
            key: "flip",
            label: "Flip",
            help: "Mirror horizontally or vertically (applied before rotation)",
            i18n_key: "opt.image.flip",
            kind: OptionKind::Enum {
                choices: vec!["none", "horizontal", "vertical"],
                default: "none",
            },
        },
    ]
}

fn schema(extra: Vec<OptionSpec>) -> OptionSchema {
    let mut specs = common_options();
    specs.extend(extra);
    OptionSchema { specs }
}

fn bool_spec(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    i18n_key: &'static str,
    default: bool,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        help,
        i18n_key,
        kind: OptionKind::Bool { default },
    }
}

// One call site per format; a builder would only restate the same fields.
#[allow(clippy::too_many_arguments)]
fn describe(
    id: FormatId,
    name: &'static str,
    extensions: &'static [&'static str],
    mime: &'static [&'static str],
    sniffers: Vec<Sniffer>,
    metadata: bool,
    writer_options: OptionSchema,
    spec: &'static str,
) -> FormatDescriptor {
    FormatDescriptor {
        id,
        name,
        category: Category::Image,
        extensions,
        mime,
        sniffers,
        capabilities: Capabilities {
            read: true,
            write: true,
            alpha: true,
            metadata,
            ..Capabilities::default()
        },
        writer_options,
        ownership: Ownership::Own,
        spec,
    }
}

pub fn register(registry: &mut Registry) {
    let netpbm_magics: Vec<Sniffer> = [&b"P1"[..], b"P2", b"P3", b"P4", b"P5", b"P6", b"P7"]
        .into_iter()
        .map(|magic| Sniffer {
            offset: 0,
            magic,
            confidence: 50,
        })
        .collect();
    let formats = [
        describe(
            PNG,
            "PNG",
            &["png"],
            &["image/png"],
            vec![Sniffer {
                offset: 0,
                magic: b"\x89PNG\r\n\x1a\n",
                confidence: 100,
            }],
            true,
            schema(vec![
                OptionSpec {
                    key: "level",
                    label: "Compression level",
                    help: "0 stores, 9 compresses hardest",
                    i18n_key: "opt.png.level",
                    kind: OptionKind::Int {
                        min: 0,
                        max: 9,
                        default: 6,
                    },
                },
                bool_spec(
                    "adaptive_filter",
                    "Adaptive filtering",
                    "Try every row filter and keep the smallest; faster when off",
                    "opt.png.adaptive_filter",
                    true,
                ),
            ]),
            "PNG 1.2 (ISO/IEC 15948)",
        ),
        describe(
            JPEG,
            "JPEG",
            &["jpg", "jpeg", "jpe", "jfif"],
            &["image/jpeg"],
            vec![Sniffer {
                offset: 0,
                magic: b"\xff\xd8\xff",
                confidence: 100,
            }],
            true,
            schema(vec![
                OptionSpec {
                    key: "quality",
                    label: "Quality",
                    help: "1 is smallest, 100 is best",
                    i18n_key: "opt.jpeg.quality",
                    kind: OptionKind::Int {
                        min: 1,
                        max: 100,
                        default: 85,
                    },
                },
                OptionSpec {
                    key: "subsampling",
                    label: "Chroma subsampling",
                    help:
                        "4:4:4 keeps all colour detail, 4:2:0 halves it both ways for smaller files",
                    i18n_key: "opt.jpeg.subsampling",
                    kind: OptionKind::Enum {
                        choices: vec!["4:4:4", "4:2:2", "4:2:0"],
                        default: "4:2:0",
                    },
                },
            ]),
            "ITU T.81 (baseline and progressive decode, baseline encode)",
        ),
        describe(
            ICO,
            "ICO / CUR",
            &["ico", "cur"],
            &["image/vnd.microsoft.icon"],
            // Reserved 0, type 1 (icon) or 2 (cursor): weak on its own.
            vec![
                Sniffer {
                    offset: 0,
                    magic: &[0, 0, 1, 0],
                    confidence: 70,
                },
                Sniffer {
                    offset: 0,
                    magic: &[0, 0, 2, 0],
                    confidence: 70,
                },
            ],
            false,
            schema(vec![OptionSpec {
                key: "sizes",
                label: "Icon sizes",
                help: "Comma-separated square sizes from 1 to 256, e.g. 16,32,48,256; empty keeps the image size",
                i18n_key: "opt.ico.sizes",
                kind: OptionKind::Text {
                    default: String::new(),
                },
            }]),
            "Microsoft ICO / CUR",
        ),
        describe(
            TIFF,
            "TIFF",
            &["tif", "tiff"],
            &["image/tiff"],
            vec![
                Sniffer {
                    offset: 0,
                    magic: b"II*\0",
                    confidence: 100,
                },
                Sniffer {
                    offset: 0,
                    magic: b"MM\0*",
                    confidence: 100,
                },
            ],
            true,
            schema(vec![
                OptionSpec {
                    key: "compression",
                    label: "Compression",
                    help: "lzw and deflate are lossless and usually smallest; packbits is simple and fast",
                    i18n_key: "opt.tiff.compression",
                    kind: OptionKind::Enum {
                        choices: vec!["none", "lzw", "deflate", "packbits"],
                        default: "lzw",
                    },
                },
                bool_spec(
                    "predictor",
                    "Horizontal predictor",
                    "Improves lzw and deflate on photographs",
                    "opt.tiff.predictor",
                    true,
                ),
            ]),
            "TIFF 6.0",
        ),
        describe(
            WEBP,
            "WebP",
            &["webp"],
            &["image/webp"],
            vec![Sniffer {
                offset: 8,
                magic: b"WEBP",
                confidence: 100,
            }],
            true,
            schema(vec![
                OptionSpec {
                    key: "effort",
                    label: "Compression effort",
                    help: "0 is fastest, 6 gives the smallest files",
                    i18n_key: "opt.webp.effort",
                    kind: OptionKind::Int {
                        min: 0,
                        max: 6,
                        default: 4,
                    },
                },
                bool_spec(
                    "clean_transparent",
                    "Clear hidden colour",
                    "Zero the colour of fully transparent pixels: smaller files, no visible change",
                    "opt.webp.clean_transparent",
                    false,
                ),
            ]),
            "RFC 9649 (lossless VP8L)",
        ),
        describe(
            BMP,
            "BMP",
            &["bmp", "dib"],
            &["image/bmp"],
            vec![Sniffer {
                offset: 0,
                magic: b"BM",
                confidence: 40,
            }],
            false,
            schema(Vec::new()),
            "Microsoft BMP / DIB",
        ),
        describe(
            TGA,
            "TGA",
            &["tga", "icb", "vda", "vst"],
            &["image/x-tga"],
            Vec::new(),
            false,
            schema(vec![bool_spec(
                "rle",
                "Run-length encoding",
                "Smaller files for flat images",
                "opt.tga.rle",
                true,
            )]),
            "Truevision TGA 2.0",
        ),
        describe(
            QOI,
            "QOI",
            &["qoi"],
            &["image/qoi"],
            vec![Sniffer {
                offset: 0,
                magic: b"qoif",
                confidence: 100,
            }],
            false,
            schema(Vec::new()),
            "QOI 1.0",
        ),
        describe(
            GIF,
            "GIF",
            &["gif"],
            &["image/gif"],
            vec![Sniffer {
                offset: 0,
                magic: b"GIF8",
                confidence: 100,
            }],
            false,
            schema(vec![
                bool_spec(
                    "dither",
                    "Dither",
                    "Dither when the image has more than 256 colours",
                    "opt.gif.dither",
                    true,
                ),
                bool_spec(
                    "interlace",
                    "Interlace",
                    "Store rows in four passes so a partial download shows a preview",
                    "opt.gif.interlace",
                    false,
                ),
            ]),
            "GIF 89a",
        ),
        describe(
            PNM,
            "Netpbm",
            &["pnm", "pbm", "pgm", "ppm", "pam"],
            &["image/x-portable-anymap"],
            netpbm_magics,
            false,
            schema(vec![bool_spec(
                "ascii",
                "ASCII variant",
                "Write the text variants (P1/P2/P3) instead of binary",
                "opt.pnm.ascii",
                false,
            )]),
            "Netpbm P1-P7",
        ),
    ];
    for f in formats {
        let (id, name) = (f.id, f.name);
        registry.add_format(f);
        registry.add_edge(Edge {
            from: id,
            to: IMAGE_IR,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 2,
            description: decode_description(name),
        });
        registry.add_edge(Edge {
            from: IMAGE_IR,
            to: id,
            kind: EdgeKind::Transcode,
            // PNG keeps depth and metadata; the others are narrower.
            loss: u32::from(id != PNG),
            ownership: Ownership::Own,
            time: 2,
            description: encode_description(name),
        });
    }
}

// Descriptions are `&'static str` so routes can be printed without allocation.
fn decode_description(name: &str) -> &'static str {
    match name {
        "PNG" => "Decode PNG",
        "BMP" => "Decode BMP",
        "TGA" => "Decode TGA",
        "QOI" => "Decode QOI",
        "GIF" => "Decode GIF",
        "JPEG" => "Decode JPEG",
        "TIFF" => "Decode TIFF",
        "WebP" => "Decode WebP",
        "ICO / CUR" => "Decode ICO",
        _ => "Decode Netpbm",
    }
}

fn encode_description(name: &str) -> &'static str {
    match name {
        "PNG" => "Encode PNG",
        "BMP" => "Encode BMP",
        "TGA" => "Encode TGA",
        "QOI" => "Encode QOI",
        "GIF" => "Encode GIF",
        "JPEG" => "Encode JPEG",
        "TIFF" => "Encode TIFF",
        "WebP" => "Encode WebP (lossless)",
        "ICO / CUR" => "Encode ICO",
        _ => "Encode Netpbm",
    }
}

pub fn decode(
    fmt: FormatId,
    data: &[u8],
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<(Image, Notes)> {
    match fmt {
        PNG => png::decode(data, limits, progress),
        BMP => bmp::decode(data, limits),
        TGA => tga::decode(data, limits),
        QOI => qoi::decode(data, limits).map(|i| (i, Notes::new())),
        GIF => gif::decode(data, limits),
        JPEG => jpeg::decode(data, limits, progress),
        TIFF => tiff::decode(data, limits, progress),
        WEBP => webp::decode(data, limits, progress),
        ICO => ico::decode(data, limits, progress),
        PNM => netpbm::decode(data, limits),
        other => Err(Error::new(
            ErrorCode::UnsupportedFormat,
            format!("{} is not an image format", other.0),
        )),
    }
}

/// Orientation and size changes requested through the common options.
pub fn transform(img: Image, opts: &Options, limits: &Limits) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let mut img = img;
    match opts.text("flip") {
        Some("horizontal") => img = ops::flip_horizontal(&img),
        Some("vertical") => img = ops::flip_vertical(&img),
        _ => {}
    }
    match opts.text("rotate") {
        Some("90") => img = ops::rotate90(&img),
        Some("180") => img = ops::rotate180(&img),
        Some("270") => img = ops::rotate270(&img),
        _ => {}
    }
    let want_w = opts.int("width").unwrap_or(0).max(0) as u64;
    let want_h = opts.int("height").unwrap_or(0).max(0) as u64;
    if want_w > 0 || want_h > 0 {
        let (w, h) = (u64::from(img.width), u64::from(img.height));
        // A missing side follows from the aspect ratio.
        let nw = if want_w > 0 {
            want_w
        } else {
            (w * want_h + h / 2) / h.max(1)
        };
        let nh = if want_h > 0 {
            want_h
        } else {
            (h * want_w + w / 2) / w.max(1)
        };
        let (nw, nh) = (nw.clamp(1, 65535) as u32, nh.clamp(1, 65535) as u32);
        if (nw, nh) != (img.width, img.height) {
            limits.check_pixels(nw, nh)?;
            limits.check_alloc(u64::from(nw) * u64::from(nh) * 8)?;
            let (ow, oh) = (img.width, img.height);
            img = ops::resize(&img, nw, nh, ops::Filter::Bilinear);
            notes.push(
                Note::new(
                    Severity::Info,
                    "image.resized",
                    format!("resized from {ow}x{oh} to {nw}x{nh}"),
                )
                .with_detail(format!("{nw}x{nh}")),
            );
        }
    }
    Ok((img, notes))
}

/// `ext` is the output file's extension: for Netpbm it picks PBM, PGM, PPM or PAM.
pub fn encode(fmt: FormatId, img: &Image, opts: &Options, ext: &str) -> Result<(Vec<u8>, Notes)> {
    let [br, bg, bb, _] = opts.color("background").unwrap_or([255, 255, 255, 255]);
    let (bytes, mut notes) = match fmt {
        PNG => png::encode(
            img,
            &png::EncodeOptions {
                level: opts.int("level").unwrap_or(6).clamp(0, 9) as u32,
                adaptive_filter: opts.bool("adaptive_filter").unwrap_or(true),
            },
        ),
        BMP => bmp::encode(img),
        TGA => tga::encode(
            img,
            &tga::EncodeOptions {
                rle: opts.bool("rle").unwrap_or(true),
            },
        ),
        QOI => qoi::encode(img),
        TIFF => tiff::encode(
            img,
            &tiff::EncodeOptions {
                compression: match opts.text("compression") {
                    Some("none") => tiff::Compression::None,
                    Some("deflate") => tiff::Compression::Deflate,
                    Some("packbits") => tiff::Compression::PackBits,
                    _ => tiff::Compression::Lzw,
                },
                predictor: opts.bool("predictor").unwrap_or(true),
            },
        )?,
        WEBP => webp::encode(
            img,
            &webp::EncodeOptions {
                effort: opts.int("effort").unwrap_or(4).clamp(0, 6) as u8,
                clean_transparent: opts.bool("clean_transparent").unwrap_or(false),
            },
        )?,
        ICO => {
            let sizes = opts
                .text("sizes")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.parse::<u32>()
                        .map_err(|_| Error::invalid_option("sizes", format!("'{s}' is not a size")))
                })
                .collect::<Result<Vec<u32>>>()?;
            ico::encode(
                img,
                &ico::EncodeOptions {
                    sizes,
                    cursor: ext == "cur",
                },
            )?
        }
        JPEG => {
            let quality = opts.int("quality").unwrap_or(85).clamp(1, 100) as u8;
            let (bytes, mut n) = jpeg::encode(
                img,
                &jpeg::EncodeOptions {
                    quality,
                    subsampling: match opts.text("subsampling") {
                        Some("4:4:4") => jpeg::Subsampling::S444,
                        Some("4:2:2") => jpeg::Subsampling::S422,
                        _ => jpeg::Subsampling::S420,
                    },
                    background: [br, bg, bb],
                },
            )?;
            n.push(Note::new(
                Severity::Info,
                "image.lossy_compression",
                format!("JPEG is lossy; saved at quality {quality}"),
            ));
            (bytes, n)
        }
        GIF => gif::encode(
            img,
            &gif::EncodeOptions {
                dither: opts.bool("dither").unwrap_or(true),
                interlace: opts.bool("interlace").unwrap_or(false),
            },
        )?,
        PNM => netpbm::encode(
            img,
            &netpbm::EncodeOptions {
                ascii: opts.bool("ascii").unwrap_or(false),
                background: [br, bg, bb],
                variant: match ext {
                    "pbm" => netpbm::Variant::Pbm,
                    "pgm" => netpbm::Variant::Pgm,
                    "ppm" => netpbm::Variant::Ppm,
                    "pam" => netpbm::Variant::Pam,
                    _ => netpbm::Variant::Auto,
                },
            },
        ),
        other => {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                format!("{} is not an image format", other.0),
            ))
        }
    };
    if fmt != PNG {
        if img.metadata.icc_profile.is_some() && fmt != JPEG && fmt != WEBP {
            notes.push(Note::new(
                Severity::Loss,
                "image.icc_dropped",
                "the colour profile cannot be stored in this format and was dropped",
            ));
        }
        if !img.metadata.text.is_empty() {
            notes.push(Note::new(
                Severity::Loss,
                "image.text_dropped",
                "text metadata cannot be stored in this format and was dropped",
            ));
        }
    }
    Ok((bytes, notes))
}
