//! AVI (RIFF): Motion JPEG, MPEG-4 part 2 and VP8 video; PCM, MP3 and AC-3 audio.
use crate::{
    codec, div_round, fps_from_ns, frame_ns, unsupported, Kind, Media, Packet, Res, Stream,
};
use oc_core::{Error, ErrorCode, Limits};

pub fn sniff(data: &[u8]) -> bool {
    data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"AVI "
}

fn le16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn le32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(o)?,
        *b.get(o + 1)?,
        *b.get(o + 2)?,
        *b.get(o + 3)?,
    ]))
}

/// Chunks of a buffer: `(id, body, offset of the chunk header)`. A truncated last chunk is clipped.
fn chunks(data: &[u8]) -> Vec<([u8; 4], &[u8], usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let id = [data[pos], data[pos + 1], data[pos + 2], data[pos + 3]];
        let size = le32(data, pos + 4).unwrap_or(0) as usize;
        let end = (pos + 8).saturating_add(size).min(data.len());
        out.push((id, &data[pos + 8..end], pos));
        pos = end + (size & 1);
    }
    out
}

struct StreamInfo {
    stream: Option<Stream>,
    scale: u32,
    rate: u32,
    sample_size: u32,
    kind: Kind,
    count: u64,
    bytes: u64,
}

fn video_codec(handler: &[u8], compression: &[u8]) -> Option<&'static str> {
    for f in [compression, handler] {
        let up: Vec<u8> = f.iter().map(u8::to_ascii_uppercase).collect();
        match &up[..] {
            b"MJPG" | b"AVRN" | b"DMB1" | b"JPEG" => return Some(codec::MJPEG),
            b"XVID" | b"DIVX" | b"DX50" | b"FMP4" | b"MP4V" => return Some(codec::MPEG4),
            b"VP80" => return Some(codec::VP8),
            _ => {}
        }
    }
    None
}

