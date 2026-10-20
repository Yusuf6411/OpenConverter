//! Audio formats: registration, option schemas, decoding, the processing chain and encoding.
//!
//! Like images, every audio format connects to a hub (`audio-ir`), so a conversion is always
//! "decode X, process, encode Y".
use oc_audio::dsp::{self, Dither, Quality};
use oc_audio::{aiff, caf, flac, vorbis, wav, Audio, Samples};
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Limits, Note, Notes,
    Ownership, Result, Severity, Sniffer,
};

pub const WAV: FormatId = FormatId("wav");
pub const AIFF: FormatId = FormatId("aiff");
pub const CAF: FormatId = FormatId("caf");
pub const FLAC: FormatId = FormatId("flac");
pub const OGG: FormatId = FormatId("ogg");
pub const AUDIO_IR: FormatId = FormatId("audio-ir");

pub const FORMATS: [FormatId; 5] = [WAV, AIFF, CAF, FLAC, OGG];

pub fn is_audio(id: FormatId) -> bool {
    FORMATS.contains(&id)
}

fn float(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    i18n: &'static str,
    min: f64,
    max: f64,
    default: f64,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        help,
        i18n_key: i18n,
        kind: OptionKind::Float { min, max, default },
    }
}

fn common_options() -> Vec<OptionSpec> {
    vec![
        OptionSpec {
            key: "sample_rate",
            label: "Sample rate",
            help: "Resample to this rate in Hz; 0 keeps the original",
            i18n_key: "opt.audio.sample_rate",
            kind: OptionKind::Int { min: 0, max: 768_000, default: 0 },
        },
        OptionSpec {
            key: "channels",
            label: "Channels",
            help: "Mix to 1 to 8 channels (surround to stereo follows ITU-R BS.775); 0 keeps the original",
            i18n_key: "opt.audio.channels",
            kind: OptionKind::Int { min: 0, max: 8, default: 0 },
        },
        OptionSpec {
            key: "resample_quality",
            label: "Resampling quality",
            help: "Longer filters reject more aliasing and cost more time",
            i18n_key: "opt.audio.resample_quality",
            kind: OptionKind::Enum { choices: vec!["low", "medium", "high", "best"], default: "high" },
        },
        OptionSpec {
            key: "dither",
            label: "Dither",
            help: "Noise added when the bit depth shrinks: none, tpdf, or shaped (tpdf pushed toward high frequencies)",
            i18n_key: "opt.audio.dither",
            kind: OptionKind::Enum { choices: vec!["none", "tpdf", "shaped"], default: "tpdf" },
        },
        float("trim_start", "Start at (seconds)", "Cut everything before this time", "opt.audio.trim_start", 0.0, 1.0e6, 0.0),
        float("trim_end", "End at (seconds)", "Cut everything after this time; 0 keeps the end", "opt.audio.trim_end", 0.0, 1.0e6, 0.0),
        float("trim_silence", "Trim silence below (dB)", "Remove leading and trailing audio quieter than this level, such as -60; 0 turns it off", "opt.audio.trim_silence", -120.0, 0.0, 0.0),
        float("fade_in", "Fade in (seconds)", "Linear fade at the start", "opt.audio.fade_in", 0.0, 600.0, 0.0),
        float("fade_out", "Fade out (seconds)", "Linear fade at the end", "opt.audio.fade_out", 0.0, 600.0, 0.0),
        float("normalize", "Normalise loudness (LUFS)", "Scale to this integrated loudness (ITU-R BS.1770); 0 turns it off. Peaks are never pushed past full scale", "opt.audio.normalize", -70.0, 0.0, 0.0),
        OptionSpec {
            key: "replaygain",
            label: "Write ReplayGain tags",
            help: "Measure loudness and store REPLAYGAIN_TRACK_GAIN and _PEAK where the format has tags",
            i18n_key: "opt.audio.replaygain",
            kind: OptionKind::Bool { default: false },
        },
    ]
}

fn depth_option(choices: Vec<&'static str>) -> OptionSpec {
    OptionSpec {
        key: "bit_depth",
        label: "Bit depth",
        help: "keep stores the source depth (rounded up to one the format has)",
        i18n_key: "opt.audio.bit_depth",
        kind: OptionKind::Enum {
            choices,
            default: "keep",
        },
    }
}

