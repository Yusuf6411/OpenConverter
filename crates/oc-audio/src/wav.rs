//! WAVE: PCM 8-32 bit, IEEE float 32/64, A-law and mu-law, IMA and Microsoft ADPCM (read),
//! WAVE_FORMAT_EXTENSIBLE with channel masks, RF64/BW64 for files over 4 GB, LIST/INFO tags.
use crate::{check_samples, default_mask, Audio, Samples, Tags};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

const TAG_PCM: u16 = 1;
const TAG_MSADPCM: u16 = 2;
const TAG_FLOAT: u16 = 3;
const TAG_ALAW: u16 = 6;
const TAG_MULAW: u16 = 7;
const TAG_IMA: u16 = 0x11;
const TAG_EXT: u16 = 0xFFFE;

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64le(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

pub fn sniff(data: &[u8]) -> bool {
    data.len() >= 12
        && (&data[..4] == b"RIFF" || &data[..4] == b"RF64" || &data[..4] == b"BW64")
        && &data[8..12] == b"WAVE"
}

#[derive(Default)]
struct Fmt {
    tag: u16,
    channels: u16,
    rate: u32,
    block_align: u16,
    bits: u16,
    valid_bits: u16,
    mask: u32,
    extra: Vec<u8>,
}

/// mu-law byte to 16-bit PCM.
pub fn mulaw_decode(b: u8) -> i16 {
    let u = !b;
    let mut t = (i32::from(u & 0x0F) << 3) + 0x84;
    t <<= (u & 0x70) >> 4;
    (if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 }) as i16
}

/// A-law byte to 16-bit PCM.
pub fn alaw_decode(b: u8) -> i16 {
    let a = b ^ 0x55;
    let mut t = i32::from(a & 0x0F) << 4;
    let seg = (a & 0x70) >> 4;
    t = match seg {
        0 => t + 8,
        1 => t + 0x108,
        s => (t + 0x108) << (s - 1),
    };
    (if a & 0x80 != 0 { t } else { -t }) as i16
}

pub fn mulaw_encode(pcm: i16) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32635;
    let mut v = i32::from(pcm);
    let sign = if v < 0 { 0x80 } else { 0 };
    if v < 0 {
        v = -v;
    }
    v = v.min(CLIP) + BIAS;
    let mut exp = 7;
    let mut mask = 0x4000;
    while exp > 0 && v & mask == 0 {
        exp -= 1;
        mask >>= 1;
    }
    let mant = (v >> (exp + 3)) & 0x0F;
    !(sign | (exp << 4) | mant) as u8
}

pub fn alaw_encode(pcm: i16) -> u8 {
    const SEG_END: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];
    let mut v = i32::from(pcm) >> 3;
    let mask: i32 = if v >= 0 {
        0xD5
    } else {
        v = -v - 1;
        0x55
    };
    let seg = SEG_END.iter().position(|&e| v <= e).unwrap_or(8);
    if seg >= 8 {
        return (0x7F ^ mask) as u8;
    }
    let mut a = (seg as i32) << 4;
    a |= if seg < 2 {
        (v >> 1) & 0xF
    } else {
        (v >> seg) & 0xF
    };
    (a ^ mask) as u8
}

const IMA_STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

fn ima_nibble(pred: &mut i32, index: &mut i32, nib: u8) -> i16 {
    let step = IMA_STEPS[*index as usize];
    let mut diff = step >> 3;
    if nib & 1 != 0 {
        diff += step >> 2;
    }
    if nib & 2 != 0 {
        diff += step >> 1;
    }
    if nib & 4 != 0 {
        diff += step;
    }
    if nib & 8 != 0 {
        *pred -= diff;
    } else {
        *pred += diff;
    }
    *pred = (*pred).clamp(-32768, 32767);
    *index = (*index + IMA_INDEX[usize::from(nib)]).clamp(0, 88);
    *pred as i16
}

