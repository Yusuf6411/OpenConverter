use super::{crc16, crc8};
use crate::tags::{parse_flac_picture, parse_vorbis_comment};
use crate::{check_samples, default_mask, Audio, Samples};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

/// Contents of the STREAMINFO block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub min_block: u16,
    pub max_block: u16,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits: u8,
    /// 0 when unknown.
    pub total_samples: u64,
    pub md5: [u8; 16],
}

/// Skips an ID3v2 tag that some taggers put in front of the stream.
fn skip_id3(data: &[u8]) -> usize {
    if data.len() >= 10 && &data[..3] == b"ID3" {
        let n = (usize::from(data[6] & 0x7F) << 21)
            | (usize::from(data[7] & 0x7F) << 14)
            | (usize::from(data[8] & 0x7F) << 7)
            | usize::from(data[9] & 0x7F);
        return 10 + n;
    }
    0
}

pub fn sniff(data: &[u8]) -> bool {
    let o = skip_id3(data);
    data.get(o..o + 4) == Some(b"fLaC")
}

pub fn parse_streaminfo(b: &[u8]) -> Option<StreamInfo> {
    if b.len() < 34 {
        return None;
    }
    let min_block = u16::from_be_bytes([b[0], b[1]]);
    let max_block = u16::from_be_bytes([b[2], b[3]]);
    let packed = u64::from_be_bytes([b[10], b[11], b[12], b[13], b[14], b[15], b[16], b[17]]);
    let mut md5 = [0u8; 16];
    md5.copy_from_slice(&b[18..34]);
    Some(StreamInfo {
        min_block,
        max_block,
        sample_rate: (packed >> 44) as u32,
        channels: ((packed >> 41) & 7) as u8 + 1,
        bits: ((packed >> 36) & 31) as u8 + 1,
        total_samples: packed & 0xF_FFFF_FFFF,
        md5,
    })
}

struct Br<'a> {
    data: &'a [u8],
    pos: usize, // bit position
}

impl<'a> Br<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn left(&self) -> usize {
        self.data.len() * 8 - self.pos.min(self.data.len() * 8)
    }
    fn word(&self) -> u64 {
        let byte = self.pos >> 3;
        let mut b = [0u8; 8];
        if let Some(s) = self.data.get(byte..) {
            let n = s.len().min(8);
            b[..n].copy_from_slice(&s[..n]);
        }
        u64::from_be_bytes(b) << (self.pos & 7)
    }
    /// Up to 56 bits.
    fn read(&mut self, n: u32) -> Result<u64> {
        if n == 0 {
            return Ok(0);
        }
        if self.left() < n as usize {
            return Err(Error::truncated("FLAC frame ends early"));
        }
        let v = self.word() >> (64 - n);
        self.pos += n as usize;
        Ok(v)
    }
    fn signed(&mut self, n: u32) -> Result<i64> {
        let v = self.read(n)?;
        Ok(((v << (64 - n)) as i64) >> (64 - n))
    }
    fn unary(&mut self) -> Result<u32> {
        let mut count = 0u32;
        loop {
            if self.left() == 0 {
                return Err(Error::truncated("FLAC frame ends early"));
            }
            let w = self.word();
            let avail = (64 - (self.pos & 7)).min(self.left()) as u32;
            let lz = w.leading_zeros();
            if lz < avail {
                self.pos += lz as usize + 1;
                return Ok(count + lz);
            }
            self.pos += avail as usize;
            count += avail;
            if count > 1 << 24 {
                return Err(Error::corrupt("runaway unary code in FLAC residual"));
            }
        }
    }
    fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
}

const RATES: [u32; 12] = [
    0, 88200, 176_400, 192_000, 8000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000,
];

struct FrameHeader {
    block: usize,
    assignment: u8,
    bits: u8,
    len: usize, // bytes consumed including the CRC-8
}

fn utf8_number(d: &[u8]) -> Option<(u64, usize)> {
    let b0 = *d.first()?;
    let n = match b0 {
        0x00..=0x7F => return Some((u64::from(b0), 1)),
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        0xF8..=0xFB => 5,
        0xFC..=0xFD => 6,
        0xFE => 7,
        _ => return None,
    };
    let mut v = if n == 7 {
        0
    } else {
        u64::from(b0 & (0x7F >> n))
    };
    for i in 1..n {
        let c = *d.get(i)?;
        if c & 0xC0 != 0x80 {
            return None;
        }
        v = (v << 6) | u64::from(c & 0x3F);
    }
    Some((v, n))
}

