//! Core Audio Format: linear PCM (integer and float, either byte order) and the `info` text.
use crate::{check_samples, default_mask, Audio, Samples, Tags};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    data.len() >= 8 && &data[..4] == b"caff" && data[4] == 0 && data[5] == 1
}

pub struct Desc {
    pub rate: f64,
    pub format: [u8; 4],
    pub flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub channels: u32,
    pub bits: u32,
}

pub fn parse_desc(b: &[u8]) -> Option<Desc> {
    if b.len() < 32 {
        return None;
    }
    let u = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let mut r = [0u8; 8];
    r.copy_from_slice(&b[..8]);
    let mut f = [0u8; 4];
    f.copy_from_slice(&b[8..12]);
    Some(Desc {
        rate: f64::from_be_bytes(r),
        format: f,
        flags: u(12),
        bytes_per_packet: u(16),
        frames_per_packet: u(20),
        channels: u(24),
        bits: u(28),
    })
}

/// Walk the chunks: (type, body). The data chunk may declare size -1 for "to the end".
pub fn chunks(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut v = Vec::new();
    let mut o = 8usize;
    while o + 12 <= data.len() {
        let mut t = [0u8; 4];
        t.copy_from_slice(&data[o..o + 4]);
        let mut s = [0u8; 8];
        s.copy_from_slice(&data[o + 4..o + 12]);
        let size = i64::from_be_bytes(s);
        let start = o + 12;
        let end = if size < 0 {
            data.len()
        } else {
            start.saturating_add(size as usize).min(data.len())
        };
        v.push((t, &data[start..end]));
        o = end;
    }
    v
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a CAF file"));
    }
    let mut desc = None;
    let mut payload: Option<&[u8]> = None;
    let mut tags = Tags::default();
    for (t, body) in chunks(data) {
        match &t {
            b"desc" => desc = parse_desc(body),
            b"data" if body.len() >= 4 => payload = Some(&body[4..]),
            b"info" if body.len() >= 4 => {
                let mut parts = body[4..].split(|&b| b == 0);
                while let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                    let key = match k {
                        b"title" => "TITLE",
                        b"artist" => "ARTIST",
                        b"album" => "ALBUM",
                        b"year" | b"recorded date" => "DATE",
                        b"genre" => "GENRE",
                        b"comments" => "COMMENT",
                        b"track number" => "TRACKNUMBER",
                        b"composer" => "COMPOSER",
                        b"copyright" => "COPYRIGHT",
                        _ => continue,
                    };
                    tags.add(key, String::from_utf8_lossy(v).to_string());
                }
            }
            _ => {}
        }
    }
    let d = desc.ok_or_else(|| Error::corrupt("no desc chunk"))?;
    let payload = payload.ok_or_else(|| Error::corrupt("no data chunk"))?;
    if &d.format != b"lpcm" {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!(
                "CAF codec '{}' is not supported here",
                String::from_utf8_lossy(&d.format)
            ),
        ));
    }
    if d.channels == 0 || d.channels > 64 || !(d.rate >= 1.0 && d.rate < 4e6) {
        return Err(Error::corrupt(
            "desc chunk has no channels or an impossible sample rate",
        ));
    }
    check_samples(payload.len() as u64 * 2, limits)?;
    let float = d.flags & 1 != 0;
    let le = d.flags & 2 != 0;
    let width = (d.bytes_per_packet / d.channels.max(1)) as usize;
    let ch = d.channels as usize;
    let mut notes = Notes::new();
    let samples = if float {
        let out: Vec<f32> = match width {
            4 => payload
                .chunks_exact(4)
                .map(|c| {
                    let b = [c[0], c[1], c[2], c[3]];
                    if le {
                        f32::from_le_bytes(b)
                    } else {
                        f32::from_be_bytes(b)
                    }
                })
                .collect(),
            8 => {
                notes.push(Note::new(
                    Severity::Info,
                    "audio.float64_narrowed",
                    "64-bit float samples were narrowed to 32-bit float",
                ));
                payload
                    .chunks_exact(8)
                    .map(|c| {
                        let mut a = [0u8; 8];
                        a.copy_from_slice(c);
                        (if le {
                            f64::from_le_bytes(a)
                        } else {
                            f64::from_be_bytes(a)
                        }) as f32
                    })
                    .collect()
            }
            _ => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "unsupported CAF float width",
                ))
            }
        };
        let mut out: Vec<f32> = out
            .into_iter()
            .map(|v| if v.is_finite() { v } else { 0.0 })
            .collect();
        out.truncate(out.len() - out.len() % ch);
        Samples::Float(out)
    } else {
        if !(1..=4).contains(&width) {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "unsupported CAF sample width",
            ));
        }
        let mut out: Vec<i32> = payload
            .chunks_exact(width)
            .map(|c| {
                let mut b = [0u8; 4];
                let x = if le {
                    b[..width].copy_from_slice(c);
                    i32::from_le_bytes(b)
                } else {
                    b[4 - width..].copy_from_slice(c);
                    i32::from_be_bytes(b)
                };
                (x << (32 - 8 * width)) >> (32 - 8 * width)
            })
            .collect();
        out.truncate(out.len() - out.len() % ch);
        let stored = (width * 8) as u32;
        let valid = d.bits.clamp(1, stored);
        if valid < stored && width > 1 {
            // Packed less-than-container samples are left-aligned in CAF.
            for v in &mut out {
                *v >>= stored - valid;
            }
        }
        Samples::Int {
            bits: if width == 1 { 8 } else { valid as u8 },
            data: out,
        }
    };
    let mut a = Audio::new(d.rate.round() as u32, d.channels as u16, samples);
    a.channel_mask = default_mask(d.channels as u16);
    a.tags = tags;
    Ok((a, notes))
}

