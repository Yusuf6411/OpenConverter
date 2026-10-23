//! MP4 and QuickTime MOV as [`Media`]: sample tables in, sample tables out.
use crate::{codec, corrupt, div_round, unsupported, Kind, Media, Packet, Res, Stream};
use oc_container::isobmff::{self, child, pack_language, BoxWriter, FourCc};
use oc_core::Limits;

pub fn sniff(data: &[u8]) -> bool {
    isobmff::sniff(data)
}

// ------------------------------------------------------------------------------- descriptors

fn read_len(b: &[u8], pos: &mut usize) -> Option<usize> {
    let mut n = 0usize;
    for _ in 0..4 {
        let c = *b.get(*pos)?;
        *pos += 1;
        n = (n << 7) | usize::from(c & 0x7F);
        if c & 0x80 == 0 {
            return Some(n);
        }
    }
    None
}

/// Object type and decoder-specific bytes from an `esds` body (after the full-box header).
fn parse_esds(body: &[u8]) -> Option<(u8, Vec<u8>)> {
    let b = body.get(4..)?;
    let mut pos = 0;
    let mut oti = None;
    let mut dsi = Vec::new();
    // Walk the descriptor tree flat: ES (3) holds DecoderConfig (4) holds DecoderSpecificInfo (5).
    while pos < b.len() {
        let tag = *b.get(pos)?;
        pos += 1;
        let len = read_len(b, &mut pos)?;
        match tag {
            3 => {
                let flags = *b.get(pos + 2)?;
                pos += 3;
                if flags & 0x80 != 0 {
                    pos += 2;
                }
                if flags & 0x40 != 0 {
                    pos += 1 + usize::from(*b.get(pos)?);
                }
                if flags & 0x20 != 0 {
                    pos += 2;
                }
            }
            4 => {
                oti = Some(*b.get(pos)?);
                pos += 13;
            }
            5 => {
                dsi = b.get(pos..pos + len)?.to_vec();
                pos += len;
            }
            _ => pos += len,
        }
    }
    oti.map(|o| (o, dsi))
}

fn put_len(out: &mut Vec<u8>, n: usize) {
    // Four-byte form, as most muxers write it.
    out.extend_from_slice(&[
        0x80 | ((n >> 21) & 0x7F) as u8,
        0x80 | ((n >> 14) & 0x7F) as u8,
        0x80 | ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]);
}

fn build_esds(oti: u8, video: bool, dsi: &[u8]) -> Vec<u8> {
    let mut dc = vec![oti, if video { 0x11 } else { 0x15 }, 0, 0, 0];
    dc.extend_from_slice(&[0; 8]);
    if !dsi.is_empty() {
        dc.push(5);
        put_len(&mut dc, dsi.len());
        dc.extend_from_slice(dsi);
    }
    let mut es = vec![0, 0, 0];
    es.push(4);
    put_len(&mut es, dc.len());
    es.extend_from_slice(&dc);
    es.extend_from_slice(&[6, 0x80, 0x80, 0x80, 1, 2]);
    let mut out = vec![3];
    put_len(&mut out, es.len());
    out.extend_from_slice(&es);
    out
}

// ---------------------------------------------------------------------------------- reading

struct Entry {
    codec: &'static str,
    private: Vec<u8>,
    width: u32,
    height: u32,
    channels: u16,
    bits: u16,
    rate: u32,
}

fn opus_head_from_dops(b: &[u8]) -> Option<Vec<u8>> {
    let ch = *b.get(1)?;
    let mut h = b"OpusHead".to_vec();
    h.push(1);
    h.push(ch);
    h.extend_from_slice(&b.get(2..4)?.iter().rev().copied().collect::<Vec<_>>());
    h.extend_from_slice(&b.get(4..8)?.iter().rev().copied().collect::<Vec<_>>());
    h.extend_from_slice(&b.get(8..10)?.iter().rev().copied().collect::<Vec<_>>());
    h.push(*b.get(10)?);
    if *b.get(10)? != 0 {
        h.extend_from_slice(b.get(11..11 + 2 + usize::from(ch))?);
    }
    Some(h)
}

fn dops_from_opus_head(h: &[u8]) -> Option<Vec<u8>> {
    if h.get(..8)? != b"OpusHead" {
        return None;
    }
    let ch = *h.get(9)?;
    let mut d = vec![0, ch];
    d.extend_from_slice(&[*h.get(11)?, *h.get(10)?]);
    d.extend_from_slice(&[*h.get(15)?, *h.get(14)?, *h.get(13)?, *h.get(12)?]);
    d.extend_from_slice(&[*h.get(17)?, *h.get(16)?]);
    let fam = *h.get(18)?;
    d.push(fam);
    if fam != 0 {
        d.extend_from_slice(h.get(19..19 + 2 + usize::from(ch))?);
    }
    Some(d)
}

