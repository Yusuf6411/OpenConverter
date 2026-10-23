//! Blending of one sample plane onto another, shared by patches and frame composition.
//! The formulas follow ISO/IEC 18181-1 section K.

/// How a plane is combined with the one below it.
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Replace,
    Add,
    Mul {
        clamp: bool,
    },
    /// Alpha blending of a colour or extra channel; `swapped` puts the new samples below.
    Blend {
        clamp: bool,
        swapped: bool,
        premultiplied: bool,
    },
    MulAdd {
        clamp: bool,
        swapped: bool,
    },
    /// The alpha channel itself under alpha blending.
    MixAlpha {
        clamp: bool,
        swapped: bool,
    },
    Skip,
}

/// A plane of samples with its row stride.
pub struct View<'a> {
    pub data: &'a [f32],
    pub stride: usize,
}

/// The rectangle to combine: `w` x `h` samples starting at `(bx, by)` in the base and
/// `(nx, ny)` in the new plane.
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub bx: usize,
    pub by: usize,
    pub nx: usize,
    pub ny: usize,
    pub w: usize,
    pub h: usize,
}

/// Combine `new` into `base`. `base_alpha` and `new_alpha` are the alpha planes the modes that
/// use alpha need, read at the same coordinates as their colour planes.
pub fn blend(
    base: &mut [f32],
    base_stride: usize,
    new: &View,
    r: Rect,
    mode: Mode,
    base_alpha: Option<&View>,
    new_alpha: Option<&View>,
) {
    for dy in 0..r.h {
        let brow = (r.by + dy) * base_stride + r.bx;
        let nrow = (r.ny + dy) * new.stride + r.nx;
        for dx in 0..r.w {
            let b = &mut base[brow + dx];
            let n = new.data[nrow + dx];
            let base_a = || base_alpha.map_or(0.0, |a| a.data[(r.by + dy) * a.stride + r.bx + dx]);
            let new_a = || new_alpha.map(|a| a.data[(r.ny + dy) * a.stride + r.nx + dx]);
            match mode {
                Mode::Replace => *b = n,
                Mode::Add => *b += n,
                Mode::Mul { clamp } => *b *= if clamp { n.clamp(0.0, 1.0) } else { n },
                Mode::Blend {
                    clamp,
                    swapped,
                    premultiplied,
                } => {
                    let Some(na) = new_a() else {
                        *b = n;
                        continue;
                    };
                    let (bs, ns, ba, mut nalpha) = if swapped {
                        (n, *b, na, base_a())
                    } else {
                        (*b, n, base_a(), na)
                    };
                    if clamp {
                        nalpha = nalpha.clamp(0.0, 1.0);
                    }
                    *b = if premultiplied {
                        ns + bs * (1.0 - nalpha)
                    } else {
                        let mixed = 1.0 - (1.0 - nalpha) * (1.0 - ba);
                        let recip = if mixed > 0.0 { mixed.recip() } else { 0.0 };
                        (nalpha * ns + ba * bs * (1.0 - nalpha)) * recip
                    };
                }
                Mode::MulAdd { clamp, swapped } => {
                    let Some(na) = new_a() else {
                        *b += n;
                        continue;
                    };
                    let (bs, ns, mut nalpha) = if swapped {
                        (n, *b, base_a())
                    } else {
                        (*b, n, na)
                    };
                    if clamp {
                        nalpha = nalpha.clamp(0.0, 1.0);
                    }
                    *b = bs + nalpha * ns;
                }
                Mode::MixAlpha { clamp, swapped } => {
                    let (bs, mut ns) = if swapped { (n, *b) } else { (*b, n) };
                    if clamp {
                        ns = ns.clamp(0.0, 1.0);
                    }
                    *b = bs + ns * (1.0 - bs);
                }
                Mode::Skip => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_and_add_cover_the_rectangle() {
        let mut base = vec![1.0f32; 16];
        let new = vec![2.0f32; 4];
        let v = View {
            data: &new,
            stride: 2,
        };
        let r = Rect {
            bx: 1,
            by: 1,
            nx: 0,
            ny: 0,
            w: 2,
            h: 2,
        };
        blend(&mut base, 4, &v, r, Mode::Replace, None, None);
        assert_eq!(base.iter().filter(|&&x| x == 2.0).count(), 4);
        blend(&mut base, 4, &v, r, Mode::Add, None, None);
        assert_eq!(base[5], 4.0);
        assert_eq!(base[0], 1.0);
    }

    #[test]
    fn opaque_new_layer_replaces_the_base() {
        let mut base = vec![0.25f32; 4];
        let ba = vec![1.0f32; 4];
        let new = vec![0.75f32; 4];
        let na = vec![1.0f32; 4];
        let r = Rect {
            bx: 0,
            by: 0,
            nx: 0,
            ny: 0,
            w: 2,
            h: 2,
        };
        blend(
            &mut base,
            2,
            &View {
                data: &new,
                stride: 2,
            },
            r,
            Mode::Blend {
                clamp: false,
                swapped: false,
                premultiplied: false,
            },
            Some(&View {
                data: &ba,
                stride: 2,
            }),
            Some(&View {
                data: &na,
                stride: 2,
            }),
        );
        assert!(base.iter().all(|&v| (v - 0.75).abs() < 1e-6));
    }
}

// ---------------------------------------------------------------------------------------------
// Frame composition

use crate::bits::bad;
use crate::header::{Blend, BlendInfo, EcType, FrameHeader, ImageHeader};
use crate::render::Planes;
use oc_core::Result;
use std::rc::Rc;

/// Place a frame's planes on a canvas of the image's size, with zeros elsewhere.
pub fn embed(p: &Planes, fh: &FrameHeader, img: &ImageHeader) -> Planes {
    let (cw, ch) = (img.width as usize, img.height as usize);
    if fh.x0 == 0 && fh.y0 == 0 && p.w == cw && p.h == ch {
        return p.clone();
    }
    let mut out = Planes {
        w: cw,
        h: ch,
        ch: vec![vec![0f32; cw * ch]; p.ch.len()],
        colour: p.colour,
        xyb: p.xyb,
    };
    if let Some((rect, _)) = clip(fh, p, cw, ch) {
        for (dst, src) in out.ch.iter_mut().zip(&p.ch) {
            let v = View {
                data: src,
                stride: p.w,
            };
            blend(dst, cw, &v, rect, Mode::Replace, None, None);
        }
    }
    out
}

/// The part of the frame that lies on the canvas, as a rectangle in canvas and frame coordinates.
fn clip(fh: &FrameHeader, p: &Planes, cw: usize, ch: usize) -> Option<(Rect, ())> {
    let (x0, y0) = (i64::from(fh.x0), i64::from(fh.y0));
    let left = x0.max(0);
    let top = y0.max(0);
    let right = (x0 + p.w as i64).min(cw as i64);
    let bottom = (y0 + p.h as i64).min(ch as i64);
    if right <= left || bottom <= top {
        return None;
    }
    Some((
        Rect {
            bx: left as usize,
            by: top as usize,
            nx: (left - x0) as usize,
            ny: (top - y0) as usize,
            w: (right - left) as usize,
            h: (bottom - top) as usize,
        },
        (),
    ))
}

fn frame_mode(
    info: &BlendInfo,
    idx: usize,
    colour: usize,
    premultiplied: bool,
    has_extra: bool,
) -> Mode {
    let is_alpha = has_extra && idx == info.alpha_channel as usize + colour;
    match info.mode {
        Blend::Replace => Mode::Replace,
        Blend::Add => Mode::Add,
        Blend::Mul => Mode::Mul { clamp: info.clamp },
        Blend::Blend => {
            if is_alpha {
                Mode::MixAlpha {
                    clamp: info.clamp,
                    swapped: false,
                }
            } else {
                Mode::Blend {
                    clamp: info.clamp,
                    swapped: false,
                    premultiplied,
                }
            }
        }
        Blend::MulAdd => {
            if is_alpha {
                Mode::Skip
            } else {
                Mode::MulAdd {
                    clamp: info.clamp,
                    swapped: false,
                }
            }
        }
    }
}

/// Blend a decoded frame (already in the file's colour space) onto the reference slots,
/// producing the canvas after this frame.
pub fn compose(
    p: Planes,
    fh: &FrameHeader,
    img: &ImageHeader,
    slots: &[Option<Rc<Planes>>; 4],
) -> Result<Planes> {
    let (cw, chh) = (img.width as usize, img.height as usize);
    let colour = p.colour;
    let n = p.ch.len();
    let has_extra = !img.ec.is_empty();
    let infos: Vec<&BlendInfo> = (0..n)
        .map(|i| {
            if i < colour {
                &fh.blending
            } else {
                &fh.ec_blending[i - colour]
            }
        })
        .collect();
    // Replacing the whole canvas needs no base at all.
    let covers = fh.x0 <= 0
        && fh.y0 <= 0
        && i64::from(fh.x0) + p.w as i64 >= cw as i64
        && i64::from(fh.y0) + p.h as i64 >= chh as i64;
    if infos.iter().all(|i| i.mode == Blend::Replace)
        && covers
        && fh.x0 == 0
        && fh.y0 == 0
        && p.w == cw
        && p.h == chh
    {
        return Ok(p);
    }
    let Some((rect, _)) = clip(fh, &p, cw, chh) else {
        return Err(bad("frame lies outside the canvas"));
    };
    let zeros = vec![0f32; cw * chh];
    let mut out = Planes {
        w: cw,
        h: chh,
        ch: Vec::with_capacity(n),
        colour,
        xyb: false,
    };
    for idx in 0..n {
        let info = infos[idx];
        let base = slots.get(info.source as usize).and_then(|s| s.as_ref());
        if let Some(b) = base {
            if b.ch.len() != n || b.w != cw || b.h != chh || b.xyb {
                return Err(bad(
                    "reference frame does not match the frame being blended",
                ));
            }
        }
        let mut plane = base.map_or_else(|| zeros.clone(), |b| b.ch[idx].clone());
        let alpha_ec = (matches!(info.mode, Blend::Blend | Blend::MulAdd) && has_extra)
            .then_some(info.alpha_channel as usize);
        let premultiplied = alpha_ec.is_some_and(|a| {
            matches!(
                img.ec.get(a).map(|e| e.ty),
                Some(EcType::Alpha { associated: true })
            )
        });
        let mode = frame_mode(info, idx, colour, premultiplied, has_extra);
        let new = View {
            data: &p.ch[idx],
            stride: p.w,
        };
        let uses_alpha = alpha_ec.filter(|&a| a < img.ec.len() && a + colour != idx);
        match uses_alpha {
            Some(a) => {
                let ai = a + colour;
                let base_alpha_plane: &[f32] = base.map_or(&zeros[..], |b| &b.ch[ai][..]);
                let base_alpha = View {
                    data: base_alpha_plane,
                    stride: cw,
                };
                let new_alpha = View {
                    data: &p.ch[ai],
                    stride: p.w,
                };
                blend(
                    &mut plane,
                    cw,
                    &new,
                    rect,
                    mode,
                    Some(&base_alpha),
                    Some(&new_alpha),
                );
            }
            None => blend(&mut plane, cw, &new, rect, mode, None, None),
        }
        out.ch.push(plane);
    }
    Ok(out)
}
