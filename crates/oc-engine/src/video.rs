//! Video container formats: registration, option schemas and the remux/transcode planner.
//!
//! Every container connects to a hub (`media-ir`). A conversion reads the container into a
//! packet-level [`Media`], decides per stream whether to copy it or re-encode it, and writes the
//! target container.
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Limits, Note, Notes,
    Ownership, Progress, Result, Severity, Sniffer,
};
use oc_image::anim::Animation;
use oc_video::frames;
use oc_video::pic::Picture;
use oc_video::{avi, codec, flv, mkv, mp4, ogv, ps, subs, ts, y4m, Kind, Media, Packet};

pub const MP4: FormatId = FormatId("mp4");
pub const MOV: FormatId = FormatId("mov");
pub const MKV: FormatId = FormatId("mkv");
pub const WEBM: FormatId = FormatId("webm");
pub const AVI: FormatId = FormatId("avi");
pub const Y4M: FormatId = FormatId("y4m");
pub const TS: FormatId = FormatId("ts");
pub const FLV: FormatId = FormatId("flv");
pub const OGV: FormatId = FormatId("ogv");
pub const PS: FormatId = FormatId("mpeg-ps");
/// A ZIP of numbered picture files, one per video frame.
pub const FRAMES: FormatId = FormatId("frames.zip");
pub const MEDIA_IR: FormatId = FormatId("media-ir");

pub const FORMATS: [FormatId; 10] = [MP4, MOV, MKV, WEBM, AVI, Y4M, TS, FLV, PS, OGV];

fn frames_options() -> OptionSchema {
    let keep = [
        "fps",
        "width",
        "height",
        "rotate",
        "flip",
        "crop_w",
        "crop_h",
        "crop_x",
        "crop_y",
        "start_ms",
        "duration_ms",
    ];
    let mut specs: Vec<OptionSpec> = options_for(MKV)
        .specs
        .into_iter()
        .filter(|s| keep.contains(&s.key))
        .collect();
    specs.push(spec(
        "frame_format",
        "opt.video.frame_format",
        "Picture format",
        "File type of each frame",
        OptionKind::Enum {
            choices: vec!["png", "jpeg"],
            default: "png",
        },
    ));
    specs.push(int(
        "quality",
        "opt.video.frame_quality",
        "JPEG quality",
        "1 (smallest) to 100 (best) for JPEG frames",
        1,
        100,
        90,
    ));
    OptionSchema { specs }
}

/// Containers this build only reads.
fn read_only(id: FormatId) -> bool {
    id == FLV || id == PS || id == OGV
}