fn parse_entry(handler: &FourCc, fourcc: &FourCc, body: &[u8]) -> Option<Entry> {
    let mut e = Entry {
        codec: "",
        private: Vec::new(),
        width: 0,
        height: 0,
        channels: 0,
        bits: 0,
        rate: 0,
    };
    let vide = handler == b"vide";
    let soun = handler == b"soun";
    let children: &[u8] = if vide {
        e.width = u32::from(isobmff::be16(body, 24)?);
        e.height = u32::from(isobmff::be16(body, 26)?);
        body.get(78..)?
    } else if soun {
        let ver = isobmff::be16(body, 8)?;
        e.channels = isobmff::be16(body, 16)?;
        e.bits = isobmff::be16(body, 18)?;
        e.rate = isobmff::be32(body, 24)? >> 16;
        let skip = match ver {
            1 => 44,
            2 => 64,
            _ => 28,
        };
        body.get(skip..)?
    } else {
        body.get(8..).unwrap_or(&[])
    };
    let get = |k: &FourCc| child(children, k).map(|b| b.body.to_vec());
    match fourcc {
        b"avc1" | b"avc3" => {
            e.codec = codec::H264;
            e.private = get(b"avcC")?;
        }
        b"hvc1" | b"hev1" => {
            e.codec = codec::HEVC;
            e.private = get(b"hvcC")?;
        }
        b"vp08" => e.codec = codec::VP8,
        b"vp09" => e.codec = codec::VP9,
        b"av01" => {
            e.codec = codec::AV1;
            e.private = get(b"av1C").unwrap_or_default();
        }
        b"jpeg" | b"mjpa" | b"mjpb" => e.codec = codec::MJPEG,
        b"mp4v" => {
            let (oti, dsi) = parse_esds(&get(b"esds")?)?;
            e.codec = match oti {
                0x6C => codec::MJPEG,
                0x20 => codec::MPEG4,
                _ => return None,
            };
            e.private = dsi;
        }
        b"mp4a" => {
            let (oti, dsi) = parse_esds(&get(b"esds")?)?;
            e.codec = match oti {
                0x40 | 0x66..=0x68 => codec::AAC,
                0x69 | 0x6B => codec::MP3,
                0xA5 => codec::AC3,
                0xA6 => codec::EAC3,
                _ => return None,
            };
            e.private = dsi;
        }
        b".mp3" => e.codec = codec::MP3,
        b"Opus" => {
            e.codec = codec::OPUS;
            e.private = opus_head_from_dops(&get(b"dOps")?)?;
            e.rate = 48000;
        }
        b"fLaC" => {
            e.codec = codec::FLAC;
            let mut p = b"fLaC".to_vec();
            p.extend_from_slice(get(b"dfLa")?.get(4..)?);
            e.private = p;
        }
        b"ac-3" => e.codec = codec::AC3,
        b"ec-3" => e.codec = codec::EAC3,
        b"sowt" => {
            e.codec = codec::PCM_S16;
            e.bits = 16;
        }
        b"raw " => {
            e.codec = codec::PCM_U8;
            e.bits = 8;
        }
        b"tx3g" => e.codec = codec::SRT,
        _ => return None,
    }
    Some(e)
}

