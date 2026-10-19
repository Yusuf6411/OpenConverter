//! VP8 key-frame decoder.
//!
//! The decoder follows RFC 6386 and agrees with libwebp bit for bit, including how it treats
//! out-of-range coefficients, so a damaged stream decodes to the same pixels in both.
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use crate::bool::BoolReader;
use crate::filter::{filter_mb, FilterInfo, PlaneMut};
use crate::predict::{predict_4x4, predict_block, DC_PRED, H_PRED, TM_PRED, V_PRED};
use crate::tables::{
    AC_TABLE, BANDS, BMODE_PROBS, COEFF_PROBS, COEFF_UPDATE_PROBS, DC_TABLE, ZIGZAG,
};
use crate::transform::{idct_add, iwht};
use oc_core::{Error, Progress, Result};

/// A decoded frame as three tightly packed planes; chroma is subsampled by two both ways,
/// rounding up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Frame {
    pub fn chroma_width(&self) -> usize {
        self.width.div_ceil(2)
    }
    pub fn chroma_height(&self) -> usize {
        self.height.div_ceil(2)
    }
}

struct Tag {
    width: usize,
    height: usize,
    first_part: usize,
}

fn parse_tag(data: &[u8]) -> Result<Tag> {
    let b = data
        .get(..10)
        .ok_or_else(|| Error::truncated("VP8 frame header"))?;
    let tag = u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16;
    if tag & 1 != 0 {
        return Err(Error::new(
            oc_core::ErrorCode::UnsupportedVariant,
            "VP8 inter frames are not supported in this container",
        ));
    }
    let profile = (tag >> 1) & 7;
    let show = (tag >> 4) & 1;
    if profile > 3 {
        return Err(Error::corrupt("unknown VP8 profile"));
    }
    if show == 0 {
        return Err(Error::corrupt("VP8 frame is not displayable"));
    }
    if b[3..6] != [0x9d, 0x01, 0x2a] {
        return Err(Error::corrupt("bad VP8 start code"));
    }
    let width = usize::from(u16::from_le_bytes([b[6], b[7]]) & 0x3fff);
    let height = usize::from(u16::from_le_bytes([b[8], b[9]]) & 0x3fff);
    if width == 0 || height == 0 {
        return Err(Error::corrupt("VP8 frame has no pixels"));
    }
    Ok(Tag {
        width,
        height,
        first_part: (tag >> 5) as usize,
    })
}

/// Width and height of the key frame at the start of `data`.
pub fn dimensions(data: &[u8]) -> Result<(usize, usize)> {
    let t = parse_tag(data)?;
    Ok((t.width, t.height))
}

#[derive(Default, Clone, Copy)]
struct Quant {
    y1: [i32; 2],
    y2: [i32; 2],
    uv: [i32; 2],
}

#[derive(Default)]
struct Segments {
    enabled: bool,
    update_map: bool,
    absolute: bool,
    quant: [i32; 4],
    filter: [i32; 4],
    probs: [u8; 3],
}

#[derive(Default)]
struct FilterHeader {
    simple: bool,
    level: i32,
    sharpness: i32,
    use_delta: bool,
    ref_delta: [i32; 4],
    mode_delta: [i32; 4],
}

type Probs = [[[[u8; 11]; 3]; 8]; 4];

#[derive(Clone, Copy, Default)]
struct Nz {
    y: [bool; 4],
    u: [bool; 2],
    v: [bool; 2],
    dc: bool,
}

fn large_value(br: &mut BoolReader, p: &[u8; 11]) -> i32 {
    if !br.get(p[3]) {
        if !br.get(p[4]) {
            2
        } else {
            3 + i32::from(br.get(p[5]))
        }
    } else if !br.get(p[6]) {
        if !br.get(p[7]) {
            5 + i32::from(br.get(159))
        } else {
            let mut v = 7 + 2 * i32::from(br.get(165));
            v += i32::from(br.get(145));
            v
        }
    } else {
        let bit1 = br.get(p[8]);
        let bit0 = br.get(p[9 + usize::from(bit1)]);
        let cat = 2 * usize::from(bit1) + usize::from(bit0);
        const CAT: [&[u8]; 4] = [
            &[173, 148, 140],
            &[176, 155, 140, 135],
            &[180, 157, 141, 134, 130],
            &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
        ];
        let mut v = 0;
        for &pr in CAT[cat] {
            v = 2 * v + i32::from(br.get(pr));
        }
        v + 3 + (8 << cat)
    }
}

