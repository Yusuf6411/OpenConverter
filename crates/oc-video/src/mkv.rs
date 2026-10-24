//! Matroska and WebM as [`Media`].
use crate::{
    assign_dts, codec, estimate_fps, fps_from_ns, frame_ns, unsupported, Kind, Media, Packet, Res,
    Stream,
};
use oc_container::matroska::{
    self, AudioInfo, Matroska, Track, TrackKind, VideoInfo, WriteOptions,
};
use oc_core::{Error, ErrorCode, Limits};
use std::collections::HashMap;

pub fn sniff(data: &[u8]) -> bool {
    matroska::sniff(data)
}

fn codec_from_id(id: &str, private: &[u8]) -> Option<(String, u16)> {
    let c = |s: &str| Some((s.to_string(), 0u16));
    match id {
        "V_MPEG4/ISO/AVC" => c(codec::H264),
        "V_MPEGH/ISO/HEVC" => c(codec::HEVC),
        "V_VP8" => c(codec::VP8),
        "V_THEORA" => c(codec::THEORA),
        "V_VP9" => c(codec::VP9),
        "V_AV1" => c(codec::AV1),
        "V_MJPEG" => c(codec::MJPEG),
        "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/AP" => c(codec::MPEG4),
        // Video for Windows: the FourCC sits in the BITMAPINFOHEADER at offset 16.
        "V_MS/VFW/FOURCC" => match private.get(16..20) {
            Some(b"MJPG" | b"mjpg") => c(codec::MJPEG),
            _ => None,
        },
        "A_AAC" | "A_AAC/MPEG4/LC" | "A_AAC/MPEG2/LC" => c(codec::AAC),
        "A_OPUS" => c(codec::OPUS),
        "A_VORBIS" => c(codec::VORBIS),
        "A_FLAC" => c(codec::FLAC),
        "A_MPEG/L3" => c(codec::MP3),
        "A_AC3" => c(codec::AC3),
        "A_EAC3" => c(codec::EAC3),
        "A_PCM/INT/LIT" => Some((String::new(), 1)),
        "A_PCM/FLOAT/IEEE" => c(codec::PCM_F32),
        "S_TEXT/UTF8" => c(codec::SRT),
        "S_TEXT/ASS" | "S_TEXT/SSA" => c(codec::ASS),
        "S_TEXT/WEBVTT" => c(codec::WEBVTT),
        _ => None,
    }
}

fn id_for(codec: &str) -> Option<&'static str> {
    Some(match codec {
        codec::H264 => "V_MPEG4/ISO/AVC",
        codec::HEVC => "V_MPEGH/ISO/HEVC",
        codec::VP8 => "V_VP8",
        codec::THEORA => "V_THEORA",
        codec::VP9 => "V_VP9",
        codec::AV1 => "V_AV1",
        codec::MJPEG => "V_MJPEG",
        codec::MPEG4 => "V_MPEG4/ISO/ASP",
        codec::AAC => "A_AAC",
        codec::OPUS => "A_OPUS",
        codec::VORBIS => "A_VORBIS",
        codec::FLAC => "A_FLAC",
        codec::MP3 => "A_MPEG/L3",
        codec::AC3 => "A_AC3",
        codec::EAC3 => "A_EAC3",
        codec::PCM_S16 | codec::PCM_S24 | codec::PCM_S32 | codec::PCM_U8 => "A_PCM/INT/LIT",
        codec::PCM_F32 => "A_PCM/FLOAT/IEEE",
        codec::SRT => "S_TEXT/UTF8",
        codec::ASS => "S_TEXT/ASS",
        codec::WEBVTT => "S_TEXT/WEBVTT",
        _ => return None,
    })
}

/// Codecs a WebM file may hold.
pub fn webm_allows(codec: &str) -> bool {
    matches!(
        codec,
        codec::VP8 | codec::VP9 | codec::AV1 | codec::OPUS | codec::VORBIS | codec::WEBVTT
    )
}

/// Codecs the Matroska writer can describe.
pub fn mkv_allows(codec: &str) -> bool {
    id_for(codec).is_some()
}