fn schema(extra: Vec<OptionSpec>) -> OptionSchema {
    let mut specs = extra;
    specs.extend(common_options());
    OptionSchema { specs }
}

/// What a writer can store.
struct Caps {
    ints: &'static [u8],
    float: bool,
}

fn caps(id: FormatId) -> Caps {
    match id {
        i if i == WAV => Caps {
            ints: &[8, 16, 24, 32],
            float: true,
        },
        i if i == AIFF || i == CAF => Caps {
            ints: &[8, 16, 24, 32],
            float: true,
        },
        _ => Caps {
            ints: &[8, 12, 16, 20, 24, 32],
            float: false,
        },
    }
}

type Row = (
    FormatId,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    Vec<Sniffer>,
    OptionSchema,
    &'static str,
);

pub fn register(registry: &mut Registry) {
    let rows: Vec<Row> = vec![
        (
            WAV,
            "WAV",
            &["wav", "wave", "bwf", "rf64"],
            &["audio/wav", "audio/x-wav", "audio/wave"],
            vec![Sniffer {
                offset: 8,
                magic: b"WAVE",
                confidence: 100,
            }],
            schema(vec![
                depth_option(vec!["keep", "8", "16", "24", "32", "float"]),
                OptionSpec {
                    key: "encoding",
                    label: "Sample encoding",
                    help: "pcm, or the 8-bit companded a-law / mu-law of telephony",
                    i18n_key: "opt.wav.encoding",
                    kind: OptionKind::Enum {
                        choices: vec!["pcm", "alaw", "mulaw"],
                        default: "pcm",
                    },
                },
            ]),
            "Microsoft RIFF WAVE, RF64 (EBU Tech 3306)",
        ),
        (
            AIFF,
            "AIFF",
            &["aiff", "aif", "aifc"],
            &["audio/aiff", "audio/x-aiff"],
            vec![
                Sniffer {
                    offset: 8,
                    magic: b"AIFF",
                    confidence: 100,
                },
                Sniffer {
                    offset: 8,
                    magic: b"AIFC",
                    confidence: 100,
                },
            ],
            schema(vec![
                depth_option(vec!["keep", "8", "16", "24", "32", "float"]),
                OptionSpec {
                    key: "little_endian",
                    label: "Little-endian samples (AIFF-C sowt)",
                    help: "What Apple software writes by default; older programs expect big-endian",
                    i18n_key: "opt.aiff.little_endian",
                    kind: OptionKind::Bool { default: false },
                },
            ]),
            "Apple AIFF 1.3 and AIFF-C",
        ),
        (
            CAF,
            "CAF",
            &["caf"],
            &["audio/x-caf"],
            vec![Sniffer {
                offset: 0,
                magic: b"caff",
                confidence: 100,
            }],
            schema(vec![depth_option(vec![
                "keep", "8", "16", "24", "32", "float",
            ])]),
            "Apple Core Audio Format",
        ),
        (
            FLAC,
            "FLAC",
            &["flac"],
            &["audio/flac", "audio/x-flac"],
            vec![Sniffer {
                offset: 0,
                magic: b"fLaC",
                confidence: 100,
            }],
            schema(vec![
                depth_option(vec!["keep", "8", "12", "16", "20", "24", "32"]),
                OptionSpec {
                    key: "level",
                    label: "Compression level",
                    help: "0 is fastest, 8 searches hardest; every level is lossless",
                    i18n_key: "opt.flac.level",
                    kind: OptionKind::Int {
                        min: 0,
                        max: 8,
                        default: 5,
                    },
                },
                OptionSpec {
                    key: "block_size",
                    label: "Block size",
                    help: "Samples per frame, 16 to 65535; 0 picks 1152 or 4096 from the level",
                    i18n_key: "opt.flac.block_size",
                    kind: OptionKind::Int {
                        min: 0,
                        max: 65535,
                        default: 0,
                    },
                },
            ]),
            "RFC 9639",
        ),
        (
            OGG,
            "Ogg Vorbis",
            &["ogg", "oga"],
            &["audio/ogg", "audio/vorbis"],
            vec![Sniffer { offset: 0, magic: b"OggS", confidence: 60 }],
            schema(vec![depth_option(vec!["keep"])]),
            "Vorbis I, RFC 3533 Ogg",
        ),
    ];
    for (id, name, extensions, mime, sniffers, writer_options, spec) in rows {
        registry.add_format(FormatDescriptor {
            id,
            name,
            category: Category::Audio,
            extensions,
            mime,
            sniffers,
            capabilities: Capabilities {
                read: true,
                write: true,
                metadata: true,
                streams: true,
                ..Capabilities::default()
            },
            writer_options,
            ownership: Ownership::Own,
            spec,
        });
        registry.add_edge(Edge {
            from: id,
            to: AUDIO_IR,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 2,
            description: "Decode audio",
        });
        if id == OGG {
            continue; // read-only until the encoder lands
        }
        registry.add_edge(Edge {
            from: AUDIO_IR,
            to: id,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 2,
            description: "Encode audio",
        });
    }
}

