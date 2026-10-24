//! Audio in Matroska (`.mka`) and WebM (`.weba`, `.webm`): Opus, Vorbis, FLAC, MP3 and PCM.
//!
//! Reading rebuilds the codec's own stream (an Ogg stream for Opus and Vorbis, a FLAC file, an
//! MP3 stream) from the packets and hands it to that codec's reader. Writing does the reverse:
//! the codec's encoder runs first, then its packets are muxed, so the audio is not re-encoded
//! on the way into the container.
use crate::tags::Tags;
use crate::{check_samples, copy, flac, mp3, ogg, opus, vorbis, Audio, Picture, Samples};
use oc_container::matroska::{
    self, Attachment, AudioInfo, Matroska, Packet, SimpleTag, Tag, Track, TrackKind, WriteOptions,
};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    matroska::sniff(data)
}

/// The codec stored in the audio track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Opus,
    Vorbis,
    Flac,
    Pcm,
}

impl Codec {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "opus" => Some(Self::Opus),
            "vorbis" => Some(Self::Vorbis),
            "flac" => Some(Self::Flac),
            "pcm" | "raw" => Some(Self::Pcm),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `None` picks FLAC for Matroska and Opus for WebM.
    pub codec: Option<Codec>,
    /// Write `webm` instead of `matroska` as the document type.
    pub webm: bool,
    pub opus: opus::Options,
    pub flac: flac::Options,
    pub vorbis: vorbis::Options,
}

pub(crate) const OPUS_RATE: u64 = 48_000;

pub(crate) fn ns_to_samples(ns: i64, rate: u64) -> i64 {
    ((i128::from(ns) * i128::from(rate) + 500_000_000) / 1_000_000_000) as i64
}

pub(crate) fn samples_to_ns(n: u64, rate: u64) -> u64 {
    (u128::from(n) * 1_000_000_000 / u128::from(rate.max(1))) as u64
}

/// Samples at 48 kHz that an Opus packet decodes to.
pub(crate) fn opus_packet_samples(p: &[u8], multistream: bool) -> usize {
    match opus::parse_packet(p, multistream) {
        Ok((toc, frames, _)) => toc.frame_size * frames.len(),
        Err(_) => 0,
    }
}

// ----------------------------------------------------------------------------------- reading

pub(crate) fn xiph_unlace(p: &[u8]) -> Result<Vec<&[u8]>> {
    let bad = || Error::corrupt("bad Xiph-laced codec private data");
    let n = usize::from(*p.first().ok_or_else(bad)?) + 1;
    let mut o = 1;
    let mut sizes = Vec::new();
    for _ in 0..n - 1 {
        let mut s = 0usize;
        loop {
            let b = *p.get(o).ok_or_else(bad)?;
            o += 1;
            s += usize::from(b);
            if b != 255 {
                break;
            }
        }
        sizes.push(s);
    }
    let mut out = Vec::new();
    for s in sizes {
        out.push(p.get(o..o + s).ok_or_else(bad)?);
        o += s;
    }
    out.push(p.get(o..).ok_or_else(bad)?);
    Ok(out)
}