pub fn read(data: &[u8], limits: &Limits) -> Res<Media> {
    let movie = isobmff::parse(data, limits)?;
    let mut media = Media {
        source: if &movie.brand == b"qt  " {
            "mov"
        } else {
            "mp4"
        }
        .into(),
        ..Media::default()
    };
    let mut skipped = 0usize;
    for t in &movie.tracks {
        let kind = match &t.handler {
            b"vide" => Kind::Video,
            b"soun" => Kind::Audio,
            b"subt" | b"text" | b"sbtl" => Kind::Subtitle,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let Some(e) = parse_entry(&t.handler, &t.codec, t.entry) else {
            skipped += 1;
            continue;
        };
        if t.timescale == 0 {
            continue;
        }
        let mut s = Stream::new(kind, e.codec);
        s.private = e.private;
        s.width = e.width;
        s.height = e.height;
        s.channels = e.channels;
        s.bits = e.bits;
        s.sample_rate = e.rate;
        s.rotation = t.rotation;
        s.language = String::from_utf8_lossy(&t.language).into_owned();
        let ts = i128::from(t.timescale);
        let ns = |ticks: i64| div_round(i128::from(ticks) * 1_000_000_000, ts);
        // Presentation starts where the first edit says; video uses it to cancel B-frame delay.
        let mut start_ticks = 0i64;
        let mut delay_ns = 0i64;
        let mut seen_media_edit = false;
        for &(seg, media_time) in &t.edits {
            if media_time < 0 && !seen_media_edit {
                delay_ns += div_round(
                    i128::from(seg) * 1_000_000_000,
                    i128::from(movie.timescale.max(1)),
                );
            } else if !seen_media_edit {
                start_ticks = media_time;
                seen_media_edit = true;
            }
        }
        let si = media.streams.len();
        let mut dts_ticks = 0i64;
        let mut durations: Vec<u32> = Vec::new();
        for smp in &t.samples {
            let raw = isobmff::sample_bytes(data, smp)?;
            let dts = dts_ticks - start_ticks;
            let pts = dts_ticks + i64::from(smp.cts_offset) - start_ticks;
            dts_ticks += i64::from(smp.duration);
            durations.push(smp.duration);
            let mut payload = raw.to_vec();
            if e.codec == codec::SRT {
                let n = usize::from(isobmff::be16(raw, 0).unwrap_or(0));
                if n == 0 {
                    continue;
                }
                payload = raw.get(2..2 + n).unwrap_or_default().to_vec();
            }
            media.packets.push(Packet {
                stream: si,
                pts: ns(pts) + delay_ns,
                dts: ns(dts) + delay_ns,
                duration: ns(i64::from(smp.duration)),
                key: smp.sync || kind != Kind::Video,
                discard_end: 0,
                data: payload,
            });
        }
        if kind == Kind::Video {
            // Most common sample duration gives the frame rate, exactly.
            let mut counts: std::collections::HashMap<u32, usize> =
                std::collections::HashMap::new();
            for d in durations.iter().filter(|d| **d > 0) {
                *counts.entry(*d).or_default() += 1;
            }
            if let Some((&d, _)) = counts
                .iter()
                .max_by_key(|(d, n)| (**n, std::cmp::Reverse(**d)))
            {
                let g = gcd(u64::from(t.timescale), u64::from(d));
                s.fps = Some((
                    (u64::from(t.timescale) / g) as u32,
                    (u64::from(d) / g) as u32,
                ));
            }
        }
        media.streams.push(s);
    }
    if media.streams.is_empty() {
        return Err(unsupported(if skipped > 0 {
            "no track in this file uses a codec this build can carry"
        } else {
            "no tracks"
        }));
    }
    media.sort_by_dts();
    Ok(media)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

// ---------------------------------------------------------------------------------- writing

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// QuickTime flavour (`qt  `, `jpeg`/`sowt` entries) instead of ISO MP4.
    pub mov: bool,
    /// Put the index in front of the media data so playback can start before the download ends.
    pub faststart: bool,
    /// Write fragments (`moof`/`mdat` pairs) after an empty `moov`, for streaming.
    pub fragmented: bool,
}

/// Whether the muxer can describe `codec` in this flavour.
pub fn allows(codec: &str, mov: bool) -> bool {
    match codec {
        codec::H264 | codec::HEVC | codec::VP8 | codec::VP9 | codec::AV1 | codec::MPEG4 => true,
        codec::MJPEG => true,
        codec::AAC | codec::MP3 | codec::OPUS | codec::FLAC => true,
        codec::PCM_S16 | codec::PCM_U8 => mov,
        codec::SRT => true,
        _ => false,
    }
}

struct VpInfo {
    profile: u8,
    bit_depth: u8,
    chroma: u8,
    full_range: bool,
}

/// Profile and colour configuration from the first VP9 key frame header.
fn vp9_info(frame: &[u8]) -> Option<VpInfo> {
    let mut r = BitReader { b: frame, pos: 0 };
    if r.bits(2)? != 2 {
        return None;
    }
    let lo = r.bits(1)?;
    let hi = r.bits(1)?;
    let profile = (hi << 1) | lo;
    if profile == 3 {
        r.bits(1)?;
    }
    if r.bits(1)? == 1 {
        return None; // show_existing_frame
    }
    if r.bits(1)? != 0 {
        return None; // not a key frame
    }
    r.bits(2)?; // show_frame, error_resilient
    if r.bits(24)? != 0x49_8342 {
        return None;
    }
    let bit_depth = if profile >= 2 {
        if r.bits(1)? == 1 {
            12
        } else {
            10
        }
    } else {
        8
    };
    let cs = r.bits(3)?;
    let (full_range, ssx, ssy);
    if cs != 7 {
        full_range = r.bits(1)? == 1;
        if profile == 1 || profile == 3 {
            ssx = r.bits(1)?;
            ssy = r.bits(1)?;
            r.bits(1)?;
        } else {
            ssx = 1;
            ssy = 1;
        }
    } else {
        full_range = true;
        ssx = 0;
        ssy = 0;
    }
    let chroma = match (ssx, ssy) {
        (1, 1) => 1,
        (1, 0) => 2,
        _ => 3,
    };
    Some(VpInfo {
        profile: profile as u8,
        bit_depth,
        chroma,
        full_range,
    })
}

struct BitReader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn bits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.b.get(self.pos / 8)?;
            v = (v << 1) | u32::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v)
    }
}

fn vpcc(info: Option<VpInfo>, w: u32, h: u32, fps: Option<(u32, u32)>, vp8: bool) -> Vec<u8> {
    let i = info.unwrap_or(VpInfo {
        profile: 0,
        bit_depth: 8,
        chroma: 1,
        full_range: false,
    });
    let lps = u64::from(w) * u64::from(h);
    let rate = fps.map_or(30.0, |(n, d)| f64::from(n) / f64::from(d.max(1)));
    let level = if vp8 {
        0
    } else {
        let sps = lps as f64 * rate;
        [
            (829_440.0, 10u8),
            (2_156_544.0, 11),
            (4_608_000.0, 20),
            (9_216_000.0, 21),
            (20_736_000.0, 30),
            (36_864_000.0, 31),
            (83_558_400.0, 40),
            (160_432_128.0, 41),
            (311_951_360.0, 50),
            (588_251_136.0, 51),
            (1_176_502_272.0, 52),
        ]
        .iter()
        .find(|(max, _)| sps <= *max)
        .map_or(62, |(_, l)| *l)
    };
    let mut w = BoxWriter::new();
    w.full(b"vpcC", 1, 0, |b| {
        b.u8(i.profile).u8(level);
        b.u8((i.bit_depth << 4) | (i.chroma << 1) | u8::from(i.full_range));
        b.u8(2).u8(2).u8(2).u16(0);
    });
    // The whole box, header included.
    w.buf
}

