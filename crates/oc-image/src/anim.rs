//! Animated images: a sequence of full-canvas frames.
//!
//! Decoders resolve each format's disposal and blending rules and hand out the picture as it
//! is shown, so every frame is a complete 8-bit RGBA image of the canvas size. Encoders get
//! the same thing and work out for themselves which part of the canvas changed.
// Pixel buffers here are all `width * height * 4` bytes, which every caller checks.
#![allow(clippy::indexing_slicing)]
use crate::image::{Depth, Image, Layout, Metadata};
use oc_core::{Limits, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The whole canvas as displayed during this frame, 8-bit RGBA.
    pub image: Image,
    /// How long the frame is shown.
    pub delay_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Frame>,
    /// Times the animation plays in total; 0 repeats forever.
    pub loops: u32,
    pub metadata: Metadata,
}

impl Animation {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            frames: Vec::new(),
            loops: 0,
            metadata: Metadata::default(),
        }
    }

    /// The first frame as a still image, carrying the animation's metadata.
    pub fn first_image(&self) -> Option<Image> {
        let mut img = self.frames.first()?.image.clone();
        img.metadata = self.metadata.clone();
        Some(img)
    }
}

/// A rectangle as `(x, y, width, height)` in pixels.
pub type Rect = (usize, usize, usize, usize);

/// Drops the alpha channel of tightly packed RGBA pixels.
pub fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, _]| [r, g, b])
        .collect()
}

/// Checks that one more frame of the canvas fits the limits.
pub fn check_frame(limits: &Limits, w: u32, h: u32, frames_so_far: usize) -> Result<()> {
    let px = limits.check_pixels(w, h)?;
    limits.check_entries(frames_so_far as u64 + 1)?;
    limits.check_alloc(
        px.saturating_mul(4)
            .saturating_mul(frames_so_far as u64 + 1),
    )
}

/// A transparent RGBA canvas.
pub fn blank_canvas(w: u32, h: u32) -> Vec<u8> {
    vec![0; w as usize * h as usize * 4]
}

pub fn canvas_image(w: u32, h: u32, rgba: Vec<u8>) -> Result<Image> {
    Image::new(w, h, Layout::Rgba, Depth::Eight, rgba)
}

/// Straight-alpha "source over" of `src` onto `dst`, the blend that WebP and APNG define.
pub fn blend_over(dst: &mut [u8], src: &[u8]) {
    let sa = u32::from(src[3]);
    if sa == 255 {
        dst[..4].copy_from_slice(&src[..4]);
        return;
    }
    if sa == 0 {
        return;
    }
    let da = u32::from(dst[3]);
    // Resulting alpha in 1/255 and the weight of the destination colour.
    let out_a = sa * 255 + da * (255 - sa);
    if out_a == 0 {
        dst[..4].fill(0);
        return;
    }
    for k in 0..3 {
        let c = u32::from(src[k]) * sa * 255 + u32::from(dst[k]) * da * (255 - sa);
        dst[k] = ((c + out_a / 2) / out_a) as u8;
    }
    dst[3] = ((out_a + 127) / 255) as u8;
}

