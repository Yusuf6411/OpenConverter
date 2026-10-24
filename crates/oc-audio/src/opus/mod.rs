//! Opus (RFC 6716) in Ogg (RFC 7845). The CELT layer is our own implementation; SILK and
//! hybrid frames are not decoded yet (see docs/formats/opus.md).
// The band and range-coder code follows the reference's integer arithmetic closely, shift
// expressions included, so that it is easy to check against RFC 6716.
#![allow(clippy::precedence)]
pub mod bands;
pub mod celt;
pub mod enc;
pub mod mdct;
pub mod pvq;
pub mod rc;
pub mod tables;

use crate::tags::parse_vorbis_comment;
use crate::{check_samples, ogg, Audio, Samples};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

pub use enc::{write, Options, RateMode};

pub fn sniff(data: &[u8]) -> bool {
    ogg::sniff(data)
        && data.len() > 40
        && data[..200.min(data.len())]
            .windows(8)
            .any(|w| w == b"OpusHead")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Silk,
    Hybrid,
    Celt,
}

/// What the table-of-contents byte says about a packet's frames.
#[derive(Debug, Clone, Copy)]
pub struct Toc {
    pub mode: Mode,
    /// 0 narrowband, 1 mediumband, 2 wideband, 3 super-wideband, 4 fullband.
    pub bandwidth: u8,
    /// Samples per frame at 48 kHz.
    pub frame_size: usize,
    pub stereo: bool,
}

pub fn parse_toc(b: u8) -> Toc {
    let config = b >> 3;
    let stereo = b & 4 != 0;
    let (mode, bandwidth, frame_size) = match config {
        0..=11 => (
            Mode::Silk,
            config / 4,
            [480, 960, 1920, 2880][usize::from(config & 3)],
        ),
        12..=15 => (
            Mode::Hybrid,
            3 + (config - 12) / 2,
            [480, 960][usize::from(config & 1)],
        ),
        _ => {
            let c = config - 16;
            let bw = match c / 4 {
                0 => 0,
                1 => 2,
                2 => 3,
                _ => 4,
            };
            (Mode::Celt, bw, [120, 240, 480, 960][usize::from(c & 3)])
        }
    };
    Toc {
        mode,
        bandwidth,
        frame_size,
        stereo,
    }
}

fn frame_len(b: &[u8], at: &mut usize) -> Result<usize> {
    let first = *b
        .get(*at)
        .ok_or_else(|| Error::truncated("Opus packet ends in a length field"))?;
    *at += 1;
    if first < 252 {
        return Ok(usize::from(first));
    }
    let second = *b
        .get(*at)
        .ok_or_else(|| Error::truncated("Opus packet ends in a length field"))?;
    *at += 1;
    Ok(usize::from(second) * 4 + usize::from(first))
}

/// Split a packet into frames. With `self_delimited`, the last frame's length is coded too
/// (multistream packets); the second value is the number of bytes the packet used.
pub fn parse_packet(p: &[u8], self_delimited: bool) -> Result<(Toc, Vec<&[u8]>, usize)> {
    let toc_byte = *p
        .first()
        .ok_or_else(|| Error::truncated("empty Opus packet"))?;
    let toc = parse_toc(toc_byte);
    let code = toc_byte & 3;
    let mut at = 1usize;
    let mut sizes: Vec<usize> = Vec::new();
    let mut padding = 0usize;
    let corrupt = |m: &str| Error::corrupt(format!("invalid Opus packet: {m}"));
    match code {
        0 => {
            if self_delimited {
                sizes.push(frame_len(p, &mut at)?);
            } else {
                sizes.push(p.len() - 1);
            }
        }
        1 => {
            if self_delimited {
                let l = frame_len(p, &mut at)?;
                sizes = vec![l, l];
            } else {
                if !(p.len() - 1).is_multiple_of(2) {
                    return Err(corrupt("odd length for two equal frames"));
                }
                sizes = vec![(p.len() - 1) / 2; 2];
            }
        }
        2 => {
            let a = frame_len(p, &mut at)?;
            if self_delimited {
                let b = frame_len(p, &mut at)?;
                sizes = vec![a, b];
            } else {
                let rest = p
                    .len()
                    .checked_sub(at + a)
                    .ok_or_else(|| corrupt("first frame overruns"))?;
                sizes = vec![a, rest];
            }
        }
        _ => {
            let ctl = *p
                .get(at)
                .ok_or_else(|| Error::truncated("Opus packet ends in its frame count"))?;
            at += 1;
            let vbr = ctl & 0x80 != 0;
            let has_padding = ctl & 0x40 != 0;
            let count = usize::from(ctl & 0x3F);
            if count == 0 || count * toc.frame_size > 5760 {
                return Err(corrupt("bad frame count"));
            }
            if has_padding {
                loop {
                    let b = *p.get(at).ok_or_else(|| Error::truncated("Opus padding"))?;
                    at += 1;
                    if b == 255 {
                        padding += 254;
                    } else {
                        padding += usize::from(b);
                        break;
                    }
                }
            }
            if vbr {
                let upto = if self_delimited { count } else { count - 1 };
                for _ in 0..upto {
                    sizes.push(frame_len(p, &mut at)?);
                }
                if !self_delimited {
                    let used: usize = sizes.iter().sum();
                    let rest = p
                        .len()
                        .checked_sub(at + padding + used)
                        .ok_or_else(|| corrupt("frames overrun"))?;
                    sizes.push(rest);
                }
            } else if self_delimited {
                let l = frame_len(p, &mut at)?;
                sizes = vec![l; count];
            } else {
                let body = p
                    .len()
                    .checked_sub(at + padding)
                    .ok_or_else(|| corrupt("padding overruns"))?;
                if body % count != 0 {
                    return Err(corrupt("CBR frames do not divide the packet"));
                }
                sizes = vec![body / count; count];
            }
        }
    }
    let mut frames = Vec::with_capacity(sizes.len());
    for s in sizes {
        if s > 1275 {
            return Err(corrupt("frame longer than 1275 bytes"));
        }
        let f = p
            .get(at..at + s)
            .ok_or_else(|| corrupt("frame overruns the packet"))?;
        frames.push(f);
        at += s;
    }
    Ok((toc, frames, at + padding))
}

