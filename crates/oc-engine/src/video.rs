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
use oc_video::{avi, codec, mkv, mp4, subs, y4m, Kind, Media, Packet};

pub const MP4: FormatId = FormatId("mp4");
pub const MOV: FormatId = FormatId("mov");
pub const MKV: FormatId = FormatId("mkv");
pub const WEBM: FormatId = FormatId("webm");
pub const AVI: FormatId = FormatId("avi");
pub const Y4M: FormatId = FormatId("y4m");
pub const MEDIA_IR: FormatId = FormatId("media-ir");

pub const FORMATS: [FormatId; 6] = [MP4, MOV, MKV, WEBM, AVI, Y4M];

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
    } else if to == MP4 || to == MOV {
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
    // Text subtitle streams the container cannot hold are rewritten in a codec it can.
    let mut rewritten: Vec<(usize, Vec<Packet>)> = Vec::new();
    for (si, s) in media.streams.iter().enumerate() {
        let drop = (s.kind == Kind::Audio && opts.bool("drop_audio") == Some(true))
            || (s.kind == Kind::Subtitle && opts.bool("drop_subtitles") == Some(true));
        if drop {
            keep.push(None);
            continue;
        }
        if container_allows(to, &s.codec) {
            keep.push(Some(out.streams.len()));
            out.streams.push(s.clone());
            continue;
        }
        let alt = [codec::SRT, codec::ASS]
            .into_iter()
            .find(|c| subs::is_text(&s.codec) && container_allows(to, c));
        if let (Some(alt), Some(track)) = (alt, subs::to_track(&media, si)) {
            let (mut ns, packets) = subs::from_track(&track, alt);
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
            keep.push(None);
            rewritten.push((out.streams.len(), packets));
            out.streams.push(ns);
            continue;
        }
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
    if out.streams.is_empty() {
        return Err(Error::new(
            ErrorCode::CodecNotInBuild,
            "none of the streams in this file can be stored in the chosen container",
        ));
    }
    for mut p in media.packets {
        if let Some(Some(n)) = keep.get(p.stream) {
            p.stream = *n;
            out.packets.push(p);
        }
    }
    for (n, packets) in rewritten {
        for mut p in packets {
            p.stream = n;
            out.packets.push(p);
        }
    }
    out.sort_by_dts();
    Ok(out)
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
            format!("the file has {} subtitle tracks; the first one was used", found.len()),
        ));
    }
    let track = subs::to_track(&media, si)
        .ok_or_else(|| Error::new(ErrorCode::InternalError, "subtitle stream vanished"))?;
    let kind = if media.streams[si].codec == codec::ASS {
        crate::subtitle::ASS
    } else {
        crate::subtitle::SRT
    };
    Ok((track, kind))
}

/// The first audio stream of a video container, re-wrapped as a Matroska audio file so the
/// audio converters can read it like any other.
pub fn extract_audio(from: FormatId, data: &[u8], limits: &Limits, notes: &mut Notes) -> Result<Vec<u8>> {
    let media = read(from, data, limits)?;
    let found: Vec<usize> = media
        .streams
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == Kind::Audio)
        .map(|(i, _)| i)
        .collect();
    let Some(&si) = found.first() else {
        return Err(Error::new(ErrorCode::UnsupportedVariant, "this file has no audio track"));
    };
    if found.len() > 1 {
        notes.push(Note::new(
            Severity::Info,
            "audio_track_chosen",
            format!("the file has {} audio tracks; the first one was used", found.len()),
        ));
    }
    let mut only = Media {
        streams: vec![media.streams[si].clone()],
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
