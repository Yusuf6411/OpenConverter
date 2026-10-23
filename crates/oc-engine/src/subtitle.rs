//! Subtitle formats: SRT, WebVTT, ASS/SSA and TTML, converted through one cue list.
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, FormatDescriptor, FormatId, Note, Notes, Ownership, Result,
    Severity,
};
use oc_subtitle::{ass, srt, ttml, vtt, Track};
use oc_text::{Encoding, Policy};

pub const SRT: FormatId = FormatId("srt");
pub const VTT: FormatId = FormatId("vtt");
pub const ASS: FormatId = FormatId("ass");
pub const TTML: FormatId = FormatId("ttml");
pub const SUBTITLE_IR: FormatId = FormatId("subtitle-ir");

pub const FORMATS: [FormatId; 4] = [SRT, VTT, ASS, TTML];

pub fn is_subtitle(id: FormatId) -> bool {
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

fn options_for(id: FormatId) -> OptionSchema {
    let mut specs = vec![
        spec(
            "from_encoding",
            "opt.subtitle.from_encoding",
            "Input encoding",
            "auto detects the byte-order mark and UTF-8, then falls back to windows-1252",
            OptionKind::Text {
                default: "auto".into(),
            },
        ),
        spec(
            "shift_ms",
            "opt.subtitle.shift_ms",
            "Shift (ms)",
            "Move every cue later (positive) or earlier (negative)",
            OptionKind::Int {
                min: -86_400_000,
                max: 86_400_000,
                default: 0,
            },
        ),
        spec(
            "fps_from",
            "opt.subtitle.fps_from",
            "Retime from frame rate",
            "With the option below, rescales times made for one frame rate to another; 0 is off",
            OptionKind::Float {
                min: 0.0,
                max: 240.0,
                default: 0.0,
            },
        ),
        spec(
            "fps_to",
            "opt.subtitle.fps_to",
            "Retime to frame rate",
            "The frame rate of the video the subtitles will be shown with; 0 is off",
            OptionKind::Float {
                min: 0.0,
                max: 240.0,
                default: 0.0,
            },
        ),
        spec(
            "strip_markup",
            "opt.subtitle.strip_markup",
            "Remove styling",
            "Drop bold, italic, underline and colour from the text",
            OptionKind::Bool { default: false },
        ),
    ];
    if id == SRT || id == ASS {
        specs.push(spec(
            "to_encoding",
            "opt.subtitle.to_encoding",
            "Output encoding",
            "utf-8 unless a player needs a legacy encoding",
            OptionKind::Text {
                default: "utf-8".into(),
            },
        ));
        specs.push(spec(
            "bom",
            "opt.subtitle.bom",
            "Write UTF-8 byte-order mark",
            "Some older players need it",
            OptionKind::Bool { default: false },
        ));
        specs.push(spec(
            "newline",
            "opt.subtitle.newline",
            "Line endings",
            "lf or crlf",
            OptionKind::Enum {
                choices: vec!["lf", "crlf"],
                default: if id == SRT { "crlf" } else { "lf" },
            },
        ));
    }
    OptionSchema { specs }
}

pub fn register(registry: &mut Registry) {
    let rows = [
        (
            SRT,
            "SubRip",
            &["srt"][..],
            &["application/x-subrip"][..],
            "SubRip text with <b> <i> <u> <font>",
        ),
        (
            VTT,
            "WebVTT",
            &["vtt"][..],
            &["text/vtt"][..],
            "W3C WebVTT (cues, settings, basic tags)",
        ),
        (
            ASS,
            "ASS/SSA",
            &["ass", "ssa"][..],
            &["text/x-ssa"][..],
            "Advanced SubStation Alpha v4+ (dialogue, bold/italic/colour, \\an)",
        ),
        (
            TTML,
            "TTML",
            &["ttml", "dfxp"][..],
            &["application/ttml+xml"][..],
            "W3C TTML1 (timed paragraphs, spans, simple alignment)",
        ),
    ];
    for (id, name, extensions, mime, spec) in rows {
        registry.add_format(FormatDescriptor {
            id,
            name,
            category: Category::Subtitle,
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
            to: SUBTITLE_IR,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 1,
            description: "Parse subtitle cues",
        });
        registry.add_edge(Edge {
            from: SUBTITLE_IR,
            to: id,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 1,
            description: "Write subtitle cues",
        });
    }
}

/// Identify a subtitle file by its decoded content.
pub fn detect(data: &[u8]) -> Option<FormatId> {
    let found = oc_text::detect(data);
    let (text, _) = oc_text::decode(
        data.get(found.bom_len..)?,
        if found.certain {
            found.encoding
        } else {
            Encoding::Latin1
        },
        Policy::Replace,
    )
    .ok()?;
    match oc_subtitle::sniff(&text)? {
        "vtt" => Some(VTT),
        "ass" => Some(ASS),
        "ttml" => Some(TTML),
        "srt" => Some(SRT),
        _ => None,
    }
}

fn decode(data: &[u8], opts: &Options, notes: &mut Notes) -> Result<String> {
    let name = opts.text("from_encoding").unwrap_or("auto");
    let (enc, skip) = if name == "auto" {
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
        let e = Encoding::from_name(name).ok_or_else(|| {
            Error::invalid_option("from_encoding", format!("unknown encoding '{name}'"))
        })?;
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
    let body = data.get(skip..).unwrap_or_default();
    let (text, n) = match oc_text::decode(body, enc, Policy::Strict) {
        Err(_) if enc == Encoding::Windows1252 => {
            oc_text::decode(body, Encoding::Latin1, Policy::Strict)?
        }
        other => other?,
    };
    notes.extend(n);
    Ok(text)
}

pub fn parse(from: FormatId, text: &str, notes: &mut Notes) -> Track {
    match from {
        f if f == VTT => vtt::parse(text, notes),
        f if f == ASS => ass::parse(text, notes),
        f if f == TTML => ttml::parse(text, notes),
        _ => srt::parse(text, notes),
    }
}

pub fn convert(
    from: FormatId,
    to: FormatId,
    data: &[u8],
    opts: &Options,
) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let text = decode(data, opts, &mut notes)?;
    let track = parse(from, &text, &mut notes);
    let out = finish(track, from, to, opts, &mut notes)?;
    Ok((out, notes))
}

/// Apply the timing options to `track` and write it as `to`.
pub fn finish(
    mut track: Track,
    from: FormatId,
    to: FormatId,
    opts: &Options,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let (ff, ft) = (
        opts.float("fps_from").unwrap_or(0.0),
        opts.float("fps_to").unwrap_or(0.0),
    );
    if ff > 0.0 && ft > 0.0 && (ff - ft).abs() > f64::EPSILON {
        track.scale(ff / ft);
    }
    let shift = opts.int("shift_ms").unwrap_or(0);
    if shift != 0 {
        track.shift(shift, notes);
    }
    if opts.bool("strip_markup") == Some(true) {
        track.strip_markup();
    }
    track.tidy(notes);
    let mut out = match to {
        t if t == VTT => vtt::write(&track, notes),
        t if t == ASS => ass::write(&track, notes),
        t if t == TTML => ttml::write(&track, notes),
        _ => srt::write(&track, notes),
    };
    if from == ASS && to != ASS && track.ass_header.is_some() {
        notes.push(Note::new(
            Severity::Info,
            "subtitle.styles_dropped",
            "ASS styles, fonts and positions are not carried over; text, emphasis and colour are",
        ));
    }
    if opts.text("newline") == Some("crlf") {
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    let bytes = if to == SRT || to == ASS {
        let name = opts.text("to_encoding").unwrap_or("utf-8");
        let enc = Encoding::from_name(name).ok_or_else(|| {
            Error::invalid_option("to_encoding", format!("unknown encoding '{name}'"))
        })?;
        let (body, n) = oc_text::encode(&out, enc, Policy::Strict)?;
        notes.extend(n);
        let mut v = Vec::with_capacity(body.len() + 4);
        if opts.bool("bom") == Some(true) {
            v.extend_from_slice(oc_text::bom(enc));
        }
        v.extend_from_slice(&body);
        v
    } else {
        out.into_bytes()
    };
    Ok(bytes)
}
