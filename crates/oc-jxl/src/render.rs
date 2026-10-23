//! Turning decoded channels into output samples.
use crate::frame::{unsupported, Decoded};
use crate::header::{EcType, FrameHeader, ImageHeader};
use crate::modular::Chan;
use oc_core::{Note, Notes, Result, Severity};

/// A decoded picture: interleaved samples of 1 to 4 channels (grey, grey+alpha, RGB, RGB+alpha),
/// 8 bits (`sixteen == false`) or 16 bits per sample.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub channels: usize,
    pub sixteen: bool,
    pub samples: Vec<u16>,
}

fn full_canvas(fh: &FrameHeader, img: &ImageHeader) -> bool {
    fh.x0 <= 0
        && fh.y0 <= 0
        && i64::from(fh.x0) + i64::from(fh.width) >= i64::from(img.width)
        && i64::from(fh.y0) + i64::from(fh.height) >= i64::from(img.height)
}

pub fn render(dec: &Decoded, img: &ImageHeader, notes: &mut Notes) -> Result<Picture> {
    let fh = &dec.header;
    if !full_canvas(fh, img) || fh.blending.mode != crate::header::Blend::Replace {
        return Err(unsupported("frames that blend onto earlier frames are not supported"));
    }
    if img.bit_depth.exp_bits.is_some() {
        return Err(unsupported("floating-point samples are not supported"));
    }
    let cc = fh.encoded_color_channels;
    let (w, h) = (img.width as usize, img.height as usize);
    // VarDCT frames keep their colour in `xyb`; `chans` then holds only the extra channels
    let ec_base = if dec.xyb.is_some() { 0 } else { cc };
    if dec.xyb.is_none() && (dec.chans.len() < cc || dec.chans[..cc].iter().any(|c| c.w != w || c.h != h)) {
        return Err(unsupported("colour channels with a different size"));
    }
    if dec.xyb.is_some() && (fh.color_w() as usize != w || fh.color_h() as usize != h) {
        return Err(unsupported("colour channels with a different size"));
    }
    let alpha = img.ec.iter().position(|e| matches!(e.ty, EcType::Alpha { .. }));
    if img.ec.len() > usize::from(alpha.is_some()) {
        notes.push(Note::new(Severity::Loss, "extra_channels_dropped", "extra channels other than alpha are dropped"));
    }
    let sixteen = img.bit_depth.bits > 8;
    let out_max: f32 = if sixteen { 65535.0 } else { 255.0 };

    // Planes as floats in 0..=1 (integer samples scaled by the bit depth, XYB converted).
    let mut planes: Vec<Vec<f32>> = Vec::new();
    if let Some([x, y, b]) = &dec.xyb {
        let (mut x, mut y, mut b) = (x.clone(), y.clone(), b.clone());
        xyb_to_srgb(&mut x, &mut y, &mut b, img);
        planes = vec![x, y, b];
    } else if img.xyb_encoded {
        planes = xyb_planes(dec, img, cc);
    } else {
        let maxv = ((1u64 << img.bit_depth.bits) - 1) as f32;
        for c in &dec.chans[..cc] {
            planes.push(c.d.iter().map(|&v| v as f32 / maxv).collect());
        }
    }
    let mut alpha_plane = None;
    if let Some(a) = alpha {
        let c: &Chan = &dec.chans[ec_base + a];
        if c.w != w || c.h != h {
            return Err(unsupported("alpha channel with a different size"));
        }
        let maxv = ((1u64 << img.ec[a].bit_depth.bits) - 1) as f32;
        alpha_plane = Some(c.d.iter().map(|&v| (v as f32 / maxv).clamp(0.0, 1.0)).collect::<Vec<f32>>());
        if let EcType::Alpha { associated: true } = img.ec[a].ty {
            let ap = alpha_plane.as_ref().unwrap();
            for p in &mut planes {
                for (v, &al) in p.iter_mut().zip(ap) {
                    *v = if al > 0.0 { *v / al } else { 0.0 };
                }
            }
        }
    }
    let grey = cc == 1 || img.is_grey();
    if grey && planes.len() == 3 {
        planes.truncate(2);
        planes.remove(0);
        planes.truncate(1);
    }
    let channels = if grey { 1 } else { 3 } + usize::from(alpha_plane.is_some());
    let mut samples = Vec::with_capacity(w * h * channels);
    for i in 0..w * h {
        for p in &planes {
            samples.push((p[i].clamp(0.0, 1.0) * out_max + 0.5) as u16);
        }
        if let Some(a) = &alpha_plane {
            samples.push((a[i] * out_max + 0.5) as u16);
        }
    }
    let (width, height, samples) = orient(img.orientation, w, h, channels, samples);
    Ok(Picture { width: width as u32, height: height as u32, channels, sixteen, samples })
}