fn parse_header(d: &[u8], info: &StreamInfo) -> Result<FrameHeader> {
    let bad = || Error::corrupt("bad FLAC frame header");
    if d.len() < 6 || d[0] != 0xFF || d[1] & 0xFE != 0xF8 {
        return Err(bad());
    }
    let bs_code = d[2] >> 4;
    let sr_code = d[2] & 15;
    let assignment = d[3] >> 4;
    let bps_code = (d[3] >> 1) & 7;
    if d[3] & 1 != 0
        || assignment > 10
        || (assignment >= 8 && info.channels != 2 && assignment != 0)
    {
        // Stereo modes only make sense for two channels; the check below is against the frame itself.
    }
    let (_, n) = utf8_number(&d[4..]).ok_or_else(bad)?;
    let mut o = 4 + n;
    let block = match bs_code {
        0 => return Err(bad()),
        1 => 192,
        2..=5 => 576 << (bs_code - 2),
        6 => {
            let v = usize::from(*d.get(o).ok_or_else(bad)?) + 1;
            o += 1;
            v
        }
        7 => {
            let v = usize::from(u16::from_be_bytes([
                *d.get(o).ok_or_else(bad)?,
                *d.get(o + 1).ok_or_else(bad)?,
            ])) + 1;
            o += 2;
            v
        }
        _ => 256 << (bs_code - 8),
    };
    let rate = match sr_code {
        0 => info.sample_rate,
        1..=11 => RATES[usize::from(sr_code)],
        12 => {
            let v = u32::from(*d.get(o).ok_or_else(bad)?) * 1000;
            o += 1;
            v
        }
        13 | 14 => {
            let v = u32::from(u16::from_be_bytes([
                *d.get(o).ok_or_else(bad)?,
                *d.get(o + 1).ok_or_else(bad)?,
            ]));
            o += 2;
            if sr_code == 14 {
                v * 10
            } else {
                v
            }
        }
        _ => return Err(bad()),
    };
    let bits = match bps_code {
        0 => info.bits,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => return Err(bad()),
    };
    let crc = *d.get(o).ok_or_else(bad)?;
    if crc8(&d[..o]) != crc {
        return Err(Error::corrupt("FLAC frame header CRC mismatch"));
    }
    let _ = rate;
    Ok(FrameHeader {
        block,
        assignment,
        bits,
        len: o + 1,
    })
}

fn read_residual(br: &mut Br, block: usize, order: usize, out: &mut [i64]) -> Result<()> {
    let method = br.read(2)?;
    if method > 1 {
        return Err(Error::corrupt("reserved FLAC residual coding method"));
    }
    let pbits = if method == 0 { 4 } else { 5 };
    let escape = (1u64 << pbits) - 1;
    let porder = br.read(4)? as usize;
    let parts = 1usize << porder;
    if !block.is_multiple_of(parts) || (block >> porder) < order {
        return Err(Error::corrupt("bad FLAC partition order"));
    }
    let per = block >> porder;
    let mut i = order;
    for p in 0..parts {
        let n = if p == 0 { per - order } else { per };
        let k = br.read(pbits)?;
        if k == escape {
            let bits = br.read(5)? as u32;
            for _ in 0..n {
                out[i] = if bits == 0 { 0 } else { br.signed(bits)? };
                i += 1;
            }
        } else {
            let k = k as u32;
            for _ in 0..n {
                let q = u64::from(br.unary()?);
                let r = br.read(k)?;
                let v = (q << k) | r;
                out[i] = ((v >> 1) as i64) ^ -((v & 1) as i64);
                i += 1;
            }
        }
    }
    Ok(())
}

