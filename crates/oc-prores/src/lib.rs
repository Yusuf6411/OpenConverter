//! Apple ProRes decoder, written from SMPTE RDD 36:2022.
//!
//! Handles 4:2:2 and 4:4:4 frames, progressive and interlaced, with or without an alpha channel.
//! Output is 10-bit samples. The inverse DCT runs in double precision (the specification permits
//! any implementation that meets its accuracy annex), so results can differ from other decoders'
//! integer transforms by one level. Encoding is not provided.
#![forbid(unsafe_code)]
#![allow(clippy::indexing_slicing)] // plane, block and table indices come from the checked layout
#![allow(clippy::needless_range_loop)] // loops index several parallel arrays by block number

mod bits;

use bits::{unsigned_to_signed, Bits};
use oc_core::{Error, ErrorCode, Limits, Result};

/// A decoded frame: planar 10-bit samples, plus alpha when the stream has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// 2 for 4:2:2, 3 for 4:4:4.
    pub chroma_format: u8,
    pub interlaced: bool,
    pub chroma_width: usize,
    pub y: Vec<u16>,
    pub cb: Vec<u16>,
    pub cr: Vec<u16>,
    /// Alpha values at the stream's own depth (`alpha_bits`), one per pixel.
    pub alpha: Option<Vec<u16>>,
    pub alpha_bits: u8,
}

/// The parts of the frame header a caller may want before decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub width: usize,
    pub height: usize,
    pub chroma_format: u8,
    pub interlace_mode: u8,
    pub alpha_type: u8,
    pub frame_rate_code: u8,
    pub color_primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

const PROGRESSIVE_SCAN: [u8; 64] = [
    0, 1, 4, 5, 16, 17, 21, 22, 2, 3, 6, 7, 18, 20, 23, 28, 8, 9, 12, 13, 19, 24, 27, 29, 10, 11,
    14, 15, 25, 26, 30, 31, 32, 33, 37, 38, 45, 46, 53, 54, 34, 36, 39, 44, 47, 52, 55, 60, 35, 40,
    43, 48, 51, 56, 59, 61, 41, 42, 49, 50, 57, 58, 62, 63,
];

const INTERLACED_SCAN: [u8; 64] = [
    0, 2, 8, 10, 32, 34, 35, 41, 1, 3, 9, 11, 33, 36, 40, 42, 4, 6, 12, 14, 37, 39, 43, 49, 5, 7,
    13, 15, 38, 44, 48, 50, 16, 18, 19, 25, 45, 47, 51, 57, 17, 20, 24, 26, 46, 52, 56, 58, 21, 23,
    27, 30, 53, 55, 59, 62, 22, 28, 29, 31, 54, 60, 61, 63,
];

fn bad(msg: &str) -> Error {
    Error::corrupt(format!("ProRes: {msg}"))
}

fn be16(d: &[u8], at: usize) -> Result<usize> {
    let b = d
        .get(at..at + 2)
        .ok_or_else(|| Error::truncated("ProRes data ended early"))?;
    Ok(usize::from(u16::from_be_bytes([b[0], b[1]])))
}

