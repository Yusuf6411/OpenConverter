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
use oc_image::anim::{self, Animation};
use oc_image::{
    bmp, dds, exr, gif, hdr, icc, icns, ico, jpeg, netpbm, ops, png, psd, qoi, tga, tiff, webp,
    Image,
};

pub const PNG: FormatId = FormatId("png");
pub const BMP: FormatId = FormatId("bmp");
pub const TGA: FormatId = FormatId("tga");
pub const QOI: FormatId = FormatId("qoi");
pub const PNM: FormatId = FormatId("pnm");
pub const GIF: FormatId = FormatId("gif");
pub const JPEG: FormatId = FormatId("jpeg");
pub const ICO: FormatId = FormatId("ico");
pub const ICNS: FormatId = FormatId("icns");
pub const DDS: FormatId = FormatId("dds");
pub const PSD: FormatId = FormatId("psd");
pub const HDR: FormatId = FormatId("hdr");
pub const EXR: FormatId = FormatId("exr");
pub const TIFF: FormatId = FormatId("tiff");
pub const WEBP: FormatId = FormatId("webp");
/// Internal hub; never shown to users.
pub const IMAGE_IR: FormatId = FormatId("image-ir");

pub const FORMATS: [FormatId; 15] = [
    PNG, JPEG, BMP, TGA, QOI, PNM, GIF, ICO, ICNS, DDS, PSD, HDR, EXR, TIFF, WEBP,
];

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
            key: "frames",
            label: "Animation frames",
            help: "For animated input: keep every frame when the output can animate, or only the first",
            i18n_key: "opt.image.frames",
            kind: OptionKind::Enum {
                choices: vec!["all", "first"],
                default: "all",
            },
        },
        OptionSpec {
            key: "profile",
            label: "Colour profile",
            help: "keep: embedded profile stays where the format can store it, otherwise the pixels are converted to sRGB; srgb: always convert to sRGB; strip: drop the profile without converting",
            i18n_key: "opt.image.profile",
            kind: OptionKind::Enum {
                choices: vec!["keep", "srgb", "strip"],
                default: "keep",
            },
        },
        OptionSpec {
            key: "intent",
            label: "Rendering intent",
            help: "How out-of-gamut colours are mapped when converting to sRGB",
            i18n_key: "opt.image.intent",
            kind: OptionKind::Enum {
                choices: vec!["relative", "perceptual", "saturation"],
                default: "relative",
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
            &["png", "apng"],
            &["image/png", "image/apng"],
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
                bool_spec(
                    "optimize",
                    "Optimise Huffman tables",
                    "Builds the entropy tables from the picture; smaller files, a little slower",
                    "opt.jpeg.optimize",
                    false,
                ),
                bool_spec(
                    "progressive",
                    "Progressive",
                    "Stores the picture in refining passes so a partial download shows a preview; also smaller",
                    "opt.jpeg.progressive",
                    false,
                ),
            ]),
            "ITU T.81 (baseline and progressive, encode and decode)",
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
            ICNS,
            "ICNS",
            &["icns"],
            &["image/icns"],
            vec![Sniffer {
                offset: 0,
                magic: b"icns",
                confidence: 100,
            }],
            false,
            schema(vec![OptionSpec {
                key: "sizes",
                label: "Icon sizes",
                help: "Comma-separated square sizes from 16, 32, 64, 128, 256, 512, 1024; empty writes the standard sizes that fit the image",
                i18n_key: "opt.icns.sizes",
                kind: OptionKind::Text {
                    default: String::new(),
                },
            }]),
            "Apple ICNS",
        ),
        describe(
            DDS,
            "DDS",
            &["dds"],
            &["image/vnd.ms-dds"],
            vec![Sniffer {
                offset: 0,
                magic: b"DDS |\0\0\0",
                confidence: 100,
            }],
            false,
            schema(vec![OptionSpec {
                key: "compression",
                label: "Compression",
                help: "bc3 (DXT5) and bc1 (DXT1) are lossy block formats GPUs read directly; none stores 32-bit BGRA",
                i18n_key: "opt.dds.compression",
                kind: OptionKind::Enum {
                    choices: vec!["bc3", "bc1", "none"],
                    default: "bc3",
                },
            }]),
            "Microsoft DDS",
        ),
        describe(
            PSD,
            "PSD",
            &["psd", "psb"],
            &["image/vnd.adobe.photoshop"],
            vec![Sniffer {
                offset: 0,
                magic: b"8BPS\0",
                confidence: 100,
            }],
            true,
            schema(vec![bool_spec(
                "rle",
                "Run-length coding",
                "Pack the rows; usually smaller. Off stores them raw",
                "opt.psd.rle",
                true,
            )]),
            "Adobe Photoshop File Format",
        ),
        describe(
            HDR,
            "Radiance HDR",
            &["hdr", "pic"],
            &["image/vnd.radiance"],
            vec![
                Sniffer {
                    offset: 0,
                    magic: b"#?RADIANCE",
                    confidence: 100,
                },
                Sniffer {
                    offset: 0,
                    magic: b"#?RGBE",
                    confidence: 100,
                },
            ],
            false,
            schema(Vec::new()),
            "Radiance picture file format",
        ),
        describe(
            EXR,
            "OpenEXR",
            &["exr"],
            &["image/x-exr"],
            vec![Sniffer {
                offset: 0,
                magic: &[0x76, 0x2f, 0x31, 0x01],
                confidence: 100,
            }],
            false,
            schema(vec![OptionSpec {
                key: "compression",
                label: "Compression",
                help: "Lossless coding of the half-float rows",
                i18n_key: "opt.exr.compression",
                kind: OptionKind::Enum {
                    choices: vec!["zip", "rle", "none"],
                    default: "zip",
                },
            }]),
            "OpenEXR file layout",
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
                bool_spec(
                    "lossless",
                    "Lossless",
                    "Keep every pixel exactly; turn off for smaller, lossy files",
                    "opt.webp.lossless",
                    true,
                ),
                OptionSpec {
                    key: "quality",
                    label: "Quality",
                    help: "Lossy only: 0 is smallest, 100 is best",
                    i18n_key: "opt.webp.quality",
                    kind: OptionKind::Int {
                        min: 0,
                        max: 100,
                        default: 75,
                    },
                },
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
            "RFC 9649 (VP8L), RFC 6386 (VP8 key frames)",
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
        "ICNS" => "Decode ICNS",
        "DDS" => "Decode DDS",
        "PSD" => "Decode PSD",
        "Radiance HDR" => "Decode HDR",
        "OpenEXR" => "Decode EXR",
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
        "WebP" => "Encode WebP",
        "ICO / CUR" => "Encode ICO",
        "ICNS" => "Encode ICNS",
        "DDS" => "Encode DDS",
        "PSD" => "Encode PSD",
        "Radiance HDR" => "Encode HDR",
        "OpenEXR" => "Encode EXR",
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
        ICNS => icns::decode(data, limits, progress),
        DDS => dds::decode(data, limits),
        PSD => psd::decode(data, limits, progress),
        HDR => hdr::decode(data, limits),
        EXR => exr::decode(data, limits, progress),
        PNM => netpbm::decode(data, limits),
        other => Err(Error::new(
            ErrorCode::UnsupportedFormat,
            format!("{} is not an image format", other.0),
        )),
    }
}