fn xyb_planes(dec: &Decoded, img: &ImageHeader, cc: usize) -> Vec<Vec<f32>> {
    // Modular XYB stores (Y, X, B - Y) as integers; dequantise to the XYB planes.
    let n = dec.chans[0].d.len();
    let (c0, c1, c2) = (&dec.chans[0].d, &dec.chans[1].d, &dec.chans[2].d);
    let mut x = vec![0f32; n];
    let mut y = vec![0f32; n];
    let mut b = vec![0f32; n];
    for i in 0..n {
        x[i] = c1[i] as f32 * dec.lf_dequant[0];
        y[i] = c0[i] as f32 * dec.lf_dequant[1];
        b[i] = (c2[i].saturating_add(c0[i])) as f32 * dec.lf_dequant[2];
    }
    let _ = cc;
    xyb_to_srgb(&mut x, &mut y, &mut b, img);
    vec![x, y, b]
}

/// Convert XYB planes to sRGB-encoded planes in 0..=1, in place (X becomes R, Y becomes G, B stays).
pub fn xyb_to_srgb(x: &mut [f32], y: &mut [f32], b: &mut [f32], img: &ImageHeader) {
    let ob = img.opsin.bias;
    let cbrt_ob = ob.map(f32::cbrt);
    let m = img.opsin.inv_mat;
    let itscale = 255.0 / 255.0f32;
    for i in 0..x.len() {
        let g_l = y[i] + x[i] - cbrt_ob[0];
        let g_m = y[i] - x[i] - cbrt_ob[1];
        let g_s = b[i] - cbrt_ob[2];
        let l = (g_l * g_l * g_l + ob[0]) * itscale;
        let mm = (g_m * g_m * g_m + ob[1]) * itscale;
        let s = (g_s * g_s * g_s + ob[2]) * itscale;
        let r = m[0][0] * l + m[0][1] * mm + m[0][2] * s;
        let g = m[1][0] * l + m[1][1] * mm + m[1][2] * s;
        let bl = m[2][0] * l + m[2][1] * mm + m[2][2] * s;
        x[i] = linear_to_srgb(r);
        y[i] = linear_to_srgb(g);
        b[i] = linear_to_srgb(bl);
    }
}

pub fn linear_to_srgb(v: f32) -> f32 {
    let a = v.abs();
    let e = if a <= 0.003_130_8 { 12.92 * a } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    e.copysign(v)
}

/// Apply the EXIF orientation (1..8) so the result is upright.
fn orient(o: u32, w: usize, h: usize, ch: usize, s: Vec<u16>) -> (usize, usize, Vec<u16>) {
    if o <= 1 || o > 8 {
        return (w, h, s);
    }
    let (nw, nh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut out = vec![0u16; s.len()];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                _ => (y, w - 1 - x),
            };
            let src = (y * w + x) * ch;
            let dst = (dy * nw + dx) * ch;
            out[dst..dst + ch].copy_from_slice(&s[src..src + ch]);
        }
    }
    (nw, nh, out)
}