/// Reads the tokens of one block into `out` (dequantised); returns the position after the
/// last coefficient read (0 when the block is empty).
fn get_coeffs(
    br: &mut BoolReader,
    probs: &[[[u8; 11]; 3]; 8],
    ctx: usize,
    dq: [i32; 2],
    first: usize,
    out: &mut [i16],
) -> usize {
    let mut n = first;
    let mut p = &probs[usize::from(BANDS[n])][ctx];
    while n < 16 {
        if !br.get(p[0]) {
            return n;
        }
        while !br.get(p[1]) {
            n += 1;
            p = &probs[usize::from(BANDS[n])][0];
            if n == 16 {
                return 16;
            }
        }
        let next = &probs[usize::from(BANDS[n + 1])];
        let v;
        if !br.get(p[2]) {
            v = 1;
            p = &next[1];
        } else {
            v = large_value(br, p);
            p = &next[2];
        }
        out[usize::from(ZIGZAG[n])] = (br.sign(v) * dq[usize::from(n > 0)]) as i16;
        n += 1;
    }
    16
}

fn nz_code(nz: usize, dc: i16) -> u8 {
    if nz > 3 {
        3
    } else if nz > 1 {
        2
    } else {
        u8::from(dc != 0)
    }
}

struct Planes {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    ys: usize,
    uvs: usize,
}

