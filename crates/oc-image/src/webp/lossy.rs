//! The lossy half of WebP: a VP8 key frame, optionally with an `ALPH` alpha plane.
//!
//! Colour conversion and chroma upsampling follow libwebp's integer arithmetic, so decoded
//! pixels are identical to its output.
#![allow(clippy::indexing_slicing)]

use super::vp8l;
use oc_core::{Error, ErrorCode, Limits, Progress, Result};
use oc_vp8::Frame;

fn mult_hi(v: i32, coeff: i32) -> i32 {
    (v * coeff) >> 8
}

fn clip8(v: i32) -> u8 {
    if v & !16383 == 0 {
        (v >> 6) as u8
    } else if v < 0 {
        0
    } else {
        255
    }
}

pub(super) fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let (y, u, v) = (i32::from(y), i32::from(u), i32::from(v));
    let luma = mult_hi(y, 19077);
    [
        clip8(luma + mult_hi(v, 26149) - 14234),
        clip8(luma - mult_hi(u, 6419) - mult_hi(v, 13320) + 8708),
        clip8(luma + mult_hi(u, 33050) - 17685),
    ]
}

/// One row of a chroma plane at full width: the nearer chroma row weighs three, the farther
/// one a quarter, and neighbours along the row are blended the same way.
fn upsample_line(near: &[u8], far: &[u8], out: &mut [u8]) {
    let w = out.len();
    let n = |i: usize| u32::from(near[i]);
    let f = |i: usize| u32::from(far[i]);
    out[0] = ((3 * n(0) + f(0) + 2) >> 2) as u8;
    for x in 1..=(w - 1) / 2 {
        let avg = n(x - 1) + n(x) + f(x - 1) + f(x) + 8;
        let d12 = (avg + 2 * (n(x) + f(x - 1))) >> 3;
        let d03 = (avg + 2 * (n(x - 1) + f(x))) >> 3;
        out[2 * x - 1] = ((d12 + n(x - 1)) >> 1) as u8;
        out[2 * x] = ((d03 + n(x)) >> 1) as u8;
    }
    if w.is_multiple_of(2) {
        let last = w / 2 - 1;
        out[w - 1] = ((3 * n(last) + f(last) + 2) >> 2) as u8;
    }
}

/// Interleaved RGB of a decoded frame, with libwebp's "fancy" chroma upsampling.
pub(super) fn frame_to_rgb(f: &Frame) -> Vec<u8> {
    let (w, h) = (f.width, f.height);
    let (cw, ch) = (f.chroma_width(), f.chroma_height());
    let mut out = vec![0u8; w * h * 3];
    let mut ul = vec![0u8; w];
    let mut vl = vec![0u8; w];
    for row in 0..h {
        let near = row / 2;
        let far = if row == 0 {
            0
        } else if row % 2 == 1 {
            (near + 1).min(ch - 1)
        } else {
            near - 1
        };
        upsample_line(
            &f.u[near * cw..(near + 1) * cw],
            &f.u[far * cw..(far + 1) * cw],
            &mut ul,
        );
        upsample_line(
            &f.v[near * cw..(near + 1) * cw],
            &f.v[far * cw..(far + 1) * cw],
            &mut vl,
        );
        let yrow = &f.y[row * w..(row + 1) * w];
        for x in 0..w {
            let px = yuv_to_rgb(yrow[x], ul[x], vl[x]);
            out[(row * w + x) * 3..(row * w + x) * 3 + 3].copy_from_slice(&px);
        }
    }
    out
}

/// Decodes the `ALPH` chunk into an 8-bit alpha plane of `w` x `h`.
pub(super) fn decode_alpha(
    chunk: &[u8],
    w: usize,
    h: usize,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Vec<u8>> {
    let (&head, data) = chunk
        .split_first()
        .ok_or_else(|| Error::truncated("WebP alpha chunk"))?;
    let method = head & 3;
    let filter = (head >> 2) & 3;
    let mut plane = match method {
        0 => data
            .get(..w * h)
            .ok_or_else(|| Error::truncated("WebP alpha plane"))?
            .to_vec(),
        1 => vp8l::decode_headerless(data, w as u32, h as u32, limits, progress)?
            .into_iter()
            .map(|p| (p >> 8) as u8)
            .collect(),
        _ => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "unknown WebP alpha compression",
            ))
        }
    };
    if plane.len() != w * h {
        return Err(Error::corrupt("WebP alpha plane has the wrong size"));
    }
    unfilter(&mut plane, w, filter);
    Ok(plane)
}