fn be32(d: &[u8], at: usize) -> Result<usize> {
    let b = d
        .get(at..at + 4)
        .ok_or_else(|| Error::truncated("ProRes data ended early"))?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

struct Header {
    info: Info,
    luma_q: [u8; 64],
    chroma_q: [u8; 64],
    /// Offset of the first picture.
    pictures_at: usize,
    frame_size: usize,
}

fn parse_header(d: &[u8]) -> Result<Header> {
    if d.len() < 28 {
        return Err(Error::truncated("ProRes frame is too short"));
    }
    if &d[4..8] != b"icpf" {
        return Err(bad("missing 'icpf' frame identifier"));
    }
    let frame_size = be32(d, 0)?;
    let hs = be16(d, 8)?;
    if hs < 20 || 8 + hs > d.len() {
        return Err(bad("frame header size is out of range"));
    }
    let version = d[11];
    if version > 1 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!("ProRes bitstream version {version} is not supported"),
        ));
    }
    let width = be16(d, 16)?;
    let height = be16(d, 18)?;
    let chroma_format = d[20] >> 6;
    let interlace_mode = (d[20] >> 2) & 3;
    let alpha_type = d[25] & 15;
    if width == 0 || height == 0 {
        return Err(bad("frame has no pixels"));
    }
    if !(2..=3).contains(&chroma_format) {
        return Err(bad("reserved chroma format"));
    }
    if interlace_mode == 3 {
        return Err(bad("reserved interlace mode"));
    }
    if alpha_type > 2 {
        return Err(bad("reserved alpha channel type"));
    }
    let flags = d[27];
    let (load_luma, load_chroma) = (flags & 2 != 0, flags & 1 != 0);
    let mut at = 28;
    let read_matrix = |at: &mut usize| -> Result<[u8; 64]> {
        let m = d
            .get(*at..*at + 64)
            .ok_or_else(|| bad("quantisation matrix runs past the header"))?;
        *at += 64;
        let mut out = [0u8; 64];
        out.copy_from_slice(m);
        Ok(out)
    };
    let mut luma_q = [4u8; 64];
    if load_luma {
        luma_q = read_matrix(&mut at)?;
    }
    let chroma_q = if load_chroma {
        read_matrix(&mut at)?
    } else {
        luma_q
    };
    if at > 8 + hs {
        return Err(bad("frame header is shorter than its matrices"));
    }
    Ok(Header {
        info: Info {
            width,
            height,
            chroma_format,
            interlace_mode,
            alpha_type,
            frame_rate_code: d[21] & 15,
            color_primaries: d[22],
            transfer: d[23],
            matrix: d[24],
        },
        luma_q,
        chroma_q,
        pictures_at: 8 + hs,
        frame_size,
    })
}

/// Read the frame header without decoding.
pub fn info(frame: &[u8]) -> Result<Info> {
    Ok(parse_header(frame)?.info)
}

/// Whether the bytes begin a ProRes frame.
pub fn is_frame(d: &[u8]) -> bool {
    d.len() >= 8 && &d[4..8] == b"icpf"
}

/// slice_size_in_mb for one macroblock row (spec 6.2).
fn slice_sizes(width_in_mb: usize, log2: u32) -> Vec<usize> {
    let mut out = Vec::new();
    let mut size = 1usize << log2;
    let mut left = width_in_mb;
    loop {
        while left >= size {
            out.push(size);
            left -= size;
        }
        size /= 2;
        if left == 0 || size == 0 {
            break;
        }
    }
    out
}

/// One picture's worth of padded planes.
struct Pic {
    wmb: usize,
    hmb: usize,
    cstride: usize,
    y: Vec<u16>,
    cb: Vec<u16>,
    cr: Vec<u16>,
    alpha: Vec<u16>,
}

struct Ctx<'a> {
    hdr: &'a Header,
    scan: &'static [u8; 64],
    cos: [[f64; 8]; 8],
}