pub fn decode(fmt: FormatId, data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    match fmt {
        f if f == WAV => wav::read(data, limits),
        f if f == AIFF => aiff::read(data, limits),
        f if f == CAF => caf::read(data, limits),
        f if f == FLAC => flac::read(data, limits),
        f if f == OGG => vorbis::read(data, limits),
        _ => Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "no audio decoder for this format",
        )),
    }
}

/// Apply the processing options in a fixed order: trim, silence, mix, resample, fades,
/// loudness.
pub fn process(mut a: Audio, opts: &Options, limits: &Limits) -> Result<(Audio, Notes)> {
    let mut notes = Notes::new();
    a.validate(limits)?;
    dsp::check_rate(a.sample_rate)?;
    let get = |k: &str| opts.float(k).unwrap_or(0.0);
    if get("trim_start") > 0.0 || get("trim_end") > 0.0 {
        dsp::trim(&mut a, get("trim_start"), get("trim_end"))?;
    }
    if get("trim_silence") < 0.0 {
        let (s, e) = dsp::trim_silence(&mut a, get("trim_silence"));
        if s + e > 0 {
            notes.push(Note::new(
                Severity::Info,
                "audio.silence_trimmed",
                format!("{s} frames were removed from the start and {e} from the end"),
            ));
        }
    }
    let ch = opts.int("channels").unwrap_or(0);
    if ch > 0 && ch as u16 != a.channels {
        let from = a.channels;
        a = dsp::remix(&a, ch as u16, true)?;
        notes.push(Note::new(
            if ch as u16 > from {
                Severity::Info
            } else {
                Severity::Loss
            },
            "audio.channels_mixed",
            format!("{from} channel(s) were mixed to {ch}"),
        ));
    }
    let rate = opts.int("sample_rate").unwrap_or(0) as u32;
    if rate > 0 && rate != a.sample_rate {
        let q = Quality::from_name(opts.text("resample_quality").unwrap_or("high"))
            .unwrap_or(Quality::High);
        let from = a.sample_rate;
        a = dsp::resample(&a, rate, q)?;
        notes.push(Note::new(
            Severity::Info,
            "audio.resampled",
            format!("resampled from {from} Hz to {rate} Hz"),
        ));
    }
    dsp::fade(&mut a, get("fade_in"), get("fade_out"));
    let target = get("normalize");
    let want_rg = opts.bool("replaygain") == Some(true);
    if target < 0.0 || want_rg {
        let l = dsp::loudness(&a);
        if target < 0.0 {
            match l.integrated {
                Some(cur) => {
                    let mut gain = target - cur;
                    // Never push the peak past full scale.
                    let headroom = -l.true_peak_db.max(-200.0);
                    if gain > headroom {
                        gain = headroom;
                        notes.push(Note::new(
                            Severity::Warning,
                            "audio.normalize_limited",
                            "the gain was limited so the peaks stay below full scale",
                        ));
                    }
                    dsp::apply_gain(&mut a, gain);
                    notes.push(Note::new(
                        Severity::Info,
                        "audio.normalized",
                        format!("gain {gain:+.2} dB applied ({cur:.1} LUFS measured)"),
                    ));
                }
                None => notes.push(Note::new(
                    Severity::Warning,
                    "audio.normalize_silent",
                    "the audio is silent; nothing to normalise",
                )),
            }
        }
        if want_rg {
            let l = dsp::loudness(&a);
            if let Some(cur) = l.integrated {
                a.tags
                    .fields
                    .retain(|(k, _)| !k.starts_with("REPLAYGAIN_TRACK"));
                a.tags
                    .add("REPLAYGAIN_TRACK_GAIN", format!("{:+.2} dB", -18.0 - cur));
                a.tags
                    .add("REPLAYGAIN_TRACK_PEAK", format!("{:.6}", l.sample_peak));
                a.tags.add("REPLAYGAIN_REFERENCE_LOUDNESS", "-18.00 LUFS");
            }
        }
    }
    Ok((a, notes))
}