fn decode_subframe(br: &mut Br, block: usize, bps: u32, out: &mut [i64]) -> Result<()> {
    if br.read(1)? != 0 {
        return Err(Error::corrupt("FLAC subframe padding bit is set"));
    }
    let kind = br.read(6)? as u32;
    let mut wasted = 0;
    if br.read(1)? == 1 {
        wasted = br.unary()? + 1;
    }
    if wasted >= bps {
        // Everything is shifted out: the samples are all zero.
        wasted = bps.saturating_sub(1);
    }
    let bps = bps - wasted;
    match kind {
        0 => {
            let v = br.signed(bps)?;
            out[..block].fill(v);
        }
        1 => {
            for s in out[..block].iter_mut() {
                *s = br.signed(bps)?;
            }
        }
        8..=12 => {
            let order = (kind - 8) as usize;
            if order > 4 || order > block {
                return Err(Error::corrupt("bad FLAC fixed predictor order"));
            }
            for s in out[..order].iter_mut() {
                *s = br.signed(bps)?;
            }
            read_residual(br, block, order, out)?;
            const C: [[i64; 4]; 5] = [
                [0; 4],
                [1, 0, 0, 0],
                [2, -1, 0, 0],
                [3, -3, 1, 0],
                [4, -6, 4, -1],
            ];
            for i in order..block {
                let mut p = 0i64;
                for j in 0..order {
                    p += C[order][j] * out[i - 1 - j];
                }
                out[i] = out[i].wrapping_add(p);
            }
        }
        32..=63 => {
            let order = (kind - 31) as usize;
            if order > block {
                return Err(Error::corrupt("FLAC LPC order exceeds the block"));
            }
            for s in out[..order].iter_mut() {
                *s = br.signed(bps)?;
            }
            let prec = br.read(4)? as u32 + 1;
            if prec == 16 {
                return Err(Error::corrupt("reserved FLAC LPC precision"));
            }
            let shift = br.signed(5)?;
            if shift < 0 {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "negative FLAC LPC shift",
                ));
            }
            let mut coef = [0i64; 32];
            for c in coef[..order].iter_mut() {
                *c = br.signed(prec)?;
            }
            read_residual(br, block, order, out)?;
            for i in order..block {
                let mut p = 0i64;
                for j in 0..order {
                    p = p.wrapping_add(coef[j].wrapping_mul(out[i - 1 - j]));
                }
                out[i] = out[i].wrapping_add(p >> shift);
            }
        }
        _ => return Err(Error::corrupt("reserved FLAC subframe type")),
    }
    if wasted > 0 {
        for s in out[..block].iter_mut() {
            *s <<= wasted;
        }
    }
    Ok(())
}

/// Decode one frame at the start of `d`; appends interleaved samples to `out`.
fn decode_frame(d: &[u8], info: &StreamInfo, out: &mut Vec<i32>) -> Result<(usize, usize)> {
    let h = parse_header(d, info)?;
    let ch = match h.assignment {
        0..=7 => usize::from(h.assignment) + 1,
        _ => 2,
    };
    if ch != usize::from(info.channels) {
        return Err(Error::corrupt(
            "FLAC frame channel count differs from STREAMINFO",
        ));
    }
    let mut br = Br::new(&d[h.len..]);
    let mut bufs: Vec<Vec<i64>> = vec![vec![0; h.block]; ch];
    for (c, buf) in bufs.iter_mut().enumerate() {
        let extra = match (h.assignment, c) {
            (8, 1) | (9, 0) | (10, 1) => 1,
            _ => 0,
        };
        decode_subframe(&mut br, h.block, u32::from(h.bits) + extra, buf)?;
    }
    br.align();
    let used = h.len + br.pos / 8;
    let crc = d
        .get(used..used + 2)
        .ok_or_else(|| Error::truncated("FLAC frame has no CRC-16"))?;
    if crc16(&d[..used]) != u16::from_be_bytes([crc[0], crc[1]]) {
        return Err(Error::corrupt("FLAC frame CRC-16 mismatch"));
    }
    match h.assignment {
        8 => {
            for i in 0..h.block {
                bufs[1][i] = bufs[0][i].wrapping_sub(bufs[1][i]);
            }
        }
        9 => {
            for i in 0..h.block {
                bufs[0][i] = bufs[0][i].wrapping_add(bufs[1][i]);
            }
        }
        10 => {
            for i in 0..h.block {
                let side = bufs[1][i];
                let mid = (bufs[0][i] << 1) | (side & 1);
                bufs[0][i] = (mid + side) >> 1;
                bufs[1][i] = (mid - side) >> 1;
            }
        }
        _ => {}
    }
    for i in 0..h.block {
        for b in &bufs {
            out.push(b[i] as i32);
        }
    }
    Ok((used + 2, h.block))
}

