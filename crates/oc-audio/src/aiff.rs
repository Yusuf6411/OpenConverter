//! AIFF and AIFF-C: big and little endian PCM (`sowt`), float, A-law/mu-law, the 80-bit
//! extended sample rate, text chunks and an embedded ID3 chunk (not interpreted).
use crate::{check_samples, default_mask, Audio, Samples, Tags};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    data.len() >= 12
        && &data[..4] == b"FORM"
        && (&data[8..12] == b"AIFF" || &data[8..12] == b"AIFC")
}

fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

pub fn extended_to_f64(b: &[u8]) -> f64 {
    if b.len() < 10 {
        return 0.0;
    }
    let sign = if b[0] & 0x80 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from(u16::from_be_bytes([b[0] & 0x7F, b[1]]));
    let mut m = [0u8; 8];
    m.copy_from_slice(&b[2..10]);
    let mant = u64::from_be_bytes(m);
    if exp == 0 && mant == 0 {
        return 0.0;
    }
    if exp == 0x7FFF {
        return f64::NAN;
    }
    sign * mant as f64 * 2f64.powi(exp - 16383 - 63)
}

pub fn f64_to_extended(v: f64) -> [u8; 10] {
    let mut out = [0u8; 10];
    if v == 0.0 || !v.is_finite() {
        return out;
    }
    let (sign, v) = if v < 0.0 { (0x8000u16, -v) } else { (0, v) };
    let exp = v.log2().floor() as i32;
    let mant = (v / 2f64.powi(exp - 63)) as u64;
    let e = (exp + 16383) as u16 | sign;
    out[..2].copy_from_slice(&e.to_be_bytes());
    out[2..].copy_from_slice(&mant.to_be_bytes());
    out
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not an AIFF file"));
    }
    let aifc = &data[8..12] == b"AIFC";
    let mut notes = Notes::new();
    let mut comm: Option<(u16, u32, u16, f64, [u8; 4])> = None;
    let mut sound: Option<&[u8]> = None;
    let mut tags = Tags::default();
    let mut o = 12usize;
    while o + 8 <= data.len() {
        let id = &data[o..o + 4];
        let size = be32(data, o + 4).unwrap_or(0) as usize;
        let start = o + 8;
        let body = data
            .get(start..start.saturating_add(size))
            .unwrap_or(&data[start..]);
        match id {
            b"COMM" if body.len() >= 18 => {
                let mut comp = *b"NONE";
                if aifc && body.len() >= 22 {
                    comp.copy_from_slice(&body[18..22]);
                }
                comm = Some((
                    be16(body, 0).unwrap_or(0),
                    be32(body, 2).unwrap_or(0),
                    be16(body, 6).unwrap_or(0),
                    extended_to_f64(&body[8..18]),
                    comp,
                ));
            }
            b"SSND" if body.len() >= 8 => {
                let off = be32(body, 0).unwrap_or(0) as usize;
                sound = body.get(8usize.saturating_add(off)..);
            }
            b"NAME" => tags.add(
                "TITLE",
                String::from_utf8_lossy(body).trim_end_matches('\0'),
            ),
            b"AUTH" => tags.add(
                "ARTIST",
                String::from_utf8_lossy(body).trim_end_matches('\0'),
            ),
            b"(c) " => tags.add(
                "COPYRIGHT",
                String::from_utf8_lossy(body).trim_end_matches('\0'),
            ),
            b"ANNO" => tags.add(
                "COMMENT",
                String::from_utf8_lossy(body).trim_end_matches('\0'),
            ),
            _ => {}
        }
        o = start.saturating_add(size).saturating_add(size & 1);
    }
    let (ch, frames, bits, rate, comp) = comm.ok_or_else(|| Error::corrupt("no COMM chunk"))?;
    let sound = sound.ok_or_else(|| Error::corrupt("no SSND chunk"))?;
    if ch == 0 || rate < 1.0 || rate > 4e6 {
        return Err(Error::corrupt(
            "COMM chunk has no channels or an impossible sample rate",
        ));
    }
    let width = usize::from(bits).div_ceil(8);
    check_samples(sound.len() as u64 * 2, limits)?;
    let chn = usize::from(ch);
    let max_samples = (frames as usize).saturating_mul(chn);
    let samples = match &comp {
        b"NONE" | b"sowt" | b"in24" | b"in32" | b"twos" | b"raw " => {
            if !(1..=32).contains(&bits) {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    format!("{bits}-bit PCM is not supported"),
                ));
            }
            let le = &comp == b"sowt";
            let raw = &comp == b"raw ";
            let n = (sound.len() / width).min(max_samples);
            let mut out = Vec::with_capacity(n - n % chn);
            for c in sound.chunks_exact(width).take(n - n % chn) {
                let mut b = [0u8; 4];
                if le {
                    for (i, v) in c.iter().enumerate() {
                        b[i] = *v;
                    }
                    // sign extend from `width` bytes
                    let mut x = i32::from_le_bytes(b);
                    x = (x << (32 - 8 * width)) >> (32 - 8 * width);
                    out.push(x);
                } else {
                    for (i, v) in c.iter().enumerate() {
                        b[4 - width + i] = *v;
                    }
                    let mut x = i32::from_be_bytes(b);
                    x = (x << (32 - 8 * width)) >> (32 - 8 * width);
                    out.push(x);
                }
                if raw {
                    // unsigned 8-bit
                    if let Some(l) = out.last_mut() {
                        *l = (*l & 0xFF) - 128;
                    }
                }
            }
            let stored = (width * 8) as u8;
            let valid = (bits as u8).min(stored);
            if valid < stored {
                for v in &mut out {
                    *v >>= stored - valid;
                }
            }
            Samples::Int {
                bits: if width == 1 {
                    8.max(valid)
                } else {
                    valid.max(width as u8 * 8 - 7).min(valid)
                },
                data: out,
            }
        }
        b"fl32" | b"FL32" => {
            let n = (sound.len() / 4).min(max_samples);
            let out: Vec<f32> = sound
                .chunks_exact(4)
                .take(n - n % chn)
                .map(|c| {
                    let v = f32::from_be_bytes([c[0], c[1], c[2], c[3]]);
                    if v.is_finite() {
                        v
                    } else {
                        0.0
                    }
                })
                .collect();
            Samples::Float(out)
        }
        b"fl64" | b"FL64" => {
            let n = (sound.len() / 8).min(max_samples);
            let out: Vec<f32> = sound
                .chunks_exact(8)
                .take(n - n % chn)
                .map(|c| {
                    let mut a = [0u8; 8];
                    a.copy_from_slice(c);
                    let v = f64::from_be_bytes(a) as f32;
                    if v.is_finite() {
                        v
                    } else {
                        0.0
                    }
                })
                .collect();
            notes.push(Note::new(
                Severity::Info,
                "audio.float64_narrowed",
                "64-bit float samples were narrowed to 32-bit float",
            ));
            Samples::Float(out)
        }
        b"alaw" | b"ALAW" | b"ulaw" | b"ULAW" => {
            let f: fn(u8) -> i16 = if comp[0] == b'a' || comp[0] == b'A' {
                crate::wav::alaw_decode
            } else {
                crate::wav::mulaw_decode
            };
            let n = sound.len().min(max_samples);
            let out: Vec<i32> = sound
                .iter()
                .take(n - n % chn)
                .map(|&b| i32::from(f(b)))
                .collect();
            Samples::Int {
                bits: 16,
                data: out,
            }
        }
        c => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!(
                    "AIFF-C compression '{}' is not supported",
                    String::from_utf8_lossy(c)
                ),
            ))
        }
    };
    let mut audio = Audio::new(rate.round() as u32, ch, samples);
    audio.channel_mask = default_mask(ch);
    audio.tags = tags;
    Ok((audio, notes))
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    if body.len() & 1 == 1 {
        out.push(0);
    }
}

