//! WebP: RIFF container, the lossless (VP8L) codec and lossy (VP8) key frames.
//!
//! Animation (ANIM/ANMF) is read and written through `decode_animation` / `encode_animation`.
mod lossy;
mod prefix;
mod vp8l;
mod vp8l_enc;

use crate::anim::{self, Animation};
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    matches!(
        (data.get(..4), data.get(8..12)),
        (Some(b"RIFF"), Some(b"WEBP"))
    )
}

struct Chunk<'a> {
    tag: [u8; 4],
    body: &'a [u8],
}

fn chunks(data: &[u8]) -> Result<Vec<Chunk<'_>>> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a WebP file"));
    }
    let declared = data
        .get(4..8)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_le_bytes) as usize;
    // The RIFF size covers everything after the first 8 bytes; tolerate trailing junk.
    let end = declared.saturating_add(8).min(data.len()).max(12);
    chunks_in(data.get(12..end).unwrap_or(&[]))
}

/// Splits a run of RIFF chunks.
fn chunks_in(buf: &[u8]) -> Result<Vec<Chunk<'_>>> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos + 8 <= buf.len() {
        let tag = buf
            .get(pos..pos + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .ok_or_else(|| Error::truncated("WebP chunk header"))?;
        let size = buf
            .get(pos + 4..pos + 8)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map_or(0, u32::from_le_bytes) as usize;
        let body = buf
            .get(pos + 8..pos + 8 + size)
            .ok_or_else(|| Error::truncated("WebP chunk data").at(pos as u64))?;
        out.push(Chunk { tag, body });
        pos += 8 + size + (size & 1);
    }
    Ok(out)
}

