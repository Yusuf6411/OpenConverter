//! Audio formats: registration, option schemas, decoding, the processing chain and encoding.
//!
//! Like images, every audio format connects to a hub (`audio-ir`), so a conversion is always
//! "decode X, process, encode Y".
use oc_audio::dsp::{self, Dither, Quality};
use oc_audio::{aiff, caf, flac, m4a, mka, mp3, opus, vorbis, wav, Audio, Samples};
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
pub const MP3: FormatId = FormatId("mp3");
pub const M4A: FormatId = FormatId("m4a");
pub const OPUS: FormatId = FormatId("opus");
pub const MKA: FormatId = FormatId("mka");
pub const WEBA: FormatId = FormatId("weba");
pub const AUDIO_IR: FormatId = FormatId("audio-ir");

pub const FORMATS: [FormatId; 10] = [WAV, AIFF, CAF, FLAC, OGG, MP3, M4A, OPUS, MKA, WEBA];

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
        // A lossy codec takes whatever the processing chain produced.
        i if i == OGG || i == MP3 || i == OPUS || i == WEBA => Caps {
            ints: &[16, 24, 32],
            float: true,
        },
        i if i == M4A => Caps {
            ints: &[16, 20, 24, 32],
            float: false,
        },
        _ => Caps {
            ints: &[8, 12, 16, 20, 24, 32],
            float: false,
        },
    }
}

fn opus_specs() -> Vec<OptionSpec> {
    vec![
                OptionSpec {
                    key: "bitrate",
                    label: "Bit rate (kbit/s)",
                    help: "Target rate for the whole file, 6 to 510; 0 picks 64 for mono, 96 for stereo and 48 per channel beyond",
                    i18n_key: "opt.opus.bitrate",
                    kind: OptionKind::Int { min: 0, max: 510, default: 0 },
                },
                OptionSpec {
                    key: "rate_mode",
                    label: "Rate control",
                    help: "cbr keeps every packet the same size, vbr follows the signal, cvbr is vbr with a small reservoir",
                    i18n_key: "opt.opus.rate_mode",
                    kind: OptionKind::Enum { choices: vec!["cbr", "vbr", "cvbr"], default: "vbr" },
                },
                OptionSpec {
                    key: "frame_ms",
                    label: "Frame duration (ms)",
                    help: "Shorter frames lower the delay and cost efficiency",
                    i18n_key: "opt.opus.frame_ms",
                    kind: OptionKind::Enum { choices: vec!["2.5", "5", "10", "20"], default: "20" },
                },
            ]
}

fn flac_specs() -> Vec<OptionSpec> {
    vec![OptionSpec {
        key: "level",
        label: "FLAC compression level",
        help: "0 is fastest, 8 searches hardest; every level is lossless",
        i18n_key: "opt.flac.level",
        kind: OptionKind::Int {
            min: 0,
            max: 8,
            default: 5,
        },
    }]
}

fn mka_specs(webm: bool) -> Vec<OptionSpec> {
    let mut v = vec![
        depth_option(vec!["keep", "8", "12", "16", "20", "24", "32"]),
        OptionSpec {
            key: "codec",
            label: "Codec",
            help: if webm {
                "auto keeps the source codec when it fits (no re-encoding) and otherwise uses opus; WebM holds opus or vorbis only"
            } else {
                "auto keeps the source codec when it fits (no re-encoding) and otherwise uses flac; pcm and flac are lossless, opus and vorbis lossy"
            },
            i18n_key: "opt.mka.codec",
            kind: OptionKind::Enum {
                choices: if webm {
                    vec!["opus", "vorbis"]
                } else {
                    vec!["flac", "opus", "vorbis", "pcm"]
                },
                default: if webm { "opus" } else { "flac" },
            },
        },
        float(
            "quality",
            "Vorbis quality",
            "-1 is smallest, 10 is best; used when the codec is vorbis",
            "opt.vorbis.quality",
            -1.0,
            10.0,
            4.0,
        ),
    ];
    v.extend(opus_specs());
    if !webm {
        v.extend(flac_specs());
    }
    v
}

/// The codec an MKA or WebM output will use.
fn mka_codec(fmt: FormatId, opts: &Options) -> mka::Codec {
    opts.text("codec")
        .and_then(mka::Codec::from_name) // "auto" parses to none: the container's default
        .unwrap_or(if fmt == WEBA {
            mka::Codec::Opus
        } else {
            mka::Codec::Flac
        })
}

