//! WebP: RIFF container, the lossless (VP8L) codec and lossy (VP8) key frames.
//!
//! Animation is not supported yet and fails with a typed error.
mod lossy;
mod prefix;
mod vp8l;
mod vp8l_enc;

use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_core::{Error, ErrorCode, Limits, Notes, Progress, Result};

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
    let mut pos = 12;
    let mut out = Vec::new();
    while pos + 8 <= end {
        let tag = data
            .get(pos..pos + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .ok_or_else(|| Error::truncated("WebP chunk header"))?;
        let size = data
            .get(pos + 4..pos + 8)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map_or(0, u32::from_le_bytes) as usize;
        let body = data
            .get(pos + 8..pos + 8 + size)
            .ok_or_else(|| Error::truncated("WebP chunk data").at(pos as u64))?;
        out.push(Chunk { tag, body });
        pos += 8 + size + (size & 1);
    }
    Ok(out)
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    let notes = Notes::new();
    let list = chunks(data)?;
    let mut vp8x_alpha = false;
    let mut canvas: Option<(u32, u32)> = None;
    let mut icc: Option<Vec<u8>> = None;
    // The first image chunk decides between the lossless and the lossy codec.
    let mut image: Option<(bool, &[u8])> = None;
    let mut alph: Option<&[u8]> = None;
    for c in &list {
        match &c.tag {
            b"VP8X" => {
                let flags = c.body.first().copied().unwrap_or(0);
                if flags & 0x02 != 0 {
                    return Err(Error::new(
                        ErrorCode::UnsupportedVariant,
                        "animated WebP is not supported yet",
                    ));
                }
                vp8x_alpha = flags & 0x10 != 0;
                let le24 = |i: usize| -> u32 {
                    (0..3).fold(0u32, |a, k| {
                        a | (u32::from(c.body.get(i + k).copied().unwrap_or(0)) << (8 * k))
                    })
                };
                canvas = Some((le24(4) + 1, le24(7) + 1));
            }
            b"ICCP" => icc = Some(c.body.to_vec()),
            b"VP8L" if image.is_none() => image = Some((true, c.body)),
            b"VP8 " if image.is_none() => image = Some((false, c.body)),
            b"ALPH" if alph.is_none() => alph = Some(c.body),
            b"ANMF" | b"ANIM" => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "animated WebP is not supported yet",
                ));
            }
            _ => {}
        }
    }
    let (is_lossless, body) = image.ok_or_else(|| Error::corrupt("WebP file has no image data"))?;
    let mut img = if is_lossless {
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
    img.metadata.icc_profile = icc;
    Ok((img, notes))
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

/// Encode as lossless WebP (VP8L), with the ICC profile in an extended header if present.
pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    if img.width > 16384 || img.height > 16384 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "WebP images are at most 16384 pixels on a side",
        ));
    }
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
    if let Some(quality) = opts.quality {
        return encode_lossy(img, px, alpha, opts, quality, notes);
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
    let mut body = Vec::new();
    body.extend_from_slice(b"WEBP");
    if let Some(icc) = &img.metadata.icc_profile {
        let mut x = vec![0x20 | if alpha { 0x10 } else { 0 }, 0, 0, 0];
        x.extend((px.width - 1).to_le_bytes().iter().take(3));
        x.extend((px.height - 1).to_le_bytes().iter().take(3));
        chunk(&mut body, b"VP8X", &x);
        chunk(&mut body, b"ICCP", icc);
    }
    chunk(&mut body, b"VP8L", &stream);
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok((out, notes))
}

fn vp8x(alpha: bool, icc: bool, w: u32, h: u32) -> Vec<u8> {
    let flags = if icc { 0x20 } else { 0 } | if alpha { 0x10 } else { 0 };
    let mut x = vec![flags, 0, 0, 0];
    x.extend((w - 1).to_le_bytes().iter().take(3));
    x.extend((h - 1).to_le_bytes().iter().take(3));
    x
}

/// Lossy WebP: a VP8 key frame, with the alpha channel (if any) in a separate lossless chunk.
fn encode_lossy(
    img: &Image,
    mut px: Image,
    alpha: bool,
    opts: &EncodeOptions,
    quality: f32,
    mut notes: Notes,
) -> Result<(Vec<u8>, Notes)> {
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
    notes.push(oc_core::Note::new(
        oc_core::Severity::Info,
        "image.lossy_compression",
        format!("lossy WebP at quality {quality}"),
    ));
    let mut body = Vec::new();
    body.extend_from_slice(b"WEBP");
    let icc = img.metadata.icc_profile.as_deref();
    if icc.is_some() || alpha {
        chunk(
            &mut body,
            b"VP8X",
            &vp8x(alpha, icc.is_some(), px.width, px.height),
        );
    }
    if let Some(icc) = icc {
        chunk(&mut body, b"ICCP", icc);
    }
    if let Some(plane) = &plane {
        chunk(
            &mut body,
            b"ALPH",
            &lossy::encode_alpha(plane, w, h, opts.effort),
        );
    }
    chunk(&mut body, b"VP8 ", &vp8);
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok((out, notes))
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
    fn animation_is_named_and_junk_is_rejected() {
        // VP8X with the animation flag.
        let mut anim = b"RIFF\x16\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        anim.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let e = decode(&anim, &Limits::default(), &NoProgress).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnsupportedVariant);
        // A lossy chunk that is not a key frame.
        let mut junk = b"RIFF\x16\0\0\0WEBPVP8 \x0a\0\0\0".to_vec();
        junk.extend_from_slice(&[0; 10]);
        assert!(decode(&junk, &Limits::default(), &NoProgress).is_err());
        assert!(sniff(&junk));
        assert!(!sniff(b"RIFF....WAVE"));
    }
}