pub fn is_video(id: FormatId) -> bool {
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

fn int(
    key: &'static str,
    i18n: &'static str,
    label: &'static str,
    help: &'static str,
    min: i64,
    max: i64,
    default: i64,
) -> OptionSpec {
    spec(
        key,
        i18n,
        label,
        help,
        OptionKind::Int { min, max, default },
    )
}

fn options_for(id: FormatId) -> OptionSchema {
    let video_codecs = match id {
        i if i == Y4M => vec!["auto"],
        i if i == TS => vec!["auto", "copy"],
        i if i == AVI => vec!["auto", "copy", "mjpeg", "vp8"],
        _ => vec!["auto", "copy", "vp8", "mjpeg"],
    };
    let mut specs = vec![
        spec(
            "video_codec",
            "opt.video.video_codec",
            "Video codec",
            "auto copies the picture unchanged when the container can hold it and nothing is being edited, and otherwise encodes",
            OptionKind::Enum { choices: video_codecs, default: "auto" },
        ),
        int("quality", "opt.video.quality", "Video quality", "1 (smallest) to 100 (best) for encoded video", 1, 100, 80),
        int("bitrate_kbps", "opt.video.bitrate", "Bit rate (kbit/s)", "Aim for this bit rate; 0 codes at the constant quality set above", 0, 200_000, 0),
        int("effort", "opt.video.effort", "Encoder effort", "0 (fastest) to 6 (smallest output) for VP8", 0, 6, 4),
        int("threads", "opt.video.threads", "Encoder threads", "0 picks from the machine", 0, 64, 0),
        int("keyframe_interval", "opt.video.keyframe_interval", "Key frame every (frames)", "Seeking granularity of encoded video", 1, 1000, 120),
        int("width", "opt.video.width", "Width", "Scale to this width in pixels; 0 keeps the original (with only one of width and height set, the aspect ratio is kept)", 0, 16384, 0),
        int("height", "opt.video.height", "Height", "Scale to this height in pixels; 0 keeps the original", 0, 16384, 0),
        spec(
            "fps",
            "opt.video.fps",
            "Frame rate",
            "Frames per second; 0 keeps the original",
            OptionKind::Float { min: 0.0, max: 240.0, default: 0.0 },
        ),
        spec(
            "pad",
            "opt.video.pad",
            "Keep proportions (add bars)",
            "With both width and height set, fit the picture inside that size and fill the rest with black instead of stretching it",
            OptionKind::Bool { default: false },
        ),
        spec(
            "deinterlace",
            "opt.video.deinterlace",
            "Remove interlacing",
            "Replace every second line by the average of its neighbours, which removes combing from interlaced footage",
            OptionKind::Bool { default: false },
        ),
        int("audio_offset_ms", "opt.video.audio_offset_ms", "Audio delay (ms)", "Move the sound later (positive) or earlier (negative) against the picture to fix lip-sync", -10_000, 10_000, 0),
        spec(
            "rotate",
            "opt.video.rotate",
            "Rotate",
            "Degrees clockwise",
            OptionKind::Enum { choices: vec!["0", "90", "180", "270"], default: "0" },
        ),
        spec(
            "flip",
            "opt.video.flip",
            "Mirror",
            "Flip the picture",
            OptionKind::Enum { choices: vec!["none", "horizontal", "vertical"], default: "none" },
        ),
        int("crop_w", "opt.video.crop_w", "Crop width", "Width of the part to keep; 0 turns cropping off", 0, 16384, 0),
        int("crop_h", "opt.video.crop_h", "Crop height", "Height of the part to keep", 0, 16384, 0),
        int("crop_x", "opt.video.crop_x", "Crop left", "Distance of the kept part from the left edge", 0, 16384, 0),
        int("crop_y", "opt.video.crop_y", "Crop top", "Distance of the kept part from the top edge", 0, 16384, 0),
        int("start_ms", "opt.video.start_ms", "Start (ms)", "Leave out everything before this time", 0, 86_400_000, 0),
        int("duration_ms", "opt.video.duration_ms", "Length (ms)", "Keep only this much; 0 keeps the rest", 0, 86_400_000, 0),
        spec(
            "audio_codec",
            "opt.video.audio_codec",
            "Audio codec",
            "auto copies the audio unchanged when the container can hold it, and otherwise converts it",
            OptionKind::Enum {
                choices: if id == Y4M { vec!["auto"] } else if id == TS { vec!["auto", "copy"] } else { vec!["auto", "copy", "opus", "vorbis", "flac", "pcm"] },
                default: "auto",
            },
        ),
        spec(
            "streams",
            "opt.video.streams",
            "Streams to keep",
            "Comma-separated stream numbers from the streams list; empty keeps every stream",
            OptionKind::Text { default: String::new() },
        ),
        spec(
            "drop_audio",
            "opt.video.drop_audio",
            "Remove audio",
            "Write the picture only",
            OptionKind::Bool { default: false },
        ),
        spec(
            "drop_subtitles",
            "opt.video.drop_subtitles",
            "Remove subtitles",
            "Leave out subtitle tracks",
            OptionKind::Bool { default: false },
        ),
        spec(
            "burn_subtitles",
            "opt.video.burn_subtitles",
            "Burn in subtitles",
            "Draw the first text subtitle track onto the picture (re-encodes the video)",
            OptionKind::Bool { default: false },
        ),
    ];
    if id == MP4 {
        specs.push(spec(
            "fragmented",
            "opt.video.fragmented",
            "Fragmented MP4",
            "Write the media as short fragments after an empty index, the form streaming players and live tools expect",
            OptionKind::Bool { default: false },
        ));
    }
    if id == MP4 || id == MOV {
        specs.push(spec(
            "faststart",
            "opt.video.faststart",
            "Index first (fast start)",
            "Put the index in front of the media data so playback can begin before the download ends",
            OptionKind::Bool { default: true },
        ));
    }
    OptionSchema { specs }
}

type Row = (
    FormatId,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    Vec<Sniffer>,
    &'static str,
);

pub fn register(registry: &mut Registry) {
    let rows: Vec<Row> =
        vec![
        (
            MP4,
            "MP4",
            &["mp4", "m4v"],
            &["video/mp4"],
            vec![Sniffer { offset: 4, magic: b"ftyp", confidence: 60 }],
            "ISO/IEC 14496-12 and 14496-14 (stream copy; VP8/VP9/AV1/MJPEG, ProRes in MOV, AAC, MP3, Opus, FLAC)",
        ),
        (
            MOV,
            "QuickTime MOV",
            &["mov", "qt"],
            &["video/quicktime"],
            vec![Sniffer { offset: 4, magic: b"ftypqt  ", confidence: 90 }],
            "QuickTime File Format (ISO base media subset)",
        ),
        (
            MKV,
            "Matroska",
            &["mkv", "mk3d"],
            &["video/x-matroska"],
            vec![Sniffer { offset: 0, magic: &[0x1A, 0x45, 0xDF, 0xA3], confidence: 50 }],
            "Matroska/EBML (RFC 8794, RFC 9559)",
        ),
        (
            AVI,
            "AVI",
            &["avi"],
            &["video/x-msvideo"],
            vec![Sniffer { offset: 8, magic: b"AVI ", confidence: 100 }],
            "Microsoft AVI (RIFF; Motion JPEG, MPEG-4 part 2, VP8; PCM, MP3, AC-3)",
        ),
        (
            Y4M,
            "YUV4MPEG2",
            &["y4m", "yuv4mpeg"],
            &["video/x-yuv4mpeg"],
            vec![Sniffer { offset: 0, magic: b"YUV4MPEG2", confidence: 100 }],
            "YUV4MPEG2 (8-bit 4:2:0, 4:2:2, 4:4:4 and mono)",
        ),
        (
            TS,
            "MPEG transport stream",
            &["ts", "m2ts", "mts"],
            &["video/mp2t"],
            vec![],
            "ISO/IEC 13818-1 (stream copy; H.264/H.265, AAC, MP3, AC-3)",
        ),
        (
            FLV,
            "Flash Video",
            &["flv"],
            &["video/x-flv"],
            vec![Sniffer { offset: 0, magic: b"FLV\x01", confidence: 100 }],
            "Adobe FLV (read only; H.264/H.265, AAC, MP3, PCM)",
        ),
        (
            OGV,
            "Ogg video",
            &["ogv"],
            &["video/ogg"],
            vec![Sniffer { offset: 0, magic: b"OggS", confidence: 30 }],
            "Ogg (RFC 3533) with Theora I video and Vorbis audio (read only)",
        ),
        (
            PS,
            "MPEG program stream",
            &["mpg", "mpeg", "vob", "ps"],
            &["video/mpeg"],
            vec![Sniffer { offset: 0, magic: &[0, 0, 1, 0xBA], confidence: 90 }],
            "ISO/IEC 13818-1 program stream (read only; H.264/H.265, AAC, MP3, AC-3)",
        ),
        (
            WEBM,
            "WebM",
            &["webm"],
            &["video/webm"],
            vec![Sniffer { offset: 0, magic: &[0x1A, 0x45, 0xDF, 0xA3], confidence: 40 }],
            "WebM profile of Matroska (VP8/VP9/AV1, Opus/Vorbis)",
        ),
    ];
    registry.add_format(FormatDescriptor {
        id: FRAMES,
        name: "Image sequence",
        category: Category::Video,
        extensions: &["frames.zip"],
        mime: &["application/zip"],
        sniffers: Vec::new(),
        capabilities: Capabilities {
            read: true,
            write: true,
            ..Capabilities::default()
        },
        writer_options: frames_options(),
        reader_options: Default::default(),
        ownership: Ownership::Own,
        spec: "ZIP of numbered PNG or JPEG pictures, one per frame",
    });
    for id in FORMATS {
        for (a, b, text) in [
            (id, FRAMES, "Decode the video into numbered pictures"),
            (FRAMES, id, "Encode the pictures as video"),
        ] {
            if read_only(b) {
                continue;
            }
            registry.add_edge(Edge {
                from: a,
                to: b,
                kind: EdgeKind::Transcode,
                loss: 2,
                ownership: Ownership::Own,
                time: 6,
                description: text,
            });
        }
    }
    for (id, name, extensions, mime, sniffers, spec) in rows {
        registry.add_format(FormatDescriptor {
            id,
            name,
            category: Category::Video,
            extensions,
            mime,
            sniffers,
            capabilities: Capabilities {
                read: true,
                write: !read_only(id),
                ..Capabilities::default()
            },
            writer_options: options_for(id),
            reader_options: Default::default(),
            ownership: Ownership::Own,
            spec,
        });
        registry.add_edge(Edge {
            from: id,
            to: MEDIA_IR,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 2,
            description: "Read streams and packets",
        });
        for sub in crate::subtitle::FORMATS {
            registry.add_edge(Edge {
                from: id,
                to: sub,
                kind: EdgeKind::Transcode,
                loss: 1,
                ownership: Ownership::Own,
                time: 3,
                description: "Extract the first text subtitle track",
            });
        }
        for aud in crate::audio::FORMATS {
            registry.add_edge(Edge {
                from: id,
                to: aud,
                kind: EdgeKind::Transcode,
                loss: 1,
                ownership: Ownership::Own,
                time: 5,
                description: "Extract the first audio track",
            });
        }
        for img in [crate::image::GIF, crate::image::WEBP, crate::image::PNG] {
            for (a, b, text) in [
                (id, img, "Decode the video into frames"),
                (img, id, "Encode the frames as video"),
            ] {
                if read_only(b) {
                    continue;
                }
                registry.add_edge(Edge {
                    from: a,
                    to: b,
                    kind: EdgeKind::Transcode,
                    loss: 2,
                    ownership: Ownership::Own,
                    time: 6,
                    description: text,
                });
            }
        }
        if read_only(id) {
            continue;
        }
        registry.add_edge(Edge {
            from: MEDIA_IR,
            to: id,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 2,
            description: "Write container",
        });
    }
}

/// Identify a video container by content.
pub fn detect(data: &[u8]) -> Option<FormatId> {
    if y4m::sniff(data) {
        return Some(Y4M);
    }
    if avi::sniff(data) {
        return Some(AVI);
    }
    if mp4::sniff(data) {
        return Some(if data.get(8..12) == Some(b"qt  ") {
            MOV
        } else {
            MP4
        });
    }
    if flv::sniff(data) {
        return Some(FLV);
    }
    if ogv::sniff(data) {
        return Some(OGV);
    }
    if ps::sniff(data) {
        return Some(PS);
    }
    if ts::sniff(data) {
        return Some(TS);
    }
    match oc_container::matroska::doc_type(data).as_deref() {
        Some("webm") => Some(WEBM),
        Some(_) => Some(MKV),
        None => None,
    }
}

pub fn read(from: FormatId, data: &[u8], limits: &Limits) -> Result<Media> {
    if from == Y4M {
        y4m::read(data, limits)
    } else if from == AVI {
        avi::read(data, limits)
    } else if from == TS {
        ts::read(data, limits)
    } else if from == FLV {
        flv::read(data, limits)
    } else if from == OGV {
        ogv::read(data, limits)
    } else if from == PS {
        ps::read(data, limits)
    } else if from == MP4 || from == MOV {
        mp4::read(data, limits)
    } else {
        mkv::read(data, limits)
    }
}

/// Whether container `to` can hold a stream of this codec as is.
pub fn container_allows(to: FormatId, c: &str) -> bool {
    if to == Y4M {
        c == codec::RAW
    } else if to == AVI {
        avi::allows(c)
    } else if to == TS {
        ts::allows(c)
    } else if to == WEBM {
        mkv::webm_allows(c)
    } else if to == MKV {
        mkv::mkv_allows(c)
    } else {
        mp4::allows(c, to == MOV)
    }
}

pub fn write(to: FormatId, media: &Media, opts: &Options) -> Result<Vec<u8>> {
    if to == Y4M {
        y4m::write(media)
    } else if to == AVI {
        avi::write(media)
    } else if to == TS {
        ts::write(media)
    } else if to == MP4 || to == MOV {
        mp4::write(
            media,
            &mp4::Options {
                mov: to == MOV,
                faststart: opts.bool("faststart").unwrap_or(true),
                fragmented: opts.bool("fragmented").unwrap_or(false),
            },
        )
    } else {
        mkv::write(media, to == WEBM, "OpenConverter")
    }
}

/// What the user asked for beyond moving streams between containers.
struct Edit {
    width: usize,
    height: usize,
    crop: Option<(usize, usize, usize, usize)>,
    rotate: u16,
    flip: String,
    deinterlace: bool,
    pad: bool,
    fps: Option<(u32, u32)>,
    start_ns: i64,
    duration_ns: Option<i64>,
    burn: bool,
}

impl Edit {
    fn from(opts: &Options) -> Self {
        let n = |k: &str| opts.int(k).unwrap_or(0).max(0) as usize;
        let fps = opts.float("fps").unwrap_or(0.0);
        let crop = (n("crop_w") > 0 && n("crop_h") > 0)
            .then(|| (n("crop_x"), n("crop_y"), n("crop_w"), n("crop_h")));
        Self {
            width: n("width"),
            height: n("height"),
            crop,
            rotate: opts
                .text("rotate")
                .and_then(|r| r.parse().ok())
                .unwrap_or(0),
            flip: opts.text("flip").unwrap_or("none").to_string(),
            deinterlace: opts.bool("deinterlace").unwrap_or(false),
            pad: opts.bool("pad").unwrap_or(false),
            fps: (fps > 0.0).then(|| {
                let milli = (fps * 1000.0).round() as u32;
                let g = gcd(milli, 1000);
                (milli / g, 1000 / g)
            }),
            start_ns: opts.int("start_ms").unwrap_or(0).max(0) * 1_000_000,
            duration_ns: opts
                .int("duration_ms")
                .filter(|d| *d > 0)
                .map(|d| d * 1_000_000),
            burn: opts.bool("burn_subtitles").unwrap_or(false),
        }
    }

    /// Only the timing fields; the picture edits are left to a later stage (image transforms).
    fn timing_only(opts: &Options) -> Self {
        let mut e = Self::from(opts);
        e.width = 0;
        e.height = 0;
        e.crop = None;
        e.rotate = 0;
        e.flip = "none".into();
        e.deinterlace = false;
        e.pad = false;
        e.burn = false;
        e
    }

    /// Whether the picture itself changes.
    fn touches_picture(&self) -> bool {
        self.width > 0
            || self.height > 0
            || self.crop.is_some()
            || self.rotate != 0
            || self.flip != "none"
            || self.deinterlace
            || self.burn
            || self.fps.is_some()
    }

    fn trims(&self) -> bool {
        self.start_ns > 0 || self.duration_ns.is_some()
    }

    fn end_ns(&self) -> i64 {
        self.duration_ns
            .map_or(i64::MAX, |d| self.start_ns.saturating_add(d))
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a.max(1)
    } else {
        gcd(b, a % b)
    }
}

fn default_video_codec(to: FormatId) -> &'static str {
    if to == Y4M {
        codec::RAW
    } else if to == AVI || to == MOV {
        codec::MJPEG
    } else {
        codec::VP8
    }
}