pub fn write(audio: &Audio, notes: &mut Notes) -> Result<Vec<u8>> {
    let ch = u32::from(audio.channels);
    let (float, bits, width, payload): (bool, u32, usize, Vec<u8>) = match &audio.samples {
        Samples::Int { bits, data } => {
            let width = usize::from(*bits).div_ceil(8);
            let shift = (width * 8) as u32 - u32::from(*bits);
            let mut p = Vec::with_capacity(data.len() * width);
            for &s in data {
                p.extend_from_slice(&(s << shift).to_be_bytes()[4 - width..]);
            }
            (false, u32::from(*bits), width, p)
        }
        Samples::Float(d) => {
            let mut p = Vec::with_capacity(d.len() * 4);
            for v in d {
                p.extend_from_slice(&v.to_be_bytes());
            }
            (true, 32, 4, p)
        }
    };
    let mut out = b"caff\x00\x01\x00\x00".to_vec();
    let push = |out: &mut Vec<u8>, t: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(t);
        out.extend_from_slice(&(body.len() as i64).to_be_bytes());
        out.extend_from_slice(body);
    };
    let mut desc = Vec::new();
    desc.extend_from_slice(&f64::from(audio.sample_rate).to_be_bytes());
    desc.extend_from_slice(b"lpcm");
    desc.extend_from_slice(&(if float { 1u32 } else { 0 }).to_be_bytes());
    desc.extend_from_slice(&(width as u32 * ch).to_be_bytes());
    desc.extend_from_slice(&1u32.to_be_bytes());
    desc.extend_from_slice(&ch.to_be_bytes());
    desc.extend_from_slice(&(if float { 32 } else { (width * 8) as u32 }).to_be_bytes());
    let _ = bits;
    push(&mut out, b"desc", &desc);
    let mut info = 0u32.to_be_bytes().to_vec();
    let mut n = 0u32;
    let mut dropped = 0;
    for (k, v) in &audio.tags.fields {
        let key = match k.as_str() {
            "TITLE" => "title",
            "ARTIST" => "artist",
            "ALBUM" => "album",
            "DATE" => "year",
            "GENRE" => "genre",
            "COMMENT" => "comments",
            "TRACKNUMBER" => "track number",
            "COMPOSER" => "composer",
            "COPYRIGHT" => "copyright",
            _ => {
                dropped += 1;
                continue;
            }
        };
        info.extend_from_slice(key.as_bytes());
        info.push(0);
        info.extend_from_slice(v.as_bytes());
        info.push(0);
        n += 1;
    }
    if dropped > 0 {
        notes.push(Note::new(
            Severity::Loss,
            "tags.dropped",
            format!("{dropped} tag(s) have no CAF equivalent and were left out"),
        ));
    }
    if !audio.pictures.is_empty() {
        notes.push(Note::new(
            Severity::Loss,
            "tags.picture_dropped",
            "CAF cannot carry cover art; the picture was left out",
        ));
    }
    if n > 0 {
        info[..4].copy_from_slice(&n.to_be_bytes());
        push(&mut out, b"info", &info);
    }
    let mut data = 0u32.to_be_bytes().to_vec();
    data.extend_from_slice(&payload);
    push(&mut out, b"data", &data);
    Ok(out)
}