pub(crate) fn xiph_lace(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![(parts.len() - 1) as u8];
    for p in &parts[..parts.len() - 1] {
        let mut n = p.len();
        while n >= 255 {
            out.push(255);
            n -= 255;
        }
        out.push(n as u8);
    }
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

pub(crate) fn tags_from(m: &Matroska, track_uid: u64) -> Tags {
    let mut tags = Tags::default();
    fn walk(prefix: &str, st: &SimpleTag, tags: &mut Tags) {
        let name = if prefix.is_empty() {
            st.name.clone()
        } else {
            format!("{prefix}/{}", st.name)
        };
        if let Some(v) = &st.value {
            if !name.is_empty() {
                tags.add(&name, v.clone());
            }
        }
        for c in &st.children {
            walk(&name, c, tags);
        }
    }
    // Album-level tags first, then those aimed at this track, so that track values come last.
    let mut sel: Vec<&Tag> = m
        .tags
        .iter()
        .filter(|t| t.track_uids.is_empty() || t.track_uids.contains(&track_uid))
        .collect();
    sel.sort_by_key(|t| {
        (
            !t.track_uids.is_empty(),
            std::cmp::Reverse(t.target_type_value),
        )
    });
    for t in sel {
        for st in &t.simple {
            walk("", st, &mut tags);
        }
    }
    tags
}

pub(crate) fn pictures_from(m: &Matroska) -> Vec<Picture> {
    m.attachments
        .iter()
        .filter(|a| a.mime.starts_with("image/"))
        .map(|a| Picture {
            mime: a.mime.clone(),
            kind: if a.name.to_ascii_lowercase().starts_with("cover") {
                3
            } else {
                0
            },
            description: a.description.clone(),
            data: a.data.clone(),
        })
        .collect()
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    let m = matroska::parse(data, limits)?;
    let track = m
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio && t.enabled)
        .or_else(|| m.tracks.iter().find(|t| t.kind == TrackKind::Audio))
        .ok_or_else(|| Error::new(ErrorCode::UnsupportedFormat, "no audio track in this file"))?;
    let pk: Vec<&Packet> = m.packets_of(track.number).collect();
    let mut notes = Notes::new();
    if m.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .count()
        > 1
    {
        notes.push(Note::new(
            Severity::Info,
            "mka.first_audio_track",
            "the file has several audio tracks; the first one was converted",
        ));
    }
    let info = track.audio.clone().unwrap_or_default();
    let (mut audio, inner) = match track.codec_id.as_str() {
        "A_OPUS" => opus_from_packets(track, &pk, limits)?,
        "A_VORBIS" => vorbis_from_packets(track, &pk, &info, limits)?,
        "A_FLAC" => flac_from_packets(track, &pk, limits)?,
        "A_MPEG/L3" => {
            let mut all = Vec::new();
            for p in &pk {
                all.extend_from_slice(&p.data);
            }
            mp3::read(&all, limits)?
        }
        id if id.starts_with("A_PCM/") => (pcm_from_packets(id, &info, &pk, limits)?, Notes::new()),
        other if other.starts_with("A_AAC") || other.starts_with("A_AC3") || other.starts_with("A_EAC3") => {
            return Err(Error::new(
                ErrorCode::CodecNotInBuild,
                "this audio is AAC or AC-3, codecs that may be covered by patents and are not in this build; remux it or use FLAC or Opus instead",
            ))
        }
        other => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!("Matroska audio codec {other} is not supported"),
            ))
        }
    };
    for n in inner.iter() {
        notes.push(n.clone());
    }
    let t = tags_from(&m, track.uid);
    if !t.is_empty() {
        audio.tags = t;
    }
    let pics = pictures_from(&m);
    if !pics.is_empty() {
        audio.pictures = pics;
    }
    Ok((audio, notes))
}

fn opus_from_packets(track: &Track, pk: &[&Packet], limits: &Limits) -> Result<(Audio, Notes)> {
    let head = opus::parse_head(&track.codec_private)?;
    let multistream = head.streams > 1;
    let mut w = ogg::Writer::new(1);
    w.packet(&track.codec_private, 0, true, false);
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags.extend_from_slice(&0u32.to_le_bytes());
    w.packet(&tags, 0, true, false);
    let mut total = 0i64;
    let sizes: Vec<usize> = pk
        .iter()
        .map(|p| opus_packet_samples(&p.data, multistream))
        .collect();
    let sum: i64 = sizes.iter().map(|&s| s as i64).sum();
    let discard = pk
        .last()
        .map_or(0, |p| ns_to_samples(p.discard_padding_ns.max(0), OPUS_RATE));
    for (i, p) in pk.iter().enumerate() {
        total += sizes[i] as i64;
        let last = i + 1 == pk.len();
        let granule = head.pre_skip as i64 + if last { sum - discard } else { total };
        w.packet(&p.data, granule.max(0), false, last);
        check_samples(total as u64, limits)?;
    }
    if pk.is_empty() {
        w.packet(&[], head.pre_skip as i64, true, true);
    }
    opus::read(&w.finish(), limits)
}