fn default_audio_codec(to: FormatId) -> &'static str {
    if to == AVI {
        "pcm"
    } else {
        "opus"
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Video => "video",
        Kind::Audio => "audio",
        Kind::Subtitle => "subtitle",
    }
}

fn holds_rotation(to: FormatId) -> bool {
    to == MP4 || to == MOV
}

/// Packets of a stream that fall inside the trim window, moved to start at zero.
fn trimmed(media: &Media, si: usize, ed: &Edit) -> Vec<Packet> {
    media
        .packets_of(si)
        .filter(|p| p.pts + p.duration.max(1) > ed.start_ns && p.pts < ed.end_ns())
        .map(|p| {
            let mut p = p.clone();
            p.pts -= ed.start_ns;
            p.dts -= ed.start_ns;
            p
        })
        .collect()
}

/// Decide what happens to each stream and build the media to write.
pub fn retarget(
    media: Media,
    to: FormatId,
    opts: &Options,
    registry: &Registry,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Media> {
    let ed = Edit::from(opts);
    let mut out = Media {
        title: media.title.clone(),
        source: media.source.clone(),
        ..Media::default()
    };
    let drop_note = |notes: &mut Notes, s: &oc_video::Stream, why: String| {
        notes.push(
            Note::new(
                Severity::Warning,
                "stream_dropped",
                format!(
                    "a {} stream ({}) was left out: {why}",
                    kind_name(s.kind),
                    s.codec
                ),
            )
            .with_detail(s.codec.clone()),
        );
    };
    let keep: Option<Vec<usize>> = opts
        .text("streams")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.split(',').filter_map(|n| n.trim().parse().ok()).collect());
    for (si, s) in media.streams.iter().enumerate() {
        if keep.as_ref().is_some_and(|k| !k.contains(&si)) {
            continue;
        }
        let drop = (s.kind == Kind::Audio && opts.bool("drop_audio") == Some(true))
            || (s.kind == Kind::Subtitle && opts.bool("drop_subtitles") == Some(true));
        if drop {
            continue;
        }
        match s.kind {
            Kind::Video => {
                let want = opts.text("video_codec").unwrap_or("auto");
                let copy_ok =
                    container_allows(to, &s.codec) && !ed.touches_picture() && !ed.trims();
                let can_dec = frames::can_decode(&s.codec);
                if want == "copy" || (want == "auto" && copy_ok) {
                    if !container_allows(to, &s.codec) {
                        return Err(Error::new(
                            ErrorCode::CodecNotInBuild,
                            format!(
                                "{} video cannot be stored in this container as it is",
                                s.codec
                            ),
                        ));
                    }
                    if ed.touches_picture() || ed.trims() {
                        return Err(Error::invalid_option(
                            "video_codec",
                            "copy cannot be combined with editing the picture or the time range",
                        ));
                    }
                    if s.rotation != 0 && !holds_rotation(to) {
                        notes.push(Note::new(
                            Severity::Warning,
                            "rotation_dropped",
                            "this video is stored rotated; the chosen container cannot say so, so it will play sideways. Re-encode to apply the rotation.",
                        ));
                    }
                    let n = out.streams.len();
                    out.streams.push(s.clone());
                    for mut p in media.packets_of(si).cloned() {
                        p.stream = n;
                        out.packets.push(p);
                    }
                    continue;
                }
                let target = if want == "auto" {
                    default_video_codec(to)
                } else {
                    want
                };
                if !can_dec {
                    if want == "auto"
                        && !container_allows(to, &s.codec)
                        && !ed.touches_picture()
                        && !ed.trims()
                    {
                        drop_note(
                            notes,
                            s,
                            "the container cannot hold it and this build cannot convert it".into(),
                        );
                        continue;
                    }
                    return Err(Error::new(
                        ErrorCode::CodecNotInBuild,
                        format!("{} video cannot be decoded in this build, so it can only be copied between containers", s.codec),
                    ));
                }
                if !container_allows(to, target) {
                    return Err(Error::invalid_option(
                        "video_codec",
                        format!("{target} cannot be stored in this container"),
                    ));
                }
                let mut burner = None;
                if ed.burn {
                    let found = media
                        .streams
                        .iter()
                        .position(|x| x.kind == Kind::Subtitle && subs::is_text(&x.codec));
                    match found.and_then(|i| subs::to_track(&media, i)) {
                        Some(t) => burner = Some(crate::burn::Burner::new(t, limits)),
                        None => notes.push(Note::new(
                            Severity::Warning,
                            "burn_no_subtitles",
                            "no text subtitle track was found, so nothing was burned in",
                        )),
                    }
                }
                let (ns, packets) = transcode_video(
                    &media,
                    si,
                    target,
                    &ed,
                    to,
                    opts,
                    limits,
                    progress,
                    notes,
                    burner.as_mut(),
                )?;
                let n = out.streams.len();
                out.streams.push(ns);
                for mut p in packets {
                    p.stream = n;
                    out.packets.push(p);
                }
            }
            Kind::Audio => {
                if to == Y4M {
                    drop_note(notes, s, "this container holds no audio".into());
                    continue;
                }
                let want = opts.text("audio_codec").unwrap_or("auto");
                let allowed = container_allows(to, &s.codec);
                if want == "copy" || (want == "auto" && allowed) {
                    if !allowed {
                        return Err(Error::new(
                            ErrorCode::CodecNotInBuild,
                            format!(
                                "{} audio cannot be stored in this container as it is",
                                s.codec
                            ),
                        ));
                    }
                    let n = out.streams.len();
                    out.streams.push(s.clone());
                    let packets = if ed.trims() {
                        trimmed(&media, si, &ed)
                    } else {
                        media.packets_of(si).cloned().collect()
                    };
                    for mut p in packets {
                        p.stream = n;
                        out.packets.push(p);
                    }
                    continue;
                }
                let target = if want == "auto" {
                    default_audio_codec(to)
                } else {
                    want
                };
                match transcode_audio(&media, si, target, &ed, to, registry, limits, notes) {
                    Ok((ns, packets)) => {
                        let n = out.streams.len();
                        out.streams.push(ns);
                        for mut p in packets {
                            p.stream = n;
                            out.packets.push(p);
                        }
                    }
                    Err(e)
                        if want == "auto" && (e.code == ErrorCode::CodecNotInBuild || to == TS) =>
                    {
                        drop_note(notes, s, e.message.clone());
                    }
                    Err(e) => return Err(e),
                }
            }
            Kind::Subtitle => {
                if to == Y4M {
                    drop_note(notes, s, "this container holds no subtitles".into());
                    continue;
                }
                let mut media_s = s.clone();
                let mut packets: Vec<Packet> = if ed.trims() {
                    trimmed(&media, si, &ed)
                } else {
                    media.packets_of(si).cloned().collect()
                };
                if container_allows(to, &s.codec) {
                    let n = out.streams.len();
                    media_s.default = s.default;
                    out.streams.push(media_s);
                    for mut p in packets {
                        p.stream = n;
                        out.packets.push(p);
                    }
                    continue;
                }
                // Text subtitle streams the container cannot hold are rewritten in a codec it can.
                let alt = [codec::SRT, codec::ASS]
                    .into_iter()
                    .find(|c| subs::is_text(&s.codec) && container_allows(to, c));
                let tmp = Media {
                    streams: vec![s.clone()],
                    packets: {
                        for p in &mut packets {
                            p.stream = 0;
                        }
                        packets
                    },
                    ..Media::default()
                };
                match (alt, subs::to_track(&tmp, 0)) {
                    (Some(alt), Some(track)) => {
                        let (mut ns, ps) = subs::from_track(&track, alt);
                        ns.name = s.name.clone();
                        ns.default = s.default;
                        ns.forced = s.forced;
                        notes.push(
                            Note::new(
                                Severity::Info,
                                "subtitle_converted",
                                format!("a {} subtitle track was rewritten as {alt}, which this container can hold", s.codec),
                            )
                            .with_detail(alt),
                        );
                        let n = out.streams.len();
                        out.streams.push(ns);
                        for mut p in ps {
                            p.stream = n;
                            out.packets.push(p);
                        }
                    }
                    _ => drop_note(notes, s, "the container cannot hold it".into()),
                }
            }
        }
    }
    if out.streams.is_empty() {
        return Err(Error::new(
            ErrorCode::CodecNotInBuild,
            "none of the streams in this file can be stored in the chosen container",
        ));
    }
    let shift = opts.int("audio_offset_ms").unwrap_or(0) * 1_000_000;
    if shift != 0 {
        let kinds: Vec<Kind> = out.streams.iter().map(|s| s.kind).collect();
        out.packets.retain_mut(|p| {
            if kinds.get(p.stream) == Some(&Kind::Video) {
                return true;
            }
            p.pts += shift;
            p.dts += shift;
            if p.pts + p.duration <= 0 {
                return false;
            }
            p.pts = p.pts.max(0);
            p.dts = p.dts.max(0);
            true
        });
    }
    out.sort_by_dts();
    Ok(out)
}