fn decode_ima(fmt: &Fmt, data: &[u8], out: &mut Vec<i32>) -> Result<()> {
    let ch = usize::from(fmt.channels);
    let align = usize::from(fmt.block_align);
    if align < 4 * ch || ch == 0 {
        return Err(Error::corrupt("IMA ADPCM block size is too small"));
    }
    for block in data.chunks(align) {
        if block.len() < 4 * ch {
            break;
        }
        let mut pred = vec![0i32; ch];
        let mut index = vec![0i32; ch];
        let mut chans: Vec<Vec<i16>> = vec![Vec::new(); ch];
        for c in 0..ch {
            pred[c] = i32::from(i16::from_le_bytes([block[4 * c], block[4 * c + 1]]));
            index[c] = i32::from(block[4 * c + 2]).min(88);
            chans[c].push(pred[c] as i16);
        }
        let body = &block[4 * ch..];
        for group in body.chunks(4 * ch) {
            for c in 0..ch {
                let Some(part) = group.get(4 * c..4 * c + 4) else {
                    continue;
                };
                for &byte in part {
                    for nib in [byte & 0xF, byte >> 4] {
                        let s = ima_nibble(&mut pred[c], &mut index[c], nib);
                        chans[c].push(s);
                    }
                }
            }
        }
        let n = chans.iter().map(Vec::len).min().unwrap_or(0);
        for i in 0..n {
            for c in &chans {
                out.push(i32::from(c[i]));
            }
        }
    }
    Ok(())
}

const MS_ADAPT: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

fn decode_msadpcm(fmt: &Fmt, data: &[u8], out: &mut Vec<i32>) -> Result<()> {
    let ch = usize::from(fmt.channels);
    let align = usize::from(fmt.block_align);
    let ncoef = usize::from(u16le(&fmt.extra, 2).unwrap_or(0));
    let mut coefs = Vec::new();
    for i in 0..ncoef {
        let a = u16le(&fmt.extra, 4 + 4 * i).map(|v| i32::from(v as i16));
        let b = u16le(&fmt.extra, 6 + 4 * i).map(|v| i32::from(v as i16));
        match (a, b) {
            (Some(a), Some(b)) => coefs.push((a, b)),
            _ => return Err(Error::corrupt("MS ADPCM coefficient table is truncated")),
        }
    }
    if ch == 0 || ch > 2 || align < 7 * ch || coefs.is_empty() {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "unsupported MS ADPCM layout",
        ));
    }
    for block in data.chunks(align) {
        if block.len() < 7 * ch {
            break;
        }
        let mut pred_idx = [0usize; 2];
        let mut delta = [0i32; 2];
        let mut s1 = [0i32; 2];
        let mut s2 = [0i32; 2];
        let mut o = 0;
        for c in 0..ch {
            pred_idx[c] = usize::from(block[o]).min(coefs.len() - 1);
            o += 1;
        }
        for c in 0..ch {
            delta[c] = i32::from(i16::from_le_bytes([block[o], block[o + 1]]));
            o += 2;
        }
        for c in 0..ch {
            s1[c] = i32::from(i16::from_le_bytes([block[o], block[o + 1]]));
            o += 2;
        }
        for c in 0..ch {
            s2[c] = i32::from(i16::from_le_bytes([block[o], block[o + 1]]));
            o += 2;
        }
        // The two history samples come out first, oldest first.
        for c in 0..ch {
            out.push(s2[c]);
        }
        for c in 0..ch {
            out.push(s1[c]);
        }
        let mut turn = 0usize;
        for &byte in &block[o..] {
            for nib in [byte >> 4, byte & 0xF] {
                let c = turn % ch;
                turn += 1;
                let (c1, c2) = coefs[pred_idx[c]];
                let signed = i32::from(nib as i8 ^ 8) - 8;
                let mut p = (s1[c] * c1 + s2[c] * c2) >> 8;
                p += signed * delta[c];
                p = p.clamp(-32768, 32767);
                s2[c] = s1[c];
                s1[c] = p;
                delta[c] = ((MS_ADAPT[usize::from(nib)] * delta[c]) >> 8).max(16);
                out.push(p);
            }
        }
    }
    Ok(())
}

