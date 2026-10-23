//! Video container formats: registration, option schemas and the remux/transcode planner.
//!
//! Every container connects to a hub (`media-ir`). A conversion reads the container into a
//! packet-level [`Media`], decides per stream whether to copy it or re-encode it, and writes the
//! target container.
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Limits, Note, Notes,
    Ownership, Result, Severity, Sniffer,
};
use oc_video::{codec, mkv, mp4, Kind, Media};

pub const MP4: FormatId = FormatId("mp4");
pub const MOV: FormatId = FormatId("mov");
pub const MKV: FormatId = FormatId("mkv");
pub const WEBM: FormatId = FormatId("webm");
pub const MEDIA_IR: FormatId = FormatId("media-ir");

pub const FORMATS: [FormatId; 4] = [MP4, MOV, MKV, WEBM];

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

fn options_for(id: FormatId) -> OptionSchema {
    let mut specs = vec![
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
    ];
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
            "ISO/IEC 14496-12 and 14496-14 (stream copy; VP8/VP9/AV1/MJPEG, AAC, MP3, Opus, FLAC)",
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
            WEBM,
            "WebM",
            &["webm"],
            &["video/webm"],
            vec![Sniffer { offset: 0, magic: &[0x1A, 0x45, 0xDF, 0xA3], confidence: 40 }],
            "WebM profile of Matroska (VP8/VP9/AV1, Opus/Vorbis)",
        ),
    ];
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
                write: true,
                ..Capabilities::default()
            },
            writer_options: options_for(id),
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
    if mp4::sniff(data) {
        return Some(if data.get(8..12) == Some(b"qt  ") {
            MOV
        } else {
            MP4
        });
    }
    match oc_container::matroska::doc_type(data).as_deref() {
        Some("webm") => Some(WEBM),
        Some(_) => Some(MKV),
        None => None,
    }
}

pub fn read(from: FormatId, data: &[u8], limits: &Limits) -> Result<Media> {
    if from == MP4 || from == MOV {
        mp4::read(data, limits)
    } else {
        mkv::read(data, limits)
    }
}

/// Whether container `to` can hold a stream of this codec as is.
pub fn container_allows(to: FormatId, c: &str) -> bool {
    if to == WEBM {
        mkv::webm_allows(c)
    } else if to == MKV {
        mkv::mkv_allows(c)
    } else {
        mp4::allows(c, to == MOV)
    }
}

pub fn write(to: FormatId, media: &Media, opts: &Options) -> Result<Vec<u8>> {
    if to == MP4 || to == MOV {
        mp4::write(
            media,
            &mp4::Options {
                mov: to == MOV,
                faststart: opts.bool("faststart").unwrap_or(true),
            },
        )
    } else {
        mkv::write(media, to == WEBM, "OpenConverter")
    }
}

/// Decide what happens to each stream and build the media to write.
pub fn retarget(media: Media, to: FormatId, opts: &Options, notes: &mut Notes) -> Result<Media> {
    let mut keep: Vec<Option<usize>> = Vec::new();
    let mut out = Media {
        title: media.title.clone(),
        source: media.source.clone(),
        ..Media::default()
    };
    for s in &media.streams {
        let drop = (s.kind == Kind::Audio && opts.bool("drop_audio") == Some(true))
            || (s.kind == Kind::Subtitle && opts.bool("drop_subtitles") == Some(true));
        if drop {
            keep.push(None);
            continue;
        }
        if container_allows(to, &s.codec) {
            keep.push(Some(out.streams.len()));
            out.streams.push(s.clone());
        } else {
            keep.push(None);
            notes.push(
                Note::new(
                    Severity::Warning,
                    "stream_dropped",
                    format!(
                        "a {} stream ({}) cannot be stored in this container and was left out",
                        match s.kind {
                            Kind::Video => "video",
                            Kind::Audio => "audio",
                            Kind::Subtitle => "subtitle",
                        },
                        s.codec
                    ),
                )
                .with_detail(s.codec.clone()),
            );
        }
    }
    if out.streams.is_empty() {
        return Err(Error::new(
            ErrorCode::CodecNotInBuild,
            "none of the streams in this file can be stored in the chosen container",
        ));
    }
    for p in media.packets {
        if let Some(Some(n)) = keep.get(p.stream) {
            let mut p = p;
            p.stream = *n;
            out.packets.push(p);
        }
    }
    let _ = codec::H264;
    Ok(out)
}

pub fn convert(
    from: FormatId,
    to: FormatId,
    data: &[u8],
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let media = read(from, data, limits)?;
    let media = retarget(media, to, opts, notes)?;
    write(to, &media, opts)
}