/// A decoded picture with its place on the timeline.
struct Timed {
    pts: i64,
    dur: i64,
    pic: Picture,
}

/// Apply the edit to each picture and hand the frames that fall on the output timeline to
/// `sink` as `(time, duration, picture)`, relative to the trim start. With `per` set the output
/// is constant-rate: a picture is repeated or skipped to fill the grid.
fn run_pipeline(
    frames: &mut dyn Iterator<Item = Result<Timed>>,
    ed: &Edit,
    per: Option<i64>,
    physical_rot: u16,
    progress: &dyn Progress,
    total: u64,
    sink: &mut dyn FnMut(i64, i64, &Picture) -> Result<()>,
) -> Result<()> {
    let process = |pic: Picture| -> Result<Picture> {
        let mut pic = pic;
        if ed.deinterlace {
            pic = pic.deinterlace();
        }
        if let Some((x, y, w, h)) = ed.crop {
            pic = pic.crop(
                x,
                y,
                w.min(pic.w.saturating_sub(x & !1)),
                h.min(pic.h.saturating_sub(y & !1)),
            )?;
        }
        if physical_rot != 0 {
            pic = pic.rotate(physical_rot);
        }
        match ed.flip.as_str() {
            "horizontal" => pic = pic.flip_horizontal(),
            "vertical" => pic = pic.flip_vertical(),
            _ => {}
        }
        if ed.width > 0 || ed.height > 0 {
            let (w, h) = match (ed.width, ed.height) {
                (w, 0) => (
                    w,
                    ((pic.h as f64 * w as f64 / pic.w as f64).round() as usize).max(1),
                ),
                (0, h) => (
                    ((pic.w as f64 * h as f64 / pic.h as f64).round() as usize).max(1),
                    h,
                ),
                (w, h) => (w, h),
            };
            pic = if ed.pad && ed.width > 0 && ed.height > 0 {
                pic.pad(w, h)
            } else {
                pic.scale(w.max(2) & !1, h.max(2) & !1)
            };
        }
        Ok(pic)
    };
    let mut next_k: i64 = 0;
    let mut emit = |t0: i64,
                    t1: i64,
                    pic: &Picture,
                    sink: &mut dyn FnMut(i64, i64, &Picture) -> Result<()>|
     -> Result<()> {
        let lo = t0.max(ed.start_ns);
        let hi = t1.min(ed.end_ns());
        if hi <= lo {
            return Ok(());
        }
        match per {
            // A frame owns the grid points nearest to it, so millisecond-rounded timestamps
            // do not duplicate or drop frames; a tie goes to the earlier frame.
            Some(per) => loop {
                let t = ed.start_ns + next_k * per;
                if t > hi - per / 2 {
                    break Ok(());
                }
                if t > lo - per / 2 {
                    sink(next_k * per, per, pic)?;
                }
                next_k += 1;
            },
            None => sink(lo - ed.start_ns, hi - lo, pic),
        }
    };
    let mut pending: Option<Timed> = None;
    for (n, f) in frames.enumerate() {
        progress.check_cancelled()?;
        if n % 8 == 0 {
            progress.report(n as u64, total.max(1), "video");
        }
        let f = f?;
        let pic = process(f.pic)?;
        if let Some(prev) = pending.take() {
            let t1 = if per.is_some() {
                f.pts.max(prev.pts)
            } else {
                prev.pts + prev.dur
            };
            emit(prev.pts, t1, &prev.pic, sink)?;
        }
        pending = Some(Timed {
            pts: f.pts,
            dur: f.dur,
            pic,
        });
    }
    if let Some(last) = pending.take() {
        emit(last.pts, last.pts + last.dur, &last.pic, sink)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn transcode_video(
    media: &Media,
    si: usize,
    target: &str,
    ed: &Edit,
    to: FormatId,
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
    mut burner: Option<&mut crate::burn::Burner>,
) -> Result<(oc_video::Stream, Vec<Packet>)> {
    let src = media
        .streams
        .get(si)
        .ok_or_else(|| Error::new(ErrorCode::InternalError, "stream index out of range"))?;
    let mut dec = frames::Decoder::new(src, limits)?;
    // Constant-rate containers need frames on a grid.
    let cfr = ed.fps.is_some() || to == AVI || to == Y4M;
    let out_fps = ed.fps.or(src.fps).unwrap_or((25, 1));
    let per = oc_video::frame_ns(out_fps).max(1);
    let physical_rot = (ed.rotate + if holds_rotation(to) { 0 } else { src.rotation }) % 360;
    let params = frames::EncodeParams {
        quality: opts.int("quality").unwrap_or(80).clamp(1, 100) as u8,
        fps: out_fps,
        keyint: opts.int("keyframe_interval").unwrap_or(120).max(1) as u32,
        bitrate_kbps: opts.int("bitrate_kbps").unwrap_or(0).max(0) as u32,
        effort: opts.int("effort").unwrap_or(4).clamp(0, 6) as u8,
        threads: opts.int("threads").unwrap_or(0).max(0) as usize,
    };
    let keyint = params.keyint;
    let mut enc: Option<frames::Encoder> = None;
    let mut stream = oc_video::Stream::new(Kind::Video, target);
    stream.language = src.language.clone();
    stream.name = src.name.clone();
    stream.default = src.default;
    // The edit's rotation is always applied to the pixels; the container flag only carries the
    // source's own, where the container has one.
    stream.rotation = if holds_rotation(to) { src.rotation } else { 0 };
    let mut packets: Vec<Packet> = Vec::new();
    let total = media.packets_of(si).count() as u64;
    let mut frame_no = 0u32;
    let mut source = media.packets_of(si).map(|p| {
        Ok(dec.decode(&p.data)?.map(|pic| Timed {
            pts: p.pts,
            dur: p.duration.max(per),
            pic,
        }))
    });
    let mut frames_iter = std::iter::from_fn(|| loop {
        match source.next()? {
            Ok(Some(t)) => return Some(Ok(t)),
            Ok(None) => continue,
            Err(e) => return Some(Err(e)),
        }
    });
    run_pipeline(
        &mut frames_iter,
        ed,
        cfr.then_some(per),
        physical_rot,
        progress,
        total,
        &mut |pts, dur, pic| {
            let burned;
            let pic = match burner.as_mut() {
                Some(b) => {
                    let mut p = pic.clone();
                    b.apply(&mut p, (pts + ed.start_ns) / 1_000_000, &mut Notes::new());
                    burned = p;
                    &burned
                }
                None => pic,
            };
            if enc.is_none() {
                let e = frames::Encoder::new(target, pic.w, pic.h, &params)?;
                stream.width = pic.w as u32;
                stream.height = pic.h as u32;
                stream.fps = if cfr { Some(out_fps) } else { src.fps };
                e.describe(&mut stream);
                enc = Some(e);
            }
            let e = enc
                .as_mut()
                .ok_or_else(|| Error::new(ErrorCode::InternalError, "encoder missing"))?;
            let (data, key) = e.encode(pic)?;
            let key = key || frame_no.is_multiple_of(keyint);
            frame_no += 1;
            packets.push(Packet {
                stream: 0,
                pts,
                dts: pts,
                duration: dur,
                key,
                discard_end: 0,
                data,
            });
            Ok(())
        },
    )?;
    if packets.is_empty() {
        return Err(Error::new(
            ErrorCode::CorruptFile,
            "no video frame could be decoded (or the time range is empty)",
        ));
    }
    if cfr && ed.fps.is_none() && src.fps.is_none() {
        notes.push(Note::new(
            Severity::Info,
            "frame_rate_guessed",
            "the source states no frame rate; 25 fps was used",
        ));
    }
    Ok((stream, packets))
}

#[allow(clippy::too_many_arguments)]
fn transcode_audio(
    media: &Media,
    si: usize,
    target: &str,
    ed: &Edit,
    to: FormatId,
    registry: &Registry,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<(oc_video::Stream, Vec<Packet>)> {
    let src = media
        .streams
        .get(si)
        .ok_or_else(|| Error::new(ErrorCode::InternalError, "stream index out of range"))?;
    let from_fmt = if codec::is_pcm(&src.codec)
        || matches!(
            src.codec.as_str(),
            codec::OPUS | codec::VORBIS | codec::FLAC | codec::MP3
        ) {
        crate::audio::MKA
    } else {
        return Err(Error::new(
            ErrorCode::CodecNotInBuild,
            format!("{} audio cannot be decoded in this build (it may be covered by patents); it can only be copied", src.codec),
        ));
    };
    let target_codec = match target {
        "opus" | "vorbis" | "flac" | "pcm" => target,
        other => {
            return Err(Error::invalid_option(
                "audio_codec",
                format!("{other} audio is not available"),
            ))
        }
    };
    let tc = match target_codec {
        "opus" => codec::OPUS,
        "vorbis" => codec::VORBIS,
        "flac" => codec::FLAC,
        _ => codec::PCM_S16,
    };
    if !container_allows(to, tc) {
        return Err(Error::invalid_option(
            "audio_codec",
            format!("{target_codec} audio cannot be stored in this container"),
        ));
    }
    let only = Media {
        streams: vec![src.clone()],
        packets: {
            let mut v = if ed.trims() {
                trimmed(media, si, ed)
            } else {
                media.packets_of(si).cloned().collect()
            };
            for p in &mut v {
                p.stream = 0;
            }
            v
        },
        ..Media::default()
    };
    let wrapped = mkv::write(&only, false, "OpenConverter")?;
    let (clip, n) = crate::audio::decode(from_fmt, &wrapped, limits)?;
    notes.extend(n);
    let mka_schema = &registry
        .format(crate::audio::MKA)
        .ok_or_else(|| Error::new(ErrorCode::InternalError, "MKA format missing"))?
        .writer_options;
    let aopts = mka_schema.validate(&[(
        "codec".to_string(),
        oc_core::options::OptionValue::Text(target_codec.to_string()),
    )])?;
    let source = crate::audio::source_depth(&clip);
    let (clip, n) = crate::audio::process(clip, &aopts, limits)?;
    notes.extend(n);
    let (bytes, n) = crate::audio::encode(crate::audio::MKA, clip, &aopts, source)?;
    notes.extend(n);
    let back = mkv::read(&bytes, limits)?;
    let mut s = back
        .streams
        .into_iter()
        .next()
        .ok_or_else(|| Error::new(ErrorCode::InternalError, "no audio stream written"))?;
    s.language = src.language.clone();
    s.name = src.name.clone();
    s.default = src.default;
    notes.push(
        Note::new(
            Severity::Info,
            "audio_transcoded",
            format!("the {} audio was converted to {}", src.codec, s.codec),
        )
        .with_detail(s.codec.clone()),
    );
    Ok((s, back.packets))
}

/// The first subtitle stream of a video container as cues, with its text codec.
pub fn extract_subtitle(
    from: FormatId,
    data: &[u8],
    limits: &Limits,
    notes: &mut Notes,
) -> Result<(oc_subtitle::Track, FormatId)> {
    let media = read(from, data, limits)?;
    let found: Vec<usize> = media
        .streams
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == Kind::Subtitle && subs::is_text(&s.codec))
        .map(|(i, _)| i)
        .collect();
    let Some(&si) = found.first() else {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "this file has no text subtitle track",
        ));
    };
    if found.len() > 1 {
        notes.push(Note::new(
            Severity::Info,
            "subtitle_track_chosen",
            format!(
                "the file has {} subtitle tracks; the first one was used",
                found.len()
            ),
        ));
    }
    let track = subs::to_track(&media, si)
        .ok_or_else(|| Error::new(ErrorCode::InternalError, "subtitle stream vanished"))?;
    let kind = if media.streams.get(si).is_some_and(|s| s.codec == codec::ASS) {
        crate::subtitle::ASS
    } else {
        crate::subtitle::SRT
    };
    Ok((track, kind))
}