pub fn write(audio: &Audio, little_endian: bool, notes: &mut Notes) -> Result<Vec<u8>> {
    let ch = usize::from(audio.channels);
    if ch == 0 {
        return Err(Error::corrupt("audio has no channels"));
    }
    let float = matches!(audio.samples, Samples::Float(_));
    // `sowt` is defined for 16-bit samples only; other widths stay big-endian.
    let sixteen = matches!(audio.samples, Samples::Int { bits: 9..=16, .. });
    if little_endian && !sixteen && !float {
        notes.push(Note::new(
            Severity::Info,
            "aiff.sowt_16_only",
            "little-endian AIFF-C is defined for 16-bit audio; the samples were written big-endian",
        ));
    }
    let little_endian = little_endian && sixteen;
    let aifc = float || little_endian;
    let (bits, payload): (u16, Vec<u8>) = match &audio.samples {
        Samples::Int { bits, data } => {
            let width = usize::from(*bits).div_ceil(8);
            let shift = (width * 8) as u32 - u32::from(*bits);
            let mut p = Vec::with_capacity(data.len() * width);
            for &s in data {
                let v = s << shift;
                if little_endian {
                    p.extend_from_slice(&v.to_le_bytes()[..width]);
                } else {
                    p.extend_from_slice(&v.to_be_bytes()[4 - width..]);
                }
            }
            (*bits as u16, p)
        }
        Samples::Float(d) => {
            let mut p = Vec::with_capacity(d.len() * 4);
            for v in d {
                p.extend_from_slice(&v.to_be_bytes());
            }
            (32, p)
        }
    };
    let mut comm = Vec::new();
    comm.extend_from_slice(&audio.channels.to_be_bytes());
    comm.extend_from_slice(&(audio.frames() as u32).to_be_bytes());
    comm.extend_from_slice(&bits.to_be_bytes());
    comm.extend_from_slice(&f64_to_extended(f64::from(audio.sample_rate)));
    if aifc {
        comm.extend_from_slice(if float { b"fl32" } else { b"sowt" });
        let name: &[u8] = if float {
            b"\x0e32-bit float\0"
        } else {
            b"\x0eLittle-endian"
        };
        comm.extend_from_slice(&name[..name.len().min(15)]);
        if comm.len() & 1 == 1 {
            comm.push(0);
        }
    }
    let mut out = Vec::with_capacity(payload.len() + 256);
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(if aifc { b"AIFC" } else { b"AIFF" });
    if aifc {
        chunk(&mut out, b"FVER", &0xA280_5140u32.to_be_bytes());
    }
    chunk(&mut out, b"COMM", &comm);
    let mut dropped = 0;
    for (k, v) in &audio.tags.fields {
        let id: &[u8; 4] = match k.as_str() {
            "TITLE" => b"NAME",
            "ARTIST" => b"AUTH",
            "COPYRIGHT" => b"(c) ",
            "COMMENT" => b"ANNO",
            _ => {
                dropped += 1;
                continue;
            }
        };
        chunk(&mut out, id, v.as_bytes());
    }
    if dropped > 0 {
        notes.push(Note::new(
            Severity::Loss,
            "tags.dropped",
            format!("{dropped} tag(s) have no AIFF equivalent and were left out"),
        ));
    }
    if !audio.pictures.is_empty() {
        notes.push(Note::new(
            Severity::Loss,
            "tags.picture_dropped",
            "AIFF cannot carry cover art here; the picture was left out",
        ));
    }
    let mut ssnd = vec![0u8; 8];
    ssnd.extend_from_slice(&payload);
    chunk(&mut out, b"SSND", &ssnd);
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_be_bytes());
    Ok(out)
}
