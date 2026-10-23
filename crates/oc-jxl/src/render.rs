//! Turning decoded frames into floating-point sample planes and, at the end, into output pixels.
use crate::colour;
use crate::frame::{unsupported, Decoded};
use crate::header::{EcType, ImageHeader};
use oc_core::{Note, Notes, Result, Severity};

/// A set of equally sized sample planes: the colour planes (1 or 3) followed by the extra
/// channels. Values are nominally 0..=1.
#[derive(Clone)]
pub struct Planes {
    pub w: usize,
    pub h: usize,
    pub ch: Vec<Vec<f32>>,
    pub colour: usize,
    /// The colour planes are still XYB (the colour transform has not been applied).
    pub xyb: bool,
}

/// The decoded picture: interleaved samples of 8 or 16 bits, upright.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub channels: usize,
    pub sixteen: bool,
    pub samples: Vec<u16>,
}

/// The planes of a decoded frame at the frame's size.
pub fn frame_planes(dec: Decoded, img: &ImageHeader) -> Result<Planes> {
    let fh = &dec.header;
    if img.bit_depth.exp_bits.is_some() {
        return Err(unsupported("floating-point samples are not supported"));
    }
    let (w, h) = (fh.color_w() as usize, fh.color_h() as usize);
    let cc = fh.encoded_color_channels;
    let mut ch: Vec<Vec<f32>>;
    let xyb;
    let ec_base;
    if let Some(mut planes) = dec.xyb {
        let ycc = fh.do_ycbcr;
        if ycc {
            ycbcr_to_rgb(&mut planes);
        }
        ch = planes.into();
        if ycc && img.is_grey() {
            ch.truncate(1);
        }
        xyb = !ycc;
        ec_base = 0;
    } else {
        if dec.chans.len() < cc || dec.chans[..cc].iter().any(|c| c.w != w || c.h != h) {
            return Err(unsupported("colour channels with a different size"));
        }
        ec_base = cc;
        if img.xyb_encoded {
            let n = w * h;
            let (c0, c1, c2) = (&dec.chans[0].d, &dec.chans[1].d, &dec.chans[2].d);
            let mut x = vec![0f32; n];
            let mut y = vec![0f32; n];
            let mut b = vec![0f32; n];
            for i in 0..n {
                // Modular XYB stores (Y, X, B - Y) as integers
                x[i] = c1[i] as f32 * dec.lf_dequant[0];
                y[i] = c0[i] as f32 * dec.lf_dequant[1];
                b[i] = (c2[i].saturating_add(c0[i])) as f32 * dec.lf_dequant[2];
            }
            ch = vec![x, y, b];
            xyb = true;
        } else {
            let maxv = ((1u64 << img.bit_depth.bits) - 1) as f32;
            ch = dec.chans[..cc]
                .iter()
                .map(|c| c.d.iter().map(|&v| v as f32 / maxv).collect())
                .collect();
            xyb = false;
        }
    }
    let colour = if xyb {
        3
    } else if ch.len() == 1 {
        1
    } else {
        cc
    };
    for (i, e) in img.ec.iter().enumerate() {
        let c = &dec.chans[ec_base + i];
        if c.w != w || c.h != h {
            return Err(unsupported("extra channels with a different size"));
        }
        let maxv = ((1u64 << e.bit_depth.bits.min(32)) - 1) as f32;
        ch.push(c.d.iter().map(|&v| v as f32 / maxv).collect());
    }
    Ok(Planes {
        w,
        h,
        ch,
        colour,
        xyb,
    })
}

/// JPEG's YCbCr to RGB on planes stored in the order Cb, Y, Cr; the planes hold the AC part
/// centred on zero, so Y gets its 128 offset back.
fn ycbcr_to_rgb(p: &mut [Vec<f32>; 3]) {
    let [cb, y, cr] = p;
    for i in 0..y.len() {
        let (b, l, r) = (cb[i], y[i] + 128.0 / 255.0, cr[i]);
        cb[i] = l + 1.402 * r;
        y[i] = l - 0.114 * 1.772 / 0.587 * b - 0.299 * 1.402 / 0.587 * r;
        cr[i] = l + 1.772 * b;
    }
}

/// Apply the colour transform: XYB becomes the file's encoded colour values.
pub fn to_native(p: &mut Planes, img: &ImageHeader) {
    if !p.xyb {
        return;
    }
    let mut it = p.ch.drain(..3);
    let (x, y, b) = (
        it.next().unwrap_or_default(),
        it.next().unwrap_or_default(),
        it.next().unwrap_or_default(),
    );
    drop(it);
    let native = colour::xyb_to_native(img, x, y, b);
    p.colour = native.len();
    let rest: Vec<Vec<f32>> = std::mem::take(&mut p.ch);
    p.ch = native;
    p.ch.extend(rest);
    p.xyb = false;
}

/// Convert the final canvas to output samples.
pub fn finish(canvas: Planes, img: &ImageHeader, notes: &mut Notes) -> Result<Picture> {
    let (w, h) = (canvas.w, canvas.h);
    let colour = canvas.colour;
    let alpha = img
        .ec
        .iter()
        .position(|e| matches!(e.ty, EcType::Alpha { .. }));
    if img.ec.len() > usize::from(alpha.is_some()) {
        notes.push(Note::new(
            Severity::Loss,
            "extra_channels_dropped",
            "extra channels other than alpha are dropped",
        ));
    }
    let sixteen = img.bit_depth.bits > 8;
    let out_max: f32 = if sixteen { 65535.0 } else { 255.0 };

    let mut planes: Vec<Vec<f32>> = canvas.ch[..colour].to_vec();
    let mut alpha_plane = None;
    if let Some(a) = alpha {
        let ap: Vec<f32> = canvas.ch[colour + a]
            .iter()
            .map(|v| v.clamp(0.0, 1.0))
            .collect();
        if let EcType::Alpha { associated: true } = img.ec[a].ty {
            for p in &mut planes {
                for (v, &al) in p.iter_mut().zip(&ap) {
                    *v = if al > 0.0 { *v / al } else { 0.0 };
                }
            }
        }
        alpha_plane = Some(ap);
    }
    if colour::is_hdr(&img.colour) {
        notes.push(Note::new(
            Severity::Loss,
            "hdr_clipped",
            "high dynamic range colours were mapped to sRGB without tone mapping",
        ));
    }
    colour::native_to_srgb(&img.colour, &mut planes);

    let channels = planes.len() + usize::from(alpha_plane.is_some());
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
    Ok(Picture {
        width: width as u32,
        height: height as u32,
        channels,
        sixteen,
        samples,
    })
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
