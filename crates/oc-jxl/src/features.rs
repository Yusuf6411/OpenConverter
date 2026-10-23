//! Frame features that are added after the image data is reconstructed: patches.
use crate::bits::{bad, unpack_signed, Br};
use crate::compose::{blend, Mode, Rect, View};
use crate::entropy::Decoder;
use crate::header::{EcType, FrameHeader, ImageHeader};
use crate::render::Planes;
use oc_core::Result;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PatchMode {
    None,
    Replace,
    Add,
    Mul,
    BlendAbove,
    BlendBelow,
    MulAddAbove,
    MulAddBelow,
}

#[derive(Debug)]
struct PatchBlend {
    mode: PatchMode,
    alpha_channel: usize,
    clamp: bool,
}

#[derive(Debug)]
struct Target {
    x: i32,
    y: i32,
    blending: Vec<PatchBlend>,
}

#[derive(Debug)]
struct PatchRef {
    ref_idx: usize,
    x0: usize,
    y0: usize,
    width: usize,
    height: usize,
    targets: Vec<Target>,
}

#[derive(Debug)]
pub struct Patches {
    refs: Vec<PatchRef>,
}

impl Patches {
    pub fn parse(br: &mut Br, img: &ImageHeader, fh: &FrameHeader) -> Result<Self> {
        let num_extra = img.ec.len();
        let alphas: Vec<usize> =
            img.ec.iter().enumerate().filter(|(_, e)| matches!(e.ty, EcType::Alpha { .. })).map(|(i, _)| i).collect();
        let mut dec = Decoder::parse(br, 10)?;
        dec.begin(br)?;
        let max_refs = (1u64 << 24).min(u64::from(fh.width) * u64::from(fh.height) / 16) as u32;
        let max_patches = u64::from(max_refs) * 4;
        let num_refs = dec.read(br, 0)?;
        if num_refs > max_refs {
            return Err(bad("too many patches"));
        }
        let mut total = 0u64;
        let mut refs = Vec::new();
        for _ in 0..num_refs {
            let ref_idx = dec.read(br, 1)? as usize;
            if ref_idx >= 4 {
                return Err(bad("patch reference slot out of range"));
            }
            let x0 = dec.read(br, 3)? as usize;
            let y0 = dec.read(br, 3)? as usize;
            let width = dec.read(br, 2)? as usize + 1;
            let height = dec.read(br, 2)? as usize + 1;
            let count = u64::from(dec.read(br, 7)?) + 1;
            total += count;
            if total > max_patches {
                return Err(bad("too many patches"));
            }
            let mut targets = Vec::new();
            let mut prev: Option<(i32, i32)> = None;
            for _ in 0..count {
                let (x, y) = match prev {
                    Some((px, py)) => {
                        let dx = unpack_signed(dec.read(br, 6)?);
                        let dy = unpack_signed(dec.read(br, 6)?);
                        (
                            dx.checked_add(px).ok_or_else(|| bad("patch position overflow"))?,
                            dy.checked_add(py).ok_or_else(|| bad("patch position overflow"))?,
                        )
                    }
                    None => (dec.read(br, 4)? as i32, dec.read(br, 4)? as i32),
                };
                prev = Some((x, y));
                let mut blending = Vec::with_capacity(num_extra + 1);
                for _ in 0..=num_extra {
                    let raw = dec.read(br, 5)?;
                    let mode = match raw {
                        0 => PatchMode::None,
                        1 => PatchMode::Replace,
                        2 => PatchMode::Add,
                        3 => PatchMode::Mul,
                        4 => PatchMode::BlendAbove,
                        5 => PatchMode::BlendBelow,
                        6 => PatchMode::MulAddAbove,
                        7 => PatchMode::MulAddBelow,
                        _ => return Err(bad("invalid patch blend mode")),
                    };
                    let alpha_channel =
                        if raw >= 4 && alphas.len() >= 2 { dec.read(br, 8)? as usize } else { alphas.first().copied().unwrap_or(0) };
                    let clamp = if raw >= 3 { dec.read(br, 9)? != 0 } else { false };
                    let uses_alpha = matches!(
                        mode,
                        PatchMode::BlendAbove | PatchMode::BlendBelow | PatchMode::MulAddAbove | PatchMode::MulAddBelow
                    );
                    if uses_alpha && alpha_channel >= num_extra {
                        return Err(bad("patch alpha channel out of range"));
                    }
                    blending.push(PatchBlend { mode, alpha_channel, clamp });
                }
                targets.push(Target { x, y, blending });
            }
            refs.push(PatchRef { ref_idx, x0, y0, width, height, targets });
        }
        dec.finalize()?;
        Ok(Self { refs })
    }

