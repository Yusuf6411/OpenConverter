//! OpenEXR (`.exr`): scanline and tiled images, half, float and uint channels, with no
//! compression, RLE, ZIP, ZIPS, PIZ, PXR24 and B44/B44A.
//!
//! Like Radiance HDR, EXR holds linear scene values that [`Image`] cannot, so reading
//! clips to [0, 1] and encodes sRGB (alpha is un-premultiplied), and writing converts sRGB
//! to linear half floats (colour premultiplied by alpha, as EXR expects). Only the first
//! level of a tiled mipmap is read; deep data, multi-part files, subsampled channels and the
//! DWA/HTJ2K compressions are refused with a typed error.
// Plane and block indices come from sizes checked against the data, or from tables sized by
// the format; the unpacking loops are bound by those sizes.
#![allow(clippy::indexing_slicing, clippy::needless_range_loop)]
use crate::hdr::{srgb_table, srgb_to_linear, to_code};
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_compress::zlib;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};
use oc_io::ByteReader;
use std::collections::HashMap;

const MAGIC: [u8; 4] = [0x76, 0x2f, 0x31, 0x01];

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(&MAGIC)
}

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorCode::UnsupportedVariant,
        format!("OpenEXR: {what} is not supported"),
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Compression {
    None,
    Rle,
    Zips,
    Zip,
    Piz,
    Pxr24,
    B44,
    B44a,
}

impl Compression {
    fn from_code(c: u8) -> Result<Self> {
        Ok(match c {
            0 => Self::None,
            1 => Self::Rle,
            2 => Self::Zips,
            3 => Self::Zip,
            4 => Self::Piz,
            5 => Self::Pxr24,
            6 => Self::B44,
            7 => Self::B44a,
            8 | 9 => return Err(unsupported("DWA compression")),
            _ => return Err(unsupported("this compression")),
        })
    }
    fn lines(self) -> usize {
        match self {
            Self::None | Self::Rle | Self::Zips => 1,
            Self::Zip | Self::Pxr24 => 16,
            Self::Piz | Self::B44 | Self::B44a => 32,
        }
    }
}

#[derive(Clone, Debug)]
struct Channel {
    name: String,
    /// 0 uint, 1 half, 2 float.
    kind: u8,
    linear: bool,
}

impl Channel {
    fn bytes(&self) -> usize {
        if self.kind == 1 {
            2
        } else {
            4
        }
    }
}

struct Header {
    channels: Vec<Channel>,
    compression: Compression,
    window: [i32; 4],
    tile: Option<(usize, usize)>,
}

fn parse_header(r: &mut ByteReader) -> Result<Header> {
    let mut channels = Vec::new();
    let mut compression = None;
    let mut window = None;
    let mut tile = None;
    let mut count = 0;
    loop {
        let name = r.until(0)?;
        if name.is_empty() {
            break;
        }
        count += 1;
        if count > 1024 {
            return Err(Error::corrupt("too many header attributes"));
        }
        let kind = r.until(0)?;
        let size = r.u32_le()? as usize;
        let mut v = r.sub(size)?;
        match (name, kind) {
            (b"channels", b"chlist") => loop {
                let n = v.until(0)?;
                if n.is_empty() {
                    break;
                }
                let kind = v.u32_le()?;
                let linear = v.u8()? != 0;
                v.skip(3)?;
                let xs = v.i32_le()?;
                let ys = v.i32_le()?;
                if xs != 1 || ys != 1 {
                    return Err(unsupported("subsampled channels"));
                }
                if kind > 2 {
                    return Err(Error::corrupt("unknown channel pixel type"));
                }
                channels.push(Channel {
                    name: String::from_utf8_lossy(n).into_owned(),
                    kind: kind as u8,
                    linear,
                });
                if channels.len() > 256 {
                    return Err(Error::corrupt("too many channels"));
                }
            },
            (b"compression", b"compression") => {
                compression = Some(Compression::from_code(v.u8()?)?);
            }
            (b"dataWindow", b"box2i") => {
                window = Some([v.i32_le()?, v.i32_le()?, v.i32_le()?, v.i32_le()?]);
            }
            (b"tiles", b"tiledesc") => {
                let (x, y, mode) = (v.u32_le()?, v.u32_le()?, v.u8()?);
                if x == 0 || y == 0 {
                    return Err(Error::corrupt("empty tile size"));
                }
                // Only the top level of a mipmap or ripmap is read.
                let _ = mode;
                tile = Some((x as usize, y as usize));
            }
            _ => {}
        }
    }
    Ok(Header {
        channels,
        compression: compression
            .ok_or_else(|| Error::corrupt("EXR has no compression attribute"))?,
        window: window.ok_or_else(|| Error::corrupt("EXR has no data window"))?,
        tile,
    })
}

// --- decompressors ------------------------------------------------------------

