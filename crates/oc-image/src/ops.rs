//! Image operations. Every lossy step adds a note to the returned [`Notes`].
#![allow(clippy::indexing_slicing)] // operates on buffers sized by `Image::new`

use crate::image::{Depth, Image, Layout, SampleWriter};
use oc_core::{Note, Notes, Severity};

fn scale_16_to_8(v: u16) -> u16 {
    ((u32::from(v) * 255 + 32767) / 65535) as u16
}

/// Convert to 8 or 16 bits per sample.
pub fn convert_depth(img: &Image, to: Depth) -> (Image, Notes) {
    let mut notes = Notes::new();
    if img.depth == to {
        return (img.clone(), notes);
    }
    let n = img.pixel_count() * img.layout.channels();
    let mut w = SampleWriter::new(to, n);
    for i in 0..n {
        let v = img.sample(i);
        w.push(match to {
            Depth::Eight => scale_16_to_8(v),
            Depth::Sixteen => v * 257,
        });
    }
    if to == Depth::Eight {
        notes.push(Note::new(
            Severity::Loss,
            "bit_depth_reduced",
            "16-bit samples were reduced to 8 bits",
        ));
    }
    let mut out = img.clone();
    out.depth = to;
    out.data = w.data;
    (out, notes)
}

/// Convert between channel layouts. Alpha is flattened onto `background` (8-bit RGB)
/// when the target has none; colour is reduced with BT.601 integer luma for gray.
pub fn convert_layout(img: &Image, to: Layout, background: [u8; 3]) -> (Image, Notes) {
    let mut notes = Notes::new();
    if img.layout == to {
        return (img.clone(), notes);
    }
    let max = u32::from(img.depth.max() as u16);
    let bg16 = |c: u8| -> u32 {
        match img.depth {
            Depth::Eight => u32::from(c),
            Depth::Sixteen => u32::from(c) * 257,
        }
    };
    let flatten = img.layout.has_alpha() && !to.has_alpha();
    if flatten && img.has_translucency() {
        notes.push(Note::new(
            Severity::Loss,
            "alpha_flattened",
            "transparency was blended onto a solid background",
        ));
    }
    if !img.layout.is_gray() && to.is_gray() {
        notes.push(Note::new(
            Severity::Loss,
            "color_to_gray",
            "colour was converted to grayscale",
        ));
    }
    let src_ch = img.layout.channels();
    let mut w = SampleWriter::new(img.depth, img.pixel_count() * to.channels());
    for p in 0..img.pixel_count() {
        let s = |c: usize| u32::from(img.sample(p * src_ch + c));
        let (mut rgb, alpha) = match img.layout {
            Layout::Gray => ([s(0); 3], max),
            Layout::GrayAlpha => ([s(0); 3], s(1)),
            Layout::Rgb => ([s(0), s(1), s(2)], max),
            Layout::Rgba => ([s(0), s(1), s(2)], s(3)),
        };
        if flatten && alpha != max {
            for (c, b) in rgb.iter_mut().zip(background) {
                *c = (*c * alpha + bg16(b) * (max - alpha) + max / 2) / max;
            }
        }
        let gray = (299 * rgb[0] + 587 * rgb[1] + 114 * rgb[2] + 500) / 1000;
        let gray = if img.layout.is_gray() { rgb[0] } else { gray };
        let keep_alpha = if to.has_alpha() { alpha } else { max };
        match to {
            Layout::Gray => w.push(gray as u16),
            Layout::GrayAlpha => {
                w.push(gray as u16);
                w.push(keep_alpha as u16);
            }
            Layout::Rgb => rgb.iter().for_each(|&c| w.push(c as u16)),
            Layout::Rgba => {
                rgb.iter().for_each(|&c| w.push(c as u16));
                w.push(keep_alpha as u16);
            }
        }
    }
    let mut out = img.clone();
    out.layout = to;
    out.data = w.data;
    (out, notes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Bilinear,
}

/// Resize to `width` x `height` (both at least 1).
pub fn resize(img: &Image, width: u32, height: u32, filter: Filter) -> Image {
    let (width, height) = (width.max(1), height.max(1));
    let ch = img.layout.channels();
    let mut w = SampleWriter::new(img.depth, width as usize * height as usize * ch);
    let (sw, sh) = (img.width as usize, img.height as usize);
    if sw == 0 || sh == 0 {
        let mut out = img.clone();
        out.width = width;
        out.height = height;
        out.data = vec![0; width as usize * height as usize * ch * img.depth.bytes()];
        return out;
    }
    let at = |x: usize, y: usize, c: usize| f64::from(img.sample((y * sw + x) * ch + c));
    for y in 0..height as usize {
        for x in 0..width as usize {
            // Map pixel centres.
            let fx = (x as f64 + 0.5) * sw as f64 / f64::from(width) - 0.5;
            let fy = (y as f64 + 0.5) * sh as f64 / f64::from(height) - 0.5;
            for c in 0..ch {
                let v = match filter {
                    Filter::Nearest => {
                        let nx = (fx + 0.5).floor().clamp(0.0, (sw - 1) as f64) as usize;
                        let ny = (fy + 0.5).floor().clamp(0.0, (sh - 1) as f64) as usize;
                        at(nx, ny, c)
                    }
                    Filter::Bilinear => {
                        let x0 = fx.floor().clamp(0.0, (sw - 1) as f64);
                        let y0 = fy.floor().clamp(0.0, (sh - 1) as f64);
                        let (x1, y1) = (
                            (x0 + 1.0).min((sw - 1) as f64),
                            (y0 + 1.0).min((sh - 1) as f64),
                        );
                        let tx = (fx - x0).clamp(0.0, 1.0);
                        let ty = (fy - y0).clamp(0.0, 1.0);
                        let (x0, y0, x1, y1) = (x0 as usize, y0 as usize, x1 as usize, y1 as usize);
                        let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                        let bot = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                        top * (1.0 - ty) + bot * ty
                    }
                };
                w.push(v.round().clamp(0.0, f64::from(img.depth.max())) as u16);
            }
        }
    }
    let mut out = img.clone();
    out.width = width;
    out.height = height;
    out.data = w.data;
    out
}

fn remap(
    img: &Image,
    new_w: u32,
    new_h: u32,
    src: impl Fn(usize, usize) -> (usize, usize),
) -> Image {
    let ch = img.layout.channels();
    let mut w = SampleWriter::new(img.depth, img.pixel_count() * ch);
    for y in 0..new_h as usize {
        for x in 0..new_w as usize {
            let (sx, sy) = src(x, y);
            for c in 0..ch {
                w.push(img.sample((sy * img.width as usize + sx) * ch + c));
            }
        }
    }
    let mut out = img.clone();
    out.width = new_w;
    out.height = new_h;
    out.data = w.data;
    out
}

pub fn flip_horizontal(img: &Image) -> Image {
    let w = img.width as usize;
    remap(img, img.width, img.height, |x, y| (w - 1 - x, y))
}

pub fn flip_vertical(img: &Image) -> Image {
    let h = img.height as usize;
    remap(img, img.width, img.height, |x, y| (x, h - 1 - y))
}

/// Rotate clockwise by 90 degrees.
pub fn rotate90(img: &Image) -> Image {
    let h = img.height as usize;
    remap(img, img.height, img.width, |x, y| (y, h - 1 - x))
}

pub fn rotate180(img: &Image) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    remap(img, img.width, img.height, |x, y| (w - 1 - x, h - 1 - y))
}