fn read_info(list: &[u8], tags: &mut Tags) {
    let mut o = 0;
    while o + 8 <= list.len() {
        let id = &list[o..o + 4];
        let size = u32le(list, o + 4).unwrap_or(0) as usize;
        let body = list.get(o + 8..o + 8 + size).unwrap_or_default();
        let text = String::from_utf8_lossy(body)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        let key = match id {
            b"INAM" => "TITLE",
            b"IART" => "ARTIST",
            b"IPRD" => "ALBUM",
            b"ICRD" => "DATE",
            b"IGNR" => "GENRE",
            b"ICMT" => "COMMENT",
            b"ITRK" | b"IPRT" => "TRACKNUMBER",
            b"ICOP" => "COPYRIGHT",
            b"ISFT" => "ENCODER",
            b"IENG" => "ENGINEER",
            b"IKEY" => "KEYWORDS",
            b"ISBJ" => "SUBJECT",
            b"IMUS" => "COMPOSER",
            _ => "",
        };
        if !key.is_empty() {
            tags.add(key, text);
        }
        o += 8 + size + (size & 1);
    }
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a WAVE file"));
    }
    let mut notes = Notes::new();
    let rf64 = &data[..4] != b"RIFF";
    let mut ds64_data: Option<u64> = None;
    let mut fmt: Option<Fmt> = None;
    let mut payload: Option<&[u8]> = None;
    let mut tags = Tags::default();
    let mut o = 12usize;
    while o + 8 <= data.len() {
        let id = &data[o..o + 4];
        let mut size = u64::from(u32le(data, o + 4).unwrap_or(0));
        let body_start = o + 8;
        if id == b"data" && size == 0xFFFF_FFFF {
            size = if rf64 {
                ds64_data.unwrap_or(u64::MAX)
            } else {
                u64::MAX
            };
        }
        if id == b"data" && size == 0 && rf64 {
            size = ds64_data.unwrap_or(0);
        }
        let avail = (data.len() - body_start) as u64;
        let take = size.min(avail) as usize;
        let body = &data[body_start..body_start + take];
        match id {
            b"ds64" => ds64_data = u64le(body, 8),
            b"fmt " => {
                if body.len() < 16 {
                    return Err(Error::corrupt("fmt chunk is too short").at(o as u64));
                }
                let mut f = Fmt {
                    tag: u16le(body, 0).unwrap_or(0),
                    channels: u16le(body, 2).unwrap_or(0),
                    rate: u32le(body, 4).unwrap_or(0),
                    block_align: u16le(body, 12).unwrap_or(0),
                    bits: u16le(body, 14).unwrap_or(0),
                    ..Fmt::default()
                };
                if body.len() > 18 {
                    let cb = usize::from(u16le(body, 16).unwrap_or(0));
                    f.extra = body.get(18..18 + cb).unwrap_or(&body[18..]).to_vec();
                }
                f.valid_bits = f.bits;
                if f.tag == TAG_EXT && f.extra.len() >= 22 {
                    f.valid_bits = u16le(&f.extra, 0).unwrap_or(f.bits);
                    f.mask = u32le(&f.extra, 2).unwrap_or(0);
                    f.tag = u16le(&f.extra, 6).unwrap_or(0);
                }
                fmt = Some(f);
            }
            b"data" => {
                if payload.is_none() {
                    payload = Some(body);
                }
            }
            b"LIST" if body.starts_with(b"INFO") => read_info(&body[4..], &mut tags),
            _ => {}
        }
        if (take as u64) < size {
            break; // the declared size runs past the end of the file
        }
        let step = size
            .checked_add(size & 1)
            .and_then(|s| usize::try_from(s).ok());
        match step.and_then(|s| body_start.checked_add(s)) {
            Some(n) => o = n,
            None => break,
        }
    }
    let fmt = fmt.ok_or_else(|| Error::corrupt("no fmt chunk"))?;
    let payload = payload.ok_or_else(|| Error::corrupt("no data chunk"))?;
    if fmt.channels == 0 || fmt.rate == 0 {
        return Err(Error::corrupt("fmt chunk has no channels or sample rate"));
    }
    if payload.len() < usize::from(fmt.block_align.max(1)) && !payload.is_empty() {
        notes.push(Note::new(
            Severity::Warning,
            "wav.short_data",
            "the data chunk holds less than one frame",
        ));
    }
    let ch = usize::from(fmt.channels);
    let bytes_per = usize::from(fmt.bits).div_ceil(8);
    check_samples((payload.len() as u64).saturating_mul(2), limits)?;
    let samples = match fmt.tag {
        TAG_PCM => {
            let bits = fmt.bits;
            if !(8..=32).contains(&bits) || bytes_per == 0 {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    format!("{bits}-bit PCM is not supported"),
                ));
            }
            let n = payload.len() / bytes_per;
            let mut out = Vec::with_capacity(n - n % ch);
            for c in payload.chunks_exact(bytes_per).take(n - n % ch) {
                let v = match bytes_per {
                    1 => i32::from(c[0]) - 128,
                    2 => i32::from(i16::from_le_bytes([c[0], c[1]])),
                    3 => i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8,
                    _ => i32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                };
                out.push(v);
            }
            // Containers wider than the valid bits hold left-justified samples.
            let stored = (bytes_per * 8) as u8;
            let valid = fmt.valid_bits.clamp(1, u16::from(stored)) as u8;
            let depth = if bytes_per == 1 { 8 } else { stored.min(32) };
            if valid < depth && bytes_per > 1 {
                let shift = depth - valid;
                for v in &mut out {
                    *v >>= shift;
                }
                Samples::Int {
                    bits: valid,
                    data: out,
                }
            } else {
                Samples::Int {
                    bits: depth,
                    data: out,
                }
            }
        }
        TAG_FLOAT => {
            let mut out = Vec::new();
            match fmt.bits {
                32 => {
                    for c in payload.chunks_exact(4) {
                        out.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                    }
                }
                64 => {
                    for c in payload.chunks_exact(8) {
                        let mut a = [0u8; 8];
                        a.copy_from_slice(c);
                        out.push(f64::from_le_bytes(a) as f32);
                    }
                    notes.push(Note::new(
                        Severity::Info,
                        "audio.float64_narrowed",
                        "64-bit float samples were narrowed to 32-bit float",
                    ));
                }
                b => {
                    return Err(Error::new(
                        ErrorCode::UnsupportedVariant,
                        format!("{b}-bit float is not supported"),
                    ))
                }
            }
            out.truncate(out.len() - out.len() % ch);
            for v in &mut out {
                if !v.is_finite() {
                    *v = 0.0;
                }
            }
            Samples::Float(out)
        }
        TAG_ALAW | TAG_MULAW => {
            let f: fn(u8) -> i16 = if fmt.tag == TAG_ALAW {
                alaw_decode
            } else {
                mulaw_decode
            };
            let mut out: Vec<i32> = payload.iter().map(|&b| i32::from(f(b))).collect();
            out.truncate(out.len() - out.len() % ch);
            notes.push(Note::new(
                Severity::Info,
                "wav.companded",
                "A-law and mu-law audio was expanded to 16-bit PCM",
            ));
            Samples::Int {
                bits: 16,
                data: out,
            }
        }
        TAG_IMA => {
            let mut out = Vec::new();
            decode_ima(&fmt, payload, &mut out)?;
            out.truncate(out.len() - out.len() % ch);
            notes.push(Note::new(
                Severity::Info,
                "wav.adpcm",
                "ADPCM audio was expanded to 16-bit PCM",
            ));
            Samples::Int {
                bits: 16,
                data: out,
            }
        }
        TAG_MSADPCM => {
            let mut out = Vec::new();
            decode_msadpcm(&fmt, payload, &mut out)?;
            out.truncate(out.len() - out.len() % ch);
            notes.push(Note::new(
                Severity::Info,
                "wav.adpcm",
                "ADPCM audio was expanded to 16-bit PCM",
            ));
            Samples::Int {
                bits: 16,
                data: out,
            }
        }
        t => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!("WAVE format tag 0x{t:04X} is not supported"),
            ))
        }
    };
    let mut audio = Audio::new(fmt.rate, fmt.channels, samples);
    audio.channel_mask = if fmt.mask != 0 && fmt.mask.count_ones() == u32::from(fmt.channels) {
        fmt.mask
    } else {
        default_mask(fmt.channels)
    };
    audio.tags = tags;
    Ok((audio, notes))
}

