//! JPEG 2000 Part 1 decoder (ITU-T T.800 / ISO/IEC 15444-1), written for OpenConverter.
//!
//! Reads raw codestreams (`.j2k`, `.j2c`) and JP2 files: tiles, every progression order, layers,
//! precincts, all code-block styles, 5/3 and 9/7 wavelets, component transforms, ROI shift,
//! subsampled components, palettes, channel definitions and ICC profiles.
#![forbid(unsafe_code)]
#![allow(
    dead_code,
    unused_assignments,
    clippy::indexing_slicing,
    clippy::manual_is_multiple_of,
    clippy::manual_div_ceil,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

mod codestream;
mod dwt;
mod mq;
mod t1;
mod t2;

use oc_core::{Error, ErrorCode, Limits, Result};

/// A decoded picture: interleaved samples, 8 or 16 bits as `sixteen` says.
#[derive(Debug, Clone, Default)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// 1 grey, 2 grey+alpha, 3 RGB, 4 RGBA
    pub channels: usize,
    pub sixteen: bool,
    pub samples: Vec<u16>,
    pub icc: Option<Vec<u8>>,
}

/// The decoded component planes before any colour handling: (width, height, samples).
pub fn decode_components(data: &[u8], limits: &Limits) -> Result<Vec<(usize, usize, Vec<i32>)>> {
    let cs = if data.starts_with(&[0xFF, 0x4F]) {
        data
    } else {
        let (s, l) = parse_jp2(data)
            .codestream
            .ok_or_else(|| Error::corrupt("JPEG 2000: no codestream box"))?;
        &data[s..s + l]
    };
    Ok(codestream::decode(cs, limits)?
        .comps
        .into_iter()
        .map(|c| (c.w, c.h, c.data))
        .collect())
}

/// True for a JP2 file or a bare codestream.
pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(&[
        0, 0, 0, 0x0c, b'j', b'P', b' ', b' ', 0x0d, 0x0a, 0x87, 0x0a,
    ]) || data.starts_with(&[0xFF, 0x4F, 0xFF, 0x51])
}

#[derive(Default)]
struct Jp2 {
    codestream: Option<(usize, usize)>,
    enumcs: Option<u32>,
    icc: Option<Vec<u8>>,
    palette: Option<Palette>,
    cmap: Vec<(u16, u8, u8)>,
    cdef: Vec<(u16, u16, u16)>,
}

struct Palette {
    entries: usize,
    cols: Vec<(u32, bool)>,
    data: Vec<Vec<i32>>,
}

fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(o)?,
        *b.get(o + 1)?,
        *b.get(o + 2)?,
        *b.get(o + 3)?,
    ]))
}

fn boxes(d: &[u8], base: usize, f: &mut dyn FnMut(&[u8; 4], usize, usize)) {
    let mut o = 0;
    while o + 8 <= d.len() {
        let size = be32(d, o).unwrap_or(0) as usize;
        let kind = [d[o + 4], d[o + 5], d[o + 6], d[o + 7]];
        let (hdr, len) = match size {
            0 => (8, d.len() - o),
            1 => match (be32(d, o + 8), be32(d, o + 12)) {
                (Some(h), Some(l)) => (16, ((u64::from(h) << 32) | u64::from(l)) as usize),
                _ => break,
            },
            n => (8, n),
        };
        if len < hdr || o + len > d.len() {
            // a truncated final box (typically the codestream) runs to the end
            if len >= hdr && kind == *b"jp2c" {
                f(&kind, base + o + hdr, d.len() - o - hdr);
            }
            break;
        }
        f(&kind, base + o + hdr, len - hdr);
        o += len;
    }
}