impl Ctx<'_> {
    /// Decode the scanned coefficients of one component of a slice (spec 5.3.2, 7.1.1).
    fn coeffs(&self, data: &[u8], blocks: usize, out: &mut [i32]) -> Result<()> {
        let total = blocks * 64;
        let mut b = Bits::new(data);
        let first = b.exp_golomb(5).ok_or_else(|| bad("bad DC code"))?;
        let mut dc = unsigned_to_signed(first);
        out[0] = dc;
        let mut prev_diff = 3i32;
        for n in 1..blocks {
            let s = match prev_diff.unsigned_abs() {
                0 => b.exp_golomb(0),
                1 => b.exp_golomb(1),
                2 => b.combo(1, 2, 3),
                _ => b.exp_golomb(3),
            }
            .ok_or_else(|| bad("bad DC code"))?;
            let v = unsigned_to_signed(s);
            let diff = if prev_diff >= 0 { v } else { -v };
            prev_diff = diff;
            dc = dc.wrapping_add(diff);
            out[n] = dc;
        }
        let (mut n, mut prev_run, mut prev_level) = (blocks, 4u32, 1u32);
        while !b.end_of_data() {
            let run = match prev_run {
                0 | 1 => b.combo(2, 0, 1),
                2 | 3 => b.combo(1, 0, 1),
                4 => b.exp_golomb(0),
                5..=8 => b.combo(1, 1, 2),
                9..=14 => b.exp_golomb(1),
                _ => b.exp_golomb(2),
            }
            .ok_or_else(|| bad("bad run code"))?;
            prev_run = run;
            let level = match prev_level {
                0 => b.combo(2, 0, 2),
                1 => b.combo(1, 0, 1),
                2 => b.combo(2, 0, 1),
                3 => b.exp_golomb(0),
                4..=7 => b.exp_golomb(1),
                _ => b.exp_golomb(2),
            }
            .ok_or_else(|| bad("bad level code"))?;
            prev_level = level;
            let sign = b.bit();
            n = n
                .checked_add(run as usize)
                .filter(|&p| p < total)
                .ok_or_else(|| bad("run passes the end of the slice"))?;
            let mag = i32::try_from(level)
                .map_err(|_| bad("level is too large"))?
                .saturating_add(1);
            out[n] = if sign == 1 { -mag } else { mag };
            n += 1;
            if b.overrun() {
                return Err(Error::truncated("ProRes slice data ended early"));
            }
        }
        Ok(())
    }

    /// Inverse quantise and transform one block, returning 10-bit samples.
    fn block(&self, q: &[u8; 64], qscale: i32, coef: impl Fn(usize) -> i32) -> [u16; 64] {
        let mut f = [0f64; 64];
        for pos in 0..64 {
            let qf = coef(usize::from(self.scan[pos]));
            f[pos] = f64::from(qf) * f64::from(q[pos]) * f64::from(qscale) / 8.0;
        }
        // Separable inverse DCT: rows, then columns.
        let mut tmp = [0f64; 64];
        for v in 0..8 {
            for x in 0..8 {
                let mut s = 0.0;
                for u in 0..8 {
                    s += f[v * 8 + u] * self.cos[u][x];
                }
                tmp[v * 8 + x] = s;
            }
        }
        let mut out = [0u16; 64];
        for x in 0..8 {
            for y in 0..8 {
                let mut s = 0.0;
                for v in 0..8 {
                    s += tmp[v * 8 + x] * self.cos[v][y];
                }
                let sample = (2.0 * (s + 256.0)).round();
                out[y * 8 + x] = sample.clamp(4.0, 1019.0) as u16;
            }
        }
        out
    }
}

fn cos_table() -> [[f64; 8]; 8] {
    let mut t = [[0f64; 8]; 8];
    for u in 0..8 {
        let c = if u == 0 {
            std::f64::consts::FRAC_1_SQRT_2
        } else {
            1.0
        };
        for x in 0..8 {
            t[u][x] = c * 0.5 * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos();
        }
    }
    t
}

fn put(plane: &mut [u16], stride: usize, x: usize, y: usize, blk: &[u16; 64]) {
    for r in 0..8 {
        let o = (y + r) * stride + x;
        plane[o..o + 8].copy_from_slice(&blk[r * 8..r * 8 + 8]);
    }
}

/// Alpha run length (Table 12).
fn alpha_run(b: &mut Bits) -> usize {
    if b.bit() == 1 {
        return 1;
    }
    let v = b.read(4);
    if v != 0 {
        return v as usize + 1;
    }
    b.read(11) as usize + 1
}

fn decode_alpha(data: &[u8], kind: u8, out: &mut [u16]) -> Result<()> {
    let mut b = Bits::new(data);
    let mask: i32 = if kind == 1 { 0xFF } else { 0xFFFF };
    let (mut n, mut prev) = (0usize, -1i32);
    while n < out.len() {
        let diff = if b.bit() == 1 {
            b.read(if kind == 1 { 8 } else { 16 }) as i32
        } else {
            let (mag, sgn) = if kind == 1 {
                (b.read(3), b.bit())
            } else {
                (b.read(6), b.bit())
            };
            let m = mag as i32 + 1;
            if sgn == 1 {
                -m
            } else {
                m
            }
        };
        prev = (prev + diff) & mask;
        let run = alpha_run(&mut b).min(out.len() - n);
        out[n..n + run].fill(prev as u16);
        n += run;
        if b.overrun() {
            return Err(Error::truncated("ProRes alpha data ended early"));
        }
    }
    Ok(())
}