/// Move the compressed audio to the target container as it is, when that is possible and the
/// options ask for nothing that would change the sound.
pub fn try_copy(
    from: FormatId,
    to: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Option<Vec<u8>>> {
    use oc_audio::copy::{Container, Kind};
    let container = |f: FormatId| match f {
        f if f == OPUS => Some(Container::OggOpus),
        f if f == OGG => Some(Container::OggVorbis),
        f if f == FLAC => Some(Container::FlacFile),
        f if f == MKA => Some(Container::Matroska),
        f if f == WEBA => Some(Container::WebM),
        _ => None,
    };
    let (Some(src), Some(dst)) = (container(from), container(to)) else {
        return Ok(None);
    };
    if from == to {
        return Ok(None);
    }
    let get = |k: &str| opts.float(k).unwrap_or(0.0);
    let untouched = [
        "trim_start",
        "trim_end",
        "trim_silence",
        "fade_in",
        "fade_out",
        "normalize",
    ]
    .iter()
    .all(|k| get(k) == 0.0)
        && opts.int("sample_rate").unwrap_or(0) == 0
        && opts.int("channels").unwrap_or(0) == 0
        && opts.bool("replaygain") != Some(true)
        && opts.text("bit_depth").unwrap_or("keep") == "keep";
    if !untouched {
        return Ok(None);
    }
    let tuned = opts.int("bitrate").unwrap_or(0) != 0
        || opts.text("rate_mode").unwrap_or("vbr") != "vbr"
        || opts.text("frame_ms").unwrap_or("20") != "20"
        || (opts.float("quality").unwrap_or(4.0) - 4.0).abs() > f64::EPSILON;
    let only = match opts.text("codec").unwrap_or("auto") {
        "opus" => Some(Kind::Opus),
        "vorbis" => Some(Kind::Vorbis),
        "flac" => Some(Kind::Flac),
        "auto" => None,
        _ => return Ok(None),
    };
    // Tuned encoder settings ask for a lossy re-encode; only a FLAC source never needs one.
    if tuned && from != FLAC {
        return Ok(None);
    }
    oc_audio::copy::copy(data, src, dst, only, limits, notes)
}

/// True when the writer lowers quality by itself, so any depth or float input will do.
fn lossy(fmt: FormatId, opts: &Options) -> bool {
    fmt == OGG
        || fmt == MP3
        || fmt == OPUS
        || ((fmt == MKA || fmt == WEBA)
            && matches!(mka_codec(fmt, opts), mka::Codec::Opus | mka::Codec::Vorbis))
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
            schema(vec![
                depth_option(vec!["keep", "8", "16", "24", "32", "float"]),
                OptionSpec {
                    key: "codec",
                    label: "Codec",
                    help: "pcm stores samples as they are; alac compresses them without loss (the bit depth becomes 16, 20, 24 or 32)",
                    i18n_key: "opt.caf.codec",
                    kind: OptionKind::Enum { choices: vec!["pcm", "alac"], default: "pcm" },
                },
            ]),
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
            vec![Sniffer {
                offset: 0,
                magic: b"OggS",
                confidence: 60,
            }],
            schema(vec![float(
                "quality",
                "Quality",
                "-1 is smallest, 10 is best; 4 is about 128 kbit/s for stereo music",
                "opt.vorbis.quality",
                -1.0,
                10.0,
                4.0,
            )]),
            "Vorbis I, RFC 3533 Ogg",
        ),
        (
            MP3,
            "MP3",
            &["mp3", "mp2"],
            &["audio/mpeg", "audio/mp3"],
            vec![
                Sniffer {
                    offset: 0,
                    magic: b"ID3",
                    confidence: 55,
                },
                Sniffer {
                    offset: 0,
                    magic: &[0xFF, 0xFB],
                    confidence: 45,
                },
                Sniffer {
                    offset: 0,
                    magic: &[0xFF, 0xFA],
                    confidence: 45,
                },
                Sniffer {
                    offset: 0,
                    magic: &[0xFF, 0xF3],
                    confidence: 45,
                },
                Sniffer {
                    offset: 0,
                    magic: &[0xFF, 0xF2],
                    confidence: 45,
                },
                Sniffer {
                    offset: 0,
                    magic: &[0xFF, 0xE3],
                    confidence: 40,
                },
                Sniffer {
                    offset: 0,
                    magic: &[0xFF, 0xE2],
                    confidence: 40,
                },
            ],
            schema(vec![
                OptionSpec {
                    key: "bitrate",
                    label: "Bit rate (kbit/s)",
                    help: "Constant bit rate; the nearest rate the format has is used. Ignored when a VBR quality is set",
                    i18n_key: "opt.mp3.bitrate",
                    kind: OptionKind::Int { min: 8, max: 320, default: 128 },
                },
                float(
                    "vbr_quality",
                    "VBR quality",
                    "0 (best) to 9 (smallest) for variable bit rate; -1 uses the constant bit rate instead",
                    "opt.mp3.vbr_quality",
                    -1.0,
                    9.0,
                    -1.0,
                ),
                OptionSpec {
                    key: "joint_stereo",
                    label: "Joint stereo",
                    help: "Let the encoder code stereo frames as mid and side where that is smaller",
                    i18n_key: "opt.mp3.joint_stereo",
                    kind: OptionKind::Bool { default: true },
                },
            ]),
            "ISO/IEC 11172-3 and 13818-3 layer III",
        ),
        (
            M4A,
            "M4A (Apple Lossless)",
            &["m4a", "m4b"],
            &["audio/mp4", "audio/x-m4a"],
            vec![Sniffer {
                offset: 4,
                magic: b"ftyp",
                confidence: 50,
            }],
            schema(vec![depth_option(vec!["keep", "16", "20", "24", "32"])]),
            "ISO/IEC 14496-12 with Apple Lossless",
        ),
        (
            OPUS,
            "Opus",
            &["opus"],
            &["audio/opus", "audio/ogg; codecs=opus"],
            vec![Sniffer {
                offset: 0,
                magic: b"OggS",
                confidence: 60,
            }],
            schema(opus_specs()),
            "RFC 6716 (CELT), RFC 7845 Ogg encapsulation",
        ),
        (
            MKA,
            "Matroska Audio",
            &["mka"],
            &["audio/x-matroska"],
            vec![Sniffer {
                offset: 0,
                magic: &[0x1A, 0x45, 0xDF, 0xA3],
                confidence: 50,
            }],
            schema(mka_specs(false)),
            "RFC 8794 (EBML), RFC 9559 (Matroska)",
        ),
        (
            WEBA,
            "WebM Audio",
            &["weba"],
            &["audio/webm"],
            vec![Sniffer {
                offset: 0,
                magic: &[0x1A, 0x45, 0xDF, 0xA3],
                confidence: 50,
            }],
            schema(mka_specs(true)),
            "WebM container (Matroska subset), Opus or Vorbis",
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
        f if f == MP3 => mp3::read(data, limits),
        f if f == M4A => m4a::read(data, limits),
        f if f == OPUS => opus::read(data, limits),
        f if f == MKA || f == WEBA => mka::read(data, limits),
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
    let cap = if fmt == CAF && opts.text("codec") == Some("alac") {
        Caps {
            ints: &[16, 20, 24, 32],
            float: false,
        }
    } else if fmt == MKA || fmt == WEBA {
        match mka_codec(fmt, opts) {
            mka::Codec::Flac => Caps {
                ints: &[8, 12, 16, 20, 24, 32],
                float: false,
            },
            mka::Codec::Pcm => Caps {
                ints: &[8, 16, 24, 32],
                float: true,
            },
            _ => Caps {
                ints: &[16, 24, 32],
                float: true,
            },
        }
    } else {
        caps(fmt)
    };
    let want = opts.text("bit_depth").unwrap_or("keep");
    let (src_bits, src_float) = source;
    let dither = match opts.text("dither").unwrap_or("tpdf") {
        "none" => Dither::None,
        "shaped" => Dither::Shaped,
        _ => Dither::Tpdf,
    };
    let target: Option<u8> = match want {
        "keep" => {
            if (src_float || lossy(fmt, opts)) && cap.float {
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
        f if f == CAF => {
            if opts.text("codec") == Some("alac") {
                caf::write_alac(&a, &mut notes)?
            } else {
                caf::write(&a, &mut notes)?
            }
        }
        f if f == M4A => m4a::write(&a, &mut notes)?,
        f if f == MP3 => {
            let q = opts.float("vbr_quality").unwrap_or(-1.0);
            let o = mp3::Options {
                bitrate: opts.int("bitrate").unwrap_or(128) as u32,
                vbr: (q >= 0.0).then_some(q as f32),
                joint_stereo: opts.bool("joint_stereo") != Some(false),
                id3: true,
            };
            mp3::write(&a, &o, &mut notes)?
        }
        f if f == OGG => {
            let o = vorbis::Options {
                quality: opts.float("quality").unwrap_or(4.0) as f32,
                ..vorbis::Options::default()
            };
            vorbis::write(&a, &o, &mut notes)?
        }
        f if f == OPUS => {
            let o = opus::Options {
                bitrate: (opts.int("bitrate").unwrap_or(0).max(0) as u32) * 1000,
                mode: opus::RateMode::from_name(opts.text("rate_mode").unwrap_or("vbr"))
                    .unwrap_or(opus::RateMode::Vbr),
                frame_ms: opts
                    .text("frame_ms")
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(20.0),
                ..opus::Options::default()
            };
            opus::write(&a, &o, &mut notes)?
        }
        f if f == MKA || f == WEBA => {
            let o = mka::Options {
                codec: Some(mka_codec(f, opts)),
                webm: f == WEBA,
                opus: opus::Options {
                    bitrate: (opts.int("bitrate").unwrap_or(0).max(0) as u32) * 1000,
                    mode: opus::RateMode::from_name(opts.text("rate_mode").unwrap_or("vbr"))
                        .unwrap_or(opus::RateMode::Vbr),
                    frame_ms: opts
                        .text("frame_ms")
                        .and_then(|t| t.parse().ok())
                        .unwrap_or(20.0),
                    ..opus::Options::default()
                },
                flac: flac::Options {
                    level: opts.int("level").unwrap_or(5) as u8,
                    ..flac::Options::default()
                },
                vorbis: vorbis::Options {
                    quality: opts.float("quality").unwrap_or(4.0) as f32,
                    ..vorbis::Options::default()
                },
            };
            mka::write(&a, &o, &mut notes)?
        }
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

pub(crate) fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub(crate) fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.4}")
    } else {
        "null".into()
    }
}

/// What the web preview shows for an audio file, as JSON:
/// `{"sample_rate","channels","frames","duration","bits","float","peaks":[[min,max]...],
/// "loudness":{"integrated","range","sample_peak_db","true_peak_db"},"tags":[[key,value]...]}`.
/// Peaks cover all channels together, scaled to -1..1, `buckets` of them across the clip.
pub fn analyse_json(fmt: FormatId, data: &[u8], buckets: usize, limits: &Limits) -> Result<String> {
    let (a, _) = decode(fmt, data, limits)?;
    a.validate(limits)?;
    let ch = usize::from(a.channels.max(1));
    let pcm = dsp::to_float(&a.samples);
    let frames = pcm.len() / ch;
    let buckets = buckets.clamp(16, 4000).min(frames.max(1));
    let mut peaks = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let (lo, hi) = (b * frames / buckets, (b + 1) * frames / buckets);
        let (mut mn, mut mx) = (0f32, 0f32);
        for v in pcm.get(lo * ch..hi * ch).unwrap_or_default() {
            mn = mn.min(*v);
            mx = mx.max(*v);
        }
        peaks.push(format!("[{mn:.4},{mx:.4}]"));
    }
    let l = dsp::loudness(&a);
    let opt = |v: Option<f64>| v.map_or("null".to_string(), num);
    let peak_db = if l.sample_peak > 0.0 {
        20.0 * f64::from(l.sample_peak).log10()
    } else {
        f64::NEG_INFINITY
    };
    let (bits, float) = match &a.samples {
        Samples::Int { bits, .. } => (bits.to_string(), false),
        Samples::Float(_) => ("null".to_string(), true),
    };
    let tags: Vec<String> = a
        .tags
        .fields
        .iter()
        .take(64)
        .map(|(k, v)| {
            format!(
                "[{},{}]",
                json_escape(k),
                json_escape(&v.chars().take(500).collect::<String>())
            )
        })
        .collect();
    Ok(format!(
        "{{\"sample_rate\":{},\"channels\":{},\"frames\":{},\"duration\":{},\"bits\":{},\"float\":{},\"peaks\":[{}],\"loudness\":{{\"integrated\":{},\"range\":{},\"sample_peak_db\":{},\"true_peak_db\":{}}},\"tags\":[{}]}}",
        a.sample_rate,
        a.channels,
        frames,
        num(a.duration_secs()),
        bits,
        float,
        peaks.join(","),
        opt(l.integrated),
        opt(l.range),
        num(peak_db),
        num(l.true_peak_db),
        tags.join(",")
    ))
}

/// A 16-bit WAV of the first `secs` seconds, for browsers that cannot play the original.
pub fn preview_wav(fmt: FormatId, data: &[u8], secs: f64, limits: &Limits) -> Result<Vec<u8>> {
    let (mut a, _) = decode(fmt, data, limits)?;
    a.validate(limits)?;
    if a.duration_secs() > secs {
        dsp::trim(&mut a, 0.0, secs)?;
    }
    let mut notes = Notes::new();
    let opts_src = vec![(
        "bit_depth".to_string(),
        oc_core::options::OptionValue::Text("16".into()),
    )];
    let schema = OptionSchema {
        specs: vec![depth_option(vec!["keep", "16"])],
    };
    let opts = schema.validate(&opts_src)?;
    let source = source_depth(&a);
    fit_depth(&mut a, WAV, &opts, source, &mut notes)?;
    wav::write(&a, wav::Encoding::Pcm, &mut notes)
}