/// How the writer stores the samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Integer PCM at the clip's bit depth rounded up to 8, 16, 24 or 32 bits.
    Pcm,
    Float32,
    Alaw,
    Mulaw,
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() & 1 == 1 {
        out.push(0);
    }
}

pub fn write(audio: &Audio, enc: Encoding, notes: &mut Notes) -> Result<Vec<u8>> {
    let ch = usize::from(audio.channels);
    if ch == 0 {
        return Err(Error::corrupt("audio has no channels"));
    }
    let (tag, bits, payload): (u16, u16, Vec<u8>) = match (enc, &audio.samples) {
        (Encoding::Pcm, Samples::Int { bits, data }) => {
            let width = if *bits <= 8 {
                8
            } else {
                (usize::from(*bits).div_ceil(8) * 8) as u16
            };
            let shift = u32::from(width) - u32::from(*bits);
            let mut p = Vec::with_capacity(data.len() * usize::from(width / 8));
            for &s in data {
                let v = if width == 8 { s + 128 } else { s << shift };
                match width {
                    8 => p.push(v as u8),
                    16 => p.extend_from_slice(&(v as i16).to_le_bytes()),
                    24 => p.extend_from_slice(&v.to_le_bytes()[..3]),
                    _ => p.extend_from_slice(&v.to_le_bytes()),
                }
            }
            (TAG_PCM, width, p)
        }
        (Encoding::Pcm, Samples::Float(_)) | (Encoding::Float32, _) => {
            let f = crate::dsp::to_float(&audio.samples);
            let mut p = Vec::with_capacity(f.len() * 4);
            for v in f.iter() {
                p.extend_from_slice(&v.to_le_bytes());
            }
            (TAG_FLOAT, 32, p)
        }
        (Encoding::Alaw | Encoding::Mulaw, s) => {
            let pcm = crate::dsp::to_int(s, 16);
            let f: fn(i16) -> u8 = if enc == Encoding::Alaw {
                alaw_encode
            } else {
                mulaw_encode
            };
            notes.push(Note::new(
                Severity::Loss,
                "wav.companded_out",
                "A-law and mu-law keep about 13 bits of dynamic range",
            ));
            (
                if enc == Encoding::Alaw {
                    TAG_ALAW
                } else {
                    TAG_MULAW
                },
                8,
                pcm.iter().map(|&v| f(v as i16)).collect(),
            )
        }
    };
    let ext = ch > 2 || bits > 16;
    let ext = ext && tag != TAG_ALAW && tag != TAG_MULAW;
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&(if ext { TAG_EXT } else { tag }).to_le_bytes());
    fmt.extend_from_slice(&audio.channels.to_le_bytes());
    fmt.extend_from_slice(&audio.sample_rate.to_le_bytes());
    let block = (bits / 8) as u32 * u32::from(audio.channels);
    fmt.extend_from_slice(&(audio.sample_rate.saturating_mul(block)).to_le_bytes());
    fmt.extend_from_slice(&(block as u16).to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if ext {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        let valid = match &audio.samples {
            Samples::Int { bits, .. } if tag == TAG_PCM => u16::from(*bits),
            _ => bits,
        };
        fmt.extend_from_slice(&valid.to_le_bytes());
        let mask = if audio.channel_mask.count_ones() == u32::from(audio.channels) {
            audio.channel_mask
        } else {
            default_mask(audio.channels)
        };
        fmt.extend_from_slice(&mask.to_le_bytes());
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    } else if tag != TAG_PCM {
        fmt.extend_from_slice(&0u16.to_le_bytes());
    }
    let mut list = b"INFO".to_vec();
    let mut dropped = 0;
    for (k, v) in &audio.tags.fields {
        let id: &[u8; 4] = match k.as_str() {
            "TITLE" => b"INAM",
            "ARTIST" => b"IART",
            "ALBUM" => b"IPRD",
            "DATE" => b"ICRD",
            "GENRE" => b"IGNR",
            "COMMENT" => b"ICMT",
            "TRACKNUMBER" => b"ITRK",
            "COPYRIGHT" => b"ICOP",
            "ENCODER" => b"ISFT",
            "COMPOSER" => b"IMUS",
            "KEYWORDS" => b"IKEY",
            "SUBJECT" => b"ISBJ",
            "ENGINEER" => b"IENG",
            _ => {
                dropped += 1;
                continue;
            }
        };
        let mut text = v.as_bytes().to_vec();
        text.push(0);
        chunk(&mut list, id, &text);
    }
    if dropped > 0 {
        notes.push(Note::new(
            Severity::Loss,
            "tags.dropped",
            format!("{dropped} tag(s) have no WAVE INFO equivalent and were left out"),
        ));
    }
    if !audio.pictures.is_empty() {
        notes.push(Note::new(
            Severity::Loss,
            "tags.picture_dropped",
            "WAVE cannot carry cover art; the picture was left out",
        ));
    }
    let big = payload.len() as u64 + 4096 > u64::from(u32::MAX);
    let mut out = Vec::with_capacity(payload.len() + 1024);
    out.extend_from_slice(if big { b"RF64" } else { b"RIFF" });
    out.extend_from_slice(&0u32.to_le_bytes()); // patched below
    out.extend_from_slice(b"WAVE");
    if big {
        out.extend_from_slice(b"ds64");
        out.extend_from_slice(&28u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 8]); // riff size, patched
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out.extend_from_slice(
            &((payload.len() / usize::from(bits / 8).max(1) / ch) as u64).to_le_bytes(),
        );
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    chunk(&mut out, b"fmt ", &fmt);
    if tag != TAG_PCM || ext {
        let frames = (payload.len() / (usize::from(bits / 8).max(1) * ch)) as u32;
        chunk(&mut out, b"fact", &frames.to_le_bytes());
    }
    if list.len() > 4 {
        chunk(&mut out, b"LIST", &list);
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(
        &(if big {
            0xFFFF_FFFFu32
        } else {
            payload.len() as u32
        })
        .to_le_bytes(),
    );
    out.extend_from_slice(&payload);
    if payload.len() & 1 == 1 {
        out.push(0);
    }
    let riff = (out.len() - 8) as u64;
    if big {
        out[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        out[20..28].copy_from_slice(&riff.to_le_bytes());
    } else {
        out[4..8].copy_from_slice(&(riff as u32).to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companding_round_trips() {
        for b in 0..=255u8 {
            let m = mulaw_decode(b);
            let m2 = mulaw_decode(mulaw_encode(m));
            assert_eq!(m, m2, "mulaw {b}");
            let a = alaw_decode(b);
            let a2 = alaw_decode(alaw_encode(a));
            assert_eq!(a, a2, "alaw {b}");
        }
    }
}