fn parse_strl(list: &[u8]) -> Option<StreamInfo> {
    let mut strh = None;
    let mut strf = None;
    let mut name = String::new();
    for (id, body, _) in chunks(list) {
        match &id {
            b"strh" => strh = Some(body),
            b"strf" => strf = Some(body),
            b"strn" => {
                name = String::from_utf8_lossy(body)
                    .trim_end_matches('\0')
                    .to_string()
            }
            _ => {}
        }
    }
    let (h, f) = (strh?, strf?);
    let kind_tag = h.get(..4)?;
    let (scale, rate, sample_size) = (le32(h, 20)?, le32(h, 24)?, le32(h, 44)?);
    let mut info = StreamInfo {
        stream: None,
        scale,
        rate,
        sample_size,
        kind: Kind::Video,
        count: 0,
        bytes: 0,
    };
    match kind_tag {
        b"vids" => {
            let compression = f.get(16..20)?;
            let c = video_codec(h.get(4..8)?, compression)?;
            let mut s = Stream::new(Kind::Video, c);
            s.width = le32(f, 4)?;
            s.height = le32(f, 8)?.cast_signed().unsigned_abs();
            s.name = name;
            if c == codec::MPEG4 {
                s.private = f.get(40..).unwrap_or(&[]).to_vec();
            }
            if rate > 0 && scale > 0 {
                let g = gcd(u64::from(rate), u64::from(scale));
                s.fps = Some(((u64::from(rate) / g) as u32, (u64::from(scale) / g) as u32));
            }
            info.stream = Some(s);
        }
        b"auds" => {
            let mut tag = le16(f, 0)?;
            let channels = le16(f, 2)?;
            let sr = le32(f, 4)?;
            let bits = le16(f, 14)?;
            if tag == 0xFFFE {
                tag = le16(f, 24)?;
            }
            let c = match tag {
                1 => match bits {
                    8 => codec::PCM_U8,
                    16 => codec::PCM_S16,
                    24 => codec::PCM_S24,
                    32 => codec::PCM_S32,
                    _ => return None,
                },
                3 if bits == 32 => codec::PCM_F32,
                0x55 => codec::MP3,
                0x2000 => codec::AC3,
                _ => return None,
            };
            let mut s = Stream::new(Kind::Audio, c);
            s.channels = channels;
            s.sample_rate = sr;
            s.bits = bits;
            s.name = name;
            info.kind = Kind::Audio;
            info.stream = Some(s);
        }
        _ => {}
    }
    Some(info)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

pub fn read(data: &[u8], limits: &Limits) -> Res<Media> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not an AVI file"));
    }
    let mut infos: Vec<StreamInfo> = Vec::new();
    // Stream numbers in chunk ids count every `strl`, including ones we cannot carry.
    let mut media = Media {
        source: "avi".into(),
        ..Media::default()
    };
    let mut map: Vec<Option<usize>> = Vec::new();
    let mut flags_idx: Vec<u32> = Vec::new();
    let mut data_chunks: Vec<([u8; 4], &[u8])> = Vec::new();
    let mut pos = 0;
    while pos + 12 <= data.len() && &data[pos..pos + 4] == b"RIFF" {
        let size = le32(data, pos + 4).unwrap_or(0) as usize;
        let end = (pos + 8).saturating_add(size).min(data.len());
        let form = &data[pos + 8..pos + 12];
        if form == b"AVI " || form == b"AVIX" {
            for (id, body, _) in chunks(&data[pos + 12..end]) {
                if &id != b"LIST" || body.len() < 4 {
                    if &id == b"idx1" {
                        flags_idx = body
                            .as_chunks::<16>()
                            .0
                            .iter()
                            .filter_map(|e| le32(e, 4))
                            .collect();
                    }
                    continue;
                }
                match &body[..4] {
                    b"hdrl" => {
                        for (hid, hbody, _) in chunks(&body[4..]) {
                            if &hid == b"LIST" && hbody.starts_with(b"strl") {
                                match parse_strl(&hbody[4..]) {
                                    Some(mut i) if i.stream.is_some() => {
                                        map.push(Some(media.streams.len()));
                                        media.streams.extend(i.stream.take());
                                        infos.push(i);
                                    }
                                    _ => {
                                        map.push(None);
                                        infos.push(StreamInfo {
                                            stream: None,
                                            scale: 1,
                                            rate: 1,
                                            sample_size: 0,
                                            kind: Kind::Video,
                                            count: 0,
                                            bytes: 0,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    b"movi" => {
                        for (cid, cbody, _) in chunks(&body[4..]) {
                            if &cid == b"LIST" && cbody.starts_with(b"rec ") {
                                for (rid, rbody, _) in chunks(&cbody[4..]) {
                                    data_chunks.push((rid, rbody));
                                }
                            } else {
                                data_chunks.push((cid, cbody));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        pos = end + (size & 1);
    }
    if media.streams.is_empty() {
        return Err(unsupported(
            "no stream in this AVI uses a codec this build can carry",
        ));
    }
    for (n, (id, body)) in data_chunks.iter().enumerate() {
        limits.check_entries(n as u64)?;
        let Some(num) = std::str::from_utf8(&id[..2])
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok())
        else {
            continue;
        };
        if !matches!(&id[2..], b"dc" | b"db" | b"wb") {
            continue;
        }
        let Some(info) = infos.get_mut(num) else {
            continue;
        };
        let Some(Some(si)) = map.get(num).copied() else {
            continue;
        };
        let ticks = if info.sample_size > 0 {
            info.bytes / u64::from(info.sample_size)
        } else {
            info.count
        };
        let t = div_round(
            i128::from(ticks) * 1_000_000_000 * i128::from(info.scale),
            i128::from(info.rate.max(1)),
        );
        info.count += 1;
        info.bytes += body.len() as u64;
        if body.is_empty() {
            continue;
        }
        let key = info.kind == Kind::Audio
            || media.streams[si].codec == codec::MJPEG
            || flags_idx.get(n).is_none_or(|f| f & 0x10 != 0);
        media.packets.push(Packet {
            stream: si,
            pts: t,
            dts: t,
            duration: 0,
            key,
            discard_end: 0,
            data: body.to_vec(),
        });
    }
    media.fill_durations();
    Ok(media)
}

pub fn allows(c: &str) -> bool {
    matches!(
        c,
        codec::MJPEG
            | codec::MPEG4
            | codec::VP8
            | codec::PCM_U8
            | codec::PCM_S16
            | codec::PCM_S24
            | codec::PCM_S32
            | codec::PCM_F32
            | codec::MP3
            | codec::AC3
    )
}

struct Chunk(Vec<u8>);

impl Chunk {
    fn new(id: &[u8; 4]) -> Self {
        let mut v = id.to_vec();
        v.extend_from_slice(&[0; 4]);
        Self(v)
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    fn finish(mut self) -> Vec<u8> {
        let n = (self.0.len() - 8) as u32;
        self.0[4..8].copy_from_slice(&n.to_le_bytes());
        if self.0.len() % 2 == 1 {
            self.0.push(0);
        }
        self.0
    }
}

/// Frame slot of each packet on a constant-rate timeline, and the number of slots.
fn slots_of(pkts: &[&Packet], fps: Option<(u32, u32)>) -> (Vec<usize>, usize) {
    let per = frame_ns(fps.unwrap_or((25, 1))).max(1);
    let mut slots = Vec::with_capacity(pkts.len());
    let mut next = 0usize;
    for p in pkts {
        let want = div_round(i128::from(p.dts.max(0)), i128::from(per)).max(0) as usize;
        let slot = want.max(next);
        slots.push(slot);
        next = slot + 1;
    }
    (slots, next)
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut c = Chunk::new(b"LIST");
    c.bytes(kind).bytes(body);
    c.finish()
}

/// Samples in one MP3 frame from the sample rate (MPEG-1 uses 1152, MPEG-2/2.5 576).
fn mp3_frame_samples(rate: u32) -> u32 {
    if rate >= 32000 {
        1152
    } else {
        576
    }
}

pub fn write(media: &Media) -> Res<Vec<u8>> {
    let mut strls = Vec::new();
    let mut video_frames = 0u32;
    let mut usec = 33_333u32;
    let mut max_bytes = 0u32;
    let (mut w, mut h) = (0u32, 0u32);
    for (si, s) in media.streams.iter().enumerate() {
        if !allows(&s.codec) {
            return Err(unsupported(format!("{} cannot go into AVI", s.codec)));
        }
        let pkts: Vec<&Packet> = media.packets_of(si).collect();
        let total_bytes: u64 = pkts.iter().map(|p| p.data.len() as u64).sum();
        let mut strh = Chunk::new(b"strh");
        let mut strf = Chunk::new(b"strf");
        match s.kind {
            Kind::Video => {
                let fps = s.fps.or_else(|| {
                    let mut pts: Vec<i64> = pkts.iter().map(|p| p.pts).collect();
                    pts.sort_unstable();
                    pts.windows(2)
                        .map(|x| x[1] - x[0])
                        .filter(|g| *g > 0)
                        .min()
                        .and_then(fps_from_ns)
                });
                let (rate, scale) = fps.unwrap_or((25, 1));
                usec = (frame_ns((rate, scale)) / 1000) as u32;
                w = s.width;
                h = s.height;
                video_frames = slots_of(&pkts, fps).1 as u32;
                let fourcc: &[u8; 4] = match s.codec.as_str() {
                    codec::MJPEG => b"MJPG",
                    codec::MPEG4 => b"XVID",
                    _ => b"VP80",
                };
                strh.bytes(b"vids")
                    .bytes(fourcc)
                    .u32(0)
                    .u16(0)
                    .u16(0)
                    .u32(0)
                    .u32(scale)
                    .u32(rate)
                    .u32(0);
                strh.u32(slots_of(&pkts, fps).1 as u32)
                    .u32(0)
                    .u32(u32::MAX)
                    .u32(0)
                    .u16(0)
                    .u16(0)
                    .u16(s.width as u16)
                    .u16(s.height as u16);
                let extra: &[u8] = if s.codec == codec::MPEG4 {
                    &s.private
                } else {
                    &[]
                };
                strf.u32(40 + extra.len() as u32)
                    .u32(s.width)
                    .u32(s.height)
                    .u16(1)
                    .u16(24)
                    .bytes(fourcc);
                strf.u32(s.width * s.height * 3)
                    .u32(0)
                    .u32(0)
                    .u32(0)
                    .u32(0)
                    .bytes(extra);
            }
            _ => {
                let (tag, bits, align, extra): (u16, u16, u16, Vec<u8>) = match s.codec.as_str() {
                    codec::PCM_U8 => (1, 8, s.channels, vec![]),
                    codec::PCM_S16 => (1, 16, 2 * s.channels, vec![]),
                    codec::PCM_S24 => (1, 24, 3 * s.channels, vec![]),
                    codec::PCM_S32 => (1, 32, 4 * s.channels, vec![]),
                    codec::PCM_F32 => (3, 32, 4 * s.channels, vec![]),
                    codec::MP3 => {
                        let n = mp3_frame_samples(s.sample_rate) as u16;
                        let mut e = vec![1, 0, 2, 0, 0, 0];
                        e.extend_from_slice(&n.to_le_bytes());
                        e.extend_from_slice(&[1, 0, 0, 0]);
                        (0x55, 0, n, e)
                    }
                    _ => (0x2000, 0, 1, vec![]),
                };
                let secs = pkts
                    .iter()
                    .map(|p| p.pts + p.duration)
                    .max()
                    .unwrap_or(0)
                    .max(1) as f64
                    / 1e9;
                let avg = if codec::is_pcm(&s.codec) {
                    s.sample_rate * u32::from(align)
                } else {
                    (total_bytes as f64 / secs) as u32
                };
                let (scale, rate, sample_size) = if codec::is_pcm(&s.codec) {
                    (
                        u32::from(align),
                        s.sample_rate * u32::from(align),
                        u32::from(align),
                    )
                } else {
                    let per = if s.codec == codec::MP3 {
                        mp3_frame_samples(s.sample_rate)
                    } else {
                        1536
                    };
                    (per, s.sample_rate, 0)
                };
                strh.bytes(b"auds")
                    .u32(0)
                    .u32(0)
                    .u16(0)
                    .u16(0)
                    .u32(0)
                    .u32(scale)
                    .u32(rate)
                    .u32(0);
                let length = if sample_size > 0 {
                    (total_bytes / u64::from(sample_size)) as u32
                } else {
                    pkts.len() as u32
                };
                strh.u32(length)
                    .u32(0)
                    .u32(u32::MAX)
                    .u32(sample_size)
                    .u16(0)
                    .u16(0)
                    .u16(0)
                    .u16(0);
                strf.u16(tag)
                    .u16(s.channels)
                    .u32(s.sample_rate)
                    .u32(avg)
                    .u16(align)
                    .u16(bits)
                    .u16(extra.len() as u16)
                    .bytes(&extra);
                max_bytes = max_bytes.max(avg);
            }
        }
        let mut body = Vec::new();
        body.extend_from_slice(&strh.finish());
        body.extend_from_slice(&strf.finish());
        strls.extend_from_slice(&list(b"strl", &body));
    }
    let mut avih = Chunk::new(b"avih");
    avih.u32(usec)
        .u32(max_bytes)
        .u32(0)
        .u32(0x110)
        .u32(video_frames)
        .u32(0)
        .u32(media.streams.len() as u32);
    avih.u32(1 << 20).u32(w).u32(h).u32(0).u32(0).u32(0).u32(0);
    let mut hdrl_body = avih.finish();
    hdrl_body.extend_from_slice(&strls);
    let hdrl = list(b"hdrl", &hdrl_body);

    // Video frames sit on a constant-rate timeline; a gap becomes an empty chunk ("dropped
    // frame") so later frames keep their times.
    let mut all: Vec<Packet> = Vec::new();
    for (si, s) in media.streams.iter().enumerate() {
        let pkts: Vec<&Packet> = media.packets_of(si).collect();
        if s.kind != Kind::Video {
            all.extend(pkts.into_iter().cloned());
            continue;
        }
        let fps = s.fps.or_else(|| {
            let mut pts: Vec<i64> = pkts.iter().map(|p| p.pts).collect();
            pts.sort_unstable();
            pts.windows(2)
                .map(|x| x[1] - x[0])
                .filter(|g| *g > 0)
                .min()
                .and_then(fps_from_ns)
        });
        let per = frame_ns(fps.unwrap_or((25, 1)));
        let (slots, count) = slots_of(&pkts, fps);
        let mut by_slot: Vec<Option<&Packet>> = vec![None; count];
        for (p, sl) in pkts.iter().zip(&slots) {
            by_slot[*sl] = Some(p);
        }
        for (sl, p) in by_slot.into_iter().enumerate() {
            let t = div_round(sl as i128 * i128::from(per), 1);
            all.push(match p {
                Some(p) => Packet {
                    dts: t,
                    pts: t,
                    ..p.clone()
                },
                None => Packet {
                    stream: si,
                    pts: t,
                    dts: t,
                    duration: per,
                    key: false,
                    discard_end: 0,
                    data: Vec::new(),
                },
            });
        }
    }
    all.sort_by_key(|p| (p.dts, p.stream));
    let packets: Vec<&Packet> = all.iter().collect();
    let mut movi = Vec::new();
    let mut idx = Vec::new();
    for p in packets {
        let s = &media.streams[p.stream];
        let suffix = match s.kind {
            Kind::Video => b"dc",
            _ => b"wb",
        };
        let id = [
            b"0123456789abcdef"[p.stream / 16 % 16],
            b"0123456789abcdef"[p.stream % 16],
            suffix[0],
            suffix[1],
        ];
        let mut c = Chunk::new(&id);
        c.bytes(&p.data);
        let off = 4 + movi.len() as u32;
        let flags = if p.key || s.kind != Kind::Video {
            0x10u32
        } else {
            0
        };
        idx.extend_from_slice(&id);
        idx.extend_from_slice(&flags.to_le_bytes());
        idx.extend_from_slice(&off.to_le_bytes());
        idx.extend_from_slice(&(p.data.len() as u32).to_le_bytes());
        movi.extend_from_slice(&c.finish());
    }
    let movi_list = list(b"movi", &movi);
    let mut idx1 = Chunk::new(b"idx1");
    idx1.bytes(&idx);
    let idx1 = idx1.finish();
    let mut body = b"AVI ".to_vec();
    body.extend_from_slice(&hdrl);
    body.extend_from_slice(&movi_list);
    body.extend_from_slice(&idx1);
    if body.len() as u64 + 8 > 0x7FFF_FFFF {
        return Err(Error::new(
            ErrorCode::LimitExceeded,
            "AVI files are limited to 2 GB here (no OpenDML)",
        ));
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}