/// Decodes one VP8L or VP8 (+ALPH) bitstream into an image.
fn decode_bitstream(
    is_lossless: bool,
    body: &[u8],
    alph: Option<&[u8]>,
    vp8x_alpha: bool,
    canvas: Option<(u32, u32)>,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Image> {
    let img = if is_lossless {
        let d = vp8l::decode(body, limits, progress)?;
        check_canvas(canvas, d.width, d.height)?;
        let alpha = d.has_alpha_hint || vp8x_alpha;
        let channels = if alpha { 4 } else { 3 };
        let mut bytes = Vec::with_capacity(d.argb.len() * channels);
        for &p in &d.argb {
            bytes.push((p >> 16) as u8);
            bytes.push((p >> 8) as u8);
            bytes.push(p as u8);
            if alpha {
                bytes.push((p >> 24) as u8);
            }
        }
        let layout = if alpha { Layout::Rgba } else { Layout::Rgb };
        Image::new(d.width, d.height, layout, Depth::Eight, bytes)?
    } else {
        let (w, h) = oc_vp8::dimensions(body)?;
        let (w, h) = (w as u32, h as u32);
        check_canvas(canvas, w, h)?;
        let px = limits.check_pixels(w, h)?;
        limits.check_alloc(px * 4)?;
        let frame = oc_vp8::decode(body, progress)?;
        let rgb = lossy::frame_to_rgb(&frame);
        let alpha = match alph {
            Some(chunk) => Some(lossy::decode_alpha(
                chunk,
                frame.width,
                frame.height,
                limits,
                progress,
            )?),
            None => None,
        };
        if alpha.is_some() || vp8x_alpha {
            let plane = alpha.unwrap_or_else(|| vec![255; frame.width * frame.height]);
            let mut bytes = Vec::with_capacity(rgb.len() / 3 * 4);
            for (px, &a) in rgb.as_chunks::<3>().0.iter().zip(&plane) {
                bytes.extend_from_slice(px);
                bytes.push(a);
            }
            Image::new(w, h, Layout::Rgba, Depth::Eight, bytes)?
        } else {
            Image::new(w, h, Layout::Rgb, Depth::Eight, rgb)?
        }
    };
    Ok(img)
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let list = chunks(data)?;
    if is_animated(&list) {
        let (anim, n) = decode_animation_inner(&list, limits, progress, true)?;
        notes.extend(n);
        notes.push(Note::new(
            Severity::Warning,
            "webp.animation_first_frame",
            "the file is animated; only the first frame was read",
        ));
        let img = anim
            .first_image()
            .ok_or_else(|| Error::corrupt("animated WebP has no frames"))?;
        return Ok((img, notes));
    }
    Ok((decode_still(&list, limits, progress)?, notes))
}

fn is_animated(list: &[Chunk<'_>]) -> bool {
    list.iter()
        .find(|c| &c.tag == b"VP8X")
        .is_some_and(|c| c.body.first().is_some_and(|f| f & 0x02 != 0))
}

fn le24(b: &[u8], i: usize) -> u32 {
    (0..3).fold(0u32, |a, k| {
        a | (u32::from(b.get(i + k).copied().unwrap_or(0)) << (8 * k))
    })
}

fn vp8x_canvas(body: &[u8]) -> (u32, u32) {
    (le24(body, 4) + 1, le24(body, 7) + 1)
}

/// Every frame of an animated WebP composited onto the canvas. A file that is not animated
/// yields its single image as one frame.
pub fn decode_animation(
    data: &[u8],
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<(Animation, Notes)> {
    let list = chunks(data)?;
    decode_animation_inner(&list, limits, progress, false)
}

fn decode_animation_inner(
    list: &[Chunk<'_>],
    limits: &Limits,
    progress: &dyn Progress,
    first_only: bool,
) -> Result<(Animation, Notes)> {
    let notes = Notes::new();
    if !is_animated(list) {
        let img = decode_still(list, limits, progress)?;
        let mut a = Animation::new(img.width, img.height);
        a.metadata = img.metadata.clone();
        a.loops = 1;
        a.frames.push(anim::Frame {
            image: ops_rgba(&img),
            delay_ms: 0,
        });
        return Ok((a, notes));
    }
    let (w, h) = list
        .iter()
        .find(|c| &c.tag == b"VP8X")
        .map_or((0, 0), |c| vp8x_canvas(c.body));
    limits.check_pixels(w, h)?;
    let mut a = Animation::new(w, h);
    // The background colour is only a hint; players show the animation on transparency.
    a.loops = list
        .iter()
        .find(|c| &c.tag == b"ANIM")
        .and_then(|c| c.body.get(4..6))
        .and_then(|b| <[u8; 2]>::try_from(b).ok())
        .map_or(0, |b| u32::from(u16::from_le_bytes(b)));
    a.metadata.icc_profile = list
        .iter()
        .find(|c| &c.tag == b"ICCP")
        .map(|c| c.body.to_vec());
    let mut canvas = anim::Compositor::new(w, h);
    for c in list.iter().filter(|c| &c.tag == b"ANMF") {
        if c.body.len() < 16 {
            return Err(Error::truncated("WebP frame header"));
        }
        let (x, y) = (le24(c.body, 0) as usize * 2, le24(c.body, 3) as usize * 2);
        let (fw, fh) = (le24(c.body, 6) + 1, le24(c.body, 9) + 1);
        let delay_ms = le24(c.body, 12);
        let flags = c.body.get(15).copied().unwrap_or(0);
        if x as u64 + u64::from(fw) > u64::from(w) || y as u64 + u64::from(fh) > u64::from(h) {
            return Err(Error::corrupt("WebP frame lies outside the canvas"));
        }
        anim::check_frame(limits, w, h, a.frames.len())?;
        let inner = chunks_in(c.body.get(16..).unwrap_or(&[]))?;
        let img = decode_still(&inner, limits, progress)?;
        if (img.width, img.height) != (fw, fh) {
            return Err(Error::corrupt("WebP frame size differs from its header"));
        }
        let img = ops_rgba(&img);
        let dispose = if flags & 1 != 0 {
            anim::Dispose::Background
        } else {
            anim::Dispose::Keep
        };
        let shown = canvas
            .draw(
                (x, y, fw as usize, fh as usize),
                &img.data,
                flags & 2 == 0,
                dispose,
            )
            .to_vec();
        a.frames.push(anim::Frame {
            image: anim::canvas_image(w, h, shown)?,
            delay_ms,
        });
        if first_only {
            break;
        }
    }
    if a.frames.is_empty() {
        return Err(Error::corrupt("animated WebP has no frames"));
    }
    Ok((a, notes))
}

fn ops_rgba(img: &Image) -> Image {
    ops::convert_layout(img, Layout::Rgba, [0, 0, 0]).0
}

/// Decodes the first image among the chunks.
fn decode_still(list: &[Chunk<'_>], limits: &Limits, progress: &dyn Progress) -> Result<Image> {
    let mut vp8x_alpha = false;
    let mut canvas = None;
    let mut icc = None;
    let mut image: Option<(bool, &[u8])> = None;
    let mut alph: Option<&[u8]> = None;
    for c in list {
        match &c.tag {
            b"VP8X" => {
                vp8x_alpha = c.body.first().is_some_and(|f| f & 0x10 != 0);
                canvas = Some(vp8x_canvas(c.body));
            }
            b"ICCP" => icc = Some(c.body.to_vec()),
            b"VP8L" if image.is_none() => image = Some((true, c.body)),
            b"VP8 " if image.is_none() => image = Some((false, c.body)),
            b"ALPH" if alph.is_none() => alph = Some(c.body),
            _ => {}
        }
    }
    let (is_lossless, body) = image.ok_or_else(|| Error::corrupt("WebP file has no image data"))?;
    let mut img = decode_bitstream(
        is_lossless,
        body,
        alph,
        vp8x_alpha,
        canvas,
        limits,
        progress,
    )?;
    img.metadata.icc_profile = icc;
    Ok(img)
}

fn check_canvas(canvas: Option<(u32, u32)>, w: u32, h: u32) -> Result<()> {
    match canvas {
        Some(c) if c != (w, h) => Err(Error::corrupt(
            "WebP canvas size differs from the image size",
        )),
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    /// 0 (fastest) to 6 (smallest).
    pub effort: u8,
    /// Zero the colour of fully transparent pixels, which compresses better and
    /// cannot be seen.
    pub clean_transparent: bool,
    /// `Some(q)` with `q` in 0..=100 encodes lossily (VP8); `None` is lossless.
    pub quality: Option<f32>,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            effort: 4,
            clean_transparent: false,
            quality: None,
        }
    }
}

fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() & 1 == 1 {
        out.push(0);
    }
}

/// An 8-bit RGB or RGBA copy of the image, as the bitstream encoders want it.
fn prepare(img: &Image, notes: &mut Notes) -> Result<(Image, bool)> {
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    let target = if e8.layout.has_alpha() {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let (px, n) = ops::convert_layout(&e8, target, [255, 255, 255]);
    notes.extend(n);
    let alpha = px.has_translucency();
    Ok((px, alpha))
}

/// The chunks that make up one image: `VP8L`, or `ALPH` (when needed) and `VP8 `.
fn image_chunks(
    px: Image,
    alpha: bool,
    opts: &EncodeOptions,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    if let Some(quality) = opts.quality {
        if px.width > 16383 || px.height > 16383 {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "lossy WebP images are at most 16383 pixels on a side",
            ));
        }
        if !(0.0..=100.0).contains(&quality) {
            return Err(Error::invalid_option(
                "quality",
                "must be between 0 and 100",
            ));
        }
        let mut px = px;
        let (w, h) = (px.width as usize, px.height as usize);
        let mut plane = None;
        if alpha {
            lossy::flatten_transparent(&mut px.data, w, h);
            plane = Some(
                px.data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| c[3])
                    .collect::<Vec<u8>>(),
            );
        }
        let frame = lossy::rgb_to_yuv(&px.data, w, h, px.layout.channels());
        let params = oc_vp8::Params {
            quality,
            effort: opts.effort.min(6),
            ..oc_vp8::Params::default()
        };
        let vp8 = oc_vp8::encode(&frame, &params, &oc_core::NoProgress)?;
        notes.push(Note::new(
            Severity::Info,
            "image.lossy_compression",
            format!("lossy WebP at quality {quality}"),
        ));
        if let Some(plane) = &plane {
            chunk(
                &mut body,
                b"ALPH",
                &lossy::encode_alpha(plane, w, h, opts.effort),
            );
        }
        chunk(&mut body, b"VP8 ", &vp8);
        return Ok(body);
    }
    let ch = px.layout.channels();
    let argb: Vec<u32> = px
        .data
        .chunks_exact(ch)
        .map(|c| match *c {
            [_, _, _, 0] if opts.clean_transparent => 0,
            [r, g, b, a] => u32::from_be_bytes([a, r, g, b]),
            [r, g, b] => u32::from_be_bytes([255, r, g, b]),
            _ => 0xff00_0000,
        })
        .collect();
    let stream = vp8l_enc::encode(
        &argb,
        px.width as usize,
        px.height as usize,
        alpha,
        opts.effort.min(6),
    );
    chunk(&mut body, b"VP8L", &stream);
    Ok(body)
}

fn riff(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(body);
    out
}

/// Encode as WebP: lossless (VP8L) by default, lossy (VP8) when a quality is given. The ICC
/// profile and a lossy image's alpha go in an extended header.
pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    if img.width > 16384 || img.height > 16384 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "WebP images are at most 16384 pixels on a side",
        ));
    }
    let (px, alpha) = prepare(img, &mut notes)?;
    let (w, h) = (px.width, px.height);
    let chunks = image_chunks(px, alpha, opts, &mut notes)?;
    let icc = img.metadata.icc_profile.as_deref();
    let mut body = Vec::new();
    if icc.is_some() || (alpha && opts.quality.is_some()) {
        chunk(&mut body, b"VP8X", &vp8x(alpha, icc.is_some(), false, w, h));
    }
    if let Some(icc) = icc {
        chunk(&mut body, b"ICCP", icc);
    }
    body.extend_from_slice(&chunks);
    Ok((riff(&body), notes))
}