pub fn rotate270(img: &Image) -> Image {
    let w = img.width as usize;
    remap(img, img.height, img.width, |x, y| (w - 1 - y, x))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    fn rgb(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Image {
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                d.extend(f(x, y));
            }
        }
        Image::new(w, h, Layout::Rgb, Depth::Eight, d).unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn depth_conversion_matches_documented_formulas() {
        let img = Image::new(1, 1, Layout::Gray, Depth::Sixteen, vec![0xFF, 0xFF])
            .unwrap_or_else(|_| unreachable!());
        let (e, n) = convert_depth(&img, Depth::Eight);
        assert_eq!(e.data, vec![255]);
        assert!(n.has_loss());
        let (s, n2) = convert_depth(&e, Depth::Sixteen);
        assert_eq!(s.data, vec![255, 255]);
        assert!(n2.is_empty());
    }

    #[test]
    fn flatten_blends_onto_background_and_notes_only_when_needed() {
        let img = Image::new(
            2,
            1,
            Layout::Rgba,
            Depth::Eight,
            vec![255, 0, 0, 128, 0, 255, 0, 255],
        )
        .unwrap_or_else(|_| unreachable!());
        let (out, notes) = convert_layout(&img, Layout::Rgb, [255, 255, 255]);
        // (255*128 + 255*127 + 127) / 255 = 255 ; (0*128 + 255*127 + 127)/255 = 127
        assert_eq!(out.data, vec![255, 127, 127, 0, 255, 0]);
        assert!(notes.has_loss());
        let opaque = Image::new(1, 1, Layout::Rgba, Depth::Eight, vec![1, 2, 3, 255])
            .unwrap_or_else(|_| unreachable!());
        assert!(convert_layout(&opaque, Layout::Rgb, [0, 0, 0]).1.is_empty());
    }

    #[test]
    fn gray_uses_bt601_integer_luma() {
        let img = rgb(1, 1, |_, _| [255, 0, 0]);
        let (g, notes) = convert_layout(&img, Layout::Gray, [0, 0, 0]);
        assert_eq!(g.data, vec![76]); // (299*255 + 500)/1000
        assert!(notes.has_loss());
        let (back, _) = convert_layout(&g, Layout::Rgb, [0, 0, 0]);
        assert_eq!(back.data, vec![76, 76, 76]);
    }

    #[test]
    fn rotations_and_flips() {
        let img = rgb(3, 2, |x, y| [(y * 3 + x) as u8, 0, 0]);
        let first = |i: &Image| i.data.iter().step_by(3).copied().collect::<Vec<u8>>();
        assert_eq!(first(&img), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(first(&flip_horizontal(&img)), vec![2, 1, 0, 5, 4, 3]);
        assert_eq!(first(&flip_vertical(&img)), vec![3, 4, 5, 0, 1, 2]);
        let r = rotate90(&img);
        assert_eq!((r.width, r.height), (2, 3));
        assert_eq!(first(&r), vec![3, 0, 4, 1, 5, 2]);
        assert_eq!(rotate270(&r).data, img.data);
        assert_eq!(first(&rotate180(&img)), vec![5, 4, 3, 2, 1, 0]);
        assert_eq!(
            rotate90(&rotate90(&rotate90(&rotate90(&img)))).data,
            img.data
        );
    }

    #[test]
    fn resize_identity_and_constant() {
        let img = rgb(5, 4, |x, y| [(x * 10) as u8, (y * 10) as u8, 7]);
        assert_eq!(resize(&img, 5, 4, Filter::Nearest).data, img.data);
        assert_eq!(resize(&img, 5, 4, Filter::Bilinear).data, img.data);
        let flat = rgb(4, 4, |_, _| [9, 9, 9]);
        let big = resize(&flat, 11, 7, Filter::Bilinear);
        assert!(big.data.iter().all(|&v| v == 9));
        assert_eq!((big.width, big.height), (11, 7));
    }
}