fn vorbis_from_packets(
    track: &Track,
    pk: &[&Packet],
    info: &AudioInfo,
    limits: &Limits,
) -> Result<(Audio, Notes)> {
    let hdr = xiph_unlace(&track.codec_private)?;
    if hdr.len() != 3 {
        return Err(Error::corrupt("Vorbis codec private data needs 3 headers"));
    }
    let datas: Vec<&[u8]> = pk.iter().map(|p| p.data.as_slice()).collect();
    let durs = vorbis::packet_durations(hdr[0], hdr[2], &datas)?;
    let sum: usize = durs.iter().sum();
    let mut w = ogg::Writer::new(1);
    w.packet(hdr[0], 0, true, false);
    w.packet(hdr[1], 0, true, false);
    w.packet(hdr[2], 0, true, false);
    // DiscardPadding on the last packet says how much of the final block is not audio.
    let rate = info.sample_rate.max(1.0) as u64;
    let discard = pk.last().map_or(0, |p| {
        ns_to_samples(p.discard_padding_ns.max(0), rate) as usize
    });
    let want = sum.saturating_sub(discard);
    let mut total = 0usize;
    for (i, p) in pk.iter().enumerate() {
        total += durs[i];
        let last = i + 1 == pk.len();
        w.packet(&p.data, if last { want } else { total } as i64, false, last);
    }
    vorbis::read(&w.finish(), limits)
}

fn flac_from_packets(track: &Track, pk: &[&Packet], limits: &Limits) -> Result<(Audio, Notes)> {
    let mut out = Vec::new();
    let cp = &track.codec_private;
    if cp.starts_with(b"fLaC") {
        out.extend_from_slice(cp);
    } else if cp.len() == 34 {
        out.extend_from_slice(b"fLaC\x80\x00\x00\x22");
        out.extend_from_slice(cp);
    } else {
        return Err(Error::corrupt("FLAC track without STREAMINFO"));
    }
    for p in pk {
        out.extend_from_slice(&p.data);
    }
    flac::read(&out, limits)
}

fn pcm_from_packets(
    codec: &str,
    info: &AudioInfo,
    pk: &[&Packet],
    limits: &Limits,
) -> Result<Audio> {
    let ch = info.channels.max(1) as u16;
    let bits = info.bit_depth.unwrap_or(16);
    let rate = info.sample_rate as u32;
    if rate == 0 {
        return Err(Error::corrupt("PCM track without a sample rate"));
    }
    let mut raw = Vec::new();
    for p in pk {
        raw.extend_from_slice(&p.data);
    }
    let width = (bits as usize).div_ceil(8);
    let samples = match codec {
        "A_PCM/FLOAT/IEEE" => {
            let v: Vec<f32> = match bits {
                32 => raw
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect(),
                64 => raw
                    .chunks_exact(8)
                    .map(|c| f64::from_le_bytes(c.try_into().unwrap_or([0; 8])) as f32)
                    .collect(),
                _ => {
                    return Err(Error::new(
                        ErrorCode::UnsupportedVariant,
                        "float PCM must be 32 or 64 bit",
                    ))
                }
            };
            Samples::Float(v)
        }
        "A_PCM/INT/LIT" | "A_PCM/INT/BIG" => {
            if !(8..=32).contains(&bits) {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    format!("{bits}-bit PCM is not supported"),
                ));
            }
            let big = codec.ends_with("BIG");
            let v: Vec<i32> = raw
                .chunks_exact(width)
                .map(|c| {
                    let mut x = 0u32;
                    if big {
                        for &b in c {
                            x = (x << 8) | u32::from(b);
                        }
                    } else {
                        for &b in c.iter().rev() {
                            x = (x << 8) | u32::from(b);
                        }
                    }
                    if bits == 8 {
                        // 8-bit Matroska PCM is unsigned
                        i32::from(x as u8) - 128
                    } else {
                        let sh = 32 - 8 * width as u32;
                        ((x << sh) as i32) >> sh
                    }
                })
                .collect();
            Samples::Int {
                bits: (8 * width) as u8,
                data: v,
            }
        }
        other => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!("Matroska audio codec {other} is not supported"),
            ))
        }
    };
    check_samples(samples.len() as u64, limits)?;
    let mut a = Audio::new(rate, ch, samples);
    let keep = a.frames() * usize::from(ch);
    match &mut a.samples {
        Samples::Int { data, .. } => data.truncate(keep),
        Samples::Float(d) => d.truncate(keep),
    }
    Ok(a)
}