#[derive(Debug, Clone)]
pub struct Head {
    pub channels: usize,
    pub pre_skip: usize,
    pub input_rate: u32,
    pub gain_q8: i16,
    pub family: u8,
    pub streams: usize,
    pub coupled: usize,
    pub mapping: Vec<u8>,
}

pub fn parse_head(p: &[u8]) -> Result<Head> {
    if p.len() < 19 || &p[..8] != b"OpusHead" || p[8] >> 4 != 0 {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "not an OpusHead packet",
        ));
    }
    let channels = usize::from(p[9]);
    if channels == 0 {
        return Err(Error::corrupt("Opus stream with no channels"));
    }
    let family = p[18];
    let (streams, coupled, mapping) = if family == 0 {
        if channels > 2 {
            return Err(Error::corrupt(
                "mapping family 0 allows only 1 or 2 channels",
            ));
        }
        (1, channels - 1, (0..channels as u8).collect())
    } else {
        if p.len() < 21 + channels {
            return Err(Error::truncated("Opus channel mapping table"));
        }
        let s = usize::from(p[19]);
        let c = usize::from(p[20]);
        if s == 0 || c > s || s + c > 255 {
            return Err(Error::corrupt("bad Opus stream counts"));
        }
        let map = p[21..21 + channels].to_vec();
        if map.iter().any(|&m| m != 255 && usize::from(m) >= s + c) {
            return Err(Error::corrupt(
                "Opus channel mapping points past the streams",
            ));
        }
        if family != 1 && family != 255 {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!("Opus channel mapping family {family} is not supported"),
            ));
        }
        (s, c, map)
    };
    Ok(Head {
        channels,
        pre_skip: usize::from(u16::from_le_bytes([p[10], p[11]])),
        input_rate: u32::from_le_bytes([p[12], p[13], p[14], p[15]]),
        gain_q8: i16::from_le_bytes([p[16], p[17]]),
        family,
        streams,
        coupled,
        mapping,
    })
}

/// Decoder for the elementary streams of one Opus channel group.
struct StreamSet {
    celts: Vec<celt::Celt>,
    coupled: usize,
}