/// Undoes the prediction filter of the alpha plane in place.
fn unfilter(plane: &mut [u8], w: usize, filter: u8) {
    if filter == 0 || w == 0 {
        return;
    }
    let mut prev_row: Option<Vec<u8>> = None;
    for row in plane.chunks_exact_mut(w) {
        match (&prev_row, filter) {
            (Some(prev), 2) => {
                for (o, p) in row.iter_mut().zip(prev) {
                    *o = o.wrapping_add(*p);
                }
            }
            (Some(prev), 3) => {
                let mut left = prev[0];
                let mut top_left = prev[0];
                for (o, &top) in row.iter_mut().zip(prev) {
                    let pred = (i32::from(left) + i32::from(top) - i32::from(top_left))
                        .clamp(0, 255) as u8;
                    left = o.wrapping_add(pred);
                    top_left = top;
                    *o = left;
                }
            }
            _ => {
                // Horizontal prediction; the first pixel of a row follows the one above it.
                let mut pred = prev_row.as_ref().map_or(0, |p| p[0]);
                for o in row.iter_mut() {
                    pred = o.wrapping_add(pred);
                    *o = pred;
                }
            }
        }
        prev_row = Some(row.to_vec());
    }
}

// Encoding side -------------------------------------------------------------------------

const YUV_HALF: i32 = 1 << 15;

fn clip_uv(v: i32) -> u8 {
    let v = (v + (YUV_HALF << 2) + (128 << 18)) >> 18;
    v.clamp(0, 255) as u8
}

/// Converts interleaved 8-bit RGB (`channels` 3) or RGBA (4) to 4:2:0 planes with libwebp's
/// coefficients; chroma averages the four pixels it covers, repeating the last row and
/// column on odd sizes.
pub(super) fn rgb_to_yuv(data: &[u8], w: usize, h: usize, channels: usize) -> Frame {
    let px = |x: usize, y: usize| -> [i32; 3] {
        let i = (y * w + x) * channels;
        [
            i32::from(data[i]),
            i32::from(data[i + 1]),
            i32::from(data[i + 2]),
        ]
    };
    let mut y_plane = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = px(x, y);
            y_plane.push(
                ((16839 * r + 33059 * g + 6420 * b + YUV_HALF + (16 << 16)) >> 16).clamp(0, 255)
                    as u8,
            );
        }
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut u_plane = Vec::with_capacity(cw * ch);
    let mut v_plane = Vec::with_capacity(cw * ch);
    for cy in 0..ch {
        for cx in 0..cw {
            let mut sum = [0i32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = px((2 * cx + dx).min(w - 1), (2 * cy + dy).min(h - 1));
                for k in 0..3 {
                    sum[k] += p[k];
                }
            }
            let [r, g, b] = sum;
            u_plane.push(clip_uv(-9719 * r - 19081 * g + 28800 * b));
            v_plane.push(clip_uv(28800 * r - 24116 * g - 4684 * b));
        }
    }
    Frame {
        width: w,
        height: h,
        y: y_plane,
        u: u_plane,
        v: v_plane,
    }
}

/// Replaces the colour under fully transparent 8x8 blocks with the colour just before them,
/// so the hidden area is flat and costs next to nothing. Only touches RGBA data.
pub(super) fn flatten_transparent(data: &mut [u8], w: usize, h: usize) {
    for by in (0..h).step_by(8) {
        for bx in (0..w).step_by(8) {
            let (x1, y1) = ((bx + 8).min(w), (by + 8).min(h));
            let hidden = (by..y1).all(|y| (bx..x1).all(|x| data[(y * w + x) * 4 + 3] == 0));
            if !hidden {
                continue;
            }
            let fill: [u8; 3] = if bx > 0 {
                let i = (by * w + bx - 1) * 4;
                [data[i], data[i + 1], data[i + 2]]
            } else if by > 0 {
                let i = ((by - 1) * w + bx) * 4;
                [data[i], data[i + 1], data[i + 2]]
            } else {
                [128, 128, 128]
            };
            for y in by..y1 {
                for x in bx..x1 {
                    let i = (y * w + x) * 4;
                    data[i..i + 3].copy_from_slice(&fill);
                }
            }
        }
    }
}

/// Predicts each alpha value from its neighbours (`filter`: 1 horizontal, 2 vertical,
/// 3 gradient), the inverse of `unfilter`.
fn filter_plane(plane: &[u8], w: usize, filter: u8) -> Vec<u8> {
    let mut out = plane.to_vec();
    for (y, row) in plane.chunks_exact(w).enumerate() {
        let prev = (y > 0).then(|| &plane[(y - 1) * w..y * w]);
        for x in 0..w {
            let pred = match (prev, filter) {
                (Some(p), 2) => p[x],
                (Some(p), 3) if x > 0 => (i32::from(row[x - 1]) + i32::from(p[x])
                    - i32::from(p[x - 1]))
                .clamp(0, 255) as u8,
                (Some(p), _) if x == 0 => p[0],
                (None, _) if x == 0 => 0,
                _ => row[x - 1],
            };
            out[y * w + x] = row[x].wrapping_sub(pred);
        }
    }
    out
}