/// Inverse of the byte reordering and delta coding used by ZIP and RLE.
fn undo_predictor_and_interleave(mut t: Vec<u8>) -> Vec<u8> {
    for i in 1..t.len() {
        t[i] = t[i - 1].wrapping_add(t[i]).wrapping_sub(128);
    }
    let n = t.len();
    let half = n.div_ceil(2);
    let mut out = vec![0u8; n];
    for k in 0..half {
        out[2 * k] = t[k];
        if 2 * k + 1 < n {
            out[2 * k + 1] = t[half + k];
        }
    }
    out
}

fn rle_decode(src: &[u8], want: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(want);
    let mut i = 0;
    while i < src.len() {
        let s = src[i] as i8;
        i += 1;
        if s < 0 {
            let n = usize::from(s.unsigned_abs());
            out.extend_from_slice(
                src.get(i..i + n)
                    .ok_or_else(|| Error::corrupt("RLE data is cut short"))?,
            );
            i += n;
        } else {
            let n = s as usize + 1;
            let v = *src
                .get(i)
                .ok_or_else(|| Error::corrupt("RLE data is cut short"))?;
            i += 1;
            out.resize(out.len() + n, v);
        }
        if out.len() > want {
            return Err(Error::corrupt("RLE data is longer than the block"));
        }
    }
    Ok(out)
}

/// Mantissa-trimming float format: the low byte of each float is dropped.
fn pxr24_decode(raw: &[u8], width: usize, rows: usize, channels: &[Channel]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    let take = |pos: &mut usize, n: usize| -> Result<&[u8]> {
        let s = raw
            .get(*pos..*pos + n)
            .ok_or_else(|| Error::corrupt("PXR24 data is cut short"))?;
        *pos += n;
        Ok(s)
    };
    for _ in 0..rows {
        for c in channels {
            match c.kind {
                1 => {
                    let hi = take(&mut pos, width)?.to_vec();
                    let lo = take(&mut pos, width)?;
                    let mut px = 0u32;
                    for x in 0..width {
                        px = px.wrapping_add(u32::from(hi[x]) << 8 | u32::from(lo[x]));
                        out.extend_from_slice(&(px as u16).to_le_bytes());
                    }
                }
                0 => {
                    let planes: Vec<&[u8]> = (0..4)
                        .map(|_| take(&mut pos, width))
                        .collect::<Result<_>>()?;
                    let mut px = 0u32;
                    for x in 0..width {
                        px = px.wrapping_add(
                            u32::from(planes[0][x]) << 24
                                | u32::from(planes[1][x]) << 16
                                | u32::from(planes[2][x]) << 8
                                | u32::from(planes[3][x]),
                        );
                        out.extend_from_slice(&px.to_le_bytes());
                    }
                }
                _ => {
                    let planes: Vec<&[u8]> = (0..3)
                        .map(|_| take(&mut pos, width))
                        .collect::<Result<_>>()?;
                    let mut px = 0u32;
                    for x in 0..width {
                        px = px.wrapping_add(
                            u32::from(planes[0][x]) << 24
                                | u32::from(planes[1][x]) << 16
                                | u32::from(planes[2][x]) << 8,
                        );
                        out.extend_from_slice(&px.to_le_bytes());
                    }
                }
            }
        }
    }
    Ok(out)
}

const HUF_ENCSIZE: usize = (1 << 16) + 1;
const HUF_DECBITS: usize = 14;
const SHORT_ZEROCODE_RUN: u64 = 59;
const LONG_ZEROCODE_RUN: u64 = 63;
const SHORTEST_LONG_RUN: usize = 2 + (LONG_ZEROCODE_RUN - SHORT_ZEROCODE_RUN) as usize;

struct BitIn<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl BitIn<'_> {
    /// `n` (<= 58) bits at the cursor, zero padded past the end of the data.
    fn peek(&self, n: usize) -> u64 {
        let byte = self.pos / 8;
        let mut v: u128 = 0;
        for k in 0..9 {
            v = v << 8 | u128::from(self.data.get(byte + k).copied().unwrap_or(0));
        }
        let shift = 72 - self.pos % 8 - n;
        ((v >> shift) & ((1u128 << n) - 1)) as u64
    }
    fn left(&self) -> usize {
        self.end.saturating_sub(self.pos)
    }
    fn take(&mut self, n: usize) -> Result<u64> {
        if self.left() < n {
            return Err(Error::corrupt("Huffman data is cut short"));
        }
        let v = self.peek(n);
        self.pos += n;
        Ok(v)
    }
}