/// The smallest rectangle `(x, y, w, h)` holding every pixel where `a` and `b` (RGBA, equal
/// size) differ, or `None` when they are identical.
pub fn changed_rect(a: &[u8], b: &[u8], w: usize, h: usize) -> Option<Rect> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        let ra = &a[y * w * 4..(y + 1) * w * 4];
        let rb = &b[y * w * 4..(y + 1) * w * 4];
        if ra == rb {
            continue;
        }
        let (pa, pb) = (ra.as_chunks::<4>().0, rb.as_chunks::<4>().0);
        let first = pa.iter().zip(pb).position(|(p, q)| p != q);
        let last = pa.iter().zip(pb).rposition(|(p, q)| p != q);
        if let (Some(f), Some(l)) = (first, last) {
            x0 = x0.min(f);
            x1 = x1.max(l + 1);
            y0 = y0.min(y);
            y1 = y;
        }
    }
    (x0 != usize::MAX).then(|| (x0, y0, x1 - x0, y1 + 1 - y0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispose {
    /// Leave the frame in place.
    Keep,
    /// Clear its rectangle to transparent.
    Background,
    /// Put back what the rectangle held before the frame.
    Previous,
}

/// The canvas of formats that draw each frame into a rectangle (APNG, WebP).
pub struct Compositor {
    w: usize,
    h: usize,
    canvas: Vec<u8>,
    after: Option<(Dispose, Rect, Vec<u8>)>,
}

impl Compositor {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w: w as usize,
            h: h as usize,
            canvas: blank_canvas(w, h),
            after: None,
        }
    }

    /// Disposes of the previous frame, draws `src` (RGBA, `rect` pixels) and returns the canvas
    /// as it is shown. A rectangle that does not fit the canvas is clipped.
    pub fn draw(&mut self, rect: Rect, src: &[u8], blend: bool, dispose: Dispose) -> &[u8] {
        let cw = self.w;
        if let Some((how, (x, y, rw, rh), saved)) = self.after.take() {
            for row in y..(y + rh).min(self.h) {
                let range = (row * cw + x) * 4..(row * cw + (x + rw).min(cw)) * 4;
                match how {
                    Dispose::Background => {
                        if let Some(d) = self.canvas.get_mut(range) {
                            d.fill(0);
                        }
                    }
                    Dispose::Previous => {
                        if let (Some(d), Some(s)) =
                            (self.canvas.get_mut(range.clone()), saved.get(range))
                        {
                            d.copy_from_slice(s);
                        }
                    }
                    Dispose::Keep => {}
                }
            }
        }
        let (fx, fy, fw, fh) = rect;
        let saved = if dispose == Dispose::Previous {
            self.canvas.clone()
        } else {
            Vec::new()
        };
        for row in 0..fh {
            if fy + row >= self.h {
                break;
            }
            let n = fw.min(cw.saturating_sub(fx));
            let at = ((fy + row) * cw + fx) * 4;
            let (Some(dst), Some(s)) = (
                self.canvas.get_mut(at..at + n * 4),
                src.get(row * fw * 4..row * fw * 4 + n * 4),
            ) else {
                continue;
            };
            if blend {
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(s.as_chunks::<4>().0)
                {
                    blend_over(d, s);
                }
            } else {
                dst.copy_from_slice(s);
            }
        }
        self.after = Some((dispose, rect, saved));
        &self.canvas
    }
}

/// Copies the rectangle out of an RGBA canvas.
pub fn crop_rgba(canvas: &[u8], w: usize, rect: Rect) -> Vec<u8> {
    let (x, y, rw, rh) = rect;
    let mut out = Vec::with_capacity(rw * rh * 4);
    for row in y..y + rh {
        out.extend_from_slice(&canvas[(row * w + x) * 4..(row * w + x + rw) * 4]);
    }
    out
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn blending_follows_source_over() {
        let mut d = [10, 20, 30, 255];
        blend_over(&mut d, &[200, 100, 0, 255]);
        assert_eq!(d, [200, 100, 0, 255]);
        let mut d = [10, 20, 30, 255];
        blend_over(&mut d, &[0, 0, 0, 0]);
        assert_eq!(d, [10, 20, 30, 255]);
        let mut d = [0, 0, 0, 255];
        blend_over(&mut d, &[255, 255, 255, 128]);
        assert_eq!(d[3], 255);
        assert!((127..=129).contains(&d[0]));
        let mut d = [0, 0, 0, 0];
        blend_over(&mut d, &[9, 8, 7, 100]);
        assert_eq!(d, [9, 8, 7, 100]);
    }

    #[test]
    fn changed_rect_bounds_the_difference() {
        let (w, h) = (6, 5);
        let a = vec![0u8; w * h * 4];
        let mut b = a.clone();
        assert_eq!(changed_rect(&a, &b, w, h), None);
        b[(2 * w + 1) * 4] = 9;
        b[(3 * w + 4) * 4 + 3] = 9;
        assert_eq!(changed_rect(&a, &b, w, h), Some((1, 2, 4, 2)));
        assert_eq!(crop_rgba(&b, w, (1, 2, 4, 2)).len(), 4 * 2 * 4);
    }
}