// ----------------------------------------------------------------------------------- writing

pub(crate) fn tag_blocks(tags: &Tags) -> Vec<Tag> {
    if tags.is_empty() {
        return Vec::new();
    }
    vec![Tag {
        target_type_value: 30,
        target_type: "TRACK".into(),
        track_uids: Vec::new(),
        simple: tags
            .fields
            .iter()
            .map(|(k, v)| SimpleTag {
                name: k.clone(),
                language: "und".into(),
                value: Some(v.clone()),
                binary: None,
                children: Vec::new(),
            })
            .collect(),
    }]
}

pub(crate) fn attachments_from(pics: &[Picture]) -> Vec<Attachment> {
    pics.iter()
        .enumerate()
        .map(|(i, p)| {
            let ext = match p.mime.as_str() {
                "image/png" => "png",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => "jpg",
            };
            let name = if p.kind == 3 && i == 0 {
                format!("cover.{ext}")
            } else {
                format!("image{i}.{ext}")
            };
            Attachment {
                uid: 0x1000 + i as u64,
                name,
                mime: p.mime.clone(),
                description: p.description.clone(),
                data: p.data.clone(),
            }
        })
        .collect()
}

pub fn write(audio: &Audio, opts: &Options, notes: &mut Notes) -> Result<Vec<u8>> {
    let codec = opts
        .codec
        .unwrap_or(if opts.webm { Codec::Opus } else { Codec::Flac });
    if opts.webm && !matches!(codec, Codec::Opus | Codec::Vorbis) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "WebM audio holds Opus or Vorbis only",
        ));
    }
    let mut cs = match codec {
        Codec::Opus => copy::CopyStream::from_ogg(&opus::write(audio, &opts.opus, notes)?)?,
        Codec::Vorbis => copy::CopyStream::from_ogg(&vorbis::write(audio, &opts.vorbis, notes)?)?,
        Codec::Flac => copy::CopyStream::from_flac(&flac::write(audio, &opts.flac, notes)?)?,
        Codec::Pcm => {
            let mut m = base(opts.webm);
            let mut track = Track::new(1, TrackKind::Audio, "A_PCM/INT/LIT");
            let (packets, total_ns) = mux_pcm(audio, &mut track)?;
            m.tracks.push(track);
            m.packets = packets;
            m.duration_ns = Some(total_ns);
            finish(&mut m, &audio.tags, &audio.pictures, opts.webm, notes);
            return matroska::write(&m, &WriteOptions::default());
        }
    };
    cs.tags = audio.tags.clone();
    cs.pictures = audio.pictures.clone();
    cs.to_mka(opts.webm, notes)
}

pub(crate) fn base(webm: bool) -> Matroska {
    Matroska {
        doc_type: if webm { "webm" } else { "matroska" }.into(),
        muxing_app: "OpenConverter".into(),
        writing_app: "OpenConverter".into(),
        ..Matroska::default()
    }
}

/// Tags and cover art; WebM has no place for pictures.
pub(crate) fn finish(
    m: &mut Matroska,
    tags: &Tags,
    pics: &[Picture],
    webm: bool,
    notes: &mut Notes,
) {
    m.tags = tag_blocks(tags);
    if !pics.is_empty() {
        if webm {
            notes.push(Note::new(
                Severity::Warning,
                "mka.webm_no_pictures",
                "WebM has no place for cover art; the pictures were left out",
            ));
        } else {
            m.attachments = attachments_from(pics);
        }
    }
}