/// OpenEXR's canonical Huffman coding of 16-bit words, as used by PIZ.
fn huf_uncompress(src: &[u8], n_raw: usize) -> Result<Vec<u16>> {
    if src.is_empty() {
        return if n_raw == 0 {
            Ok(Vec::new())
        } else {
            Err(Error::corrupt("empty Huffman block"))
        };
    }
    let rd = |at: usize| -> Result<usize> {
        let b = src
            .get(at..at + 4)
            .ok_or_else(|| Error::corrupt("Huffman header is cut short"))?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let (im, im_max, n_bits) = (rd(0)?, rd(4)?, rd(12)?);
    if im >= HUF_ENCSIZE || im_max >= HUF_ENCSIZE || im > im_max {
        return Err(Error::corrupt("bad Huffman symbol range"));
    }
    let body = src.get(20..).unwrap_or(&[]);
    // Code lengths, six bits each, with runs of zeros.
    let mut table = BitIn {
        data: body,
        pos: 0,
        end: body.len() * 8,
    };
    let mut lens = vec![0u64; HUF_ENCSIZE];
    let mut i = im;
    while i <= im_max {
        let l = table.take(6)?;
        lens[i] = l;
        if l == LONG_ZEROCODE_RUN {
            let run = table.take(8)? as usize + SHORTEST_LONG_RUN;
            if i + run > im_max + 1 {
                return Err(Error::corrupt("Huffman zero run is too long"));
            }
            for k in 0..run {
                lens[i + k] = 0;
            }
            i += run;
        } else if l >= SHORT_ZEROCODE_RUN {
            let run = (l - SHORT_ZEROCODE_RUN) as usize + 2;
            if i + run > im_max + 1 {
                return Err(Error::corrupt("Huffman zero run is too long"));
            }
            for k in 0..run {
                lens[i + k] = 0;
            }
            i += run;
        } else {
            i += 1;
        }
    }
    let table_bytes = table.pos.div_ceil(8);
    // Canonical codes: assigned from the longest length to the shortest.
    let mut n = [0u64; 59];
    for &l in &lens {
        n[l as usize] += 1;
    }
    let mut c = 0u64;
    for l in (1..59).rev() {
        let nc = (c + n[l]) >> 1;
        n[l] = c;
        c = nc;
    }
    let mut short: Vec<(u8, u32)> = vec![(0, 0); 1 << HUF_DECBITS];
    let mut long: HashMap<(usize, u64), u32> = HashMap::new();
    for (sym, &l) in lens.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let l = l as usize;
        let code = n[l];
        n[l] += 1;
        if code >> l != 0 {
            return Err(Error::corrupt("Huffman code overflows its length"));
        }
        if l <= HUF_DECBITS {
            let first = (code as usize) << (HUF_DECBITS - l);
            for e in short
                .get_mut(first..first + (1 << (HUF_DECBITS - l)))
                .ok_or_else(|| Error::corrupt("bad Huffman code"))?
            {
                *e = (l as u8, sym as u32);
            }
        } else {
            long.insert((l, code), sym as u32);
        }
    }
    let data = body
        .get(table_bytes..)
        .ok_or_else(|| Error::corrupt("Huffman data is cut short"))?;
    if n_bits > data.len() * 8 {
        return Err(Error::corrupt("Huffman data is cut short"));
    }
    let mut bits = BitIn {
        data,
        pos: 0,
        end: n_bits,
    };
    let rlc = im_max as u32;
    let mut out: Vec<u16> = Vec::with_capacity(n_raw);
    while out.len() < n_raw {
        if bits.left() == 0 {
            return Err(Error::corrupt("Huffman data ended early"));
        }
        let (l, sym) = short[bits.peek(HUF_DECBITS) as usize];
        let (l, sym) = if l != 0 {
            (usize::from(l), sym)
        } else {
            let mut found = None;
            for len in HUF_DECBITS + 1..=58 {
                if let Some(&s) = long.get(&(len, bits.peek(len))) {
                    found = Some((len, s));
                    break;
                }
            }
            found.ok_or_else(|| Error::corrupt("bad Huffman code in the data"))?
        };
        if bits.left() < l {
            return Err(Error::corrupt("Huffman data ended early"));
        }
        bits.pos += l;
        if sym == rlc {
            let count = bits.take(8)? as usize;
            let last = *out
                .last()
                .ok_or_else(|| Error::corrupt("Huffman run with nothing to repeat"))?;
            if out.len() + count > n_raw {
                return Err(Error::corrupt("Huffman run passes the end"));
            }
            out.resize(out.len() + count, last);
        } else {
            out.push(sym as u16);
        }
    }
    Ok(out)
}

fn wdec14(l: u16, h: u16) -> (u16, u16) {
    let ls = l as i16;
    let hs = h as i16;
    let hi = i32::from(hs);
    let ai = i32::from(ls) + (hi & 1) + (hi >> 1);
    let a = ai as i16;
    let b = (ai - hi) as i16;
    (a as u16, b as u16)
}

fn wdec16(l: u16, h: u16) -> (u16, u16) {
    const MOD_MASK: i32 = 0xffff;
    const NEG_OFFSET: i32 = 1 << 15;
    let m = i32::from(l);
    let d = i32::from(h);
    let bb = (m - (d >> 1)) & MOD_MASK;
    let aa = (d + bb - NEG_OFFSET) & MOD_MASK;
    (aa as u16, bb as u16)
}

