//! Lossless JPEG transforms: mirroring, transposing, rotating and cropping on the
//! quantised DCT coefficients, so no pixel is decoded and nothing is lost. A mirror
//! negates the odd-frequency coefficients, a transpose swaps their two indices, and both
//! move whole blocks. Edges that do not fill an MCU cannot move without damage, so a
//! mirror trims them (a loss, reported).
#![allow(clippy::indexing_slicing)] // block and coefficient indices are range-checked by construction

use super::decode::{read_coefficients, Coefs};
use super::encode::{progressive_script, segment, write_scan, Comp, Scan};
use super::tables::ZIGZAG;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Mirror left to right.
    FlipH,
    /// Mirror top to bottom.
    FlipV,
    /// Swap rows and columns.
    Transpose,
}

#[derive(Debug, Clone, Default)]
pub struct Transform {
    /// Applied in order after the crop.
    pub ops: Vec<Op>,
    /// Left, top, width, height in source pixels. The left and top edges move down to the
    /// MCU grid and the rectangle grows to keep its far edges; it is clipped to the picture.
    pub crop: Option<[usize; 4]>,
    /// Write progressive (true) or sequential (false) scans; the input's kind when unset.
    pub progressive: Option<bool>,
}

impl Transform {
    /// Clockwise rotation by 0, 90, 180 or 270 degrees after an optional mirror.
    pub fn rotate_flip(degrees: u32, flip_h: bool, flip_v: bool) -> Option<Self> {
        let mut ops = Vec::new();
        if flip_h {
            ops.push(Op::FlipH);
        }
        if flip_v {
            ops.push(Op::FlipV);
        }
        match degrees % 360 {
            0 => {}
            90 => ops.extend([Op::Transpose, Op::FlipH]),
            180 => ops.extend([Op::FlipH, Op::FlipV]),
            270 => ops.extend([Op::Transpose, Op::FlipV]),
            _ => return None,
        }
        Some(Self {
            ops,
            crop: None,
            progressive: None,
        })
    }
}

fn negate_odd(b: &mut [i16; 64], odd_rows: bool) {
    for v in 0..8 {
        for u in 0..8 {
            if (if odd_rows { v } else { u }) % 2 == 1 {
                b[v * 8 + u] = b[v * 8 + u].saturating_neg();
            }
        }
    }
}

fn trim(c: &mut Coefs, notes: &mut Notes, horizontal: bool) -> Result<()> {
    let (size, unit) = if horizontal {
        (c.width, 8 * c.hmax)
    } else {
        (c.height, 8 * c.vmax)
    };
    if size % unit == 0 {
        return Ok(());
    }
    let kept = size / unit * unit;
    if kept == 0 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "the picture is smaller than one block of pixels in a direction that must be mirrored",
        ));
    }
    notes.push(
        Note::new(
            Severity::Loss,
            "jpeg.edge_trimmed",
            "a mirror moves partial edge blocks to the other side, so the picture was trimmed to whole blocks",
        )
        .with_detail(format!(
            "{} pixels removed from the {}",
            size - kept,
            if horizontal { "right" } else { "bottom" }
        )),
    );
    let units = kept / unit;
    if horizontal {
        c.width = kept;
        c.mcus_x = units;
    } else {
        c.height = kept;
        c.mcus_y = units;
    }
    for comp in &mut c.comps {
        let (nbw, nbh) = (c.mcus_x * comp.h, c.mcus_y * comp.v);
        let mut blocks = Vec::with_capacity(nbw * nbh);
        for by in 0..nbh {
            blocks.extend_from_slice(&comp.blocks[by * comp.bw..by * comp.bw + nbw]);
        }
        comp.blocks = blocks;
        comp.bw = nbw;
        comp.bh = nbh;
    }
    Ok(())
}

fn flip(c: &mut Coefs, notes: &mut Notes, horizontal: bool) -> Result<()> {
    trim(c, notes, horizontal)?;
    for comp in &mut c.comps {
        let (bw, bh) = (comp.bw, comp.bh);
        let mut out = vec![[0i16; 64]; bw * bh];
        for by in 0..bh {
            for bx in 0..bw {
                let (nx, ny) = if horizontal {
                    (bw - 1 - bx, by)
                } else {
                    (bx, bh - 1 - by)
                };
                let mut b = comp.blocks[by * bw + bx];
                negate_odd(&mut b, !horizontal);
                out[ny * bw + nx] = b;
            }
        }
        comp.blocks = out;
    }
    Ok(())
}

fn transpose(c: &mut Coefs) {
    for comp in &mut c.comps {
        let (bw, bh) = (comp.bw, comp.bh);
        let mut out = vec![[0i16; 64]; bw * bh];
        for by in 0..bh {
            for bx in 0..bw {
                let src = comp.blocks[by * bw + bx];
                let dst = &mut out[bx * bh + by];
                for v in 0..8 {
                    for u in 0..8 {
                        dst[u * 8 + v] = src[v * 8 + u];
                    }
                }
            }
        }
        comp.blocks = out;
        std::mem::swap(&mut comp.bw, &mut comp.bh);
        std::mem::swap(&mut comp.h, &mut comp.v);
    }
    std::mem::swap(&mut c.width, &mut c.height);
    std::mem::swap(&mut c.hmax, &mut c.vmax);
    std::mem::swap(&mut c.mcus_x, &mut c.mcus_y);
    for q in c.q.iter_mut().flatten() {
        let src = *q;
        for v in 0..8 {
            for u in 0..8 {
                q[u * 8 + v] = src[v * 8 + u];
            }
        }
    }
}