/// Re-express the samples in a format the writer can store.
fn fit_depth(
    a: &mut Audio,
    fmt: FormatId,
    opts: &Options,
    source: (Option<u8>, bool),
    notes: &mut Notes,
) -> Result<()> {
    let cap = caps(fmt);
    let want = opts.text("bit_depth").unwrap_or("keep");
    let (src_bits, src_float) = source;
    let dither = match opts.text("dither").unwrap_or("tpdf") {
        "none" => Dither::None,
        "shaped" => Dither::Shaped,
        _ => Dither::Tpdf,
    };
    let target: Option<u8> = match want {
        "keep" => {
            if src_float && cap.float {
                None
            } else if src_float {
                Some(
                    *cap.ints
                        .iter()
                        .rfind(|&&b| b == 24)
                        .or(cap.ints.last())
                        .unwrap_or(&24),
                )
            } else {
                let b = src_bits.unwrap_or(16);
                Some(
                    cap.ints
                        .iter()
                        .copied()
                        .find(|&c| c >= b)
                        .or(cap.ints.last().copied())
                        .unwrap_or(16),
                )
            }
        }
        "float" => {
            if !cap.float {
                return Err(Error::invalid_option(
                    "bit_depth",
                    "this format cannot store floating-point samples",
                ));
            }
            None
        }
        n => Some(
            n.parse::<u8>()
                .map_err(|_| Error::invalid_option("bit_depth", "unknown bit depth"))?,
        ),
    };
    let was = a.bits();
    let was_float = matches!(a.samples, Samples::Float(_));
    match target {
        None => {
            if !was_float {
                dsp::convert_format(a, None, Dither::None);
            }
        }
        Some(bits) => {
            let same = matches!(a.samples, Samples::Int { bits: b, .. } if b == bits);
            if !same {
                dsp::convert_format(a, Some(bits), dither);
                if was_float || bits < was {
                    notes.push(Note::new(
                        Severity::Info,
                        "audio.bit_depth",
                        format!(
                            "samples were rounded to {bits} bits{}",
                            if dither == Dither::None {
                                ""
                            } else {
                                " with dither"
                            }
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

pub fn encode(
    fmt: FormatId,
    mut a: Audio,
    opts: &Options,
    source: (Option<u8>, bool),
) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    fit_depth(&mut a, fmt, opts, source, &mut notes)?;
    let out = match fmt {
        f if f == WAV => {
            let enc = match opts.text("encoding").unwrap_or("pcm") {
                "alaw" => wav::Encoding::Alaw,
                "mulaw" => wav::Encoding::Mulaw,
                _ if matches!(a.samples, Samples::Float(_)) => wav::Encoding::Float32,
                _ => wav::Encoding::Pcm,
            };
            wav::write(&a, enc, &mut notes)?
        }
        f if f == AIFF => aiff::write(&a, opts.bool("little_endian") == Some(true), &mut notes)?,
        f if f == CAF => caf::write(&a, &mut notes)?,
        f if f == FLAC => {
            let o = flac::Options {
                level: opts.int("level").unwrap_or(5) as u8,
                block_size: match opts.int("block_size").unwrap_or(0) {
                    0 => None,
                    n => Some(n as usize),
                },
                ..flac::Options::default()
            };
            flac::write(&a, &o, &mut notes)?
        }
        _ => {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                "no audio encoder for this format",
            ))
        }
    };
    Ok((out, notes))
}

/// Source depth facts needed later by `encode`, taken before processing turns samples to float.
pub fn source_depth(a: &Audio) -> (Option<u8>, bool) {
    match &a.samples {
        Samples::Int { bits, .. } => (Some(*bits), false),
        Samples::Float(_) => (None, true),
    }
}