/// The first audio stream of a video container, re-wrapped as a Matroska audio file so the
/// audio converters can read it like any other.
pub fn extract_audio(
    from: FormatId,
    data: &[u8],
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let media = read(from, data, limits)?;
    let found: Vec<usize> = media
        .streams
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == Kind::Audio)
        .map(|(i, _)| i)
        .collect();
    let Some(&si) = found.first() else {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "this file has no audio track",
        ));
    };
    if found.len() > 1 {
        notes.push(Note::new(
            Severity::Info,
            "audio_track_chosen",
            format!(
                "the file has {} audio tracks; the first one was used",
                found.len()
            ),
        ));
    }
    let mut only = Media {
        streams: media.streams.get(si).cloned().into_iter().collect(),
        title: media.title.clone(),
        ..Media::default()
    };
    for mut p in media.packets {
        if p.stream == si {
            p.stream = 0;
            only.packets.push(p);
        }
    }
    mkv::write(&only, false, "OpenConverter")
}

#[allow(clippy::too_many_arguments)]
pub fn convert(
    from: FormatId,
    to: FormatId,
    data: &[u8],
    opts: &Options,
    registry: &Registry,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let media = read(from, data, limits)?;
    let media = retarget(media, to, opts, registry, limits, progress, notes)?;
    write(to, &media, opts)
}