fn end_band(bw: u8) -> usize {
    match bw {
        0 => 13,
        1 | 2 => 17,
        3 => 19,
        _ => 21,
    }
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    let (streams, bad) = ogg::read(data)?;
    let mut notes = Notes::new();
    if bad > 0 {
        notes.push(Note::new(
            Severity::Warning,
            "ogg.damaged_pages",
            format!("{bad} damaged Ogg page(s) were skipped"),
        ));
    }
    let s = streams
        .iter()
        .find(|s| {
            s.packets
                .first()
                .is_some_and(|p| p.data.starts_with(b"OpusHead"))
        })
        .ok_or_else(|| {
            Error::new(
                ErrorCode::UnsupportedFormat,
                "no Opus stream in this Ogg file",
            )
        })?;
    let head = parse_head(&s.packets[0].data)?;
    let mut audio_tags = crate::Tags::default();
    let mut pictures = Vec::new();
    let mut first_audio = 1;
    if let Some(p) = s.packets.get(1) {
        if p.data.starts_with(b"OpusTags") {
            let (_, t, pics) = parse_vorbis_comment(&p.data[8..]);
            audio_tags = t;
            pictures = pics;
            first_audio = 2;
        }
    }
    let ch = head.channels;
    let mut set = StreamSet {
        celts: (0..head.streams)
            .map(|i| celt::Celt::new(if i < head.coupled { 2 } else { 1 }))
            .collect(),
        coupled: head.coupled,
    };
    let mut pcm: Vec<f32> = Vec::new();
    let mut skipped_unsupported = false;
    for pk in &s.packets[first_audio..] {
        // decode each stream's frames of this packet
        let mut per_stream: Vec<Vec<f32>> = vec![Vec::new(); head.streams];
        let mut rest: &[u8] = &pk.data;
        let mut frames_len: Option<usize> = None;
        for (si, out) in per_stream.iter_mut().enumerate() {
            let last = si + 1 == head.streams;
            let (toc, frames, used) = parse_packet(rest, !last)?;
            let chs = if si < set.coupled { 2 } else { 1 };
            for f in frames {
                match toc.mode {
                    Mode::Celt => {
                        set.celts[si].decode(
                            f,
                            (toc.frame_size / 120).trailing_zeros() as usize,
                            if toc.stereo { 2 } else { 1 }.min(chs).max(1),
                            end_band(toc.bandwidth),
                            out,
                        );
                    }
                    _ => {
                        skipped_unsupported = true;
                        out.extend(std::iter::repeat_n(0.0, toc.frame_size * chs));
                    }
                }
            }
            rest = rest.get(used..).unwrap_or(&[]);
            let n = out.len() / chs;
            if *frames_len.get_or_insert(n) != n {
                return Err(Error::corrupt(
                    "Opus streams disagree on the packet duration",
                ));
            }
        }
        let n = frames_len.unwrap_or(0);
        for i in 0..n {
            for k in 0..ch {
                let m = head.mapping[k];
                let v = if m == 255 {
                    0.0
                } else {
                    let m = usize::from(m);
                    if m < 2 * head.coupled {
                        per_stream[m / 2][i * 2 + m % 2]
                    } else {
                        let si = head.coupled + (m - 2 * head.coupled);
                        per_stream[si][i]
                    }
                };
                pcm.push(v);
            }
        }
        check_samples(pcm.len() as u64, limits)?;
    }
    if skipped_unsupported {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "this Opus file uses SILK or hybrid frames, which this build cannot decode yet",
        ));
    }
    let skip = (head.pre_skip * ch).min(pcm.len());
    pcm.drain(..skip);
    if let Some(g) = s
        .packets
        .iter()
        .rev()
        .find(|p| p.granule >= 0)
        .map(|p| p.granule)
    {
        let want = (g.max(0) as usize)
            .saturating_sub(head.pre_skip)
            .saturating_mul(ch);
        if want < pcm.len() {
            pcm.truncate(want);
        }
    }
    if head.gain_q8 != 0 {
        let g = 10f32.powf(f32::from(head.gain_q8) / (20.0 * 256.0));
        for v in &mut pcm {
            *v *= g;
        }
    }
    if ch >= 3 && head.family == 1 {
        let (order, _) = crate::vorbis::layout(ch);
        let frames = pcm.len() / ch;
        let mut re = Vec::with_capacity(pcm.len());
        for f in 0..frames {
            for &src in &order {
                re.push(pcm[f * ch + src]);
            }
        }
        pcm = re;
    }
    let mut a = Audio::new(48000, ch as u16, Samples::Float(pcm));
    if ch >= 3 && head.family == 1 {
        a.channel_mask = crate::vorbis::layout(ch).1;
    }
    a.tags = audio_tags;
    a.pictures = pictures;
    Ok((a, notes))
}

/// Test hook: the range decoder's final state after each CELT frame of a single-stream file.
#[doc(hidden)]
pub fn final_ranges(data: &[u8]) -> Result<Vec<u32>> {
    let (streams, _) = ogg::read(data)?;
    let s = streams.first().ok_or_else(|| Error::corrupt("no stream"))?;
    let head = parse_head(&s.packets[0].data)?;
    let mut celt = celt::Celt::new(head.channels);
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    for pk in &s.packets[2..] {
        let (toc, frames, _) = parse_packet(&pk.data, false)?;
        let mut last = 0;
        for f in frames {
            celt.decode(
                f,
                (toc.frame_size / 120).trailing_zeros() as usize,
                if toc.stereo { 2 } else { 1 },
                end_band(toc.bandwidth),
                &mut scratch,
            );
            last = celt.final_range();
        }
        out.push(last);
    }
    Ok(out)
}