/// 2D Haar-like wavelet decoding of an `nx` x `ny` array with strides `ox`, `oy` (in words).
fn wav2_decode(data: &mut [u16], base: usize, nx: usize, ox: usize, ny: usize, oy: usize, mx: u16) {
    let w14 = mx < (1 << 14);
    let dec = |l: u16, h: u16| if w14 { wdec14(l, h) } else { wdec16(l, h) };
    let n = nx.min(ny);
    let mut p = 1usize;
    while p <= n {
        p <<= 1;
    }
    p >>= 1;
    let mut p2 = p;
    p >>= 1;
    while p >= 1 {
        let (oy1, oy2, ox1, ox2) = (oy * p, oy * p2, ox * p, ox * p2);
        let mut py = base;
        let ey = base + oy * (ny - p2);
        while py <= ey {
            let mut px = py;
            let ex = py + ox * (nx - p2);
            while px <= ex {
                let (p01, p10, p11) = (px + ox1, px + oy1, px + oy1 + ox1);
                let (i00, i10) = dec(data[px], data[p10]);
                let (i01, i11) = dec(data[p01], data[p11]);
                let (a, b) = dec(i00, i01);
                data[px] = a;
                data[p01] = b;
                let (a, b) = dec(i10, i11);
                data[p10] = a;
                data[p11] = b;
                px += ox2;
            }
            if nx & p != 0 {
                let p10 = px + oy1;
                let (a, b) = dec(data[px], data[p10]);
                data[px] = a;
                data[p10] = b;
            }
            py += oy2;
        }
        if ny & p != 0 {
            let mut px = py;
            let ex = py + ox * (nx - p2);
            while px <= ex {
                let p01 = px + ox1;
                let (a, b) = dec(data[px], data[p01]);
                data[px] = a;
                data[p01] = b;
                px += ox2;
            }
        }
        p2 = p;
        p >>= 1;
    }
}