fn parse_jp2(data: &[u8]) -> Jp2 {
    let mut j = Jp2::default();
    let mut jp2h: Option<(usize, usize)> = None;
    boxes(data, 0, &mut |k, s, l| match k {
        b"jp2c" if j.codestream.is_none() => j.codestream = Some((s, l)),
        b"jp2h" => jp2h = Some((s, l)),
        _ => {}
    });
    if let Some((s, l)) = jp2h {
        let inner = &data[s..s + l];
        boxes(inner, s, &mut |k, bs, bl| {
            let b = &data[bs..bs + bl];
            match k {
                b"colr" => {
                    if b.first() == Some(&1) && j.enumcs.is_none() {
                        j.enumcs = be32(b, 3);
                    } else if b.first() == Some(&2) && j.icc.is_none() {
                        j.icc = Some(b[3..].to_vec());
                    }
                }
                b"pclr" => {
                    if let (Some(ne), Some(&npc)) = (be16(b, 0), b.get(2)) {
                        let (ne, npc) = (usize::from(ne), usize::from(npc));
                        let mut cols = Vec::new();
                        for i in 0..npc {
                            let v = b.get(3 + i).copied().unwrap_or(7);
                            cols.push((u32::from(v & 0x7f) + 1, v & 0x80 != 0));
                        }
                        let mut o = 3 + npc;
                        let mut data_v = vec![Vec::with_capacity(ne); npc];
                        'outer: for _ in 0..ne {
                            for (i, &(bits, signed)) in cols.iter().enumerate() {
                                let nb = (bits as usize).div_ceil(8);
                                let Some(s) = b.get(o..o + nb) else {
                                    break 'outer;
                                };
                                let mut v: i64 = 0;
                                for &x in s {
                                    v = (v << 8) | i64::from(x);
                                }
                                if signed && v >= 1 << (bits - 1) {
                                    v -= 1 << bits;
                                }
                                data_v[i].push(v as i32);
                                o += nb;
                            }
                        }
                        j.palette = Some(Palette {
                            entries: ne,
                            cols,
                            data: data_v,
                        });
                    }
                }
                b"cmap" => {
                    let mut o = 0;
                    while o + 4 <= b.len() {
                        j.cmap.push((be16(b, o).unwrap_or(0), b[o + 2], b[o + 3]));
                        o += 4;
                    }
                }
                b"cdef" => {
                    let n = usize::from(be16(b, 0).unwrap_or(0));
                    for i in 0..n {
                        let o = 2 + i * 6;
                        if let (Some(c), Some(t), Some(a)) =
                            (be16(b, o), be16(b, o + 2), be16(b, o + 4))
                        {
                            j.cdef.push((c, t, a));
                        }
                    }
                }
                _ => {}
            }
        });
    }
    j
}

/// One output channel at full resolution.
struct Channel {
    w: usize,
    h: usize,
    prec: u32,
    signed: bool,
    data: Vec<i32>,
}