pub fn decode(data: &[u8], progress: &dyn Progress) -> Result<Frame> {
    let tag = parse_tag(data)?;
    let part0 = data
        .get(10..10 + tag.first_part)
        .ok_or_else(|| Error::truncated("VP8 first partition"))?;
    let rest = &data[10 + tag.first_part..];
    let mb_w = tag.width.div_ceil(16);
    let mb_h = tag.height.div_ceil(16);
    let mut br = BoolReader::new(part0);

    // Frame header.
    let _colorspace = br.get_bit();
    let _clamp = br.get_bit();
    let mut seg = Segments {
        probs: [255; 3],
        absolute: true,
        ..Segments::default()
    };
    seg.enabled = br.get_bit();
    if seg.enabled {
        seg.update_map = br.get_bit();
        if br.get_bit() {
            seg.absolute = br.get_bit();
            for q in &mut seg.quant {
                *q = if br.get_bit() { br.signed(7) } else { 0 };
            }
            for f in &mut seg.filter {
                *f = if br.get_bit() { br.signed(6) } else { 0 };
            }
        }
        if seg.update_map {
            for p in &mut seg.probs {
                *p = if br.get_bit() { br.value(8) as u8 } else { 255 };
            }
        }
    }
    let mut fh = FilterHeader {
        simple: br.get_bit(),
        level: br.value(6) as i32,
        sharpness: br.value(3) as i32,
        ..FilterHeader::default()
    };
    fh.use_delta = br.get_bit();
    if fh.use_delta && br.get_bit() {
        for d in &mut fh.ref_delta {
            if br.get_bit() {
                *d = br.signed(6);
            }
        }
        for d in &mut fh.mode_delta {
            if br.get_bit() {
                *d = br.signed(6);
            }
        }
    }
    let filter_type = if fh.level == 0 {
        0
    } else if fh.simple {
        1
    } else {
        2
    };

    // Token partitions.
    let num_parts = 1usize << br.value(2);
    let sizes_len = 3 * (num_parts - 1);
    if rest.len() < sizes_len {
        return Err(Error::truncated("VP8 partition sizes"));
    }
    let (sizes, mut body) = rest.split_at(sizes_len);
    let mut parts: Vec<&[u8]> = Vec::with_capacity(num_parts);
    for i in 0..num_parts - 1 {
        let s = &sizes[3 * i..3 * i + 3];
        let size =
            (usize::from(s[0]) | usize::from(s[1]) << 8 | usize::from(s[2]) << 16).min(body.len());
        let (a, b) = body.split_at(size);
        parts.push(a);
        body = b;
    }
    if body.is_empty() {
        return Err(Error::truncated("VP8 token partition"));
    }
    parts.push(body);
    let mut token_brs: Vec<BoolReader> = parts.into_iter().map(BoolReader::new).collect();

    // Quantisers.
    let base_q = br.value(7) as i32;
    let mut delta = || if br.get_bit() { br.signed(4) } else { 0 };
    let (dqy1_dc, dqy2_dc, dqy2_ac, dquv_dc, dquv_ac) =
        (delta(), delta(), delta(), delta(), delta());
    let clip = |v: i32, m: i32| v.clamp(0, m) as usize;
    let mut quants = [Quant::default(); 4];
    for (i, m) in quants.iter_mut().enumerate() {
        let q = if seg.enabled {
            let mut q = seg.quant[i];
            if !seg.absolute {
                q += base_q;
            }
            q
        } else {
            base_q
        };
        m.y1 = [
            i32::from(DC_TABLE[clip(q + dqy1_dc, 127)]),
            i32::from(AC_TABLE[clip(q, 127)]),
        ];
        m.y2 = [
            i32::from(DC_TABLE[clip(q + dqy2_dc, 127)]) * 2,
            ((i32::from(AC_TABLE[clip(q + dqy2_ac, 127)]) * 101_581) >> 16).max(8),
        ];
        m.uv = [
            i32::from(DC_TABLE[clip(q + dquv_dc, 117)]),
            i32::from(AC_TABLE[clip(q + dquv_ac, 127)]),
        ];
    }
    let _refresh_entropy = br.get_bit();

    // Token probabilities.
    let mut probs: Probs = COEFF_PROBS;
    for t in 0..4 {
        for b in 0..8 {
            for c in 0..3 {
                for p in 0..11 {
                    if br.get(COEFF_UPDATE_PROBS[t][b][c][p]) {
                        probs[t][b][c][p] = br.value(8) as u8;
                    }
                }
            }
        }
    }
    let use_skip = br.get_bit();
    let skip_prob = if use_skip { br.value(8) as u8 } else { 0 };
    if br.eof() {
        return Err(Error::truncated("VP8 frame header"));
    }

    // Loop filter strengths per segment and per prediction kind.
    let mut fstrength = [[FilterInfo::default(); 2]; 4];
    if filter_type > 0 {
        for (s, row) in fstrength.iter_mut().enumerate() {
            let base = if seg.enabled {
                let mut b = seg.filter[s];
                if !seg.absolute {
                    b += fh.level;
                }
                b
            } else {
                fh.level
            };
            for (i4, info) in row.iter_mut().enumerate() {
                let mut level = base;
                if fh.use_delta {
                    level += fh.ref_delta[0];
                    if i4 == 1 {
                        level += fh.mode_delta[0];
                    }
                }
                let level = level.clamp(0, 63);
                if level > 0 {
                    let mut il = level;
                    if fh.sharpness > 0 {
                        il >>= if fh.sharpness > 4 { 2 } else { 1 };
                        il = il.min(9 - fh.sharpness);
                    }
                    let il = il.max(1);
                    info.ilevel = il as u8;
                    info.limit = (2 * level + il) as u8;
                    info.hev_thresh = if level >= 40 {
                        2
                    } else if level >= 15 {
                        1
                    } else {
                        0
                    };
                }
                info.inner = i4 == 1;
            }
        }
    }

    // Planes with a one-pixel border above and to the left.
    let ys = mb_w * 16 + 1;
    let uvs = mb_w * 8 + 1;
    let mut pl = Planes {
        y: vec![0; ys * (mb_h * 16 + 1)],
        u: vec![0; uvs * (mb_h * 8 + 1)],
        v: vec![0; uvs * (mb_h * 8 + 1)],
        ys,
        uvs,
    };
    for (buf, stride) in [(&mut pl.y, ys), (&mut pl.u, uvs), (&mut pl.v, uvs)] {
        buf[..stride].fill(127);
        for row in buf.chunks_exact_mut(stride).skip(1) {
            row[0] = 129;
        }
    }

    let mut top_modes = vec![0u8; mb_w * 4];
    let mut top_nz = vec![Nz::default(); mb_w];
    let mut finfo: Vec<FilterInfo> = vec![FilterInfo::default(); mb_w * mb_h];
    let mut coeffs = [0i16; 384];

    for mb_y in 0..mb_h {
        progress.check_cancelled()?;
        progress.report(mb_y as u64, mb_h as u64, "decode");
        let mut left_modes = [0u8; 4];
        let mut left_nz = Nz::default();
        let tbr = token_brs.len() - 1;
        let tok = &mut token_brs[mb_y & tbr];
        for mb_x in 0..mb_w {
            // Modes (first partition).
            let segment = if seg.update_map {
                if !br.get(seg.probs[0]) {
                    usize::from(br.get(seg.probs[1]))
                } else {
                    usize::from(br.get(seg.probs[2])) + 2
                }
            } else {
                0
            };
            let skip_flag = use_skip && br.get(skip_prob);
            let is_i4 = !br.get(145);
            let mut ymode = 0u8;
            let mut bmodes = [0u8; 16];
            if !is_i4 {
                ymode = if br.get(156) {
                    if br.get(128) {
                        TM_PRED
                    } else {
                        H_PRED
                    }
                } else if br.get(163) {
                    V_PRED
                } else {
                    DC_PRED
                };
                top_modes[mb_x * 4..mb_x * 4 + 4].fill(ymode);
                left_modes.fill(ymode);
            } else {
                for by in 0..4 {
                    let mut left = left_modes[by];
                    for bx in 0..4 {
                        let pr =
                            &BMODE_PROBS[usize::from(top_modes[mb_x * 4 + bx])][usize::from(left)];
                        let mut i = [
                            0i8, 1, -1, 2, -2, 3, 4, 6, -3, 5, -4, -5, -6, 7, -7, 8, -8, -9,
                        ][usize::from(br.get(pr[0]))];
                        while i > 0 {
                            let idx = 2 * i as usize + usize::from(br.get(pr[i as usize]));
                            i = [
                                0i8, 1, -1, 2, -2, 3, 4, 6, -3, 5, -4, -5, -6, 7, -7, 8, -8, -9,
                            ][idx];
                        }
                        let mode = (-i) as u8;
                        top_modes[mb_x * 4 + bx] = mode;
                        left = mode;
                        bmodes[by * 4 + bx] = mode;
                    }
                    left_modes[by] = left;
                }
            }
            let uvmode = if !br.get(142) {
                DC_PRED
            } else if !br.get(114) {
                V_PRED
            } else if br.get(183) {
                TM_PRED
            } else {
                H_PRED
            };

            // Residuals (token partition).
            coeffs.fill(0);
            let mut codes = [0u8; 24];
            let top = &mut top_nz[mb_x];
            let mut skip = skip_flag;
            if !skip {
                let q = &quants[segment];
                let mut first = 0;
                let mut ac_type = 3;
                if !is_i4 {
                    let mut dc = [0i16; 16];
                    let ctx = usize::from(top.dc) + usize::from(left_nz.dc);
                    let nz = get_coeffs(tok, &probs[1], ctx, q.y2, 0, &mut dc);
                    top.dc = nz > 0;
                    left_nz.dc = nz > 0;
                    if nz > 1 {
                        iwht(&dc, &mut coeffs);
                    } else {
                        let dc0 = ((i32::from(dc[0]) + 3) >> 3) as i16;
                        for b in 0..16 {
                            coeffs[b * 16] = dc0;
                        }
                    }
                    first = 1;
                    ac_type = 0;
                }
                let mut any = 0u8;
                for by in 0..4 {
                    let mut l = left_nz.y[by];
                    for bx in 0..4 {
                        let b = by * 4 + bx;
                        let ctx = usize::from(l) + usize::from(top.y[bx]);
                        let nz = get_coeffs(
                            tok,
                            &probs[ac_type],
                            ctx,
                            q.y1,
                            first,
                            &mut coeffs[b * 16..b * 16 + 16],
                        );
                        l = nz > first;
                        top.y[bx] = l;
                        codes[b] = nz_code(nz, coeffs[b * 16]);
                        any |= codes[b];
                    }
                    left_nz.y[by] = l;
                }
                for ch in 0..2 {
                    let (tn, ln) = if ch == 0 {
                        (&mut top.u, &mut left_nz.u)
                    } else {
                        (&mut top.v, &mut left_nz.v)
                    };
                    for by in 0..2 {
                        let mut l = ln[by];
                        for bx in 0..2 {
                            let b = 16 + ch * 4 + by * 2 + bx;
                            let ctx = usize::from(l) + usize::from(tn[bx]);
                            let nz = get_coeffs(
                                tok,
                                &probs[2],
                                ctx,
                                q.uv,
                                0,
                                &mut coeffs[b * 16..b * 16 + 16],
                            );
                            l = nz > 0;
                            tn[bx] = l;
                            codes[b] = nz_code(nz, coeffs[b * 16]);
                            any |= codes[b];
                        }
                        ln[by] = l;
                    }
                }
                skip = any == 0;
            } else {
                top.y = [false; 4];
                top.u = [false; 2];
                top.v = [false; 2];
                left_nz.y = [false; 4];
                left_nz.u = [false; 2];
                left_nz.v = [false; 2];
                if !is_i4 {
                    top.dc = false;
                    left_nz.dc = false;
                }
            }
            if tok.eof() {
                return Err(Error::truncated("VP8 token partition"));
            }
            if filter_type > 0 {
                let mut info = fstrength[segment][usize::from(is_i4)];
                info.inner |= !skip;
                finfo[mb_y * mb_w + mb_x] = info;
            }

            // Reconstruction.
            reconstruct_luma(
                &mut pl, mb_x, mb_y, mb_w, is_i4, ymode, &bmodes, &coeffs, &codes,
            );
            reconstruct_chroma(&mut pl, mb_x, mb_y, uvmode, &coeffs, &codes);
        }
        if br.eof() {
            return Err(Error::truncated("VP8 first partition"));
        }
    }

    if filter_type > 0 {
        let (yo, uo) = (ys + 1, uvs + 1);
        let Planes { y, u, v, .. } = &mut pl;
        let mut yp = PlaneMut {
            data: y,
            stride: ys,
            origin: yo,
        };
        let mut up = PlaneMut {
            data: u,
            stride: uvs,
            origin: uo,
        };
        let mut vp = PlaneMut {
            data: v,
            stride: uvs,
            origin: uo,
        };
        for mb_y in 0..mb_h {
            progress.check_cancelled()?;
            for mb_x in 0..mb_w {
                filter_mb(
                    filter_type == 1,
                    finfo[mb_y * mb_w + mb_x],
                    mb_x,
                    mb_y,
                    &mut yp,
                    &mut up,
                    &mut vp,
                );
            }
        }
    }
    progress.report(mb_h as u64, mb_h as u64, "decode");

    // Crop to the visible area.
    let (w, h) = (tag.width, tag.height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let crop = |src: &[u8], stride: usize, w: usize, h: usize| -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            let s = (row + 1) * stride + 1;
            out.extend_from_slice(&src[s..s + w]);
        }
        out
    };
    Ok(Frame {
        width: w,
        height: h,
        y: crop(&pl.y, ys, w, h),
        u: crop(&pl.u, uvs, cw, ch),
        v: crop(&pl.v, uvs, cw, ch),
    })
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_luma(
    pl: &mut Planes,
    mb_x: usize,
    mb_y: usize,
    mb_w: usize,
    is_i4: bool,
    ymode: u8,
    bmodes: &[u8; 16],
    coeffs: &[i16; 384],
    codes: &[u8; 24],
) {
    let stride = pl.ys;
    let ox = mb_x * 16 + 1;
    let oy = mb_y * 16 + 1;
    let origin = oy * stride + ox;
    if !is_i4 {
        let mut top = [0u8; 16];
        let mut left = [0u8; 16];
        top.copy_from_slice(&pl.y[origin - stride..origin - stride + 16]);
        for (r, l) in left.iter_mut().enumerate() {
            *l = pl.y[origin + r * stride - 1];
        }
        let tl = pl.y[origin - stride - 1];
        let mut pred = [0u8; 256];
        predict_block(ymode, 16, &top, &left, tl, mb_y > 0, mb_x > 0, &mut pred);
        for r in 0..16 {
            pl.y[origin + r * stride..origin + r * stride + 16]
                .copy_from_slice(&pred[r * 16..r * 16 + 16]);
        }
        for b in 0..16 {
            if codes[b] != 0 {
                let pos = origin + (b / 4) * 4 * stride + (b % 4) * 4;
                idct_add(&coeffs[b * 16..b * 16 + 16], &mut pl.y, pos, stride);
            }
        }
        return;
    }
    // Above-right samples for the right-hand column of sub-blocks.
    let tr: [u8; 4] = if mb_y == 0 {
        [127; 4]
    } else if mb_x == mb_w - 1 {
        [pl.y[origin - stride + 15]; 4]
    } else {
        let s = origin - stride + 16;
        [pl.y[s], pl.y[s + 1], pl.y[s + 2], pl.y[s + 3]]
    };
    for b in 0..16 {
        let (bx, by) = (b % 4, b / 4);
        let pos = origin + by * 4 * stride + bx * 4;
        let mut top = [0u8; 8];
        top[..4].copy_from_slice(&pl.y[pos - stride..pos - stride + 4]);
        if bx < 3 {
            top[4..].copy_from_slice(&pl.y[pos - stride + 4..pos - stride + 8]);
        } else {
            top[4..].copy_from_slice(&tr);
        }
        let left = [
            pl.y[pos - 1],
            pl.y[pos + stride - 1],
            pl.y[pos + 2 * stride - 1],
            pl.y[pos + 3 * stride - 1],
        ];
        let x = pl.y[pos - stride - 1];
        let pred = predict_4x4(bmodes[b], x, &top, &left);
        for r in 0..4 {
            pl.y[pos + r * stride..pos + r * stride + 4].copy_from_slice(&pred[r * 4..r * 4 + 4]);
        }
        if codes[b] != 0 {
            idct_add(&coeffs[b * 16..b * 16 + 16], &mut pl.y, pos, stride);
        }
    }
}