fn mux_pcm(audio: &Audio, track: &mut Track) -> Result<(Vec<Packet>, u64)> {
    let ch = usize::from(audio.channels.max(1));
    let rate = u64::from(audio.sample_rate);
    let (codec, bits, bytes): (&str, u32, Vec<u8>) = match &audio.samples {
        Samples::Int { bits, data } => {
            let width = usize::from(*bits).div_ceil(8).max(1);
            let shift = (8 * width as u32).saturating_sub(u32::from(*bits));
            let mut b = Vec::with_capacity(data.len() * width);
            for &v in data {
                if width == 1 {
                    b.push((v + 128).clamp(0, 255) as u8);
                } else {
                    let x = (v << shift).to_le_bytes();
                    b.extend_from_slice(&x[..width]);
                }
            }
            ("A_PCM/INT/LIT", 8 * width as u32, b)
        }
        Samples::Float(d) => {
            let mut b = Vec::with_capacity(d.len() * 4);
            for &v in d {
                b.extend_from_slice(&v.to_le_bytes());
            }
            ("A_PCM/FLOAT/IEEE", 32, b)
        }
    };
    track.codec_id = codec.into();
    track.audio = Some(AudioInfo {
        sample_rate: rate as f64,
        output_sample_rate: None,
        channels: ch as u32,
        bit_depth: Some(bits),
    });
    let frame_bytes = ch * (bits as usize / 8);
    let chunk_frames = (rate / 50).max(1) as usize;
    let mut out = Vec::new();
    let mut pos = 0u64;
    for c in bytes.chunks(chunk_frames * frame_bytes) {
        let n = (c.len() / frame_bytes) as u64;
        out.push(Packet {
            track: 1,
            pts_ns: samples_to_ns(pos, rate) as i64,
            duration_ns: Some(samples_to_ns(n, rate)),
            keyframe: true,
            discard_padding_ns: 0,
            data: c.to_vec(),
        });
        pos += n;
    }
    Ok((out, samples_to_ns(pos, rate)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, ch: u16, secs: f32) -> Audio {
        let n = (rate as f32 * secs) as usize;
        let mut d = Vec::new();
        for i in 0..n {
            for c in 0..ch {
                let t = i as f32 / rate as f32;
                d.push(
                    (((t * 440.0 * (1.0 + c as f32 * 0.5)) * std::f32::consts::TAU).sin() * 12000.0)
                        as i32,
                );
            }
        }
        let mut a = Audio::new(rate, ch, Samples::Int { bits: 16, data: d });
        a.tags.add("TITLE", "Tone");
        a.tags.add("ARTIST", "OC");
        a.pictures.push(Picture {
            mime: "image/png".into(),
            kind: 3,
            description: "c".into(),
            data: vec![1, 2, 3, 4],
        });
        a
    }

    fn lim() -> Limits {
        Limits::default()
    }

    #[test]
    fn flac_and_pcm_in_matroska_are_lossless() {
        for codec in [Codec::Flac, Codec::Pcm] {
            let a = tone(44100, 2, 0.5);
            let mut notes = Notes::new();
            let bytes = write(
                &a,
                &Options {
                    codec: Some(codec),
                    ..Options::default()
                },
                &mut notes,
            )
            .unwrap();
            assert!(sniff(&bytes));
            let (b, _) = read(&bytes, &lim()).unwrap();
            assert_eq!(b.sample_rate, 44100);
            assert_eq!(b.channels, 2);
            assert_eq!(b.samples, a.samples, "{codec:?}");
            assert_eq!(b.tags.get("TITLE"), Some("Tone"));
            assert_eq!(b.pictures.len(), 1);
            assert_eq!(b.pictures[0].data, vec![1, 2, 3, 4]);
        }
    }

    #[test]
    fn opus_and_vorbis_in_webm_keep_length() {
        for codec in [Codec::Opus, Codec::Vorbis] {
            let a = tone(48000, 2, 0.6);
            let mut notes = Notes::new();
            let bytes = write(
                &a,
                &Options {
                    codec: Some(codec),
                    webm: true,
                    ..Options::default()
                },
                &mut notes,
            )
            .unwrap();
            let (b, _) = read(&bytes, &lim()).unwrap();
            assert_eq!(b.channels, 2);
            assert_eq!(b.frames(), a.frames(), "{codec:?}");
            assert_eq!(b.tags.get("ARTIST"), Some("OC"));
            assert!(b.pictures.is_empty());
        }
    }

    #[test]
    fn webm_refuses_flac() {
        let a = tone(44100, 1, 0.1);
        let mut notes = Notes::new();
        let e = write(
            &a,
            &Options {
                codec: Some(Codec::Flac),
                webm: true,
                ..Options::default()
            },
            &mut notes,
        );
        assert!(e.is_err());
    }
}