fn piz_decode(src: &[u8], width: usize, rows: usize, channels: &[Channel]) -> Result<Vec<u8>> {
    let mut r = ByteReader::new(src);
    let words_per_px: Vec<usize> = channels.iter().map(|c| c.bytes() / 2).collect();
    let total_words: usize = words_per_px.iter().map(|w| w * width * rows).sum();
    let mut out = Vec::with_capacity(total_words * 2);
    if total_words == 0 {
        return Ok(out);
    }
    let min_nz = usize::from(r.u16_le()?);
    let max_nz = usize::from(r.u16_le()?);
    if max_nz >= 8192 {
        return Err(Error::corrupt("PIZ bitmap is too large"));
    }
    let mut bitmap = vec![0u8; 8192];
    if min_nz <= max_nz {
        let b = r.take(max_nz - min_nz + 1)?;
        bitmap[min_nz..=max_nz].copy_from_slice(b);
    }
    // Reverse lookup table of the values that occur (0 always does).
    let mut lut: Vec<u16> = Vec::new();
    for v in 0..65536usize {
        if v == 0 || bitmap[v >> 3] & (1 << (v & 7)) != 0 {
            lut.push(v as u16);
        }
    }
    let max_value = lut.len() as u16 - 1;
    lut.resize(65536, 0);
    let len = r.u32_le()? as usize;
    let huf = r.take(len)?;
    let mut tmp = huf_uncompress(huf, total_words)?;
    // Wavelet decode each channel's words.
    let mut start = 0;
    let mut starts = Vec::new();
    for &w in &words_per_px {
        starts.push(start);
        for j in 0..w {
            wav2_decode(&mut tmp, start + j, width, w, rows, width * w, max_value);
        }
        start += width * rows * w;
    }
    for v in tmp.iter_mut() {
        *v = lut[usize::from(*v)];
    }
    // Back to scanline order: row by row, channel by channel.
    for y in 0..rows {
        for (c, &w) in words_per_px.iter().enumerate() {
            let from = starts[c] + y * width * w;
            for v in &tmp[from..from + width * w] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    Ok(out)
}

fn b44_unpack14(b: &[u8]) -> [u16; 16] {
    let b: Vec<u32> = b.iter().map(|&v| u32::from(v)).collect();
    let mut s = [0u32; 16];
    s[0] = b[0] << 8 | b[1];
    let shift = b[2] >> 2;
    let bias = 0x20u32 << shift;
    let d = |v: u32| (v << shift).wrapping_sub(bias);
    s[4] = s[0].wrapping_add(d(((b[2] << 4) | (b[3] >> 4)) & 0x3f));
    s[8] = s[4].wrapping_add(d(((b[3] << 2) | (b[4] >> 6)) & 0x3f));
    s[12] = s[8].wrapping_add(d(b[4] & 0x3f));
    s[1] = s[0].wrapping_add(d(b[5] >> 2));
    s[5] = s[4].wrapping_add(d(((b[5] << 4) | (b[6] >> 4)) & 0x3f));
    s[9] = s[8].wrapping_add(d(((b[6] << 2) | (b[7] >> 6)) & 0x3f));
    s[13] = s[12].wrapping_add(d(b[7] & 0x3f));
    s[2] = s[1].wrapping_add(d(b[8] >> 2));
    s[6] = s[5].wrapping_add(d(((b[8] << 4) | (b[9] >> 4)) & 0x3f));
    s[10] = s[9].wrapping_add(d(((b[9] << 2) | (b[10] >> 6)) & 0x3f));
    s[14] = s[13].wrapping_add(d(b[10] & 0x3f));
    s[3] = s[2].wrapping_add(d(b[11] >> 2));
    s[7] = s[6].wrapping_add(d(((b[11] << 4) | (b[12] >> 4)) & 0x3f));
    s[11] = s[10].wrapping_add(d(((b[12] << 2) | (b[13] >> 6)) & 0x3f));
    s[15] = s[14].wrapping_add(d(b[13] & 0x3f));
    let mut out = [0u16; 16];
    for i in 0..16 {
        let v = s[i] as u16;
        out[i] = if v & 0x8000 != 0 { v & 0x7fff } else { !v };
    }
    out
}

fn b44_decode(src: &[u8], width: usize, rows: usize, channels: &[Channel]) -> Result<Vec<u8>> {
    if channels.iter().any(|c| c.linear && c.kind == 1) {
        return Err(unsupported("B44 with perceptually linear channels"));
    }
    let mut pos = 0;
    let mut planes: Vec<Vec<u8>> = Vec::new();
    for c in channels {
        if c.kind != 1 {
            let n = width * rows * 4;
            planes.push(
                src.get(pos..pos + n)
                    .ok_or_else(|| Error::corrupt("B44 data is cut short"))?
                    .to_vec(),
            );
            pos += n;
            continue;
        }
        let mut plane = vec![0u8; width * rows * 2];
        for by in (0..rows).step_by(4) {
            for bx in (0..width).step_by(4) {
                let head = src
                    .get(pos..pos + 3)
                    .ok_or_else(|| Error::corrupt("B44 data is cut short"))?;
                let px: [u16; 16] = if head[2] >= (13 << 2) {
                    pos += 3;
                    let t = u16::from(head[0]) << 8 | u16::from(head[1]);
                    let t = if t & 0x8000 != 0 { t & 0x7fff } else { !t };
                    [t; 16]
                } else {
                    let block = src
                        .get(pos..pos + 14)
                        .ok_or_else(|| Error::corrupt("B44 data is cut short"))?;
                    pos += 14;
                    b44_unpack14(block)
                };
                for dy in 0..4 {
                    for dx in 0..4 {
                        let (x, y) = (bx + dx, by + dy);
                        if x < width && y < rows {
                            let o = (y * width + x) * 2;
                            plane[o..o + 2].copy_from_slice(&px[dy * 4 + dx].to_le_bytes());
                        }
                    }
                }
            }
        }
        planes.push(plane);
    }
    let mut out =
        Vec::with_capacity(width * rows * channels.iter().map(Channel::bytes).sum::<usize>());
    for y in 0..rows {
        for (c, ch) in channels.iter().enumerate() {
            let rb = width * ch.bytes();
            out.extend_from_slice(&planes[c][y * rb..(y + 1) * rb]);
        }
    }
    Ok(out)
}

/// Unpacks one block to scanline-interleaved little-endian samples (all lines, then for each
/// line every channel in file order).
fn unpack(
    comp: Compression,
    data: &[u8],
    width: usize,
    rows: usize,
    channels: &[Channel],
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Vec<u8>> {
    let expected = width * rows * channels.iter().map(Channel::bytes).sum::<usize>();
    // A block that did not shrink is stored as it is.
    if data.len() == expected || comp == Compression::None {
        return if data.len() == expected {
            Ok(data.to_vec())
        } else {
            Err(Error::corrupt("uncompressed block has the wrong size"))
        };
    }
    let mut lim = limits.clone();
    lim.max_output = (expected as u64 + 1024).min(lim.max_output);
    lim.max_ratio = u64::MAX;
    let out = match comp {
        Compression::Rle => undo_predictor_and_interleave(rle_decode(data, expected)?),
        Compression::Zip | Compression::Zips => {
            undo_predictor_and_interleave(zlib::decompress(data, &lim, progress)?)
        }
        Compression::Pxr24 => pxr24_decode(
            &zlib::decompress(data, &lim, progress)?,
            width,
            rows,
            channels,
        )?,
        Compression::Piz => piz_decode(data, width, rows, channels)?,
        Compression::B44 | Compression::B44a => b44_decode(data, width, rows, channels)?,
        Compression::None => unreachable!(),
    };
    if out.len() != expected {
        return Err(Error::corrupt("decompressed block has the wrong size"));
    }
    Ok(out)
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let e = i32::from((h >> 10) & 0x1f);
    let m = f32::from(h & 0x3ff);
    if e == 0 {
        sign * m * 2f32.powi(-24)
    } else if e == 31 {
        if m == 0.0 {
            sign * f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        sign * (1.0 + m / 1024.0) * 2f32.powi(e - 15)
    }
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    if !sniff(data) {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "not an OpenEXR file",
        ));
    }
    let mut r = ByteReader::new(data);
    r.skip(4)?;
    let version = r.u32_le()?;
    if version & 0xff != 2 {
        return Err(unsupported("this file version"));
    }
    let tiled_flag = version & 0x200 != 0;
    if version & 0x800 != 0 {
        return Err(unsupported("deep data"));
    }
    if version & 0x1000 != 0 {
        return Err(unsupported("multi-part files"));
    }
    let h = parse_header(&mut r)?;
    if tiled_flag != h.tile.is_some() {
        return Err(Error::corrupt("tile flag and tile attribute disagree"));
    }
    let [x0, y0, x1, y1] = h.window;
    if x1 < x0 || y1 < y0 {
        return Err(Error::corrupt("empty data window"));
    }
    let (w, ht) = (
        (i64::from(x1) - i64::from(x0) + 1) as usize,
        (i64::from(y1) - i64::from(y0) + 1) as usize,
    );
    let (w32, h32) = (
        u32::try_from(w).map_err(|_| Error::corrupt("image is too wide"))?,
        u32::try_from(ht).map_err(|_| Error::corrupt("image is too tall"))?,
    );
    limits.check_pixels(w32, h32)?;
    let find = |n: &str| h.channels.iter().position(|c| c.name == n);
    let (ri, gi, bi, ai, yi) = (find("R"), find("G"), find("B"), find("A"), find("Y"));
    let colour: Vec<usize> = match (ri, gi, bi, yi) {
        (Some(r), Some(g), Some(b), _) => vec![r, g, b],
        (_, _, _, Some(y)) => vec![y],
        _ => return Err(unsupported("files without R, G, B or Y channels")),
    };
    let nc = colour.len() + usize::from(ai.is_some());
    limits.check_alloc((w * ht * nc * 4) as u64)?;
    let mut pixels = vec![0f32; w * ht * nc];
    let bytes_per_px: usize = h.channels.iter().map(Channel::bytes).sum();
    let mut notes = Notes::new();

    // Chunks.
    let (block_w, block_h, per_row, per_col) = match h.tile {
        Some((tw, th)) => (tw, th, w.div_ceil(tw), ht.div_ceil(th)),
        None => (
            w,
            h.compression.lines(),
            1,
            ht.div_ceil(h.compression.lines()),
        ),
    };
    let n_chunks = per_row * per_col;
    limits.check_entries(n_chunks as u64)?;
    let mut offsets = Vec::with_capacity(n_chunks);
    for _ in 0..n_chunks {
        offsets.push(r.u64_le()? as usize);
    }
    let mut missing = false;
    for (idx, &off) in offsets.iter().enumerate() {
        if idx % 8 == 0 {
            progress.check_cancelled()?;
        }
        let mut c = match data.get(off..) {
            Some(rest) if off != 0 => ByteReader::new(rest),
            _ => {
                missing = true;
                continue;
            }
        };
        let (px0, py0) = if h.tile.is_some() {
            let tx = c.i32_le()?;
            let ty = c.i32_le()?;
            let (lx, ly) = (c.i32_le()?, c.i32_le()?);
            if lx != 0 || ly != 0 {
                missing = true;
                continue;
            }
            (tx.max(0) as usize * block_w, ty.max(0) as usize * block_h)
        } else {
            let y = c.i32_le()?;
            ((0), (i64::from(y) - i64::from(y0)).max(0) as usize)
        };
        let size = c.i32_le()?;
        let size = usize::try_from(size).map_err(|_| Error::corrupt("negative block size"))?;
        let body = c.take(size)?;
        if px0 >= w || py0 >= ht {
            return Err(Error::corrupt("block lies outside the image"));
        }
        let bw = block_w.min(w - px0);
        let rows = block_h.min(ht - py0);
        let raw = unpack(h.compression, body, bw, rows, &h.channels, limits, progress)?;
        // Convert to floats.
        let row_bytes = bw * bytes_per_px;
        for ry in 0..rows {
            let line = &raw[ry * row_bytes..(ry + 1) * row_bytes];
            let mut at = 0;
            let mut chan_off = Vec::with_capacity(h.channels.len());
            for ch in &h.channels {
                chan_off.push(at);
                at += bw * ch.bytes();
            }
            let sample = |ci: usize, x: usize| -> f32 {
                let ch = &h.channels[ci];
                let o = chan_off[ci] + x * ch.bytes();
                match ch.kind {
                    1 => half_to_f32(u16::from_le_bytes([line[o], line[o + 1]])),
                    2 => f32::from_le_bytes([line[o], line[o + 1], line[o + 2], line[o + 3]]),
                    _ => {
                        u32::from_le_bytes([line[o], line[o + 1], line[o + 2], line[o + 3]]) as f32
                    }
                }
            };
            for x in 0..bw {
                let o = ((py0 + ry) * w + px0 + x) * nc;
                for (k, &ci) in colour.iter().enumerate() {
                    pixels[o + k] = sample(ci, x);
                }
                if let Some(a) = ai {
                    pixels[o + colour.len()] = sample(a, x);
                }
            }
        }
    }
    if missing {
        notes.push(Note::new(
            Severity::Warning,
            "exr.truncated",
            "some image data was missing; the affected area is black",
        ));
    }
    // Linear to sRGB, alpha un-premultiplied.
    let table = srgb_table();
    let layout = match (colour.len(), ai.is_some()) {
        (1, false) => Layout::Gray,
        (1, true) => Layout::GrayAlpha,
        (_, false) => Layout::Rgb,
        (_, true) => Layout::Rgba,
    };
    let mut out = Vec::with_capacity(w * ht * nc);
    for px in pixels.chunks_exact(nc) {
        let alpha = if ai.is_some() { px[colour.len()] } else { 1.0 };
        for &v in &px[..colour.len()] {
            let v = if ai.is_some() && alpha > 0.0 {
                v / alpha
            } else {
                v
            };
            out.push(to_code(&table, v));
        }
        if ai.is_some() {
            out.push((alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    notes.push(Note::new(
        Severity::Info,
        "exr.tone",
        "OpenEXR holds linear scene values; values above 1.0 were clipped and the rest encoded as sRGB",
    ));
    Ok((Image::new(w32, h32, layout, Depth::Eight, out)?, notes))
}

// --- writing --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeCompression {
    None,
    Rle,
    Zip,
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    pub compression: EncodeCompression,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            compression: EncodeCompression::Zip,
        }
    }
}

fn f32_to_half(f: f32) -> u16 {
    let bits = f.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 31 {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let round = u32::from(rem > mid || (rem == mid && half & 1 == 1));
        return sign | (half + round) as u16;
    }
    let half = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    let round = u32::from(rem > 0x1000 || (rem == 0x1000 && half & 1 == 1));
    sign | (half + round) as u16
}

fn attr(out: &mut Vec<u8>, name: &str, kind: &str, value: &[u8]) {
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(kind.as_bytes());
    out.push(0);
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
}

fn predict_and_reorder(raw: &[u8]) -> Vec<u8> {
    let n = raw.len();
    let half = n.div_ceil(2);
    let mut t = vec![0u8; n];
    for (k, &b) in raw.iter().enumerate() {
        if k % 2 == 0 {
            t[k / 2] = b;
        } else {
            t[half + k / 2] = b;
        }
    }
    for i in (1..n).rev() {
        t[i] = t[i].wrapping_sub(t[i - 1]).wrapping_add(128);
    }
    t
}

fn rle_encode(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let mut run = 1;
        while i + run < src.len() && src[i + run] == src[i] && run < 127 {
            run += 1;
        }
        if run >= 3 {
            out.push((run - 1) as u8);
            out.push(src[i]);
            i += run;
        } else {
            let start = i;
            while i < src.len() && i - start < 127 {
                let mut r = 1;
                while i + r < src.len() && src[i + r] == src[i] && r < 3 {
                    r += 1;
                }
                if r >= 3 {
                    break;
                }
                i += 1;
            }
            out.push((-((i - start) as i8)) as u8);
            out.extend_from_slice(&src[start..i]);
        }
    }
    out
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let has_alpha = img.layout.has_alpha();
    let target = if img.layout.is_gray() {
        if has_alpha {
            Layout::GrayAlpha
        } else {
            Layout::Gray
        }
    } else if has_alpha {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let (img, n) = ops::convert_layout(img, target, [255, 255, 255]);
    notes.extend(n);
    let (w, ht) = (img.width as usize, img.height as usize);
    // Channels in file (alphabetical) order.
    let names: Vec<&str> = match target {
        Layout::Gray => vec!["Y"],
        Layout::GrayAlpha => vec!["A", "Y"],
        Layout::Rgb => vec!["B", "G", "R"],
        _ => vec!["A", "B", "G", "R"],
    };
    let nch = img.layout.channels();
    // Source channel index for each file channel.
    let source: Vec<usize> = names
        .iter()
        .map(|n| match (*n, nch) {
            ("Y", _) | ("R", _) => 0,
            ("G", _) => 1,
            ("B", _) => 2,
            (_, 2) => 1,
            _ => 3,
        })
        .collect();
    let bytes = img.depth.bytes();
    let lut: Vec<f32> = match img.depth {
        Depth::Eight => (0..256u32)
            .map(|i| srgb_to_linear(f64::from(i) / 255.0) as f32)
            .collect(),
        Depth::Sixteen => (0..65536u32)
            .map(|i| srgb_to_linear(f64::from(i) / 65535.0) as f32)
            .collect(),
    };
    let sample = |px: &[u8], c: usize| -> usize {
        if bytes == 1 {
            usize::from(px[c])
        } else {
            usize::from(u16::from_be_bytes([px[2 * c], px[2 * c + 1]]))
        }
    };
    let alpha_max = img.depth.max() as f32;
    let comp_code = match opts.compression {
        EncodeCompression::None => 0u8,
        EncodeCompression::Rle => 1,
        EncodeCompression::Zip => 3,
    };
    let lines_per_block = if comp_code == 3 { 16 } else { 1 };
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&2u32.to_le_bytes());
    let mut chl = Vec::new();
    for n in &names {
        chl.extend_from_slice(n.as_bytes());
        chl.push(0);
        chl.extend_from_slice(&1u32.to_le_bytes()); // half
        chl.extend_from_slice(&[0, 0, 0, 0]);
        chl.extend_from_slice(&1i32.to_le_bytes());
        chl.extend_from_slice(&1i32.to_le_bytes());
    }
    chl.push(0);
    attr(&mut out, "channels", "chlist", &chl);
    attr(&mut out, "compression", "compression", &[comp_code]);
    let win = [0i32, 0, w as i32 - 1, ht as i32 - 1];
    let win_bytes: Vec<u8> = win.iter().flat_map(|v| v.to_le_bytes()).collect();
    attr(&mut out, "dataWindow", "box2i", &win_bytes);
    attr(&mut out, "displayWindow", "box2i", &win_bytes);
    attr(&mut out, "lineOrder", "lineOrder", &[0]);
    attr(&mut out, "pixelAspectRatio", "float", &1f32.to_le_bytes());
    attr(&mut out, "screenWindowCenter", "v2f", &[0; 8]);
    attr(&mut out, "screenWindowWidth", "float", &1f32.to_le_bytes());
    out.push(0);
    let blocks = ht.div_ceil(lines_per_block);
    let table_at = out.len();
    out.resize(out.len() + blocks * 8, 0);
    let has_a = names.contains(&"A");
    for b in 0..blocks {
        let y0 = b * lines_per_block;
        let rows = lines_per_block.min(ht - y0);
        let mut raw = Vec::with_capacity(rows * w * names.len() * 2);
        for y in y0..y0 + rows {
            let row = &img.data[y * w * nch * bytes..(y + 1) * w * nch * bytes];
            for (ci, &name) in names.iter().enumerate() {
                for px in row.chunks_exact(nch * bytes) {
                    let a = if has_a {
                        sample(px, nch - 1) as f32 / alpha_max
                    } else {
                        1.0
                    };
                    let v = if name == "A" {
                        a
                    } else {
                        lut[sample(px, source[ci])] * a
                    };
                    raw.extend_from_slice(&f32_to_half(v).to_le_bytes());
                }
            }
        }
        let packed = match opts.compression {
            EncodeCompression::None => raw.clone(),
            EncodeCompression::Rle => rle_encode(&predict_and_reorder(&raw)),
            EncodeCompression::Zip => zlib::compress(&predict_and_reorder(&raw), 6),
        };
        let body = if packed.len() < raw.len() {
            packed
        } else {
            raw
        };
        let at = out.len() as u64;
        out[table_at + b * 8..table_at + b * 8 + 8].copy_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&(y0 as i32).to_le_bytes());
        out.extend_from_slice(&(body.len() as i32).to_le_bytes());
        out.extend_from_slice(&body);
    }
    notes.push(Note::new(
        Severity::Info,
        "exr.linear",
        "sRGB samples were converted to the linear half floats that OpenEXR stores",
    ));
    Ok((out, notes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use oc_core::NoProgress;

    #[test]
    fn half_conversion_round_trips() {
        for h in 0..=0xffffu16 {
            if (h >> 10) & 0x1f == 31 {
                continue;
            }
            assert_eq!(f32_to_half(half_to_f32(h)), h, "{h:#06x}");
        }
        assert_eq!(f32_to_half(1.0), 0x3c00);
        assert_eq!(f32_to_half(70000.0), 0x7c00);
    }

    #[test]
    fn round_trip_through_every_writer_mode() {
        let mut data = Vec::new();
        for y in 0..40u32 {
            for x in 0..37u32 {
                data.extend_from_slice(&[
                    (x * 7) as u8,
                    (y * 6) as u8,
                    ((x ^ y) * 5) as u8,
                    (x + y) as u8 * 3,
                ]);
            }
        }
        let img = Image::new(37, 40, Layout::Rgba, Depth::Eight, data.clone()).unwrap();
        for c in [
            EncodeCompression::None,
            EncodeCompression::Rle,
            EncodeCompression::Zip,
        ] {
            let (bytes, _) = encode(&img, &EncodeOptions { compression: c }).unwrap();
            let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
            assert_eq!(
                (back.width, back.height, back.layout),
                (37, 40, Layout::Rgba)
            );
            for (a, b) in data.chunks(4).zip(back.data.chunks(4)) {
                assert!(a[3].abs_diff(b[3]) <= 1);
                if a[3] > 40 {
                    for k in 0..3 {
                        assert!(a[k].abs_diff(b[k]) <= 3, "{c:?} {a:?} {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode(&MAGIC, &Limits::default(), &NoProgress).is_err());
        assert!(decode(b"nope", &Limits::default(), &NoProgress).is_err());
    }
}