    /// Add the patches to `base` (a frame-sized set of planes) from the reference slots.
    pub fn apply(&self, base: &mut Planes, slots: &[Option<Rc<Planes>>; 4], img: &ImageHeader) -> Result<()> {
        let colour = base.colour;
        let (bw, bh) = (base.w as i64, base.h as i64);
        for p in &self.refs {
            let Some(src) = slots[p.ref_idx].as_ref() else {
                return Err(bad("patch refers to an empty reference slot"));
            };
            if src.ch.len() != base.ch.len() || src.colour != colour || src.xyb != base.xyb {
                return Err(bad("patch reference does not match the frame"));
            }
            for t in &p.targets {
                let (tx, ty) = (i64::from(t.x), i64::from(t.y));
                let bx0 = tx.max(0);
                let by0 = ty.max(0);
                let bx1 = (tx + p.width as i64).min(bw);
                let by1 = (ty + p.height as i64).min(bh);
                if bx1 <= bx0 || by1 <= by0 {
                    continue;
                }
                let nx0 = p.x0 as i64 + (bx0 - tx);
                let ny0 = p.y0 as i64 + (by0 - ty);
                let w = (bx1 - bx0).min(src.w as i64 - nx0);
                let h = (by1 - by0).min(src.h as i64 - ny0);
                if w <= 0 || h <= 0 {
                    continue;
                }
                let rect = Rect { bx: bx0 as usize, by: by0 as usize, nx: nx0 as usize, ny: ny0 as usize, w: w as usize, h: h as usize };
                for idx in 0..base.ch.len() {
                    let info = if idx < colour { &t.blending[0] } else { &t.blending[1 + idx - colour] };
                    let alpha_idx = info.alpha_channel + colour;
                    let is_alpha = idx == alpha_idx;
                    let premultiplied = info.alpha_channel < img.ec.len() && matches!(img.ec[info.alpha_channel].ty, EcType::Alpha { associated: true });
                    let mode = match info.mode {
                        PatchMode::None => continue,
                        PatchMode::Replace => Mode::Replace,
                        PatchMode::Add => Mode::Add,
                        PatchMode::Mul => Mode::Mul { clamp: info.clamp },
                        PatchMode::BlendAbove | PatchMode::BlendBelow => {
                            let swapped = info.mode == PatchMode::BlendBelow;
                            if is_alpha {
                                Mode::MixAlpha { clamp: info.clamp, swapped }
                            } else {
                                Mode::Blend { clamp: info.clamp, swapped, premultiplied }
                            }
                        }
                        PatchMode::MulAddAbove | PatchMode::MulAddBelow => {
                            let swapped = info.mode == PatchMode::MulAddBelow;
                            if is_alpha {
                                if swapped {
                                    Mode::Replace
                                } else {
                                    Mode::Skip
                                }
                            } else {
                                Mode::MulAdd { clamp: info.clamp, swapped }
                            }
                        }
                    };
                    let new = View { data: &src.ch[idx], stride: src.w };
                    let uses_alpha = matches!(mode, Mode::Blend { .. } | Mode::MulAdd { .. });
                    if uses_alpha && alpha_idx < base.ch.len() && !is_alpha {
                        let new_alpha = View { data: &src.ch[alpha_idx], stride: src.w };
                        let (target, alpha) = split_pair(&mut base.ch, idx, alpha_idx);
                        let base_alpha = View { data: alpha, stride: base.w };
                        blend(target, base.w, &new, rect, mode, Some(&base_alpha), Some(&new_alpha));
                    } else {
                        blend(&mut base.ch[idx], base.w, &new, rect, mode, None, None);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Mutable access to `v[i]` together with shared access to `v[j]`, `i != j`.
fn split_pair(v: &mut [Vec<f32>], i: usize, j: usize) -> (&mut Vec<f32>, &Vec<f32>) {
    if i < j {
        let (a, b) = v.split_at_mut(j);
        (&mut a[i], &b[0])
    } else {
        let (a, b) = v.split_at_mut(i);
        (&mut b[0], &a[j])
    }
}