/// Frames of the first video stream as an animation (for GIF, animated WebP or APNG output).
/// Only timing options apply here; resizing and the like are left to the image transforms.
pub fn video_to_animation(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Animation> {
    video_frames(from, data, opts, limits, progress, notes, true)
}

/// The decoded frames of the first video stream; with `timing_only` false the picture edits
/// (resize, crop, rotate, mirror) are applied as well.
pub fn video_frames(
    from: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
    timing_only: bool,
) -> Result<Animation> {
    let media = read(from, data, limits)?;
    let si = media
        .streams
        .iter()
        .position(|s| s.kind == Kind::Video)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::UnsupportedVariant,
                "this file has no video track",
            )
        })?;
    let src = media
        .streams
        .get(si)
        .ok_or_else(|| Error::new(ErrorCode::InternalError, "stream index out of range"))?;
    let ed = if timing_only {
        Edit::timing_only(opts)
    } else {
        Edit::from(opts)
    };
    let mut dec = frames::Decoder::new(src, limits)?;
    let per = ed.fps.map(|f| oc_video::frame_ns(f).max(1));
    let total = media.packets_of(si).count() as u64;
    let mut source = media.packets_of(si).map(|p| {
        Ok(dec.decode(&p.data)?.map(|pic| Timed {
            pts: p.pts,
            dur: p.duration.max(1),
            pic,
        }))
    });
    let mut frames_iter = std::iter::from_fn(|| loop {
        match source.next()? {
            Ok(Some(t)) => return Some(Ok(t)),
            Ok(None) => continue,
            Err(e) => return Some(Err(e)),
        }
    });
    let mut anim = Animation::new(0, 0);
    run_pipeline(
        &mut frames_iter,
        &ed,
        per,
        0,
        progress,
        total,
        &mut |_, dur, pic| {
            oc_image::anim::check_frame(limits, pic.w as u32, pic.h as u32, anim.frames.len())?;
            if anim.frames.is_empty() {
                anim.width = pic.w as u32;
                anim.height = pic.h as u32;
            }
            let image = oc_image::Image::new(
                pic.w as u32,
                pic.h as u32,
                oc_image::Layout::Rgba,
                oc_image::Depth::Eight,
                pic.to_rgba(),
            )?;
            anim.frames.push(oc_image::anim::Frame {
                image,
                delay_ms: ((dur + 500_000) / 1_000_000).clamp(1, u32::MAX as i64) as u32,
            });
            Ok(())
        },
    )?;
    if anim.frames.is_empty() {
        return Err(Error::new(
            ErrorCode::CorruptFile,
            "no video frame could be decoded (or the time range is empty)",
        ));
    }
    if src.rotation != 0 {
        notes.push(Note::new(
            Severity::Info,
            "rotation_not_applied",
            "the video is stored rotated; use the rotate option to turn the frames",
        ));
    }
    Ok(anim)
}