fn reconstruct_chroma(
    pl: &mut Planes,
    mb_x: usize,
    mb_y: usize,
    mode: u8,
    coeffs: &[i16; 384],
    codes: &[u8; 24],
) {
    let stride = pl.uvs;
    let origin = (mb_y * 8 + 1) * stride + mb_x * 8 + 1;
    for ch in 0..2 {
        let plane = if ch == 0 { &mut pl.u } else { &mut pl.v };
        let mut top = [0u8; 8];
        let mut left = [0u8; 8];
        top.copy_from_slice(&plane[origin - stride..origin - stride + 8]);
        for (r, l) in left.iter_mut().enumerate() {
            *l = plane[origin + r * stride - 1];
        }
        let tl = plane[origin - stride - 1];
        let mut pred = [0u8; 64];
        predict_block(mode, 8, &top, &left, tl, mb_y > 0, mb_x > 0, &mut pred);
        for r in 0..8 {
            plane[origin + r * stride..origin + r * stride + 8]
                .copy_from_slice(&pred[r * 8..r * 8 + 8]);
        }
        for b in 0..4 {
            let blk = 16 + ch * 4 + b;
            if codes[blk] != 0 {
                let pos = origin + (b / 2) * 4 * stride + (b % 2) * 4;
                idct_add(&coeffs[blk * 16..blk * 16 + 16], plane, pos, stride);
            }
        }
    }
}