/// The body of an `ALPH` chunk for `plane`: losslessly coded, with the prediction filter that
/// gives the smallest result, or stored raw when that is smaller.
pub(super) fn encode_alpha(plane: &[u8], w: usize, h: usize, effort: u8) -> Vec<u8> {
    let kinds: &[u8] = if effort >= 2 { &[0, 1, 2, 3] } else { &[0] };
    let mut best: Vec<u8> = Vec::with_capacity(plane.len() + 1);
    best.push(0);
    best.extend_from_slice(plane);
    for &kind in kinds {
        let filtered = if kind == 0 {
            plane.to_vec()
        } else {
            filter_plane(plane, w, kind)
        };
        let argb: Vec<u32> = filtered
            .iter()
            .map(|&a| 0xff00_0000 | (u32::from(a) << 8))
            .collect();
        let stream = super::vp8l_enc::encode(&argb, w, h, false, effort.min(6));
        // The five-byte image header is implied for alpha.
        if let Some(body) = stream.get(5..) {
            if body.len() + 1 < best.len() {
                best.clear();
                best.push(1 | (kind << 2));
                best.extend_from_slice(body);
            }
        }
    }
    best
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::chunks_exact_to_as_chunks)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn plane(w: usize, h: usize, kind: u8) -> Vec<u8> {
        let mut s = 99u32;
        (0..w * h)
            .map(|i| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let (x, y) = (i % w, i / w);
                match kind {
                    0 => ((x * 255) / w) as u8,
                    1 => ((x + y) * 3 % 256) as u8,
                    2 => (s >> 24) as u8,
                    _ => {
                        if (x / 5 + y / 3) % 2 == 0 {
                            255
                        } else {
                            0
                        }
                    }
                }
            })
            .collect()
    }

    #[test]
    fn alpha_planes_roundtrip_through_every_filter() {
        for (w, h) in [(1, 1), (7, 5), (40, 33)] {
            for kind in 0..4 {
                let p = plane(w, h, kind);
                for filter in 0..4u8 {
                    let filtered = if filter == 0 {
                        p.clone()
                    } else {
                        filter_plane(&p, w, filter)
                    };
                    let mut back = filtered.clone();
                    unfilter(&mut back, w, filter);
                    assert_eq!(back, p, "{w}x{h} kind {kind} filter {filter}");
                }
                for effort in [0, 4] {
                    let chunk = encode_alpha(&p, w, h, effort);
                    let got = decode_alpha(&chunk, w, h, &Limits::default(), &NoProgress).unwrap();
                    assert_eq!(got, p, "{w}x{h} kind {kind} effort {effort}");
                }
            }
        }
    }

    #[test]
    fn colour_survives_the_yuv_round_trip() {
        let (w, h) = (17, 9);
        let mut rgb = Vec::new();
        for (r, g, b) in [
            (200u8, 30u8, 40u8),
            (20, 180, 60),
            (240, 240, 240),
            (5, 5, 5),
        ] {
            rgb.clear();
            for _ in 0..w * h {
                rgb.extend_from_slice(&[r, g, b]);
            }
            let back = frame_to_rgb(&rgb_to_yuv(&rgb, w, h, 3));
            for (a, b) in back.iter().zip(&rgb) {
                assert!(a.abs_diff(*b) <= 3, "{a} vs {b}");
            }
        }
    }

    #[test]
    fn hidden_colour_is_flattened_but_visible_pixels_stay() {
        let (w, h) = (16, 16);
        let mut px = vec![0u8; w * h * 4];
        for (i, c) in px.chunks_exact_mut(4).enumerate() {
            let x = i % w;
            c.copy_from_slice(&[x as u8 * 9, 40, 90, if x < 8 { 255 } else { 0 }]);
        }
        let before = px.clone();
        flatten_transparent(&mut px, w, h);
        for (i, (a, b)) in px.chunks_exact(4).zip(before.chunks_exact(4)).enumerate() {
            if i % w < 8 {
                assert_eq!(a, b);
            } else {
                assert_eq!(a[3], 0);
                assert_eq!(a[..3], px[(i / w * w + 7) * 4..(i / w * w + 7) * 4 + 3]);
            }
        }
    }
}