/// A video container holding `anim` (an animated GIF, WebP or APNG, or a single picture shown
/// for `duration_ms`).
#[allow(clippy::too_many_arguments)]
pub fn animation_to_video(
    anim: &Animation,
    to: FormatId,
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    animation_to_video_at(anim, to, opts, limits, progress, notes, None)
}

/// Like [`animation_to_video`], with every frame shown for `frame_ns` when that is given
/// (an image sequence has no times of its own).
#[allow(clippy::too_many_arguments)]
pub fn animation_to_video_at(
    anim: &Animation,
    to: FormatId,
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
    frame_ns: Option<i64>,
) -> Result<Vec<u8>> {
    let ed = Edit::from(opts);
    let target = match opts.text("video_codec").unwrap_or("auto") {
        "auto" | "copy" => default_video_codec(to),
        other => other,
    };
    if !container_allows(to, target) {
        return Err(Error::invalid_option(
            "video_codec",
            format!("{target} cannot be stored in this container"),
        ));
    }
    let single = anim.frames.len() == 1;
    let delays: Vec<i64> = anim
        .frames
        .iter()
        .map(|f| {
            if f.delay_ms == 0 {
                100
            } else {
                i64::from(f.delay_ms)
            }
        })
        .collect();
    let cfr = ed.fps.is_some() || to == AVI || to == Y4M || single;
    // Frame rate of the grid: the one asked for, else the one that fits the most common delay.
    let out_fps = ed.fps.unwrap_or_else(|| {
        let mut sorted = delays.clone();
        sorted.sort_unstable();
        let mid = sorted.get(sorted.len() / 2).copied().unwrap_or(100);
        oc_video::fps_from_ns(mid * 1_000_000).unwrap_or((10, 1))
    });
    let per = oc_video::frame_ns(out_fps).max(1);
    let mut t = 0i64;
    let mut timed = anim.frames.iter().zip(&delays).map(|(f, d)| {
        let dur = if single {
            ed.duration_ns.unwrap_or(1_000_000_000)
        } else {
            frame_ns.unwrap_or(d * 1_000_000)
        };
        let rgba = anim_over_black(&f.image);
        let pts = t;
        t += dur;
        Ok(Timed {
            pts,
            dur,
            pic: Picture::from_rgba(f.image.width as usize, f.image.height as usize, &rgba)?,
        })
    });
    let mut frames_iter = std::iter::from_fn(|| timed.next());
    let mut ed2 = Edit::from(opts);
    if single {
        ed2.duration_ns = None;
    }
    let params = frames::EncodeParams {
        quality: opts.int("quality").unwrap_or(80).clamp(1, 100) as u8,
        fps: out_fps,
        keyint: opts.int("keyframe_interval").unwrap_or(120).max(1) as u32,
        bitrate_kbps: opts.int("bitrate_kbps").unwrap_or(0).max(0) as u32,
        effort: opts.int("effort").unwrap_or(4).clamp(0, 6) as u8,
        threads: opts.int("threads").unwrap_or(0).max(0) as usize,
    };
    let mut enc: Option<frames::Encoder> = None;
    let mut stream = oc_video::Stream::new(Kind::Video, target);
    let mut packets: Vec<Packet> = Vec::new();
    let mut frame_no = 0u32;
    let _ = limits;
    run_pipeline(
        &mut frames_iter,
        &ed2,
        cfr.then_some(per),
        0,
        progress,
        anim.frames.len() as u64,
        &mut |pts, dur, pic| {
            if enc.is_none() {
                let e = frames::Encoder::new(target, pic.w, pic.h, &params)?;
                stream.width = pic.w as u32;
                stream.height = pic.h as u32;
                stream.fps = cfr.then_some(out_fps);
                e.describe(&mut stream);
                enc = Some(e);
            }
            let e = enc
                .as_mut()
                .ok_or_else(|| Error::new(ErrorCode::InternalError, "encoder missing"))?;
            let (data, key) = e.encode(pic)?;
            packets.push(Packet {
                stream: 0,
                pts,
                dts: pts,
                duration: dur,
                key: key || frame_no.is_multiple_of(params.keyint),
                discard_end: 0,
                data,
            });
            frame_no += 1;
            Ok(())
        },
    )?;
    if packets.is_empty() {
        return Err(Error::new(ErrorCode::CorruptFile, "no frame to write"));
    }
    if anim.frames.iter().any(|f| {
        f.image
            .data
            .chunks(4)
            .any(|p| p.get(3).is_some_and(|a| *a < 255))
    }) {
        notes.push(Note::new(
            Severity::Info,
            "transparency_dropped",
            "video has no transparency; transparent areas became black",
        ));
    }
    let media = Media {
        streams: vec![stream],
        packets,
        ..Media::default()
    };
    write(to, &media, opts)
}

fn anim_over_black(img: &oc_image::Image) -> Vec<u8> {
    let (img, _) = oc_image::ops::convert_depth(img, oc_image::Depth::Eight);
    let (img, _) = oc_image::ops::convert_layout(&img, oc_image::Layout::Rgba, [0, 0, 0]);
    let mut d = img.data;
    for px in d.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        for c in &mut px[..3] {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
        px[3] = 255;
    }
    d
}