/// Decode frames from `data` (starting at the first frame) into interleaved samples.
pub fn decode_stream(
    data: &[u8],
    info: &StreamInfo,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<i32>> {
    let mut out: Vec<i32> = Vec::new();
    let mut pos = 0usize;
    let ch = usize::from(info.channels);
    while pos + 6 < data.len() {
        if data[pos] != 0xFF || data[pos + 1] & 0xFE != 0xF8 {
            // Junk between frames: look for the next sync code.
            match data[pos + 1..]
                .windows(2)
                .position(|w| w[0] == 0xFF && w[1] & 0xFE == 0xF8)
            {
                Some(n) => {
                    pos += n + 1;
                    notes.push(Note::new(
                        Severity::Warning,
                        "flac.junk",
                        "bytes that are not part of a frame were skipped",
                    ));
                    continue;
                }
                None => break,
            }
        }
        match decode_frame(&data[pos..], info, &mut out) {
            Ok((n, _)) => pos += n,
            Err(e) => {
                if out.is_empty() {
                    return Err(e);
                }
                notes.push(
                    Note::new(
                        Severity::Warning,
                        "flac.damaged",
                        "decoding stopped at a damaged frame; the audio before it is kept",
                    )
                    .with_detail(e.message),
                );
                break;
            }
        }
        check_samples(out.len() as u64, limits)?;
        if info.total_samples > 0 && out.len() as u64 >= info.total_samples * ch as u64 {
            break;
        }
    }
    if info.total_samples > 0 {
        let want = (info.total_samples as usize).saturating_mul(ch);
        if out.len() > want {
            out.truncate(want);
        } else if out.len() < want {
            notes.push(Note::new(
                Severity::Warning,
                "flac.short",
                "the stream holds fewer samples than STREAMINFO says",
            ));
        }
    }
    Ok(out)
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a FLAC file"));
    }
    let mut notes = Notes::new();
    let mut o = skip_id3(data) + 4;
    let mut info = None;
    let mut tags = crate::Tags::default();
    let mut pics = Vec::new();
    loop {
        let h = *data
            .get(o)
            .ok_or_else(|| Error::truncated("FLAC metadata ends early"))?;
        let len = data
            .get(o + 1..o + 4)
            .map(|b| (usize::from(b[0]) << 16) | (usize::from(b[1]) << 8) | usize::from(b[2]))
            .ok_or_else(|| Error::truncated("FLAC metadata ends early"))?;
        let body = data
            .get(o + 4..o + 4 + len)
            .ok_or_else(|| Error::truncated("FLAC metadata block ends early"))?;
        match h & 0x7F {
            0 => info = parse_streaminfo(body),
            4 => {
                let (_, t, p) = parse_vorbis_comment(body);
                tags = t;
                pics.extend(p);
            }
            6 => pics.extend(parse_flac_picture(body)),
            _ => {}
        }
        o += 4 + len;
        if h & 0x80 != 0 {
            break;
        }
    }
    let info = info.ok_or_else(|| Error::corrupt("FLAC stream has no STREAMINFO"))?;
    if info.channels == 0 || info.sample_rate == 0 || !(4..=32).contains(&info.bits) {
        return Err(Error::corrupt("STREAMINFO has an impossible format"));
    }
    if info.total_samples > 0 {
        check_samples(
            info.total_samples.saturating_mul(u64::from(info.channels)),
            limits,
        )?;
    }
    let samples = decode_stream(&data[o..], &info, limits, &mut notes)?;
    if info.md5 != [0; 16] {
        let width = usize::from(info.bits).div_ceil(8);
        let mut m = oc_checksum::Md5::new();
        let mut buf = Vec::with_capacity(samples.len().min(1 << 16) * width);
        for chunk in samples.chunks(1 << 14) {
            buf.clear();
            for &s in chunk {
                buf.extend_from_slice(&s.to_le_bytes()[..width]);
            }
            m.update(&buf);
        }
        if m.finish() != info.md5 {
            notes.push(Note::new(
                Severity::Warning,
                "flac.md5_mismatch",
                "the decoded audio does not match the digest stored in the file",
            ));
        }
    }
    let mut audio = Audio::new(
        info.sample_rate,
        u16::from(info.channels),
        Samples::Int {
            bits: info.bits,
            data: samples,
        },
    );
    audio.channel_mask = default_mask(u16::from(info.channels));
    audio.tags = tags;
    audio.pictures = pics;
    Ok((audio, notes))
}