struct SampleOut {
    stream: usize,
    dts: i64,
    pts: i64,
    dur: i64,
    key: bool,
    data: Vec<u8>,
}

#[derive(Clone)]
struct TrackOut {
    handler: FourCc,
    ts: u32,
    entry: Vec<u8>,
    lang: [u8; 3],
    width: u32,
    height: u32,
    rotation: u16,
    audio: bool,
    sizes: Vec<u32>,
    durs: Vec<u32>,
    cts: Vec<i32>,
    keys: Vec<bool>,
    /// First decode time in nanoseconds, subtracted so the track's own times start at zero.
    media_time: i64,
    empty_ns: i64,
    /// Presentation span of the track in nanoseconds (what the edit list shows).
    shown_ns: i64,
    /// Media duration in track ticks, at least as long as the last presentation time.
    media_ticks: u64,
    chunks: Vec<(u32, u32, u64)>, // first sample, count, offset in mdat
}

fn visual_entry(kind: &FourCc, w: u32, h: u32, children: &[u8]) -> Vec<u8> {
    let mut b = BoxWriter::new();
    b.boxed(kind, |e| {
        e.zeros(6).u16(1).u16(0).u16(0).zeros(12);
        e.u16(w as u16).u16(h as u16);
        e.u32(0x0048_0000).u32(0x0048_0000).u32(0).u16(1);
        e.zeros(32).u16(0x18).u16(0xFFFF);
        e.bytes(children);
    });
    b.buf
}

fn audio_entry(kind: &FourCc, ch: u16, bits: u16, rate: u32, children: &[u8]) -> Vec<u8> {
    let mut b = BoxWriter::new();
    b.boxed(kind, |e| {
        e.zeros(6).u16(1).u16(0).u16(0).u32(0);
        e.u16(ch).u16(bits).u16(0).u16(0);
        e.u32(if rate > 0xFFFF { 0 } else { rate << 16 });
        e.bytes(children);
    });
    b.buf
}

fn wrap(kind: &FourCc, body: &[u8]) -> Vec<u8> {
    let mut b = BoxWriter::new();
    b.boxed(kind, |w| {
        w.bytes(body);
    });
    b.buf
}

fn sample_entry(s: &Stream, mov: bool, first: Option<&[u8]>) -> Res<Vec<u8>> {
    Ok(match s.codec.as_str() {
        codec::H264 => visual_entry(b"avc1", s.width, s.height, &wrap(b"avcC", &s.private)),
        codec::HEVC => visual_entry(b"hvc1", s.width, s.height, &wrap(b"hvcC", &s.private)),
        codec::AV1 => visual_entry(b"av01", s.width, s.height, &wrap(b"av1C", &s.private)),
        codec::VP8 => visual_entry(
            b"vp08",
            s.width,
            s.height,
            &vpcc(None, s.width, s.height, s.fps, true),
        ),
        codec::VP9 => visual_entry(
            b"vp09",
            s.width,
            s.height,
            &vpcc(first.and_then(vp9_info), s.width, s.height, s.fps, false),
        ),
        codec::MJPEG if mov => visual_entry(b"jpeg", s.width, s.height, &[]),
        codec::MJPEG | codec::MPEG4 => {
            let oti = if s.codec == codec::MJPEG { 0x6C } else { 0x20 };
            let mut w = BoxWriter::new();
            w.full(b"esds", 0, 0, |b| {
                b.bytes(&build_esds(oti, true, &s.private));
            });
            visual_entry(b"mp4v", s.width, s.height, &w.buf)
        }
        codec::AAC | codec::MP3 => {
            let oti = if s.codec == codec::AAC { 0x40 } else { 0x6B };
            let mut w = BoxWriter::new();
            w.full(b"esds", 0, 0, |b| {
                b.bytes(&build_esds(oti, false, &s.private));
            });
            audio_entry(b"mp4a", s.channels, 16, s.sample_rate, &w.buf)
        }
        codec::OPUS => {
            let d = dops_from_opus_head(&s.private).ok_or_else(|| corrupt("bad OpusHead"))?;
            audio_entry(b"Opus", s.channels, 16, 48000, &wrap(b"dOps", &d))
        }
        codec::FLAC => {
            let blocks = s
                .private
                .get(4..)
                .ok_or_else(|| corrupt("bad FLAC header"))?;
            let mut w = BoxWriter::new();
            w.full(b"dfLa", 0, 0, |b| {
                b.bytes(blocks);
            });
            audio_entry(b"fLaC", s.channels, s.bits.max(16), s.sample_rate, &w.buf)
        }
        codec::PCM_S16 => audio_entry(b"sowt", s.channels, 16, s.sample_rate, &[]),
        codec::PCM_U8 => audio_entry(b"raw ", s.channels, 8, s.sample_rate, &[]),
        codec::SRT => {
            let mut b = BoxWriter::new();
            b.boxed(b"tx3g", |e| {
                e.zeros(6).u16(1).u32(0).u8(1).u8(0xFF).u32(0);
                e.u16(0).u16(0).u16(0).u16(0);
                e.u16(0).u16(0).u16(1).u8(0).u8(18).u32(0xFFFF_FFFF);
                e.boxed(b"ftab", |f| {
                    f.u16(1).u16(1).u8(5).bytes(b"Serif");
                });
            });
            b.buf
        }
        other => return Err(unsupported(format!("{other} cannot go into MP4"))),
    })
}