pub fn read(data: &[u8], limits: &Limits) -> Res<Media> {
    let m = matroska::parse(data, limits)?;
    let mut media = Media {
        title: m.title.clone(),
        source: m.doc_type.clone(),
        ..Media::default()
    };
    let mut map: HashMap<u64, usize> = HashMap::new();
    // Opus: a block's time is that of the first decoded sample, `CodecDelay` before the first
    // sample that is shown. Presentation times in the model are the shown ones.
    let mut delay: Vec<i64> = Vec::new();
    let mut skipped = 0usize;
    for t in &m.tracks {
        let kind = match t.kind {
            TrackKind::Video => Kind::Video,
            TrackKind::Audio => Kind::Audio,
            TrackKind::Subtitle => Kind::Subtitle,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let Some((mut name, pcm)) = codec_from_id(&t.codec_id, &t.codec_private) else {
            skipped += 1;
            continue;
        };
        let bits = t.audio.as_ref().and_then(|a| a.bit_depth).unwrap_or(16);
        if pcm == 1 {
            name = match bits {
                8 => codec::PCM_U8,
                24 => codec::PCM_S24,
                32 => codec::PCM_S32,
                _ => codec::PCM_S16,
            }
            .to_string();
        }
        let mut s = Stream::new(kind, &name);
        s.private = t.codec_private.clone();
        s.language = t.language.clone();
        s.name = t.name.clone();
        s.default = t.default;
        s.forced = t.forced;
        if let Some(v) = &t.video {
            s.width = v.width;
            s.height = v.height;
        }
        if let Some(a) = &t.audio {
            s.sample_rate = a.sample_rate.round() as u32;
            s.channels = a.channels as u16;
            s.bits = a.bit_depth.unwrap_or(0) as u16;
        }
        if kind == Kind::Video {
            s.fps = t.default_duration_ns.and_then(|d| fps_from_ns(d as i64));
        }
        delay.push(if name == codec::OPUS {
            t.codec_delay_ns as i64
        } else {
            0
        });
        map.insert(t.number, media.streams.len());
        media.streams.push(s);
    }
    // Presentation times come from the file; decode times are derived per stream.
    let mut per_stream: Vec<Vec<usize>> = vec![Vec::new(); media.streams.len()];
    for p in &m.packets {
        let Some(&si) = map.get(&p.track) else {
            continue;
        };
        per_stream[si].push(media.packets.len());
        media.packets.push(Packet {
            stream: si,
            pts: p.pts_ns - delay[si],
            dts: p.pts_ns - delay[si],
            duration: p.duration_ns.unwrap_or(0) as i64,
            key: p.keyframe,
            discard_end: p.discard_padding_ns.max(0),
            data: p.data.clone(),
        });
    }
    for (si, idx) in per_stream.iter().enumerate() {
        if media.streams[si].kind != Kind::Video {
            continue;
        }
        let pts: Vec<i64> = idx.iter().map(|&i| media.packets[i].pts).collect();
        for (&i, d) in idx.iter().zip(assign_dts(&pts)) {
            media.packets[i].dts = d;
        }
    }
    for si in 0..media.streams.len() {
        if media.streams[si].kind == Kind::Video && media.streams[si].fps.is_none() {
            // No DefaultDuration: estimate from the spacing of presentation times.
            let pts: Vec<i64> = media.packets_of(si).map(|p| p.pts).collect();
            media.streams[si].fps = estimate_fps(&pts);
        }
    }
    media.fill_durations();
    if media.streams.is_empty() {
        let why = if skipped > 0 {
            "no track in this file uses a codec this build can carry"
        } else {
            "no tracks"
        };
        return Err(Error::new(ErrorCode::UnsupportedVariant, why));
    }
    Ok(media)
}

pub fn write(media: &Media, webm: bool, app: &str) -> Res<Vec<u8>> {
    let mut m = Matroska {
        doc_type: if webm { "webm" } else { "matroska" }.into(),
        timestamp_scale: 1_000_000,
        title: media.title.clone(),
        muxing_app: app.into(),
        writing_app: app.into(),
        ..Matroska::default()
    };
    for (i, s) in media.streams.iter().enumerate() {
        let id = id_for(&s.codec)
            .ok_or_else(|| unsupported(format!("{} cannot go into Matroska", s.codec)))?;
        if webm && !webm_allows(&s.codec) {
            return Err(unsupported(format!("{} is not allowed in WebM", s.codec)));
        }
        let kind = match s.kind {
            Kind::Video => TrackKind::Video,
            Kind::Audio => TrackKind::Audio,
            Kind::Subtitle => TrackKind::Subtitle,
        };
        let mut t = Track::new(i as u64 + 1, kind, id);
        t.codec_private = s.private.clone();
        t.language = s.language.clone();
        t.name = s.name.clone();
        t.default = s.default;
        t.forced = s.forced;
        match s.kind {
            Kind::Video => {
                t.video = Some(VideoInfo {
                    width: s.width,
                    height: s.height,
                    display_width: None,
                    display_height: None,
                });
                t.default_duration_ns = s.fps.map(|f| frame_ns(f) as u64);
            }
            Kind::Audio => {
                t.audio = Some(AudioInfo {
                    sample_rate: f64::from(s.sample_rate),
                    output_sample_rate: None,
                    channels: u32::from(s.channels),
                    bit_depth: match s.codec.as_str() {
                        codec::PCM_U8 => Some(8),
                        codec::PCM_S16 => Some(16),
                        codec::PCM_S24 => Some(24),
                        codec::PCM_S32 | codec::PCM_F32 => Some(32),
                        _ => None,
                    },
                });
                if s.codec == codec::OPUS {
                    let pre = s
                        .private
                        .get(10..12)
                        .map_or(0, |b| u64::from(u16::from_le_bytes([b[0], b[1]])));
                    t.codec_delay_ns = pre * 1_000_000_000 / 48_000;
                    t.seek_pre_roll_ns = 80_000_000;
                }
            }
            Kind::Subtitle => {}
        }
        m.tracks.push(t);
    }
    let pre: Vec<i64> = media
        .streams
        .iter()
        .map(|s| {
            if s.codec == codec::OPUS {
                s.private.get(10..12).map_or(0, |b| {
                    i64::from(u16::from_le_bytes([b[0], b[1]])) * 1_000_000_000 / 48_000
                })
            } else {
                0
            }
        })
        .collect();
    for p in &media.packets {
        let s = media
            .streams
            .get(p.stream)
            .ok_or_else(|| crate::corrupt("packet for unknown stream"))?;
        m.packets.push(matroska::Packet {
            track: p.stream as u64 + 1,
            pts_ns: p.pts + pre[p.stream],
            duration_ns: (s.kind == Kind::Subtitle || p.duration > 0)
                .then_some(p.duration.max(0) as u64),
            keyframe: p.key || s.kind != Kind::Video,
            discard_padding_ns: p.discard_end,
            data: p.data.clone(),
        });
    }
    matroska::write(
        &m,
        &WriteOptions {
            writing_app: app.into(),
            ..WriteOptions::default()
        },
    )
}