/// Whether `fmt` can store an animation. PNG does so only as APNG, picked by the `.apng`
/// extension, because a plain `.png` is expected to open as a still everywhere.
pub fn writes_animation(fmt: FormatId, ext: &str) -> bool {
    matches!(fmt, GIF | WEBP) || (fmt == PNG && ext == "apng")
}

/// The frames of an animated GIF, WebP or PNG; `None` for other formats and for files with a
/// single image.
pub fn decode_animation(
    fmt: FormatId,
    data: &[u8],
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Option<(Animation, Notes)>> {
    let (anim, notes) = match fmt {
        GIF => gif::decode_animation(data, limits)?,
        WEBP => webp::decode_animation(data, limits, progress)?,
        PNG if png::is_animated(data) => png::decode_animation(data, limits, progress)?,
        _ => return Ok(None),
    };
    Ok((anim.frames.len() > 1).then_some((anim, notes)))
}

/// Applies the common transforms to every frame, so the frames stay the same size.
pub fn transform_animation(
    anim: Animation,
    opts: &Options,
    limits: &Limits,
) -> Result<(Animation, Notes)> {
    let mut notes = Notes::new();
    let mut out = Animation::new(anim.width, anim.height);
    out.loops = anim.loops;
    out.metadata = anim.metadata;
    for (i, f) in anim.frames.into_iter().enumerate() {
        let (image, n) = transform(f.image, opts, limits)?;
        if i == 0 {
            notes.extend(n);
            out.width = image.width;
            out.height = image.height;
        }
        limits.check_alloc(
            u64::from(image.width) * u64::from(image.height) * 4 * (out.frames.len() as u64 + 1),
        )?;
        out.frames.push(anim::Frame {
            image,
            delay_ms: f.delay_ms,
        });
    }
    Ok((out, notes))
}

pub fn encode_animation(
    fmt: FormatId,
    anim: &Animation,
    opts: &Options,
) -> Result<(Vec<u8>, Notes)> {
    let (bytes, mut notes) = match fmt {
        GIF => gif::encode_animation(
            anim,
            &gif::EncodeOptions {
                dither: opts.bool("dither").unwrap_or(true),
                interlace: false,
            },
        )?,
        WEBP => webp::encode_animation(anim, &webp_options(opts))?,
        PNG => png::encode_animation(anim, &png_options(opts))?,
        other => {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                format!("{} cannot store an animation", other.0),
            ))
        }
    };
    notes.push(Note::new(
        Severity::Info,
        "image.animation",
        format!("wrote an animation of {} frames", anim.frames.len()),
    ));
    Ok((bytes, notes))
}