fn timescale_for(s: &Stream) -> u32 {
    match s.kind {
        Kind::Video => match s.fps {
            Some((n, _)) if n > 0 => n * 10_000u32.div_ceil(n).max(1),
            _ => 90_000,
        },
        Kind::Audio => {
            if s.codec == codec::OPUS {
                48_000
            } else {
                s.sample_rate.max(8000)
            }
        }
        Kind::Subtitle => 1000,
    }
}

pub fn write(media: &Media, opts: &Options) -> Res<Vec<u8>> {
    // 1. Samples per stream, subtitles padded with empty samples so times stay contiguous.
    let mut samples: Vec<SampleOut> = Vec::new();
    for (si, s) in media.streams.iter().enumerate() {
        if !allows(&s.codec, opts.mov) {
            return Err(unsupported(format!(
                "{} cannot go into {}",
                s.codec,
                if opts.mov { "MOV" } else { "MP4" }
            )));
        }
        let mut ps: Vec<&Packet> = media.packets_of(si).collect();
        if s.codec == codec::SRT {
            ps.sort_by_key(|p| p.pts);
            let mut t = 0i64;
            for (i, p) in ps.iter().enumerate() {
                let end = (p.pts + p.duration.max(1))
                    .min(ps.get(i + 1).map_or(i64::MAX, |n| n.pts))
                    .max(p.pts);
                if p.pts > t {
                    samples.push(SampleOut {
                        stream: si,
                        dts: t,
                        pts: t,
                        dur: p.pts - t,
                        key: true,
                        data: vec![0, 0],
                    });
                }
                let mut d = (p.data.len().min(65535) as u16).to_be_bytes().to_vec();
                d.extend_from_slice(&p.data[..p.data.len().min(65535)]);
                samples.push(SampleOut {
                    stream: si,
                    dts: p.pts,
                    pts: p.pts,
                    dur: end - p.pts,
                    key: true,
                    data: d,
                });
                t = end;
            }
        } else {
            for p in ps {
                samples.push(SampleOut {
                    stream: si,
                    dts: p.dts,
                    pts: p.pts,
                    dur: p.duration,
                    key: p.key || s.kind != Kind::Video,
                    data: p.data.clone(),
                });
            }
        }
    }
    // Interleave by decode time; within a stream the given order is kept.
    let mut order: Vec<usize> = (0..samples.len()).collect();
    order.sort_by_key(|&i| (samples[i].dts, samples[i].stream, i));

    // 2. Track tables.
    let mut tracks: Vec<TrackOut> = Vec::new();
    for (si, s) in media.streams.iter().enumerate() {
        let ts = timescale_for(s);
        let mine: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&i| samples[i].stream == si)
            .collect();
        let first_data = mine.first().map(|&i| samples[i].data.as_slice());
        let entry = sample_entry(s, opts.mov, first_data)?;
        let tick = |ns: i64| div_round(i128::from(ns) * i128::from(ts), 1_000_000_000);
        let base_dts = mine.first().map_or(0, |&i| samples[i].dts);
        let min_pts = mine.iter().map(|&i| samples[i].pts).min().unwrap_or(0);
        let mut t = TrackOut {
            handler: match s.kind {
                Kind::Video => *b"vide",
                Kind::Audio => *b"soun",
                Kind::Subtitle => {
                    if opts.mov {
                        *b"sbtl"
                    } else {
                        *b"text"
                    }
                }
            },
            ts,
            entry,
            lang: {
                let l = s.language.as_bytes();
                if l.len() == 3 && l.iter().all(u8::is_ascii_lowercase) {
                    [l[0], l[1], l[2]]
                } else {
                    *b"und"
                }
            },
            width: s.width,
            height: s.height,
            rotation: s.rotation,
            audio: s.kind == Kind::Audio,
            sizes: Vec::new(),
            durs: Vec::new(),
            cts: Vec::new(),
            keys: Vec::new(),
            media_time: tick(min_pts.max(0) - base_dts).max(0),
            empty_ns: min_pts.max(0),
            shown_ns: mine
                .iter()
                .map(|&i| samples[i].pts + samples[i].dur.max(0))
                .max()
                .unwrap_or(0)
                - min_pts.max(0),
            media_ticks: 0,
            chunks: Vec::new(),
        };
        let dts_ticks: Vec<i64> = mine
            .iter()
            .map(|&i| tick(samples[i].dts - base_dts))
            .collect();
        for (k, &i) in mine.iter().enumerate() {
            let smp = &samples[i];
            let dur = match dts_ticks.get(k + 1) {
                Some(n) => n - dts_ticks[k],
                None => tick(smp.dur.max(0))
                    .max(i64::from(k > 0) * t.durs.last().map_or(0, |d| i64::from(*d))),
            };
            t.durs.push(dur.clamp(0, i64::from(u32::MAX)) as u32);
            t.sizes.push(smp.data.len() as u32);
            t.cts.push(
                (tick(smp.pts - base_dts) - dts_ticks[k])
                    .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            );
            t.keys.push(smp.key);
        }
        if s.kind != Kind::Video {
            t.cts.iter_mut().for_each(|c| *c = 0);
        }
        let sum: u64 = t.durs.iter().map(|d| u64::from(*d)).sum();
        let end = tick(
            mine.iter()
                .map(|&i| samples[i].pts + samples[i].dur.max(0))
                .max()
                .unwrap_or(0)
                - base_dts,
        )
        .max(0) as u64;
        t.media_ticks = sum.max(end);
        tracks.push(t);
    }

    if opts.fragmented {
        return Ok(write_fragmented(media, &tracks, &samples, &order, opts));
    }

    // 3. Chunks and the media data, in `order`.
    let mut mdat: Vec<u8> = Vec::new();
    let mut counts = vec![0u32; tracks.len()];
    let mut open: Option<(usize, i64)> = None; // (stream, chunk start dts)
    for &i in &order {
        let smp = &samples[i];
        let new_chunk = match open {
            Some((s, t0)) => s != smp.stream || smp.dts - t0 > 1_000_000_000,
            None => true,
        };
        if new_chunk {
            tracks[smp.stream]
                .chunks
                .push((counts[smp.stream], 0, mdat.len() as u64));
            open = Some((smp.stream, smp.dts));
        }
        if let Some(c) = tracks[smp.stream].chunks.last_mut() {
            c.1 += 1;
        }
        counts[smp.stream] += 1;
        mdat.extend_from_slice(&smp.data);
    }

    // 4. Header boxes. Offsets inside `moov` depend on its own size, so build it twice.
    let brand: &[u8; 4] = if opts.mov { b"qt  " } else { b"isom" };
    let mut ftyp = BoxWriter::new();
    ftyp.boxed(b"ftyp", |b| {
        b.bytes(brand).u32(0x200).bytes(brand);
        if !opts.mov {
            b.bytes(b"iso2").bytes(b"mp41");
        }
    });
    let wide = mdat.len() as u64 + 4096 > u64::from(u32::MAX);
    let build = |base: u64| moov(media, &tracks, base, wide, opts);
    let probe = build(0);
    let data_start = if opts.faststart {
        ftyp.buf.len() as u64 + probe.len() as u64 + 8
    } else {
        ftyp.buf.len() as u64 + 8
    };
    let moov_final = build(data_start);
    let mut out = ftyp.buf;
    if opts.faststart {
        out.extend_from_slice(&moov_final);
    }
    if mdat.len() as u64 + 8 > u64::from(u32::MAX) {
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&(mdat.len() as u64 + 16).to_be_bytes());
    } else {
        out.extend_from_slice(&(mdat.len() as u32 + 8).to_be_bytes());
        out.extend_from_slice(b"mdat");
    }
    out.extend_from_slice(&mdat);
    if !opts.faststart {
        out.extend_from_slice(&moov_final);
    }
    Ok(out)
}

