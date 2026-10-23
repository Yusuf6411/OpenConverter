//! Engine: assembles the format registry and runs conversions.
//!
//! Covers text files (encoding and line-ending conversion), tables (CSV, TSV, JSON) and
//! raster images. Further crates register their formats here as they are written.
//! Process isolation is provided by [`isolated`]; the CLI runs every conversion through it.
#![deny(unsafe_code)]

pub mod archive;
pub mod audio;
pub mod font;
pub mod image;
pub mod isolated;
pub mod subtitle;
pub mod svg;
pub mod table;
pub mod video;

use oc_core::options::{OptionKind, OptionSchema, OptionSpec, OptionValue, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Limits, Note, Notes,
    Ownership, Progress, Result, Severity,
};
use oc_text::{Encoding, LineEnding, Policy};
use std::path::{Path, PathBuf};

pub const TXT: FormatId = FormatId("txt");

/// What a quick look at an upload found out.
#[derive(Debug, Clone, Copy, Default)]
pub struct Probe {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alpha: bool,
}

pub struct Engine {
    registry: Registry,
    pub limits: Limits,
}

#[derive(Debug, Clone)]
pub struct ConvertRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub overwrite: bool,
    pub options: Vec<(String, OptionValue)>,
    /// The name the input had before it was stored under another path (uploads), used for
    /// names written into archives. Defaults to the input path's file name.
    pub display_name: Option<String>,
}

#[derive(Debug)]
pub struct ConvertReport {
    pub input_format: FormatId,
    pub output_format: FormatId,
    pub route: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub notes: Notes,
}