fn crop(c: &mut Coefs, rect: [usize; 4], notes: &mut Notes) -> Result<()> {
    let (mw, mh) = (8 * c.hmax, 8 * c.vmax);
    let [x, y, w, h] = rect;
    let (x0, y0) = (x / mw * mw, y / mh * mh);
    if x0 >= c.width || y0 >= c.height || w == 0 || h == 0 {
        return Err(Error::new(
            ErrorCode::InvalidOptions,
            "the crop rectangle is empty or outside the picture",
        ));
    }
    if (x0, y0) != (x, y) {
        notes.push(
            Note::new(
                Severity::Info,
                "jpeg.crop_aligned",
                "a lossless crop starts on a block boundary, so the rectangle grew up and to the left",
            )
            .with_detail(format!("starts at {x0},{y0}")),
        );
    }
    // The rectangle grows to keep its right and bottom edges, as jpegtran does.
    let (nw, nh) = (
        (x + w - x0).min(c.width - x0),
        (y + h - y0).min(c.height - y0),
    );
    let (nmx, nmy) = (nw.div_ceil(mw), nh.div_ceil(mh));
    for comp in &mut c.comps {
        let (nbw, nbh) = (nmx * comp.h, nmy * comp.v);
        let (sx, sy) = (x0 / mw * comp.h, y0 / mh * comp.v);
        let mut blocks = Vec::with_capacity(nbw * nbh);
        for by in 0..nbh {
            let at = (sy + by) * comp.bw + sx;
            blocks.extend_from_slice(&comp.blocks[at..at + nbw]);
        }
        comp.blocks = blocks;
        comp.bw = nbw;
        comp.bh = nbh;
    }
    c.width = nw;
    c.height = nh;
    c.mcus_x = nmx;
    c.mcus_y = nmy;
    Ok(())
}

/// Applies `t` to a Huffman-coded 8-bit DCT JPEG and writes the result with optimised
/// tables, progressive when the input was or `t` asks (and it has one or three components). Other
/// markers (JFIF, Exif, ICC, Adobe, comments) are kept as they were.
pub fn apply(
    data: &[u8],
    t: &Transform,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let mut c = read_coefficients(data, limits, progress)?;
    if c.truncated {
        notes.push(Note::new(
            Severity::Warning,
            "jpeg.truncated",
            "the image data ended early; the rest of the picture is gray",
        ));
    }
    if let Some(rect) = t.crop {
        crop(&mut c, rect, &mut notes)?;
    }
    for op in &t.ops {
        progress.check_cancelled()?;
        match op {
            Op::FlipH => flip(&mut c, &mut notes, true)?,
            Op::FlipV => flip(&mut c, &mut notes, false)?,
            Op::Transpose => transpose(&mut c),
        }
    }
    Ok((write(&c, t.progressive.unwrap_or(c.progressive)), notes))
}

fn write(c: &Coefs, progressive: bool) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    for (marker, body) in &c.kept {
        segment(&mut out, *marker, body);
    }
    let mut used: Vec<usize> = c.comps.iter().map(|k| k.tq).collect();
    used.sort_unstable();
    used.dedup();
    for id in used {
        let Some(q) = c.q.get(id).and_then(|q| q.as_ref()) else {
            continue;
        };
        let wide = q.iter().any(|&v| v > 255);
        let mut body = vec![(u8::from(wide) << 4) | id as u8];
        for &z in &ZIGZAG {
            if wide {
                body.extend_from_slice(&q[z].to_be_bytes());
            } else {
                body.push(q[z] as u8);
            }
        }
        segment(&mut out, 0xDB, &body);
    }
    let progressive = progressive && matches!(c.comps.len(), 1 | 3);
    let mut sof = vec![8];
    sof.extend_from_slice(&(c.height as u16).to_be_bytes());
    sof.extend_from_slice(&(c.width as u16).to_be_bytes());
    sof.push(c.comps.len() as u8);
    for k in &c.comps {
        sof.extend_from_slice(&[k.id, ((k.h << 4) | k.v) as u8, k.tq as u8]);
    }
    segment(&mut out, if progressive { 0xC2 } else { 0xC0 }, &sof);
    let comps: Vec<Comp> = c
        .comps
        .iter()
        .enumerate()
        .map(|(i, k)| Comp {
            id: k.id,
            // Damaged files can hold values no 8-bit stream can code (DC beyond 11 bits,
            // AC beyond 10).
            blocks: k
                .blocks
                .iter()
                .map(|b| {
                    let mut b = *b;
                    b[0] = b[0].clamp(-1024, 1023);
                    for v in &mut b[1..] {
                        *v = (*v).clamp(-1023, 1023);
                    }
                    b
                })
                .collect(),
            stride: k.bw,
            hb: k.h,
            vb: k.v,
            real_w: (c.width * k.h).div_ceil(c.hmax).div_ceil(8),
            real_h: (c.height * k.v).div_ceil(c.vmax).div_ceil(8),
            table: usize::from(i > 0),
        })
        .collect();
    let mcus = (c.mcus_x, c.mcus_y);
    if progressive {
        for scan in progressive_script(c.comps.len() == 3) {
            write_scan(&mut out, &comps, &scan, mcus, true, None);
        }
    } else {
        let all = Scan {
            comps: (0..comps.len()).collect(),
            ss: 0,
            se: 63,
            ah: 0,
            al: 0,
        };
        write_scan(&mut out, &comps, &all, mcus, false, None);
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}