fn parse_sizes(opts: &Options) -> Result<Vec<u32>> {
    opts.text("sizes")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u32>()
                .map_err(|_| Error::invalid_option("sizes", format!("'{s}' is not a size")))
        })
        .collect()
}

fn webp_options(opts: &Options) -> webp::EncodeOptions {
    webp::EncodeOptions {
        effort: opts.int("effort").unwrap_or(4).clamp(0, 6) as u8,
        clean_transparent: opts.bool("clean_transparent").unwrap_or(false),
        quality: if opts.bool("lossless").unwrap_or(true) {
            None
        } else {
            Some(opts.int("quality").unwrap_or(75).clamp(0, 100) as f32)
        },
    }
}

fn png_options(opts: &Options) -> png::EncodeOptions {
    png::EncodeOptions {
        level: opts.int("level").unwrap_or(6).clamp(0, 9) as u32,
        adaptive_filter: opts.bool("adaptive_filter").unwrap_or(true),
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

/// Formats whose writers embed `Metadata::icc_profile`.
fn stores_icc(fmt: FormatId) -> bool {
    matches!(fmt, PNG | JPEG | WEBP | TIFF | PSD)
}

/// What to do with an embedded colour profile before encoding: keep it where the format
/// can store it, otherwise (or on request) convert the pixels to sRGB. `None` means the
/// image goes through unchanged.
fn apply_profile_policy(fmt: FormatId, img: &Image, opts: &Options) -> Option<(Image, Notes)> {
    img.metadata.icc_profile.as_ref()?;
    let mode = opts.text("profile").unwrap_or("keep");
    let mut notes = Notes::new();
    if mode == "strip" {
        let mut out = img.clone();
        out.metadata.icc_profile = None;
        notes.push(Note::new(
            Severity::Info,
            "image.icc_stripped",
            "the colour profile was removed without converting the pixels",
        ));
        return Some((out, notes));
    }
    if mode == "keep" && stores_icc(fmt) {
        return None;
    }
    let intent = match opts.text("intent") {
        Some("perceptual") => icc::Intent::Perceptual,
        Some("saturation") => icc::Intent::Saturation,
        _ => icc::Intent::RelativeColorimetric,
    };
    match icc::to_srgb(img, intent, true) {
        Ok(out) => {
            notes.push(Note::new(
                Severity::Info,
                "image.icc_converted",
                "the pixels were converted from the embedded colour profile to sRGB",
            ));
            Some((out, notes))
        }
        Err(e) => {
            let mut out = img.clone();
            out.metadata.icc_profile = None;
            notes.push(
                Note::new(
                    Severity::Warning,
                    "image.icc_unusable",
                    "the colour profile could not be applied; it was dropped and colours may look different",
                )
                .with_detail(e.to_string()),
            );
            Some((out, notes))
        }
    }
}

/// `ext` is the output file's extension: for Netpbm it picks PBM, PGM, PPM or PAM.
pub fn encode(fmt: FormatId, img: &Image, opts: &Options, ext: &str) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let managed;
    let img = match apply_profile_policy(fmt, img, opts) {
        Some((converted, n)) => {
            notes.extend(n);
            managed = converted;
            &managed
        }
        None => img,
    };
    let [br, bg, bb, _] = opts.color("background").unwrap_or([255, 255, 255, 255]);
    let (bytes, n) = match fmt {
        PNG => png::encode(img, &png_options(opts)),
        BMP => bmp::encode(img),
        TGA => tga::encode(
            img,
            &tga::EncodeOptions {
                rle: opts.bool("rle").unwrap_or(true),
            },
        ),
        QOI => qoi::encode(img),
        HDR => hdr::encode(img)?,
        EXR => exr::encode(
            img,
            &exr::EncodeOptions {
                compression: match opts.text("compression") {
                    Some("none") => exr::EncodeCompression::None,
                    Some("rle") => exr::EncodeCompression::Rle,
                    _ => exr::EncodeCompression::Zip,
                },
            },
        )?,
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
        WEBP => webp::encode(img, &webp_options(opts))?,
        ICO => ico::encode(
            img,
            &ico::EncodeOptions {
                sizes: parse_sizes(opts)?,
                cursor: ext == "cur",
            },
        )?,
        PSD => psd::encode(
            img,
            &psd::EncodeOptions {
                rle: opts.bool("rle").unwrap_or(true),
                large: ext == "psb",
            },
        )?,
        DDS => dds::encode(
            img,
            &dds::EncodeOptions {
                compression: match opts.text("compression") {
                    Some("none") => dds::Compression::None,
                    Some("bc1") => dds::Compression::Bc1,
                    _ => dds::Compression::Bc3,
                },
            },
        )?,
        ICNS => icns::encode(
            img,
            &icns::EncodeOptions {
                sizes: parse_sizes(opts)?,
            },
        )?,
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
                    optimize: opts.bool("optimize") == Some(true),
                    progressive: opts.bool("progressive") == Some(true),
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
    notes.extend(n);
    if fmt != PNG {
        if img.metadata.icc_profile.is_some() && !stores_icc(fmt) {
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