fn text_options() -> OptionSchema {
    OptionSchema {
        specs: vec![
            OptionSpec {
                key: "from_encoding",
                label: "Input encoding",
                help: "auto, utf-8, utf-16le, utf-16be, utf-32le, utf-32be, ascii, iso-8859-1, windows-1252",
                i18n_key: "opt.text.from_encoding",
                kind: OptionKind::Text { default: "auto".into() },
            },
            OptionSpec {
                key: "to_encoding",
                label: "Output encoding",
                help: "same names as the input encoding, except auto",
                i18n_key: "opt.text.to_encoding",
                kind: OptionKind::Text { default: "utf-8".into() },
            },
            OptionSpec {
                key: "line_ending",
                label: "Line endings",
                help: "keep, lf, crlf or cr",
                i18n_key: "opt.text.line_ending",
                kind: OptionKind::Enum { choices: vec!["keep", "lf", "crlf", "cr"], default: "keep" },
            },
            OptionSpec {
                key: "bom",
                label: "Write byte-order mark",
                help: "Only meaningful for Unicode encodings",
                i18n_key: "opt.text.bom",
                kind: OptionKind::Bool { default: false },
            },
            OptionSpec {
                key: "lossy",
                label: "Replace unencodable characters",
                help: "Otherwise a character the output encoding cannot hold is an error",
                i18n_key: "opt.text.lossy",
                kind: OptionKind::Bool { default: false },
            },
        ],
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        let mut registry = Registry::new();
        registry.add_format(FormatDescriptor {
            id: TXT,
            name: "Plain text",
            category: Category::Text,
            extensions: &["txt", "text", "log", "md"],
            mime: &["text/plain"],
            sniffers: Vec::new(),
            capabilities: Capabilities {
                read: true,
                write: true,
                ..Capabilities::default()
            },
            writer_options: text_options(),
            ownership: Ownership::Own,
            spec: "Unicode 15 / WHATWG Encoding (subset)",
        });
        registry.add_edge(Edge {
            from: TXT,
            to: TXT,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 1,
            description: "Decode text -> convert line endings -> encode text",
        });
        image::register(&mut registry);
        audio::register(&mut registry);
        subtitle::register(&mut registry);
        table::register(&mut registry);
        video::register(&mut registry);
        font::register(&mut registry);
        svg::register(&mut registry);
        let packable: Vec<FormatId> = std::iter::once(TXT)
            .chain(image::FORMATS)
            .chain(audio::FORMATS)
            .chain(subtitle::FORMATS)
            .chain(table::FORMATS)
            .chain(video::FORMATS)
            .chain(font::FORMATS)
            .chain(svg::FORMATS)
            .collect();
        archive::register(&mut registry, &packable);
        Self {
            registry,
            limits: Limits::default(),
        }
    }

    /// Look up a registered format by its id string.
    pub fn format_by_name(&self, name: &str) -> Option<FormatId> {
        if name == archive::FILE.0 {
            return Some(archive::FILE);
        }
        self.registry_formats()
            .find(|f| f.id.0 == name)
            .map(|f| f.id)
    }

    /// Turn raw `key=value` strings into typed values using the output format's schema.
    pub fn parse_options(
        &self,
        output: &Path,
        raw: &[(String, String)],
    ) -> Result<Vec<(String, OptionValue)>> {
        let fmt = self.format_for_path(output).ok_or_else(|| {
            Error::new(ErrorCode::UnsupportedFormat, "unknown output file type")
                .with_detail(output.display().to_string())
        })?;
        let schema = &self
            .registry
            .format(fmt)
            .ok_or_else(|| Error::new(ErrorCode::InternalError, "format vanished"))?
            .writer_options;
        raw.iter()
            .map(|(k, v)| Ok((k.clone(), schema.parse_arg(k, v)?)))
            .collect()
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Identify a format from a path's extension.
    /// The longest matching suffix wins, so `a.tar.gz` is a tar.gz, not a gz.
    pub fn format_for_path(&self, path: &Path) -> Option<FormatId> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        self.registry_formats()
            .flat_map(|f| f.extensions.iter().map(move |e| (f.id, *e)))
            .filter(|(_, e)| {
                name.len() > e.len() && name.ends_with(&format!(".{}", e.to_ascii_lowercase()))
            })
            .max_by_key(|(_, e)| e.len())
            .map(|(id, _)| id)
    }

    fn video_to_frames(
        &self,
        from: FormatId,
        data: &[u8],
        opts: &Options,
        progress: &dyn Progress,
        notes: &mut Notes,
    ) -> Result<Vec<u8>> {
        let anim = video::video_frames(from, data, opts, &self.limits, progress, notes, false)?;
        let jpeg = opts.text("frame_format") == Some("jpeg");
        let schema = self
            .registry
            .format(image::JPEG)
            .map(|f| f.writer_options.clone())
            .unwrap_or_default();
        let jpeg_opts = schema.validate(&[(
            "quality".to_string(),
            OptionValue::Int(opts.int("quality").unwrap_or(90)),
        )])?;
        let mut entries = Vec::with_capacity(anim.frames.len());
        for (i, f) in anim.frames.iter().enumerate() {
            progress.check_cancelled()?;
            let (bytes, ext) = if jpeg {
                (
                    image::encode(image::JPEG, &f.image, &jpeg_opts, "jpg")?.0,
                    "jpg",
                )
            } else {
                (
                    image::encode(image::PNG, &f.image, &Options::default(), "png")?.0,
                    "png",
                )
            };
            entries.push(oc_archive::Entry::file(
                format!("frame_{:06}.{ext}", i + 1),
                bytes,
            ));
        }
        notes.push(Note::new(
            Severity::Info,
            "frames_untimed",
            "an image sequence stores no frame times; set fps to space the frames evenly",
        ));
        let cx = oc_archive::Context {
            limits: &self.limits,
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

    fn frames_to_video(
        &self,
        data: &[u8],
        to: FormatId,
        opts: &Options,
        progress: &dyn Progress,
        notes: &mut Notes,
    ) -> Result<Vec<u8>> {
        let cx = oc_archive::Context {
            limits: &self.limits,
            progress,
            password: None,
            list_only: false,
        };
        let archive = oc_archive::format::read(oc_archive::Format::Zip, data, &cx, None)?;
        let mut files: Vec<&oc_archive::Entry> = archive
            .entries
            .iter()
            .filter(|e| e.kind == oc_archive::EntryKind::File)
            .collect();
        files.sort_by(|a, b| natural_cmp(&a.path, &b.path));
        let mut anim: Option<oc_image::anim::Animation> = None;
        for e in files {
            progress.check_cancelled()?;
            let Some(fmt) = self
                .registry
                .detect(&e.data)
                .filter(|f| image::is_image(*f))
            else {
                notes.push(Note::new(
                    Severity::Warning,
                    "frame_skipped",
                    format!("{} is not a picture this build can read", e.path),
                ));
                continue;
            };
            let (img, _) = image::decode(fmt, &e.data, &self.limits, progress)?;
            let a =
                anim.get_or_insert_with(|| oc_image::anim::Animation::new(img.width, img.height));
            if (img.width, img.height) != (a.width, a.height) {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    format!(
                        "{} is {}x{}, but the first picture is {}x{}",
                        e.path, img.width, img.height, a.width, a.height
                    ),
                ));
            }
            a.frames.push(oc_image::anim::Frame {
                image: img,
                delay_ms: 40,
            });
        }
        let anim =
            anim.ok_or_else(|| Error::new(ErrorCode::CorruptFile, "the ZIP holds no pictures"))?;
        let fps = opts.float("fps").filter(|f| *f > 0.0).unwrap_or_else(|| {
            // No rate given: 25 frames a second, as ffmpeg assumes for a numbered sequence.
            notes.push(Note::new(
                Severity::Info,
                "frames_default_rate",
                "no frame rate was set, so the pictures play at 25 frames a second",
            ));
            25.0
        });
        let mut o = opts.clone();
        o.set("fps", OptionValue::Float(fps));
        let per = (1e9 / fps).round() as i64;
        video::animation_to_video_at(&anim, to, &o, &self.limits, progress, notes, Some(per))
    }

    fn registry_formats(&self) -> impl Iterator<Item = &FormatDescriptor> {
        std::iter::once(TXT)
            .chain(image::FORMATS)
            .chain(audio::FORMATS)
            .chain(subtitle::FORMATS)
            .chain(table::FORMATS)
            .chain(video::FORMATS)
            .chain([video::FRAMES])
            .chain(font::FORMATS)
            .chain(svg::FORMATS)
            .chain(archive::FORMATS)
            .filter_map(|id| self.registry.format(id))
    }

    pub fn formats(&self) -> Vec<&FormatDescriptor> {
        self.registry_formats().collect()
    }

    pub fn convert(&self, req: &ConvertRequest, progress: &dyn Progress) -> Result<ConvertReport> {
        let to = self.format_for_path(&req.output).ok_or_else(|| {
            Error::new(ErrorCode::UnsupportedFormat, "unknown output file type")
                .with_detail(req.output.display().to_string())
        })?;
        // Any file can be stored in an archive, whatever its type.
        let ext_from = self
            .format_for_path(&req.input)
            .or_else(|| archive::is_archive(to).then_some(archive::FILE))
            .ok_or_else(|| {
                Error::new(ErrorCode::UnsupportedFormat, "unknown input file type")
                    .with_detail(req.input.display().to_string())
            })?;
        // Fail early on impossible pairs, before reading anything.
        self.registry.plan(ext_from, to, false)?;
        if req.output.exists() && !req.overwrite {
            return Err(
                Error::new(ErrorCode::OutputExists, "output file already exists")
                    .with_detail(req.output.display().to_string()),
            );
        }
        let schema = &self
            .registry
            .format(to)
            .ok_or_else(|| Error::new(ErrorCode::InternalError, "format vanished"))?
            .writer_options;
        let opts = schema.validate(&req.options)?;
        progress.check_cancelled()?;

        let meta = std::fs::metadata(&req.input)?;
        self.limits.check_alloc(meta.len())?;
        let data = std::fs::read(&req.input)?;
        progress.report(1, 3, "read");
        progress.check_cancelled()?;

        // The file's content wins over its extension for images (a .png that is really a
        // BMP still converts), and the mismatch is reported.
        let mut notes = Notes::new();
        let mut from = ext_from;
        if image::is_image(ext_from) {
            if let Some(found) = self.detect_image(&data) {
                if found != ext_from {
                    notes.push(
                        Note::new(
                            Severity::Warning,
                            "format_mismatch",
                            format!(
                                "the file extension says {} but the content is {}",
                                ext_from.0, found.0
                            ),
                        )
                        .with_detail(found.0),
                    );
                    from = found;
                }
            }
        }
        if audio::is_audio(ext_from) {
            if let Some(found) = self.detect_audio(&data) {
                if found != ext_from {
                    notes.push(
                        Note::new(
                            Severity::Warning,
                            "format_mismatch",
                            format!(
                                "the file extension says {} but the content is {}",
                                ext_from.0, found.0
                            ),
                        )
                        .with_detail(found.0),
                    );
                    from = found;
                }
            }
        }
        if subtitle::is_subtitle(ext_from) {
            if let Some(found) = subtitle::detect(&data) {
                if found != ext_from {
                    notes.push(
                        Note::new(
                            Severity::Warning,
                            "format_mismatch",
                            format!(
                                "the file extension says {} but the content is {}",
                                ext_from.0, found.0
                            ),
                        )
                        .with_detail(found.0),
                    );
                    from = found;
                }
            }
        }
        if video::is_video(ext_from) {
            if let Some(found) = video::detect(&data) {
                if found != ext_from {
                    notes.push(
                        Note::new(
                            Severity::Warning,
                            "format_mismatch",
                            format!(
                                "the file extension says {} but the content is {}",
                                ext_from.0, found.0
                            ),
                        )
                        .with_detail(found.0),
                    );
                    from = found;
                }
            }
        }
        if font::is_font(ext_from) {
            if let Some(found) = font::detect(&data) {
                if found != ext_from && !(found == font::TTC && ext_from == font::TTC) {
                    notes.push(
                        Note::new(
                            Severity::Warning,
                            "format_mismatch",
                            format!(
                                "the file extension says {} but the content is {}",
                                ext_from.0, found.0
                            ),
                        )
                        .with_detail(found.0),
                    );
                    from = found;
                }
            }
        }
        if archive::is_archive(from) {
            if let Some(found) = archive::sniff_conflict(ext_from, &data) {
                notes.push(
                    Note::new(
                        Severity::Warning,
                        "format_mismatch",
                        format!(
                            "the file extension says {} but the content is {}",
                            ext_from.0, found.0
                        ),
                    )
                    .with_detail(found.0),
                );
                from = found;
            }
        }
        let route = self.registry.plan(from, to, false)?;
        // Pulling one track out of a video container: the rest of the conversion then runs on
        // that track as if it had been a file of its own.
        let extracted: Option<Vec<u8>>;
        let mut sub_track: Option<(oc_subtitle::Track, FormatId)> = None;
        let (data, from): (&[u8], FormatId) = if video::is_video(from) && audio::is_audio(to) {
            extracted = Some(video::extract_audio(from, &data, &self.limits, &mut notes)?);
            (extracted.as_deref().unwrap_or_default(), audio::MKA)
        } else if video::is_video(from) && subtitle::is_subtitle(to) {
            let (t, k) = video::extract_subtitle(from, &data, &self.limits, &mut notes)?;
            sub_track = Some((t, k));
            extracted = None;
            (&data[..], from)
        } else {
            extracted = None;
            (&data[..], from)
        };
        let _ = &extracted;

        let (out, route_text) = if archive::is_archive(to) {
            archive::convert(
                from,
                to,
                data,
                &req.input,
                req.display_name.as_deref(),
                &opts,
                &self.limits,
                progress,
                &mut notes,
            )?
        } else if video::is_video(from) && to == video::FRAMES {
            let out = self.video_to_frames(from, data, &opts, progress, &mut notes)?;
            let name = |id| self.registry.format(id).map_or("video", |f| f.name);
            (
                out,
                format!(
                    "Decode {} frames -> Write numbered pictures in a ZIP",
                    name(from)
                ),
            )
        } else if from == video::FRAMES && video::is_video(to) {
            let out = self.frames_to_video(data, to, &opts, progress, &mut notes)?;
            let name = |id| self.registry.format(id).map_or("video", |f| f.name);
            (
                out,
                format!("Read numbered pictures -> Encode video in {}", name(to)),
            )
        } else if let Some(out) = if audio::is_audio(from) && audio::is_audio(to) {
            audio::try_copy(from, to, data, &opts, &self.limits, &mut notes)?
        } else {
            None
        } {
            progress.report(3, 4, "copy");
            let name = |id| self.registry.format(id).map_or("audio", |f| f.name);
            (
                out,
                format!(
                    "Copy the {} audio stream into {} (no re-encoding)",
                    name(from),
                    name(to)
                ),
            )
        } else if audio::is_audio(from) {
            let (clip, n) = audio::decode(from, data, &self.limits)?;
            notes.extend(n);
            progress.report(2, 4, "decode");
            progress.check_cancelled()?;
            let source = audio::source_depth(&clip);
            let (clip, n) = audio::process(clip, &opts, &self.limits)?;
            notes.extend(n);
            progress.check_cancelled()?;
            let (out, n) = audio::encode(to, clip, &opts, source)?;
            notes.extend(n);
            let name = |id| self.registry.format(id).map_or("audio", |f| f.name);
            (
                out,
                format!("Decode {} -> Process -> Encode {}", name(from), name(to)),
            )
        } else if video::is_video(from) && image::is_image(to) {
            let anim =
                video::video_to_animation(from, data, &opts, &self.limits, progress, &mut notes)?;
            let (anim, n) = image::transform_animation(anim, &opts, &self.limits)?;
            notes.extend(n);
            let out_ext = req
                .output
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let name = |id| self.registry.format(id).map_or("video", |f| f.name);
            if image::writes_animation(to, &out_ext) {
                let (out, n) = image::encode_animation(to, &anim, &opts)?;
                notes.extend(n);
                (
                    out,
                    format!(
                        "Decode {} frames -> Encode animated {}",
                        name(from),
                        name(to)
                    ),
                )
            } else {
                let first = anim
                    .first_image()
                    .ok_or_else(|| Error::new(ErrorCode::InternalError, "no frame"))?;
                let (out, n) = image::encode(to, &first, &opts, &out_ext)?;
                notes.extend(n);
                notes.push(Note::new(
                    Severity::Info,
                    "image.first_frame_only",
                    "this output holds one picture; the first frame was used",
                ));
                (
                    out,
                    format!("Decode first {} frame -> Encode {}", name(from), name(to)),
                )
            }
        } else if image::is_image(from) && video::is_video(to) {
            let anim = match image::decode_animation(from, data, &self.limits, progress)? {
                Some((a, n)) => {
                    notes.extend(n);
                    a
                }
                None => {
                    let (img, n) = image::decode(from, data, &self.limits, progress)?;
                    notes.extend(n);
                    let mut a = oc_image::anim::Animation::new(img.width, img.height);
                    a.frames.push(oc_image::anim::Frame {
                        image: img,
                        delay_ms: 1000,
                    });
                    a
                }
            };
            let out =
                video::animation_to_video(&anim, to, &opts, &self.limits, progress, &mut notes)?;
            let name = |id| self.registry.format(id).map_or("image", |f| f.name);
            (
                out,
                format!(
                    "Decode {} frames -> Encode video in {}",
                    name(from),
                    name(to)
                ),
            )
        } else if let Some((track, kind)) = sub_track {
            let out = subtitle::finish(track, kind, to, &opts, &mut notes)?;
            let name = |id| self.registry.format(id).map_or("video", |f| f.name);
            (
                out,
                format!(
                    "Extract subtitles from {} -> Write {}",
                    name(from),
                    name(to)
                ),
            )
        } else if video::is_video(from) {
            let out = video::convert(
                from,
                to,
                data,
                &opts,
                &self.registry,
                &self.limits,
                progress,
                &mut notes,
            )?;
            let name = |id| self.registry.format(id).map_or("video", |f| f.name);
            (
                out,
                format!("Read {} streams -> Write {}", name(from), name(to)),
            )
        } else if svg::is_svg(from) && image::is_image(to) {
            let (img, n) = svg::render(data, &opts, &self.limits)?;
            notes.extend(n);
            let out_ext = req
                .output
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            // The size was applied while drawing; resizing again would blur the result.
            let mut after = opts.clone();
            after.set("width", oc_core::options::OptionValue::Int(0));
            after.set("height", oc_core::options::OptionValue::Int(0));
            let (img, n) = image::transform(img, &after, &self.limits)?;
            notes.extend(n);
            let (out, n) = image::encode(to, &img, &opts, &out_ext)?;
            notes.extend(n);
            (out, "Draw SVG -> Encode picture".to_string())
        } else if font::is_font(from) && image::is_image(to) {
            let (img, n) = font::specimen(from, data, &opts, &self.limits)?;
            notes.extend(n);
            let out_ext = req
                .output
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let (img, n) = image::transform(img, &opts, &self.limits)?;
            notes.extend(n);
            let (out, n) = image::encode(to, &img, &opts, &out_ext)?;
            notes.extend(n);
            let name = |id| self.registry.format(id).map_or("font", |f| f.name);
            (
                out,
                format!(
                    "Read {} -> Draw specimen -> Encode {}",
                    name(from),
                    name(to)
                ),
            )
        } else if font::is_font(from) && font::is_font(to) {
            let (out, n) = font::convert(from, to, data, &opts, &self.limits)?;
            notes.extend(n);
            let name = |id| self.registry.format(id).map_or("font", |f| f.name);
            (out, format!("Read {} -> Write {}", name(from), name(to)))
        } else if subtitle::is_subtitle(from) {
            let (out, n) = subtitle::convert(from, to, data, &opts)?;
            notes.extend(n);
            let name = |id| self.registry.format(id).map_or("subtitle", |f| f.name);
            (
                out,
                format!("Parse {} cues -> Write {}", name(from), name(to)),
            )
        } else if table::is_table(from) {
            let (out, n) = table::convert(from, to, data, &opts, &self.limits, progress)?;
            notes.extend(n);
            let name = |id| self.registry.format(id).map_or("table", |f| f.name);
            (out, format!("Read {} -> Write {}", name(from), name(to)))
        } else if from == TXT {
            let (out, n) = convert_text(data, &opts)?;
            notes.extend(n);
            // Same-format conversions plan to an empty route; describe the actual work.
            (
                out,
                "Decode text -> convert line endings -> encode text".to_string(),
            )
        } else {
            let out_ext = req
                .output
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let lossless = if from == image::JPEG
                && to == image::JPEG
                && opts.bool("lossless") == Some(true)
            {
                image::lossless_jpeg(data, &opts, &self.limits, progress)?
            } else {
                None
            };
            if opts.bool("lossless") == Some(true) && lossless.is_none() && to == image::JPEG {
                notes.push(Note::new(
                    Severity::Info,
                    "jpeg.lossless_skipped",
                    "the lossless route needs a JPEG input and only rotate, flip or progressive set; the picture was decoded and encoded instead",
                ));
            }
            if let Some((out, n)) = lossless {
                notes.extend(n);
                (
                    out,
                    "Lossless JPEG transform on DCT coefficients".to_string(),
                )
            } else {
                // An animation stays one when the output can hold it; otherwise the still path
                // below reads the first frame and says so.
                let animation = if image::writes_animation(to, &out_ext)
                    && opts.text("frames") != Some("first")
                {
                    image::decode_animation(from, data, &self.limits, progress)?
                } else {
                    None
                };
                if let Some((anim, n)) = animation {
                    notes.extend(n);
                    progress.report(2, 4, "decode");
                    progress.check_cancelled()?;
                    let (anim, n) = image::transform_animation(anim, &opts, &self.limits)?;
                    notes.extend(n);
                    let (out, n) = image::encode_animation(to, &anim, &opts)?;
                    notes.extend(n);
                    let name = |id| self.registry.format(id).map_or("image", |f| f.name);
                    (
                        out,
                        format!(
                            "Decode {} animation -> Encode {} animation",
                            name(from),
                            name(to)
                        ),
                    )
                } else {
                    let (img, n) = image::decode(from, data, &self.limits, progress)?;
                    notes.extend(n);
                    progress.report(2, 4, "decode");
                    progress.check_cancelled()?;
                    let (img, n) = image::transform(img, &opts, &self.limits)?;
                    notes.extend(n);
                    let (out, n) = image::encode(to, &img, &opts, &out_ext)?;
                    notes.extend(n);
                    let route_text = if route.edges.is_empty() {
                        // Same format in and out still decodes and re-encodes.
                        let name = |id| self.registry.format(id).map_or("image", |f| f.name);
                        format!("Decode {} -> Encode {}", name(from), name(to))
                    } else {
                        route.explain()
                    };
                    (out, route_text)
                }
            }
        };
        progress.report(3, 4, "convert");
        progress.check_cancelled()?;

        let tmp = req.output.with_extension("oc-partial");
        std::fs::write(&tmp, &out)?;
        if let Err(e) = std::fs::rename(&tmp, &req.output) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        progress.report(4, 4, "write");
        Ok(ConvertReport {
            input_format: from,
            output_format: to,
            route: route_text,
            bytes_in: data.len() as u64,
            bytes_out: out.len() as u64,
            notes,
        })
    }

    /// The format of an uploaded file: images by content, anything else by the extension of
    /// `name`.
    pub fn detect(&self, data: &[u8], name: &Path) -> Option<FormatId> {
        self.detect_image(data)
            .or_else(|| self.detect_audio(data))
            .or_else(|| self.format_for_path(name).filter(|f| !image::is_image(*f)))
            .or_else(|| oc_archive::Format::sniff(data).and_then(archive::id_of))
            // A file of a type we do not know can still be stored in an archive, but a file
            // that claims a known type and does not match it is refused.
            .or_else(|| {
                self.format_for_path(name)
                    .is_none()
                    .then_some(archive::FILE)
            })
    }

    /// Entries of an archive without reading their contents where the format allows.
    pub fn archive_list(
        &self,
        path: &Path,
        password: Option<&str>,
        progress: &dyn Progress,
    ) -> Result<(FormatId, Vec<archive::EntryInfo>)> {
        archive::list(path, password, &self.limits, progress)
    }

    /// Extract an archive below `dest`, refusing unsafe paths and links.
    pub fn archive_extract(
        &self,
        path: &Path,
        dest: &Path,
        opts: &oc_archive::safety::ExtractOptions,
        password: Option<&str>,
        progress: &dyn Progress,
    ) -> Result<oc_archive::safety::ExtractReport> {
        archive::extract(path, dest, opts, password, &self.limits, progress)
    }

    /// Create an archive at `output` (format chosen by its name) from files and folders.
    pub fn archive_create(
        &self,
        sources: &[PathBuf],
        output: &Path,
        overwrite: bool,
        options: &[(String, OptionValue)],
        progress: &dyn Progress,
    ) -> Result<ConvertReport> {
        let to = self
            .format_for_path(output)
            .filter(|f| archive::is_archive(*f))
            .ok_or_else(|| {
                Error::new(ErrorCode::UnsupportedFormat, "unknown archive type")
                    .with_detail(output.display().to_string())
            })?;
        if output.exists() && !overwrite {
            return Err(
                Error::new(ErrorCode::OutputExists, "output file already exists")
                    .with_detail(output.display().to_string()),
            );
        }
        let schema = &self
            .registry
            .format(to)
            .ok_or_else(|| Error::new(ErrorCode::InternalError, "format vanished"))?
            .writer_options;
        let opts = schema.validate(options)?;
        let (bytes, n) = archive::create(sources, to, &opts, &self.limits, progress)?;
        let tmp = output.with_extension("oc-partial");
        std::fs::write(&tmp, &bytes)?;
        if let Err(e) = std::fs::rename(&tmp, output) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(ConvertReport {
            input_format: archive::FILE,
            output_format: to,
            route: format!("Collect {n} entries -> Write archive"),
            bytes_in: 0,
            bytes_out: bytes.len() as u64,
            notes: Notes::new(),
        })
    }

    /// Formats a file of format `from` can be converted to (same format included when it
    /// has options that make rewriting meaningful).
    pub fn targets(&self, from: FormatId) -> Vec<FormatId> {
        self.registry_formats()
            .filter(|f| f.capabilities.write)
            .filter(|f| self.registry.plan(from, f.id, false).is_ok())
            .map(|f| f.id)
            .collect()
    }

    /// Size and transparency of an image; `None` for non-images. Decodes the file, so run
    /// it in a worker for untrusted input.
    pub fn probe(&self, from: FormatId, data: &[u8], progress: &dyn Progress) -> Result<Probe> {
        if !image::is_image(from) {
            return Ok(Probe::default());
        }
        let (img, _) = image::decode(from, data, &self.limits, progress)?;
        Ok(Probe {
            width: Some(img.width),
            height: Some(img.height),
            alpha: img.layout.has_alpha(),
        })
    }

    /// The first rows of a table or of one sheet of a workbook, as JSON (see
    /// [`table::preview_json`]). Parses the whole file, so run it in a worker for untrusted input.
    pub fn table_preview(
        &self,
        from: FormatId,
        data: &[u8],
        sheet: &str,
        max_rows: usize,
        progress: &dyn Progress,
    ) -> Result<String> {
        if !table::is_table(from) {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                "not a table format",
            ));
        }
        table::preview_json(from, data, sheet, max_rows, 60, &self.limits, progress)
    }

    /// Waveform peaks, loudness and tags of an audio file as JSON (see `audio::analyse_json`).
    pub fn audio_analysis(&self, from: FormatId, data: &[u8], buckets: usize) -> Result<String> {
        if !audio::is_audio(from) {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                "not an audio format",
            ));
        }
        audio::analyse_json(from, data, buckets, &self.limits)
    }

    /// The first `secs` seconds of an audio file as 16-bit WAV, for playback in the browser.
    pub fn audio_preview(&self, from: FormatId, data: &[u8], secs: f64) -> Result<Vec<u8>> {
        if !audio::is_audio(from) {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                "not an audio format",
            ));
        }
        audio::preview_wav(from, data, secs, &self.limits)
    }

    /// Streams and a thumbnail strip of a video file, as JSON.
    pub fn video_analysis(&self, from: FormatId, data: &[u8], thumbs: usize) -> Result<String> {
        if !video::is_video(from) {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                "not a video format",
            ));
        }
        video::analyse_json(from, data, &self.limits, thumbs, 160)
    }

    /// A PNG preview of at most `max` pixels on a side.
    pub fn thumbnail(
        &self,
        from: FormatId,
        data: &[u8],
        max: u32,
        progress: &dyn Progress,
    ) -> Result<Vec<u8>> {
        use oc_image::ops;
        let (img, _) = image::decode(from, data, &self.limits, progress)?;
        let (w, h) = (img.width.max(1), img.height.max(1));
        let img = if w.max(h) > max {
            let scale = f64::from(max) / f64::from(w.max(h));
            let nw = ((f64::from(w) * scale).round() as u32).max(1);
            let nh = ((f64::from(h) * scale).round() as u32).max(1);
            ops::resize(&img, nw, nh, ops::Filter::Bilinear)
        } else {
            img
        };
        let (img, _) = ops::convert_depth(&img, oc_image::Depth::Eight);
        Ok(oc_image::png::encode(&img, &oc_image::png::EncodeOptions::default()).0)
    }

    /// Identify an audio file by its leading bytes among the registered audio formats.
    fn detect_audio(&self, data: &[u8]) -> Option<FormatId> {
        audio::FORMATS
            .into_iter()
            .filter_map(|id| self.registry.format(id))
            .filter_map(|f| f.sniff(data).map(|c| (c, f.id)))
            // MP3 has no real magic number: its candidates must also parse as frames.
            .filter(|(_, id)| *id != audio::MP3 || oc_audio::mp3::sniff(data))
            .filter(|(_, id)| *id != audio::M4A || oc_audio::m4a::sniff(data))
            .filter(|(_, id)| *id != audio::OPUS || oc_audio::opus::sniff(data))
            .filter(|(_, id)| *id != audio::OGG || !oc_audio::opus::sniff(data))
            .filter(|(_, id)| {
                let doc = oc_container::matroska::doc_type(data);
                match *id {
                    i if i == audio::MKA => {
                        doc.as_deref() == Some("matroska")
                            && !oc_container::matroska::has_video(data)
                    }
                    i if i == audio::WEBA => {
                        doc.as_deref() == Some("webm") && !oc_container::matroska::has_video(data)
                    }
                    _ => true,
                }
            })
            .max_by_key(|(c, _)| *c)
            .map(|(_, id)| id)
            .or_else(|| oc_audio::mp3::sniff(data).then_some(audio::MP3))
    }

    /// Identify an image by its leading bytes among the registered image formats.
    fn detect_image(&self, data: &[u8]) -> Option<FormatId> {
        image::FORMATS
            .into_iter()
            .filter_map(|id| self.registry.format(id))
            .filter_map(|f| f.sniff(data).map(|c| (c, f.id)))
            .max_by_key(|(c, _)| *c)
            .map(|(_, id)| id)
    }
}