fn moov(
    media: &Media,
    tracks: &[TrackOut],
    data_start: u64,
    wide: bool,
    opts: &Options,
) -> Vec<u8> {
    let stripped: Vec<TrackOut> = if opts.fragmented {
        tracks
            .iter()
            .map(|t| TrackOut {
                sizes: Vec::new(),
                durs: Vec::new(),
                cts: Vec::new(),
                keys: Vec::new(),
                chunks: Vec::new(),
                ..t.clone()
            })
            .collect()
    } else {
        Vec::new()
    };
    let full = tracks;
    let tracks = if opts.fragmented {
        &stripped[..]
    } else {
        tracks
    };
    const MS: u64 = 1000;
    let track_ms = |t: &TrackOut| -> u64 { ((t.shown_ns.max(0) + t.empty_ns) / 1_000_000) as u64 };
    let movie_ms = tracks.iter().map(track_ms).max().unwrap_or(0);
    let identity = [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
    let mut w = BoxWriter::new();
    w.boxed(b"moov", |m| {
        m.full(b"mvhd", 0, 0, |b| {
            b.u32(0)
                .u32(0)
                .u32(MS as u32)
                .u32(movie_ms.min(u64::from(u32::MAX)) as u32);
            b.u32(0x0001_0000).u16(0x0100).zeros(10);
            for v in identity {
                b.u32(v);
            }
            b.zeros(24).u32(tracks.len() as u32 + 1);
        });
        for (n, t) in tracks.iter().enumerate() {
            m.boxed(b"trak", |tk| {
                tk.full(b"tkhd", 0, 3, |b| {
                    b.u32(0).u32(0).u32(n as u32 + 1).u32(0);
                    b.u32(track_ms(t).min(u64::from(u32::MAX)) as u32);
                    b.zeros(8)
                        .u16(0)
                        .u16(0)
                        .u16(if t.audio { 0x0100 } else { 0 })
                        .u16(0);
                    let matrix = match t.rotation {
                        90 => [0, 0x0001_0000, 0, 0xFFFF_0000, 0, 0, 0, 0, 0x4000_0000],
                        180 => [0xFFFF_0000, 0, 0, 0, 0xFFFF_0000, 0, 0, 0, 0x4000_0000],
                        270 => [0, 0xFFFF_0000, 0, 0x0001_0000, 0, 0, 0, 0, 0x4000_0000],
                        _ => identity,
                    };
                    for v in matrix {
                        b.u32(v);
                    }
                    b.u32(t.width << 16).u32(t.height << 16);
                });
                if t.media_time > 0 || t.empty_ns > 0 {
                    tk.boxed(b"edts", |e| {
                        e.full(b"elst", 0, 0, |b| {
                            let seg = (t.shown_ns.max(0) / 1_000_000) as u32;
                            let empty = (t.empty_ns / 1_000_000) as u32;
                            b.u32(1 + u32::from(empty > 0));
                            if empty > 0 {
                                b.u32(empty).u32(u32::MAX).u16(1).u16(0);
                            }
                            b.u32(seg).u32(t.media_time as u32).u16(1).u16(0);
                        });
                    });
                }
                tk.boxed(b"mdia", |d| {
                    d.full(b"mdhd", 0, 0, |b| {
                        b.u32(0)
                            .u32(0)
                            .u32(t.ts)
                            .u32(t.media_ticks.min(u64::from(u32::MAX)) as u32);
                        b.u16(pack_language(t.lang)).u16(0);
                    });
                    d.full(b"hdlr", 0, 0, |b| {
                        let name: &[u8] = match &t.handler {
                            b"vide" => b"VideoHandler\0",
                            b"soun" => b"SoundHandler\0",
                            _ => b"SubtitleHandler\0",
                        };
                        b.u32(0).bytes(&t.handler).zeros(12).bytes(name);
                    });
                    d.boxed(b"minf", |n| {
                        match &t.handler {
                            b"vide" => {
                                n.full(b"vmhd", 0, 1, |b| {
                                    b.u16(0).zeros(6);
                                });
                            }
                            b"soun" => {
                                n.full(b"smhd", 0, 0, |b| {
                                    b.u16(0).u16(0);
                                });
                            }
                            _ => {
                                n.full(b"nmhd", 0, 0, |_| {});
                            }
                        }
                        n.boxed(b"dinf", |i| {
                            i.full(b"dref", 0, 0, |r| {
                                r.u32(1);
                                r.full(b"url ", 0, 1, |_| {});
                            });
                        });
                        n.boxed(b"stbl", |s| {
                            s.full(b"stsd", 0, 0, |b| {
                                b.u32(1).bytes(&t.entry);
                            });
                            let mut runs: Vec<(u32, u32)> = Vec::new();
                            for &dur in &t.durs {
                                match runs.last_mut() {
                                    Some((c, d)) if *d == dur => *c += 1,
                                    _ => runs.push((1, dur)),
                                }
                            }
                            s.full(b"stts", 0, 0, |b| {
                                b.u32(runs.len() as u32);
                                for (c, dur) in &runs {
                                    b.u32(*c).u32(*dur);
                                }
                            });
                            if t.cts.iter().any(|c| *c != 0) {
                                let signed = t.cts.iter().any(|c| *c < 0);
                                let mut runs: Vec<(u32, i32)> = Vec::new();
                                for &c in &t.cts {
                                    match runs.last_mut() {
                                        Some((n, v)) if *v == c => *n += 1,
                                        _ => runs.push((1, c)),
                                    }
                                }
                                s.full(b"ctts", u8::from(signed), 0, |b| {
                                    b.u32(runs.len() as u32);
                                    for (c, v) in &runs {
                                        b.u32(*c).u32(*v as u32);
                                    }
                                });
                            }
                            if t.keys.iter().any(|k| !*k) {
                                s.full(b"stss", 0, 0, |b| {
                                    let keys: Vec<u32> = t
                                        .keys
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, k)| **k)
                                        .map(|(i, _)| i as u32 + 1)
                                        .collect();
                                    b.u32(keys.len() as u32);
                                    for k in keys {
                                        b.u32(k);
                                    }
                                });
                            }
                            s.full(b"stsc", 0, 0, |b| {
                                let mut runs: Vec<(u32, u32)> = Vec::new();
                                for (ci, c) in t.chunks.iter().enumerate() {
                                    if runs.last().is_none_or(|r| r.1 != c.1) {
                                        runs.push((ci as u32 + 1, c.1));
                                    }
                                }
                                b.u32(runs.len() as u32);
                                for (first, per) in runs {
                                    b.u32(first).u32(per).u32(1);
                                }
                            });
                            s.full(b"stsz", 0, 0, |b| {
                                b.u32(0).u32(t.sizes.len() as u32);
                                for z in &t.sizes {
                                    b.u32(*z);
                                }
                            });
                            if wide {
                                s.full(b"co64", 0, 0, |b| {
                                    b.u32(t.chunks.len() as u32);
                                    for c in &t.chunks {
                                        b.u64(c.2 + data_start);
                                    }
                                });
                            } else {
                                s.full(b"stco", 0, 0, |b| {
                                    b.u32(t.chunks.len() as u32);
                                    for c in &t.chunks {
                                        b.u32((c.2 + data_start) as u32);
                                    }
                                });
                            }
                        });
                    });
                });
            });
        }
        if opts.fragmented {
            m.boxed(b"mvex", |x| {
                x.full(b"mehd", 0, 0, |b| {
                    b.u32(movie_ms.min(u64::from(u32::MAX)) as u32);
                });
                for n in 0..full.len() {
                    x.full(b"trex", 0, 0, |b| {
                        b.u32(n as u32 + 1).u32(1).u32(0).u32(0).u32(0);
                    });
                }
            });
        }
        if !media.title.is_empty() && !opts.mov {
            m.boxed(b"udta", |u| {
                u.full(b"meta", 0, 0, |b| {
                    b.full(b"hdlr", 0, 0, |h| {
                        h.u32(0).bytes(b"mdir").bytes(b"appl").zeros(8).u8(0);
                    });
                    b.boxed(b"ilst", |i| {
                        i.boxed(b"\xA9nam", |n| {
                            n.full(b"data", 0, 1, |d| {
                                d.u32(0).bytes(media.title.as_bytes());
                            });
                        });
                    });
                });
            });
        }
    });
    w.buf
}