fn vp8x(alpha: bool, icc: bool, animated: bool, w: u32, h: u32) -> Vec<u8> {
    let flags =
        if icc { 0x20 } else { 0 } | if animated { 0x02 } else { 0 } | if alpha { 0x10 } else { 0 };
    let mut x = vec![flags, 0, 0, 0];
    x.extend((w - 1).to_le_bytes().iter().take(3));
    x.extend((h - 1).to_le_bytes().iter().take(3));
    x
}

/// Writes an animated WebP. Frame 0 is the whole canvas; every later frame replaces the
/// rectangle (with even corner coordinates, as the format requires) that changed.
pub fn encode_animation(anim: &Animation, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let (w, h) = (anim.width as usize, anim.height as usize);
    if anim.frames.is_empty() || w == 0 || h == 0 {
        return Err(Error::new(
            ErrorCode::InvalidOptions,
            "an animation needs frames",
        ));
    }
    if anim.width > 16384 || anim.height > 16384 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "WebP images are at most 16384 pixels on a side",
        ));
    }
    let mut shown: Vec<Vec<u8>> = Vec::with_capacity(anim.frames.len());
    for f in &anim.frames {
        let (e8, n) = ops::convert_depth(&f.image, Depth::Eight);
        notes.extend(n);
        let rgba = ops_rgba(&e8);
        if rgba.width as usize != w || rgba.height as usize != h {
            return Err(Error::new(
                ErrorCode::InvalidOptions,
                "animation frames differ in size",
            ));
        }
        shown.push(rgba.data);
    }
    let alpha = shown
        .iter()
        .any(|d| d.as_chunks::<4>().0.iter().any(|p| p[3] != 255));
    let icc = anim.metadata.icc_profile.as_deref();
    let mut body = Vec::new();
    chunk(
        &mut body,
        b"VP8X",
        &vp8x(alpha, icc.is_some(), true, anim.width, anim.height),
    );
    if let Some(icc) = icc {
        chunk(&mut body, b"ICCP", icc);
    }
    let mut anim_chunk = vec![0, 0, 0, 0];
    anim_chunk.extend_from_slice(&(anim.loops.min(0xFFFF) as u16).to_le_bytes());
    chunk(&mut body, b"ANIM", &anim_chunk);
    let blank = anim::blank_canvas(anim.width, anim.height);
    for (i, cur) in shown.iter().enumerate() {
        let (mut x, mut y, mut rw, mut rh) = if i == 0 {
            (0, 0, w, h)
        } else {
            anim::changed_rect(shown.get(i - 1).unwrap_or(&blank), cur, w, h)
                .unwrap_or((0, 0, 1, 1))
        };
        // Offsets are stored halved, so grow the rectangle to start on even pixels.
        rw += x & 1;
        rh += y & 1;
        x &= !1;
        y &= !1;
        let region = anim::crop_rgba(cur, w, (x, y, rw, rh));
        let region = if alpha {
            region
        } else {
            anim::rgba_to_rgb(&region)
        };
        let layout = if alpha { Layout::Rgba } else { Layout::Rgb };
        let img = Image::new(rw as u32, rh as u32, layout, Depth::Eight, region)?;
        let has_alpha = alpha && img.has_translucency();
        let mut frame_notes = Notes::new();
        let data = image_chunks(img, has_alpha, opts, &mut frame_notes)?;
        if i == 0 {
            notes.extend(frame_notes);
        }
        let mut anmf = Vec::with_capacity(16 + data.len());
        for v in [x as u32 / 2, y as u32 / 2, rw as u32 - 1, rh as u32 - 1] {
            anmf.extend_from_slice(&v.to_le_bytes()[..3]);
        }
        let ms = anim.frames.get(i).map_or(0, |f| f.delay_ms).min(0xFF_FFFF);
        anmf.extend_from_slice(&ms.to_le_bytes()[..3]);
        anmf.push(0b10); // keep the previous frame, do not blend
        anmf.extend_from_slice(&data);
        chunk(&mut body, b"ANMF", &anmf);
    }
    Ok((riff(&body), notes))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn fixtures() -> Vec<std::path::PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/webp");
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "webp"))
            .collect();
        v.sort();
        v
    }

    fn rgba_of(img: &Image) -> Vec<u8> {
        let ch = img.layout.channels();
        let mut out = Vec::new();
        for px in img.data.chunks(ch) {
            out.extend_from_slice(&px[..3]);
            out.push(if ch == 4 { px[3] } else { 255 });
        }
        out
    }

    #[test]
    fn matches_libwebp_via_pillow() {
        let files = fixtures();
        assert!(files.len() >= 25, "fixtures missing");
        for f in files {
            let data = std::fs::read(&f).unwrap();
            let (img, _) = decode(&data, &Limits::default(), &NoProgress)
                .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            let want = std::fs::read(f.with_extension("rgba")).unwrap();
            assert_eq!(rgba_of(&img), want, "{}", f.display());
        }
    }

    fn lossy_fixtures() -> Vec<std::path::PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/webp_lossy");
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "webp"))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn lossy_matches_libwebp_via_pillow() {
        let files = lossy_fixtures();
        assert!(files.len() >= 30, "fixtures missing");
        for f in files {
            let data = std::fs::read(&f).unwrap();
            let (img, _) = decode(&data, &Limits::default(), &NoProgress)
                .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            let rgba = rgba_of(&img);
            let sum = std::fs::read_to_string(f.with_extension("sum")).unwrap();
            let want: Vec<&str> = sum.split_whitespace().collect();
            let got = [
                rgba.len().to_string(),
                format!("{:08x}", oc_checksum::crc32(&rgba)),
                format!("{:08x}", oc_checksum::adler32(&rgba)),
            ];
            assert_eq!(got.as_slice(), want.as_slice(), "{}", f.display());
        }
    }

    #[test]
    fn distance_table_matches_libwebp_encoding() {
        // libwebp stores each entry as (y << 4) | (8 - x).
        const PACKED: [u8; 120] = [
            0x18, 0x07, 0x17, 0x19, 0x28, 0x06, 0x27, 0x29, 0x16, 0x1a, 0x26, 0x2a, 0x38, 0x05,
            0x37, 0x39, 0x15, 0x1b, 0x36, 0x3a, 0x25, 0x2b, 0x48, 0x04, 0x47, 0x49, 0x14, 0x1c,
            0x35, 0x3b, 0x46, 0x4a, 0x24, 0x2c, 0x58, 0x45, 0x4b, 0x34, 0x3c, 0x03, 0x57, 0x59,
            0x13, 0x1d, 0x56, 0x5a, 0x23, 0x2d, 0x44, 0x4c, 0x55, 0x5b, 0x33, 0x3d, 0x68, 0x02,
            0x67, 0x69, 0x12, 0x1e, 0x66, 0x6a, 0x22, 0x2e, 0x54, 0x5c, 0x43, 0x4d, 0x65, 0x6b,
            0x32, 0x3e, 0x78, 0x01, 0x77, 0x79, 0x53, 0x5d, 0x11, 0x1f, 0x64, 0x6c, 0x42, 0x4e,
            0x76, 0x7a, 0x21, 0x2f, 0x75, 0x7b, 0x31, 0x3f, 0x63, 0x6d, 0x52, 0x5e, 0x00, 0x74,
            0x7c, 0x41, 0x4f, 0x10, 0x20, 0x62, 0x6e, 0x30, 0x73, 0x7d, 0x51, 0x5f, 0x40, 0x72,
            0x7e, 0x61, 0x6f, 0x50, 0x71, 0x7f, 0x60, 0x70,
        ];
        for (i, &p) in PACKED.iter().enumerate() {
            let want = (
                8 - i8::try_from(p & 0xf).unwrap(),
                i8::try_from(p >> 4).unwrap(),
            );
            assert_eq!(vp8l::DISTANCE_MAP[i], want, "entry {i}");
        }
    }

    fn sample_images() -> Vec<Image> {
        let mut out = Vec::new();
        for f in fixtures() {
            let data = std::fs::read(&f).unwrap();
            out.push(decode(&data, &Limits::default(), &NoProgress).unwrap().0);
        }
        // Gray, gray+alpha and noise.
        let mut seed = 12345u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        for (layout, w, h) in [
            (Layout::Gray, 33, 17),
            (Layout::GrayAlpha, 20, 20),
            (Layout::Rgba, 64, 3),
            (Layout::Rgb, 1, 40),
            (Layout::Rgb, 40, 1),
        ] {
            let n = w * h * layout.channels();
            let data: Vec<u8> = (0..n).map(|_| rnd()).collect();
            out.push(Image::new(w as u32, h as u32, layout, Depth::Eight, data).unwrap());
        }
        out
    }

    #[test]
    fn every_effort_roundtrips_exactly() {
        for (k, img) in sample_images().iter().enumerate() {
            // Debug builds are slow: big pictures only get one pass.
            let efforts: &[u8] = if img.pixel_count() > 6000 {
                &[4]
            } else {
                &[0, 2, 4, 6]
            };
            for &effort in efforts {
                let (bytes, _) = encode(
                    img,
                    &EncodeOptions {
                        effort,
                        clean_transparent: false,
                        quality: None,
                    },
                )
                .unwrap();
                let (back, _) = decode(&bytes, &Limits::default(), &NoProgress)
                    .unwrap_or_else(|e| panic!("image {k} effort {effort}: {e}"));
                let want = ops::convert_layout(img, Layout::Rgba, [255, 255, 255]).0;
                assert_eq!(rgba_of(&back), want.data, "image {k} effort {effort}");
            }
        }
    }

    #[test]
    fn damaged_files_fail_cleanly() {
        let mut seed = 7u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as usize
        };
        let limits = Limits {
            max_pixels: 1 << 22,
            ..Limits::default()
        };
        for f in fixtures().into_iter().chain(lossy_fixtures()) {
            let data = std::fs::read(&f).unwrap();
            // Every cut either decodes (a prefix that happens to be complete) or errors.
            for cut in (0..data.len()).step_by(data.len() / 60 + 1) {
                let _ = decode(&data[..cut], &limits, &NoProgress);
            }
            for _ in 0..10 {
                let mut d = data.clone();
                for _ in 0..1 + next() % 4 {
                    let i = next() % d.len();
                    d[i] ^= 1 << (next() % 8);
                }
                let _ = decode(&d, &limits, &NoProgress);
            }
        }
    }

    #[test]
    fn empty_animation_and_junk_are_rejected() {
        // VP8X with the animation flag but no frames.
        let mut anim = b"RIFF\x16\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        anim.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let e = decode(&anim, &Limits::default(), &NoProgress).unwrap_err();
        assert_eq!(e.code, ErrorCode::CorruptFile);
        // A lossy chunk that is not a key frame.
        let mut junk = b"RIFF\x16\0\0\0WEBPVP8 \x0a\0\0\0".to_vec();
        junk.extend_from_slice(&[0; 10]);
        assert!(decode(&junk, &Limits::default(), &NoProgress).is_err());
        assert!(sniff(&junk));
        assert!(!sniff(b"RIFF....WAVE"));
    }
}