fn encoding_option(name: &str, key: &str) -> Result<Encoding> {
    Encoding::from_name(name)
        .ok_or_else(|| Error::invalid_option(key, format!("unknown encoding '{name}'")))
}

/// Convert text bytes according to validated options.
pub fn convert_text(data: &[u8], opts: &Options) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let from_name = opts.text("from_encoding").unwrap_or("auto");
    let (enc_in, skip) = if from_name == "auto" {
        let d = oc_text::detect(data);
        if !d.certain && d.encoding == Encoding::Windows1252 {
            notes.push(Note::new(
                Severity::Warning,
                "encoding_guessed",
                "input is not valid UTF-8; assumed windows-1252",
            ));
        }
        (d.encoding, d.bom_len)
    } else {
        let e = encoding_option(from_name, "from_encoding")?;
        let bom = oc_text::bom(e);
        (
            e,
            if !bom.is_empty() && data.starts_with(bom) {
                bom.len()
            } else {
                0
            },
        )
    };
    let to = encoding_option(opts.text("to_encoding").unwrap_or("utf-8"), "to_encoding")?;
    let policy = if opts.bool("lossy") == Some(true) {
        Policy::Replace
    } else {
        Policy::Strict
    };

    let (mut text, n) = oc_text::decode(data.get(skip..).unwrap_or(&[]), enc_in, policy)?;
    notes.extend(n);
    match opts.text("line_ending").unwrap_or("keep") {
        "lf" => text = oc_text::convert_line_endings(&text, LineEnding::Lf),
        "crlf" => text = oc_text::convert_line_endings(&text, LineEnding::CrLf),
        "cr" => text = oc_text::convert_line_endings(&text, LineEnding::Cr),
        _ => {}
    }
    let (body, n) = oc_text::encode(&text, to, policy)?;
    notes.extend(n);
    let mut out = Vec::with_capacity(body.len() + 4);
    if opts.bool("bom") == Some(true) {
        out.extend_from_slice(oc_text::bom(to));
    }
    out.extend_from_slice(&body);
    Ok((out, notes))
}