/// Whether this build can decode a video stream's pictures.
fn can_decode_video(c: &str) -> bool {
    matches!(
        c,
        codec::MJPEG | codec::RAW | codec::VP8 | codec::THEORA | codec::PRORES
    )
}

fn can_decode_audio(c: &str) -> bool {
    codec::is_pcm(c) || matches!(c, codec::OPUS | codec::VORBIS | codec::FLAC | codec::MP3)
}

/// The streams of a video file and a strip of thumbnails, as JSON for the interface.
/// `thumbs` is how many pictures to cut from the first video stream (0 for none).
pub fn analyse_json(
    from: FormatId,
    data: &[u8],
    limits: &Limits,
    thumbs: usize,
    thumb_px: u32,
) -> Result<String> {
    use crate::audio::{json_escape, num};
    let media = read(from, data, limits)?;
    let duration = media.duration() as f64 / 1e9;
    let mut streams = Vec::new();
    for (si, s) in media.streams.iter().enumerate() {
        let (mut n, mut bytes) = (0usize, 0usize);
        for p in media.packets_of(si) {
            n += 1;
            bytes += p.data.len();
        }
        let span = {
            let (lo, hi) = media
                .packets_of(si)
                .fold((i64::MAX, i64::MIN), |(lo, hi), p| {
                    (lo.min(p.pts), hi.max(p.pts + p.duration))
                });
            (hi - lo).max(0) as f64 / 1e9
        };
        let kbps = if span > 0.0 {
            num(bytes as f64 * 8.0 / span / 1000.0)
        } else {
            "null".into()
        };
        let decodable = match s.kind {
            Kind::Video => can_decode_video(&s.codec),
            Kind::Audio => can_decode_audio(&s.codec),
            Kind::Subtitle => subs::is_text(&s.codec),
        };
        let copy_to: Vec<String> = FORMATS
            .iter()
            .filter(|f| !read_only(**f) && container_allows(**f, &s.codec))
            .map(|f| json_escape(f.0))
            .collect();
        let fps = s.fps.map_or("null".to_string(), |(a, b)| {
            num(f64::from(a) / f64::from(b.max(1)))
        });
        streams.push(format!(
            "{{\"index\":{si},\"kind\":\"{}\",\"codec\":{},\"width\":{},\"height\":{},\"fps\":{fps},\
\"sample_rate\":{},\"channels\":{},\"packets\":{n},\"bytes\":{bytes},\"bitrate_kbps\":{kbps},\
\"language\":{},\"name\":{},\"default\":{},\"forced\":{},\"rotation\":{},\"decodable\":{decodable},\
\"copy_to\":[{}]}}",
            kind_name(s.kind),
            json_escape(&s.codec),
            s.width,
            s.height,
            s.sample_rate,
            s.channels,
            json_escape(&s.language),
            json_escape(&s.name),
            s.default,
            s.forced,
            s.rotation,
            copy_to.join(",")
        ));
    }
    let total_bytes: usize = media.packets.iter().map(|p| p.data.len()).sum();
    let bitrate = if duration > 0.0 {
        num(total_bytes as f64 * 8.0 / duration / 1000.0)
    } else {
        "null".into()
    };
    let strip = if thumbs > 0 {
        thumbnail_strip(&media, limits, thumbs.min(24), thumb_px.clamp(32, 512))
    } else {
        Err("no thumbnails were asked for".to_string())
    };
    let strip_json = match strip {
        Ok((si, w, h, items)) => format!(
            "{{\"stream\":{si},\"width\":{w},\"height\":{h},\"items\":[{}]}}",
            items.join(",")
        ),
        Err(why) => format!("{{\"items\":[],\"note\":{}}}", json_escape(&why)),
    };
    Ok(format!(
        "{{\"container\":{},\"duration\":{},\"bitrate_kbps\":{bitrate},\"title\":{},\"streams\":[{}],\"thumbs\":{strip_json}}}",
        json_escape(&media.source),
        num(duration),
        json_escape(&media.title),
        streams.join(",")
    ))
}

type Strip = (usize, usize, usize, Vec<String>);

fn thumbnail_strip(
    media: &Media,
    limits: &Limits,
    count: usize,
    max_px: u32,
) -> std::result::Result<Strip, String> {
    let si = media
        .streams
        .iter()
        .position(|s| s.kind == Kind::Video)
        .ok_or_else(|| "this file has no video track".to_string())?;
    let s = media
        .streams
        .get(si)
        .ok_or_else(|| "stream index out of range".to_string())?;
    if !can_decode_video(&s.codec) {
        return Err(format!(
            "{} pictures cannot be decoded in this build, so no thumbnails are shown",
            s.codec
        ));
    }
    let packets: Vec<&Packet> = media.packets_of(si).collect();
    let (lo, hi) = packets.iter().fold((i64::MAX, i64::MIN), |(lo, hi), p| {
        (lo.min(p.pts), hi.max(p.pts))
    });
    if packets.is_empty() {
        return Err("the video track holds no pictures".into());
    }
    let mut items = Vec::new();
    let (mut tw, mut th) = (0, 0);
    for i in 0..count {
        let t = lo + ((hi - lo) as f64 * (i as f64 + 0.5) / count as f64) as i64;
        let Some((j, _)) = packets
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| (p.pts - t).abs())
        else {
            continue;
        };
        let mut k = j;
        while k > 0 && !packets.get(k).is_some_and(|p| p.key) && j - k < 300 {
            k -= 1;
        }
        let mut dec = frames::Decoder::new(s, limits).map_err(|e| e.message)?;
        let mut pic = None;
        for p in packets.get(k..=j).unwrap_or_default() {
            if let Ok(Some(x)) = dec.decode(&p.data) {
                pic = Some(x);
            }
        }
        let Some(pic) = pic else {
            continue;
        };
        let scale = f64::from(max_px) / pic.w.max(pic.h).max(1) as f64;
        let (w, h) = if scale < 1.0 {
            (
                ((pic.w as f64 * scale).round() as usize).max(1),
                ((pic.h as f64 * scale).round() as usize).max(1),
            )
        } else {
            (pic.w, pic.h)
        };
        let small = if (w, h) == (pic.w, pic.h) {
            pic
        } else {
            pic.scale(w, h)
        };
        let img = oc_image::Image::new(
            w as u32,
            h as u32,
            oc_image::Layout::Rgba,
            oc_image::Depth::Eight,
            small.to_rgba(),
        )
        .map_err(|e| e.message)?;
        let png = oc_image::png::encode(&img, &oc_image::png::EncodeOptions::default()).0;
        (tw, th) = (w, h);
        items.push(format!(
            "{{\"t\":{},\"png\":\"data:image/png;base64,{}\"}}",
            crate::audio::num((packets.get(j).map_or(lo, |p| p.pts) - lo) as f64 / 1e9),
            oc_audio::tags::base64_encode(&png)
        ));
    }
    if items.is_empty() {
        return Err("no picture could be decoded".into());
    }
    Ok((si, tw, th, items))
}