fn decode_picture(ctx: &Ctx, d: &[u8], vsize: usize) -> Result<(Pic, usize)> {
    let info = &ctx.hdr.info;
    let ph = usize::from(
        *d.first()
            .ok_or_else(|| Error::truncated("ProRes picture is missing"))?
            >> 3,
    );
    let pic_size = be32(d, 1)?;
    let log2 = u32::from((d.get(7).copied().unwrap_or(0) >> 4) & 3);
    if ph < 8 || ph > d.len() || pic_size > d.len() || pic_size < ph {
        return Err(bad("picture header sizes are out of range"));
    }
    let wmb = info.width.div_ceil(16);
    let hmb = vsize.div_ceil(16);
    let sizes = slice_sizes(wmb, log2);
    let per_row = sizes.len();
    let table = d
        .get(ph..ph + hmb * per_row * 2)
        .ok_or_else(|| Error::truncated("ProRes slice table is cut off"))?;
    let d = &d[..pic_size];
    let c444 = info.chroma_format == 3;
    let cstride = if c444 { wmb * 16 } else { wmb * 8 };
    let mut pic = Pic {
        wmb,
        hmb,
        cstride,
        y: vec![0; wmb * 16 * hmb * 16],
        cb: vec![0; cstride * hmb * 16],
        cr: vec![0; cstride * hmb * 16],
        alpha: if info.alpha_type != 0 {
            vec![0; wmb * 16 * vsize]
        } else {
            Vec::new()
        },
    };
    let mut at = ph + table.len();
    let ystride = wmb * 16;
    let nc = if c444 { 4 } else { 2 };
    for i in 0..hmb {
        let mut mb0 = 0usize;
        for (j, &s) in sizes.iter().enumerate() {
            let k = (i * per_row + j) * 2;
            let size = usize::from(u16::from_be_bytes([table[k], table[k + 1]]));
            let slice = d
                .get(at..at + size)
                .ok_or_else(|| Error::truncated("ProRes slice runs past the picture"))?;
            at += size;
            let hs = usize::from(*slice.first().ok_or_else(|| bad("empty slice"))? >> 3);
            let hdr_len = if info.alpha_type != 0 { 8 } else { 6 };
            if hs < hdr_len || hs > slice.len() {
                return Err(bad("slice header size is out of range"));
            }
            let qi = i32::from(slice[1]);
            if !(1..=224).contains(&qi) {
                return Err(bad("quantisation index is out of range"));
            }
            let qscale = if qi <= 128 { qi } else { 128 + 4 * (qi - 128) };
            let ysz = be16(slice, 2)?;
            let cbsz = be16(slice, 4)?;
            let rest = slice.len() - hs;
            let crsz = if info.alpha_type != 0 {
                be16(slice, 6)?
            } else {
                rest.saturating_sub(ysz + cbsz)
            };
            if ysz + cbsz + crsz > rest {
                return Err(bad("slice component sizes exceed the slice"));
            }
            let ydata = &slice[hs..hs + ysz];
            let cbdata = &slice[hs + ysz..hs + ysz + cbsz];
            let crdata = &slice[hs + ysz + cbsz..hs + ysz + cbsz + crsz];
            let adata = &slice[hs + ysz + cbsz + crsz..];

            let ny = 4 * s;
            let ncb = nc * s;
            let mut yc = vec![0i32; ny * 64];
            let mut cbc = vec![0i32; ncb * 64];
            let mut crc = vec![0i32; ncb * 64];
            ctx.coeffs(ydata, ny, &mut yc)?;
            ctx.coeffs(cbdata, ncb, &mut cbc)?;
            ctx.coeffs(crdata, ncb, &mut crc)?;
            for m in 0..s {
                let mbx = mb0 + m;
                for b in 0..4 {
                    let blk = ctx.block(&ctx.hdr.luma_q, qscale, |k| yc[ny * k + 4 * m + b]);
                    put(
                        &mut pic.y,
                        ystride,
                        16 * mbx + 8 * (b & 1),
                        16 * i + 8 * (b >> 1),
                        &blk,
                    );
                }
                for b in 0..nc {
                    let (x, y) = if c444 {
                        (16 * mbx + 8 * (b >> 1), 16 * i + 8 * (b & 1))
                    } else {
                        (8 * mbx, 16 * i + 8 * b)
                    };
                    let blk = ctx.block(&ctx.hdr.chroma_q, qscale, |k| cbc[ncb * k + nc * m + b]);
                    put(&mut pic.cb, cstride, x, y, &blk);
                    let blk = ctx.block(&ctx.hdr.chroma_q, qscale, |k| crc[ncb * k + nc * m + b]);
                    put(&mut pic.cr, cstride, x, y, &blk);
                }
            }
            if info.alpha_type != 0 {
                let rows = (vsize - 16 * i).min(16);
                let w = 16 * s;
                let mut vals = vec![0u16; w * rows];
                decode_alpha(adata, info.alpha_type, &mut vals)?;
                for r in 0..rows {
                    let o = (16 * i + r) * ystride + 16 * mb0;
                    pic.alpha[o..o + w].copy_from_slice(&vals[r * w..(r + 1) * w]);
                }
            }
            mb0 += s;
        }
    }
    Ok((pic, pic_size))
}

