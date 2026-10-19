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