fn upsample(c: &codestream::Component, w: usize, h: usize, xo: u32, yo: u32) -> Vec<i32> {
    if c.w == w && c.h == h {
        return c.data.clone();
    }
    let (dx, dy) = (c.info.dx as usize, c.info.dy as usize);
    // component sample (i, j) covers reference positions (i*dx .. ) relative to the component origin
    let cx0 = (xo as usize).div_ceil(dx);
    let cy0 = (yo as usize).div_ceil(dy);
    let mut out = vec![0; w * h];
    for y in 0..h {
        let cy = ((y + yo as usize) / dy)
            .saturating_sub(cy0)
            .min(c.h.saturating_sub(1));
        for x in 0..w {
            let cx = ((x + xo as usize) / dx)
                .saturating_sub(cx0)
                .min(c.w.saturating_sub(1));
            out[y * w + x] = c.data[cy * c.w + cx];
        }
    }
    out
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<Picture> {
    let (jp2, cs): (Option<Jp2>, &[u8]) = if data.starts_with(&[0xFF, 0x4F]) {
        (None, data)
    } else {
        let j = parse_jp2(data);
        let (s, l) = j
            .codestream
            .ok_or_else(|| Error::corrupt("JPEG 2000: no codestream box"))?;
        (Some(j), &data[s..s + l])
    };
    let dec = codestream::decode(cs, limits)?;
    let (w, h) = (dec.width as usize, dec.height as usize);
    // Reference grid origin is needed for subsampled upsampling; recover it from the first component.
    let mut chans: Vec<Channel> = dec
        .comps
        .iter()
        .map(|c| Channel {
            w,
            h,
            prec: c.info.prec,
            signed: c.info.signed,
            data: upsample(c, w, h, 0, 0),
        })
        .collect();
    let mut icc = None;
    let mut enumcs = None;
    let mut cdef: Vec<(u16, u16, u16)> = Vec::new();
    if let Some(j) = &jp2 {
        icc = j.icc.clone();
        enumcs = j.enumcs;
        cdef = j.cdef.clone();
        if let (Some(p), false) = (&j.palette, j.cmap.is_empty()) {
            let mut out = Vec::new();
            for &(cmp, mtyp, pcol) in &j.cmap {
                let src = chans
                    .get(usize::from(cmp))
                    .ok_or_else(|| Error::corrupt("JPEG 2000: cmap component"))?;
                if mtyp == 1 {
                    let col = usize::from(pcol);
                    let (bits, signed) = *p
                        .cols
                        .get(col)
                        .ok_or_else(|| Error::corrupt("JPEG 2000: cmap column"))?;
                    let mut d = Vec::with_capacity(src.data.len());
                    for &v in &src.data {
                        let i = (v.max(0) as usize).min(p.entries.saturating_sub(1));
                        d.push(p.data[col].get(i).copied().unwrap_or(0));
                    }
                    out.push(Channel {
                        w,
                        h,
                        prec: bits,
                        signed,
                        data: d,
                    });
                } else {
                    out.push(Channel {
                        w,
                        h,
                        prec: src.prec,
                        signed: src.signed,
                        data: src.data.clone(),
                    });
                }
            }
            chans = out;
        }
    }
    // Order channels by the channel definitions: colour channels by association, then alpha.
    let n = chans.len();
    let mut colour: Vec<usize> = Vec::new();
    let mut alpha: Option<usize> = None;
    if cdef.is_empty() {
        let k = match n {
            1 | 3 => n,
            2 => 1,
            _ => 3,
        };
        colour = (0..k).collect();
        if n == 2 || n >= 4 {
            alpha = Some(k);
        }
    } else {
        let mut cols: Vec<(u16, usize)> = Vec::new();
        for &(c, t, a) in &cdef {
            let c = usize::from(c);
            if c >= n {
                continue;
            }
            match t {
                0 if a >= 1 => cols.push((a, c)),
                1 | 2 if alpha.is_none() => alpha = Some(c),
                _ => {}
            }
        }
        cols.sort_unstable();
        colour = cols.into_iter().map(|x| x.1).collect();
        if colour.is_empty() {
            colour = (0..n.min(3)).collect();
        }
    }
    // sYCC to RGB; CMYK to RGB
    let to_f = |c: &Channel, i: usize| -> f32 {
        let off = if c.signed { 1i64 << (c.prec - 1) } else { 0 };
        (i64::from(c.data[i]) + off) as f32 / ((1u64 << c.prec) - 1) as f32
    };
    let npx = w * h;
    let mut rgb: Vec<[f32; 3]> = Vec::new();
    let mut grey: Vec<f32> = Vec::new();
    let is_rgb;
    match (enumcs, colour.len()) {
        (Some(18), 3) | (Some(24), 3) => {
            is_rgb = true;
            for i in 0..npx {
                let (y, cb, cr) = (
                    to_f(&chans[colour[0]], i),
                    to_f(&chans[colour[1]], i) - 0.5,
                    to_f(&chans[colour[2]], i) - 0.5,
                );
                rgb.push([
                    y + 1.402 * cr,
                    y - 0.344_136 * cb - 0.714_136 * cr,
                    y + 1.772 * cb,
                ]);
            }
        }
        (Some(12), k) if k >= 4 => {
            is_rgb = true;
            for i in 0..npx {
                let (c, m, y, kk) = (
                    to_f(&chans[colour[0]], i),
                    to_f(&chans[colour[1]], i),
                    to_f(&chans[colour[2]], i),
                    to_f(&chans[colour[3]], i),
                );
                rgb.push([
                    (1.0 - c) * (1.0 - kk),
                    (1.0 - m) * (1.0 - kk),
                    (1.0 - y) * (1.0 - kk),
                ]);
            }
        }
        (_, 1) | (_, 2) => {
            is_rgb = false;
            for i in 0..npx {
                grey.push(to_f(&chans[colour[0]], i));
            }
        }
        _ => {
            is_rgb = true;
            if colour.len() < 3 {
                return Err(Error::new(
                    ErrorCode::UnsupportedFormat,
                    "JPEG 2000: unsupported channel layout",
                ));
            }
        }
    }
    let max_prec = chans.iter().map(|c| c.prec).max().unwrap_or(8);
    let sixteen = max_prec > 8;
    let omax = if sixteen { 65535.0 } else { 255.0 };
    let ch = usize::from(is_rgb) * 2 + 1 + usize::from(alpha.is_some());
    let mut samples = Vec::with_capacity(npx * ch);
    let q = |v: f32| (v * omax + 0.5).clamp(0.0, omax) as u16;
    for i in 0..npx {
        if is_rgb {
            if rgb.is_empty() {
                for &c in colour.iter().take(3) {
                    samples.push(q(to_f(&chans[c], i)));
                }
            } else {
                for v in rgb[i] {
                    samples.push(q(v));
                }
            }
        } else {
            samples.push(q(grey[i]));
        }
        if let Some(a) = alpha {
            samples.push(q(to_f(&chans[a], i)));
        }
    }
    Ok(Picture {
        width: dec.width,
        height: dec.height,
        channels: ch,
        sixteen,
        samples,
        icc,
    })
}