/// Decode one frame (the whole of a ProRes packet).
pub fn decode(frame: &[u8], limits: &Limits) -> Result<Frame> {
    let hdr = parse_header(frame)?;
    let info = hdr.info.clone();
    limits.check_pixels(info.width as u32, info.height as u32)?;
    let interlaced = info.interlace_mode != 0;
    let ctx = Ctx {
        hdr: &hdr,
        scan: if interlaced {
            &INTERLACED_SCAN
        } else {
            &PROGRESSIVE_SCAN
        },
        cos: cos_table(),
    };
    let c444 = info.chroma_format == 3;
    let cw = if c444 {
        info.width
    } else {
        info.width.div_ceil(2)
    };
    let (w, h) = (info.width, info.height);
    let mut out = Frame {
        width: w,
        height: h,
        chroma_format: info.chroma_format,
        interlaced,
        chroma_width: cw,
        y: vec![0; w * h],
        cb: vec![0; cw * h],
        cr: vec![0; cw * h],
        alpha: (info.alpha_type != 0).then(|| vec![0; w * h]),
        alpha_bits: match info.alpha_type {
            1 => 8,
            2 => 16,
            _ => 0,
        },
    };
    let mut at = hdr.pictures_at;
    let count = if interlaced { 2 } else { 1 };
    for p in 0..count {
        let (vsize, row0, step) = if !interlaced {
            (h, 0, 1)
        } else {
            let top = h.div_ceil(2);
            let bottom = h / 2;
            let is_top = (info.interlace_mode == 1) == (p == 0);
            if is_top {
                (top, 0, 2)
            } else {
                (bottom, 1, 2)
            }
        };
        let d = frame
            .get(at..)
            .ok_or_else(|| Error::truncated("ProRes second picture is missing"))?;
        let (pic, used) = decode_picture(&ctx, d, vsize)?;
        at += used;
        let ys = pic.wmb * 16;
        for r in 0..vsize {
            let fr = row0 + r * step;
            out.y[fr * w..(fr + 1) * w].copy_from_slice(&pic.y[r * ys..r * ys + w]);
            out.cb[fr * cw..(fr + 1) * cw]
                .copy_from_slice(&pic.cb[r * pic.cstride..r * pic.cstride + cw]);
            out.cr[fr * cw..(fr + 1) * cw]
                .copy_from_slice(&pic.cr[r * pic.cstride..r * pic.cstride + cw]);
            if let Some(a) = out.alpha.as_mut() {
                a[fr * w..(fr + 1) * w].copy_from_slice(&pic.alpha[r * ys..r * ys + w]);
            }
        }
        let _ = pic.hmb;
    }
    let _ = hdr.frame_size;
    Ok(out)
}