/// Orders names so that `frame2` comes before `frame10`.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let nx = x.iter().take_while(|c| c.is_ascii_digit()).count();
                let ny = y.iter().take_while(|c| c.is_ascii_digit()).count();
                let (dx, rest_x) = x.split_at(nx);
                let (dy, rest_y) = y.split_at(ny);
                // Leading zeros do not count: compare by length, then digit by digit.
                let tx = dx.get(dx.iter().take_while(|c| **c == b'0').count().min(nx - 1)..);
                let ty = dy.get(dy.iter().take_while(|c| **c == b'0').count().min(ny - 1)..);
                let (tx, ty) = (tx.unwrap_or_default(), ty.unwrap_or_default());
                let ord = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if ord != Ordering::Equal {
                    return ord;
                }
                x = rest_x;
                y = rest_y;
            }
            (Some(p), Some(q)) => {
                if p != q {
                    return p.cmp(q);
                }
                x = x.get(1..).unwrap_or_default();
                y = y.get(1..).unwrap_or_default();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::unwrap_used)]
    use super::*;
    use oc_core::NoProgress;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oc-engine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).ok();
        d
    }

    fn req(dir: &Path, input: &[u8], opts: &[(&str, OptionValue)]) -> (ConvertRequest, PathBuf) {
        let i = dir.join("in.txt");
        std::fs::write(&i, input).ok();
        let o = dir.join("out.txt");
        (
            ConvertRequest {
                input: i,
                output: o.clone(),
                overwrite: false,
                display_name: None,
                options: opts
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), v.clone()))
                    .collect(),
            },
            o,
        )
    }

    #[test]
    fn utf16_to_utf8_with_crlf_normalised() {
        let dir = tmpdir("a");
        let input = oc_text::encode("a\r\nb\rc", Encoding::Utf16Le, Policy::Strict)
            .map(|r| r.0)
            .unwrap_or_default();
        let mut bytes = oc_text::bom(Encoding::Utf16Le).to_vec();
        bytes.extend(input);
        let (r, out) = req(
            &dir,
            &bytes,
            &[("line_ending", OptionValue::Text("lf".into()))],
        );
        let rep = Engine::new().convert(&r, &NoProgress).ok();
        assert_eq!(std::fs::read(out).ok().as_deref(), Some(&b"a\nb\nc"[..]));
        assert_eq!(rep.map(|r| r.route.contains("line endings")), Some(true));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unencodable_is_error_unless_lossy() {
        let dir = tmpdir("b");
        let (r, _) = req(
            &dir,
            "日本".as_bytes(),
            &[("to_encoding", OptionValue::Text("iso-8859-1".into()))],
        );
        let e = Engine::new().convert(&r, &NoProgress).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::UnsupportedVariant));
        let (mut r2, out) = req(
            &dir,
            "日本".as_bytes(),
            &[
                ("to_encoding", OptionValue::Text("iso-8859-1".into())),
                ("lossy", OptionValue::Bool(true)),
            ],
        );
        r2.overwrite = true;
        let rep = Engine::new().convert(&r2, &NoProgress).ok();
        assert_eq!(std::fs::read(out).ok().as_deref(), Some(&b"??"[..]));
        assert_eq!(rep.map(|r| r.notes.has_loss()), Some(true));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn existing_output_is_protected_and_bad_options_rejected() {
        let dir = tmpdir("c");
        let (r, out) = req(&dir, b"x", &[]);
        std::fs::write(&out, b"keep").ok();
        let e = Engine::new().convert(&r, &NoProgress).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::OutputExists));
        assert_eq!(std::fs::read(&out).ok().as_deref(), Some(&b"keep"[..]));
        let (mut r2, _) = req(
            &dir,
            b"x",
            &[("to_encoding", OptionValue::Text("klingon".into()))],
        );
        r2.overwrite = true;
        assert_eq!(
            Engine::new()
                .convert(&r2, &NoProgress)
                .err()
                .map(|e| e.code),
            Some(ErrorCode::InvalidOptions)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn png_fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../oc-image/tests/data/png")
            .join(name)
    }

    fn decode_rgba(path: &Path) -> Vec<u8> {
        let e = Engine::new();
        let fmt = e.format_for_path(path).unwrap();
        let data = std::fs::read(path).unwrap();
        let (img, _) = image::decode(fmt, &data, &e.limits, &NoProgress).unwrap();
        oc_image::ops::convert_layout(&img, oc_image::Layout::Rgba, [0, 0, 0])
            .0
            .data
    }

    fn run(from: &Path, to: &Path, opts: &[(&str, &str)]) -> Result<ConvertReport> {
        let e = Engine::new();
        let raw: Vec<(String, String)> = opts
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let options = e.parse_options(to, &raw)?;
        e.convert(
            &ConvertRequest {
                input: from.to_path_buf(),
                output: to.to_path_buf(),
                overwrite: true,
                display_name: None,
                options,
            },
            &NoProgress,
        )
    }

    #[test]
    fn every_image_format_roundtrips_opaque_and_translucent_pictures() {
        let dir = tmpdir("img");
        for fixture in ["rgb8", "pal8_trns"] {
            let src = png_fixture(&format!("{fixture}.png"));
            let want = std::fs::read(png_fixture(&format!("{fixture}.rgba"))).unwrap();
            for ext in [
                "png", "bmp", "tga", "qoi", "pam", "ppm", "gif", "ico", "tiff", "webp",
            ] {
                if ext == "ppm" && fixture == "pal8_trns" {
                    continue; // PPM has no alpha
                }
                if ext == "gif" {
                    continue; // palette format: covered by its own tests below
                }
                let mid = dir.join(format!("{fixture}.{ext}"));
                let rep = run(&src, &mid, &[]).unwrap();
                assert!(
                    rep.route.starts_with("Decode PNG -> Encode"),
                    "{}",
                    rep.route
                );
                assert_eq!(decode_rgba(&mid), want, "{fixture} via {ext}");
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn jpeg_round_trip_is_close_and_says_it_is_lossy() {
        let dir = tmpdir("jpg");
        let jpg = dir.join("o.jpg");
        let rep = run(
            &png_fixture("rgb8.png"),
            &jpg,
            &[("quality", "95"), ("subsampling", "4:4:4")],
        )
        .unwrap();
        assert!(rep
            .notes
            .iter()
            .any(|n| n.code == "image.lossy_compression"));
        assert!(!rep.notes.has_loss());
        let (a, b) = (
            decode_rgba(&jpg),
            std::fs::read(png_fixture("rgb8.rgba")).unwrap(),
        );
        let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(worst < 25, "worst channel error {worst}");
        // And back to PNG, with the extension spelled differently.
        let back = dir.join("back.png");
        let rep = run(&jpg, &back, &[]).unwrap();
        assert_eq!(rep.input_format, image::JPEG);
        assert_eq!(
            run(
                &png_fixture("rgb8.png"),
                &dir.join("x.jpg"),
                &[("quality", "0")]
            )
            .err()
            .map(|e| e.code),
            Some(ErrorCode::InvalidOptions)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ico_sizes_option() {
        let dir = tmpdir("ico");
        let out = dir.join("o.ico");
        run(&png_fixture("rgb8.png"), &out, &[("sizes", "16, 32,64")]).unwrap();
        let e = Engine::new();
        let (img, n) = image::decode(
            image::ICO,
            &std::fs::read(&out).unwrap(),
            &e.limits,
            &NoProgress,
        )
        .unwrap();
        assert_eq!((img.width, img.height), (64, 64));
        assert!(n.iter().any(|n| n.code == "ico.largest_image"));
        assert_eq!(
            run(&png_fixture("rgb8.png"), &out, &[("sizes", "abc")])
                .err()
                .map(|e| e.code),
            Some(ErrorCode::InvalidOptions)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn webp_options_and_metadata() {
        let dir = tmpdir("webp");
        let src = png_fixture("pal8_trns.png");
        let want = std::fs::read(png_fixture("pal8_trns.rgba")).unwrap();
        let mut sizes = Vec::new();
        for effort in ["0", "4", "6"] {
            let out = dir.join(format!("e{effort}.webp"));
            let rep = run(&src, &out, &[("effort", effort)]).unwrap();
            assert!(!rep.notes.has_loss());
            assert_eq!(decode_rgba(&out), want, "effort {effort}");
            sizes.push(std::fs::metadata(&out).unwrap().len());
        }
        assert!(sizes[2] <= sizes[0], "{sizes:?}");
        assert_eq!(
            run(&src, &dir.join("bad.webp"), &[("effort", "9")])
                .err()
                .map(|e| e.code),
            Some(ErrorCode::InvalidOptions)
        );
        // Hidden colour under transparent pixels is cleared on request, and only then.
        let keep = dir.join("keep.webp");
        let clean = dir.join("clean.webp");
        run(&src, &keep, &[]).unwrap();
        run(&src, &clean, &[("clean_transparent", "true")]).unwrap();
        for (path, cleaned) in [(&keep, false), (&clean, true)] {
            let got = decode_rgba(path);
            for (g, w) in got.chunks(4).zip(want.chunks(4)) {
                assert_eq!(g[3], w[3]);
                if w[3] == 0 && cleaned {
                    assert_eq!(&g[..3], &[0, 0, 0]);
                } else if w[3] != 0 || !cleaned {
                    assert_eq!(g, w);
                }
            }
        }
        // The colour profile travels in the file.
        let e = Engine::new();
        let (mut img, _) = image::decode(
            image::PNG,
            &std::fs::read(&src).unwrap(),
            &e.limits,
            &NoProgress,
        )
        .unwrap();
        img.metadata.icc_profile = Some(vec![1, 2, 3, 4, 5]);
        let (bytes, notes) = image::encode(image::WEBP, &img, &Options::default(), "webp").unwrap();
        assert!(!notes.iter().any(|n| n.code == "image.icc_dropped"));
        let (back, _) = image::decode(image::WEBP, &bytes, &e.limits, &NoProgress).unwrap();
        assert_eq!(back.metadata.icc_profile, Some(vec![1, 2, 3, 4, 5]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn colour_profiles_follow_the_target() {
        let e = Engine::new();
        let profile = std::fs::read(format!(
            "{}/../oc-image/tests/data/icc/m_gamma22.icc",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let mut img = oc_image::Image::new(
            2,
            1,
            oc_image::Layout::Rgb,
            oc_image::Depth::Eight,
            vec![0, 255, 0, 255, 0, 0],
        )
        .unwrap();
        img.metadata.icc_profile = Some(profile);
        let enc =
            |img: &oc_image::Image, fmt| image::encode(fmt, img, &Options::default(), "x").unwrap();
        // PNG stores it, so the pixels stay as they are.
        let (bytes, notes) = enc(&img, image::PNG);
        assert!(!notes.iter().any(|n| n.code.starts_with("image.icc")));
        let (back, _) = image::decode(image::PNG, &bytes, &e.limits, &NoProgress).unwrap();
        assert!(back.metadata.icc_profile.is_some());
        assert_eq!(back.data, img.data);
        // BMP cannot: the pixels are converted and the profile is gone.
        let (bytes, notes) = enc(&img, image::BMP);
        assert!(notes.iter().any(|n| n.code == "image.icc_converted"));
        let (back, _) = image::decode(image::BMP, &bytes, &e.limits, &NoProgress).unwrap();
        assert!(back.metadata.icc_profile.is_none());
        // Adobe RGB green is more saturated than sRGB can show, so it clips to pure green.
        assert_eq!(&back.data[..3], &[0, 255, 0]);
        assert!(back.data[3] > 200, "{:?}", back.data);
        // A profile we cannot read is dropped with a warning instead of failing the conversion.
        img.metadata.icc_profile = Some(vec![1, 2, 3]);
        let (_, notes) = enc(&img, image::BMP);
        assert!(notes.iter().any(|n| n.code == "image.icc_unusable"));
    }

    #[test]
    fn webp_lossy_quality_and_alpha() {
        let dir = tmpdir("webp_lossy");
        for name in ["rgb8.png", "pal8_trns.png"] {
            let src = png_fixture(name);
            let want = std::fs::read(png_fixture(&name.replace(".png", ".rgba"))).unwrap();
            let mut sizes = Vec::new();
            for quality in ["10", "60", "95"] {
                let out = dir.join(format!("{name}_{quality}.webp"));
                let rep = run(&src, &out, &[("lossless", "false"), ("quality", quality)]).unwrap();
                assert!(rep
                    .notes
                    .iter()
                    .any(|n| n.code == "image.lossy_compression"));
                let got = decode_rgba(&out);
                assert_eq!(got.len(), want.len());
                let (mut err, mut alpha_ok) = (0u64, true);
                for (g, w) in got.chunks(4).zip(want.chunks(4)) {
                    alpha_ok &= g[3] == w[3];
                    if w[3] != 0 {
                        for k in 0..3 {
                            err += u64::from(g[k].abs_diff(w[k]));
                        }
                    }
                }
                assert!(alpha_ok, "{name} q{quality}: alpha must survive exactly");
                let mean = err as f64 / want.len() as f64 * 4.0 / 3.0;
                assert!(mean < 40.0, "{name} q{quality}: mean error {mean}");
                sizes.push(std::fs::metadata(&out).unwrap().len());
            }
            assert!(sizes[0] <= sizes[2], "{name}: {sizes:?}");
        }
        let out = dir.join("bad.webp");
        for (k, v) in [("quality", "101"), ("quality", "-1")] {
            assert_eq!(
                run(
                    &png_fixture("rgb8.png"),
                    &out,
                    &[("lossless", "false"), (k, v)]
                )
                .err()
                .map(|e| e.code),
                Some(ErrorCode::InvalidOptions),
                "{k}={v}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    fn anim_fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../oc-image/tests/data/anim")
            .join(name)
    }

    fn frame_count(path: &Path) -> usize {
        let data = std::fs::read(path).unwrap();
        let e = Engine::new();
        image::decode_animation(
            e.format_for_path(path).unwrap(),
            &data,
            &e.limits,
            &NoProgress,
        )
        .unwrap()
        .map_or(1, |(a, _)| a.frames.len())
    }

    #[test]
    fn animations_stay_animations_where_the_output_allows() {
        let dir = tmpdir("anim");
        let src = anim_fixture("gif_clear_d2.gif");
        for name in ["a.webp", "a.gif", "a.apng"] {
            let out = dir.join(name);
            let rep = run(&src, &out, &[]).unwrap();
            assert!(rep.route.contains("animation"), "{name}: {}", rep.route);
            assert_eq!(frame_count(&out), 5, "{name}");
        }
        // A plain PNG is a still, and the report says what was dropped.
        let out = dir.join("a.png");
        let rep = run(&src, &out, &[]).unwrap();
        assert!(rep
            .notes
            .iter()
            .any(|n| n.code == "gif.animation_first_frame"));
        assert_eq!(frame_count(&out), 1);
        // The user can ask for the first frame even where animation would work.
        let out = dir.join("first.webp");
        run(&src, &out, &[("frames", "first")]).unwrap();
        assert_eq!(frame_count(&out), 1);
        // Transforms reach every frame.
        let out = dir.join("small.gif");
        run(&src, &out, &[("width", "16")]).unwrap();
        let data = std::fs::read(&out).unwrap();
        let (a, _) = oc_image::gif::decode_animation(&data, &Limits::default()).unwrap();
        assert_eq!((a.width, a.height, a.frames.len()), (16, 12, 5));
        assert!(a.frames.iter().all(|f| f.image.width == 16));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gif_output_reports_quantisation() {
        let dir = tmpdir("gifq");
        let out = dir.join("o.gif");
        let rep = run(&png_fixture("rgb8.png"), &out, &[("dither", "false")]).unwrap();
        assert!(rep.notes.iter().any(|n| n.code == "gif.quantized"));
        assert!(rep.notes.has_loss());
        let rep = run(&png_fixture("pal8_trns.png"), &out, &[]).unwrap();
        assert!(rep.notes.iter().any(|n| n.code == "gif.alpha_binary"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn content_wins_over_extension_and_is_reported() {
        let dir = tmpdir("mismatch");
        let fake = dir.join("really_a_png.bmp");
        std::fs::copy(png_fixture("rgb8.png"), &fake).unwrap();
        let out = dir.join("out.qoi");
        let rep = run(&fake, &out, &[]).unwrap();
        assert_eq!(rep.input_format, image::PNG);
        assert!(rep.notes.iter().any(|n| n.code == "format_mismatch"));
        assert_eq!(
            decode_rgba(&out),
            std::fs::read(png_fixture("rgb8.rgba")).unwrap()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn subtitles_convert_between_all_formats_with_retiming() {
        let dir = tmpdir("subtitles");
        let srt = dir.join("in.srt");
        std::fs::write(
            &srt,
            "1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Caf\u{e9}</i> <b>bar</b>\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nSecond\r\n",
        )
        .unwrap();
        let ass = dir.join("a.ass");
        let rep = run(&srt, &ass, &[("shift_ms", "500")]).unwrap();
        assert_eq!(rep.output_format, subtitle::ASS);
        let text = std::fs::read_to_string(&ass).unwrap();
        assert!(text.contains("Dialogue: 0,0:00:01.50,0:00:03.00,Default,,0,0,0,,{\\i1}Caf\u{e9}{\\i0} {\\b1}bar{\\b0}"));
        let vtt = dir.join("b.vtt");
        run(&ass, &vtt, &[]).unwrap();
        let text = std::fs::read_to_string(&vtt).unwrap();
        assert!(text.starts_with("WEBVTT"));
        assert!(text.contains("00:00:01.500 --> 00:00:03.000"));
        assert!(text.contains("<i>Caf\u{e9}</i> <b>bar</b>"));
        let ttml = dir.join("c.ttml");
        run(&vtt, &ttml, &[]).unwrap();
        let back = dir.join("d.srt");
        run(&ttml, &back, &[("fps_from", "25"), ("fps_to", "24")]).unwrap();
        let text = std::fs::read_to_string(&back).unwrap();
        // 1.5 s * 25/24 = 1.5625 s
        assert!(text.contains("00:00:01,563 --> 00:00:03,125"), "{text}");
        assert!(text.contains("<i>Caf\u{e9}</i> <b>bar</b>"));
        // Legacy output encoding.
        let latin = dir.join("e.srt");
        run(&srt, &latin, &[("to_encoding", "windows-1252")]).unwrap();
        assert!(std::fs::read(&latin).unwrap().contains(&0xE9));
        // Legacy input is read back with the encoding named.
        let again = dir.join("f.vtt");
        run(&latin, &again, &[("from_encoding", "windows-1252")]).unwrap();
        assert!(std::fs::read_to_string(&again)
            .unwrap()
            .contains("Caf\u{e9}"));
    }

    #[test]
    fn audio_converts_through_opus_and_alac() {
        let dir = tmpdir("audio-formats");
        // One second of a 440 Hz tone with a little noise at 44.1 kHz, as WAV.
        let mut pcm = Vec::new();
        let mut seed = 7u32;
        for i in 0..44100u32 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = ((seed >> 16) as f32 / 65536.0 - 0.5) * 0.02;
            let v =
                ((i as f32 * 440.0 * std::f32::consts::TAU / 44100.0).sin() * 0.5 + n) * 32767.0;
            let s = v as i16;
            pcm.extend_from_slice(&s.to_le_bytes());
            pcm.extend_from_slice(&s.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&(44100u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(&pcm);
        let src = dir.join("in.wav");
        std::fs::write(&src, &wav).unwrap();

        // Opus: always 48 kHz, so the resampling is reported; the content sniffs as Opus.
        let opus = dir.join("out.opus");
        let rep = run(&src, &opus, &[("bitrate", "96"), ("rate_mode", "cbr")]).unwrap();
        assert_eq!(rep.output_format, audio::OPUS);
        assert!(rep.notes.iter().any(|n| n.code == "opus.resampled"));
        let back = dir.join("back.wav");
        let rep = run(&opus, &back, &[]).unwrap();
        assert_eq!(rep.input_format, audio::OPUS);
        let (a, _) =
            oc_audio::wav::read(&std::fs::read(&back).unwrap(), &Limits::default()).unwrap();
        assert_eq!(a.sample_rate, 48000);
        assert!((a.duration_secs() - 1.0).abs() < 0.01);

        // The Ogg extension holds Vorbis, and a renamed Opus file is still recognised.
        let renamed = dir.join("renamed.ogg");
        std::fs::copy(&opus, &renamed).unwrap();
        let rep = run(&renamed, &dir.join("again.wav"), &[]).unwrap();
        assert_eq!(rep.input_format, audio::OPUS);

        // ALAC in M4A is lossless.
        let m4a = dir.join("out.m4a");
        run(&src, &m4a, &[]).unwrap();
        let back2 = dir.join("back2.wav");
        run(&m4a, &back2, &[]).unwrap();
        let (b, _) =
            oc_audio::wav::read(&std::fs::read(&back2).unwrap(), &Limits::default()).unwrap();
        let (c, _) = oc_audio::wav::read(&wav, &Limits::default()).unwrap();
        assert_eq!(b.samples, c.samples);

        // Matroska holds FLAC without loss; WebM takes Opus and tells the formats apart.
        let mka = dir.join("out.mka");
        let rep = run(&src, &mka, &[]).unwrap();
        assert_eq!(rep.output_format, audio::MKA);
        let back3 = dir.join("back3.wav");
        let rep = run(&mka, &back3, &[]).unwrap();
        assert_eq!(rep.input_format, audio::MKA);
        let (d, _) =
            oc_audio::wav::read(&std::fs::read(&back3).unwrap(), &Limits::default()).unwrap();
        assert_eq!(d.samples, c.samples);
        let weba = dir.join("out.weba");
        run(&src, &weba, &[("bitrate", "64")]).unwrap();
        // Opus moves between Ogg and WebM as packets: no new encoding, so the audio is identical.
        let opus2 = dir.join("copy.opus");
        let rep = run(&weba, &opus2, &[]).unwrap();
        assert!(rep.route.contains("no re-encoding"), "{}", rep.route);
        let (x, _) =
            oc_audio::opus::read(&std::fs::read(&opus).unwrap(), &Limits::default()).unwrap();
        let weba2 = dir.join("copy.weba");
        run(&opus, &weba2, &[]).unwrap();
        let (y, _) =
            oc_audio::mka::read(&std::fs::read(&weba2).unwrap(), &Limits::default()).unwrap();
        assert_eq!(x.samples, y.samples);
        // Asking for a different rate means a real re-encode.
        let rep = run(&opus, &dir.join("again.weba"), &[("bitrate", "32")]).unwrap();
        assert!(!rep.route.contains("no re-encoding"));
        let rep = run(&weba, &dir.join("back4.wav"), &[]).unwrap();
        assert_eq!(rep.input_format, audio::WEBA);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn size_rotation_and_background_options() {
        let dir = tmpdir("opts");
        let src = png_fixture("rgb8.png");
        let (w, h) = {
            let d = std::fs::read(&src).unwrap();
            let (i, _) = oc_image::png::decode(&d, &Limits::default(), &NoProgress).unwrap();
            (i.width, i.height)
        };
        let out = dir.join("o.png");
        let rep = run(&src, &out, &[("width", "10")]).unwrap();
        let d = std::fs::read(&out).unwrap();
        let (i, _) = oc_image::png::decode(&d, &Limits::default(), &NoProgress).unwrap();
        assert_eq!(i.width, 10);
        assert_eq!(
            u64::from(i.height),
            (u64::from(h) * 10 + u64::from(w) / 2) / u64::from(w)
        );
        assert!(rep.notes.iter().any(|n| n.code == "image.resized"));

        run(&src, &out, &[("rotate", "90")]).unwrap();
        let d = std::fs::read(&out).unwrap();
        let (i, _) = oc_image::png::decode(&d, &Limits::default(), &NoProgress).unwrap();
        assert_eq!((i.width, i.height), (h, w));

        // Transparent pixels take the chosen background in a format without alpha.
        let ppm = dir.join("o.ppm");
        run(
            &png_fixture("pal8_trns.png"),
            &ppm,
            &[("background", "#ff0000")],
        )
        .unwrap();
        let got = decode_rgba(&ppm);
        let want = std::fs::read(png_fixture("pal8_trns.rgba")).unwrap();
        let mut saw_red = false;
        for (g, w) in got.as_chunks::<4>().0.iter().zip(want.as_chunks::<4>().0) {
            if w[3] == 0 {
                assert_eq!(*g, [255, 0, 0, 255]);
                saw_red = true;
            }
        }
        assert!(saw_red, "fixture should contain fully transparent pixels");

        assert_eq!(
            run(&src, &out, &[("rotate", "45")]).err().map(|e| e.code),
            Some(ErrorCode::InvalidOptions)
        );
        assert_eq!(
            run(&src, &out, &[("level", "x")]).err().map(|e| e.code),
            Some(ErrorCode::InvalidOptions)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn metadata_loss_is_reported() {
        let dir = tmpdir("meta");
        let out = dir.join("o.bmp");
        // The PNG fixture set includes text/profile chunks only in generated data; build one.
        let (mut img, _) = {
            let d = std::fs::read(png_fixture("rgb8.png")).unwrap();
            oc_image::png::decode(&d, &Limits::default(), &NoProgress).unwrap()
        };
        img.metadata.text.push(("Title".into(), "x".into()));
        let src = dir.join("meta.png");
        std::fs::write(&src, oc_image::png::encode(&img, &Default::default()).0).unwrap();
        let rep = run(&src, &out, &[]).unwrap();
        assert!(rep.notes.has_loss());
        assert!(rep.notes.iter().any(|n| n.code == "image.text_dropped"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn text_cannot_become_an_image() {
        let dir = tmpdir("cross");
        let t = dir.join("a.txt");
        std::fs::write(&t, "hi").unwrap();
        assert_eq!(
            run(&t, &dir.join("a.png"), &[]).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedFormat)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_extension_is_unsupported() {
        let dir = tmpdir("d");
        let r = ConvertRequest {
            input: dir.join("a.xyz"),
            output: dir.join("b.txt"),
            overwrite: false,
            display_name: None,
            options: vec![],
        };
        assert_eq!(
            Engine::new().convert(&r, &NoProgress).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedFormat)
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