/// An empty `moov` followed by `moof`/`mdat` pairs cut at video key frames about every two
/// seconds (or at any sample when there is no video).
fn write_fragmented(
    media: &Media,
    tracks: &[TrackOut],
    samples: &[SampleOut],
    order: &[usize],
    opts: &Options,
) -> Vec<u8> {
    let has_video = media.streams.iter().any(|s| s.kind == Kind::Video);
    let mut frags: Vec<Vec<usize>> = Vec::new();
    let mut start = 0i64;
    for &i in order {
        let s = &samples[i];
        let cut = (!has_video || media.streams[s.stream].kind == Kind::Video && s.key)
            && s.dts.saturating_sub(start) >= 2_000_000_000;
        if cut || frags.is_empty() {
            if frags.last().is_some_and(|f| f.is_empty()) {
                frags.pop();
            }
            frags.push(Vec::new());
            start = s.dts;
        }
        if let Some(f) = frags.last_mut() {
            f.push(i);
        }
    }
    let mut ftyp = BoxWriter::new();
    ftyp.boxed(b"ftyp", |b| {
        b.bytes(b"iso5")
            .u32(0x200)
            .bytes(b"iso5")
            .bytes(b"iso6")
            .bytes(b"mp41");
    });
    let mut out = ftyp.buf;
    out.extend_from_slice(&moov(media, tracks, 0, false, opts));
    let mut next = vec![0usize; tracks.len()];
    let mut base = vec![0u64; tracks.len()];
    for (seq, frag) in frags.iter().enumerate() {
        // Samples of the fragment grouped by track; the table index of each is its running
        // position in the track.
        let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
        for &i in frag {
            let t = samples[i].stream;
            match groups.iter_mut().find(|g| g.0 == t) {
                Some(g) => g.1.push(i),
                None => groups.push((t, vec![i])),
            }
        }
        // (track, first table index, count, base decode time)
        let mut runs = Vec::new();
        for (t, ids) in &groups {
            runs.push((*t, next[*t], ids.len(), base[*t]));
            base[*t] += tracks[*t].durs[next[*t]..next[*t] + ids.len()]
                .iter()
                .map(|d| u64::from(*d))
                .sum::<u64>();
            next[*t] += ids.len();
        }
        let build = |offsets: &[u32]| {
            let mut w = BoxWriter::new();
            w.boxed(b"moof", |m| {
                m.full(b"mfhd", 0, 0, |b| {
                    b.u32(seq as u32 + 1);
                });
                for (k, (t, first, count, bdt)) in runs.iter().enumerate() {
                    let tr = &tracks[*t];
                    let signed = tr.cts[*first..first + count].iter().any(|c| *c < 0);
                    m.boxed(b"traf", |tf| {
                        tf.full(b"tfhd", 0, 0x02_0000, |b| {
                            b.u32(*t as u32 + 1);
                        });
                        tf.full(b"tfdt", 1, 0, |b| {
                            b.u64(*bdt);
                        });
                        tf.full(b"trun", u8::from(signed), 0xF01, |b| {
                            b.u32(*count as u32).u32(offsets[k]);
                            for j in *first..first + count {
                                let flags: u32 = if tr.keys[j] { 0x0200_0000 } else { 0x0101_0000 };
                                b.u32(tr.durs[j])
                                    .u32(tr.sizes[j])
                                    .u32(flags)
                                    .u32(tr.cts[j] as u32);
                            }
                        });
                    });
                }
            });
            w.buf
        };
        let size = build(&vec![0; runs.len()]).len() as u32;
        let mut offsets = Vec::new();
        let mut at = size + 8;
        for g in &groups {
            offsets.push(at);
            at +=
                g.1.iter()
                    .map(|&i| samples[i].data.len() as u32)
                    .sum::<u32>();
        }
        out.extend_from_slice(&build(&offsets));
        let total: usize = frag.iter().map(|&i| samples[i].data.len()).sum();
        out.extend_from_slice(&(total as u32 + 8).to_be_bytes());
        out.extend_from_slice(b"mdat");
        for (_, ids) in &groups {
            for &i in ids {
                out.extend_from_slice(&samples[i].data);
            }
        }
    }
    out
}
