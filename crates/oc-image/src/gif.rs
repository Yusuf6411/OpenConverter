//! GIF 87a / 89a.
//!
//! Read: the first image of the file, drawn on a transparent logical screen; interlacing,
//! local and global colour tables and the transparency index are honoured. Further frames
//! are not decoded and a note says so (the image model has no frames yet).
//! Write: a single frame with an exact palette when the image has at most 256 colours,
//! otherwise median-cut quantisation with optional dithering; alpha is on/off.
use crate::anim::{self, Animation};
use crate::image::{Depth, Image, Layout};
use crate::ops;
use crate::quant;
use oc_compress::lzw;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};
use oc_io::ByteReader;

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")
}

/// Concatenate the data sub-blocks that start at the reader's position.
fn read_sub_blocks(r: &mut ByteReader<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let n = usize::from(r.u8()?);
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(r.take(n)?);
    }
}

fn read_table(r: &mut ByteReader<'_>, size_bits: u8) -> Result<Vec<[u8; 3]>> {
    let n = 1usize << (usize::from(size_bits & 7) + 1);
    let raw = r.take(n * 3)?;
    Ok(raw
        .as_chunks::<3>()
        .0
        .iter()
        .map(|&[r, g, b]| [r, g, b])
        .collect())
}

/// Source row for the n-th stored row of an interlaced image.
fn interlaced_row(n: usize, height: usize) -> usize {
    let mut seen = 0;
    for (start, step) in [(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
        let rows = if height > start {
            (height - start).div_ceil(step)
        } else {
            0
        };
        if n < seen + rows {
            return start + (n - seen) * step;
        }
        seen += rows;
    }
    n
}

/// Walks the file and returns the picture as shown after each frame. With `first_only` it
/// stops at the first frame and notes that more follow.
fn walk(data: &[u8], limits: &Limits, first_only: bool, notes: &mut Notes) -> Result<Animation> {
    let mut r = ByteReader::new(data);
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a GIF file"));
    }
    r.skip(6)?;
    let screen_w = u32::from(r.u16_le()?);
    let screen_h = u32::from(r.u16_le()?);
    let packed = r.u8()?;
    r.skip(2)?; // background colour index, pixel aspect ratio
    let global = if packed & 0x80 != 0 {
        Some(read_table(&mut r, packed)?)
    } else {
        None
    };

    // Graphic control extension state; it applies to the next image only.
    let (mut transparent, mut disposal, mut delay_ms): (Option<u8>, u8, u32) = (None, 0, 0);
    let mut loops = 1;
    let mut anim = Animation::new(screen_w, screen_h);
    let mut canvas: Vec<u8> = Vec::new();
    // What the previous frame asks to happen before the next one is drawn.
    let mut pending: Option<(u8, anim::Rect, Option<Vec<u8>>)> = None;
    while let Ok(block) = r.u8() {
        match block {
            0x3B => break,
            0x21 => {
                let label = r.u8()?;
                let body = read_sub_blocks(&mut r)?;
                match (label, body.as_slice()) {
                    (0xF9, &[flags, d0, d1, index, ..]) => {
                        transparent = (flags & 1 != 0).then_some(index);
                        disposal = (flags >> 2) & 7;
                        delay_ms = u32::from(u16::from_le_bytes([d0, d1])) * 10;
                    }
                    // The identifier and its data sub-block arrive joined.
                    (0xFF, b) if b.starts_with(b"NETSCAPE2.0") => {
                        if let Some(&[1, lo, hi]) =
                            b.get(11..14).and_then(|x| <&[u8; 3]>::try_from(x).ok())
                        {
                            let n = u32::from(u16::from_le_bytes([lo, hi]));
                            loops = if n == 0 { 0 } else { n + 1 };
                        }
                    }
                    _ => {}
                }
            }
            0x2C => {
                if first_only && !anim.frames.is_empty() {
                    notes.push(Note::new(
                        Severity::Warning,
                        "gif.animation_first_frame",
                        "the file has more than one frame; only the first was read",
                    ));
                    break;
                }
                let left = usize::from(r.u16_le()?);
                let top = usize::from(r.u16_le()?);
                let fw = usize::from(r.u16_le()?);
                let fh = usize::from(r.u16_le()?);
                let flags = r.u8()?;
                let local = if flags & 0x80 != 0 {
                    Some(read_table(&mut r, flags)?)
                } else {
                    None
                };
                let table = local
                    .as_ref()
                    .or(global.as_ref())
                    .ok_or_else(|| Error::corrupt("GIF frame has no colour table"))?;
                let min_code = u32::from(r.u8()?);
                if !(1..=8).contains(&min_code) {
                    return Err(Error::corrupt("GIF has an invalid LZW code size"));
                }
                if canvas.is_empty() {
                    // Some writers leave the screen size at 0; fall back to the frame.
                    if screen_w == 0 || screen_h == 0 {
                        anim.width = fw as u32;
                        anim.height = fh as u32;
                    }
                }
                let (cw, ch) = (anim.width, anim.height);
                anim::check_frame(limits, cw, ch, anim.frames.len())?;
                let compressed = read_sub_blocks(&mut r)?;
                let mut lim = limits.clone();
                lim.max_output = lim.max_output.min((fw * fh) as u64 + 4096);
                // The decoder is bounded by the frame size; a ratio check would reject solid
                // colours, which compress by orders of magnitude.
                lim.max_ratio = u64::MAX;
                let indices = lzw::decode(&compressed, lzw::Config::gif(min_code.max(2)), &lim)?;
                if indices.len() < fw * fh {
                    return Err(Error::truncated("GIF pixel data"));
                }
                let (cw, ch) = (cw as usize, ch as usize);
                if canvas.is_empty() {
                    canvas = anim::blank_canvas(cw as u32, ch as u32);
                }
                // Dispose of the previous frame.
                if let Some((how, (px, py, pw, ph), saved)) = pending.take() {
                    match (how, saved) {
                        (2, _) => {
                            for y in py..(py + ph).min(ch) {
                                let row = (y * cw + px) * 4..(y * cw + (px + pw).min(cw)) * 4;
                                if let Some(d) = canvas.get_mut(row) {
                                    d.fill(0);
                                }
                            }
                        }
                        (3, Some(saved)) => canvas = saved,
                        _ => {}
                    }
                }
                let saved = (disposal == 3).then(|| canvas.clone());
                for n in 0..fh {
                    let row = if flags & 0x40 != 0 {
                        interlaced_row(n, fh)
                    } else {
                        n
                    };
                    for x in 0..fw {
                        let (px, py) = (left + x, top + row);
                        if px >= cw || py >= ch {
                            continue;
                        }
                        let idx = indices.get(n * fw + x).copied().unwrap_or(0);
                        let [cr, cg, cb] = table.get(usize::from(idx)).copied().unwrap_or([0; 3]);
                        let clear = transparent == Some(idx);
                        if let Some(d) = canvas.get_mut((py * cw + px) * 4..(py * cw + px) * 4 + 4)
                        {
                            // A transparent index leaves the canvas alone; over empty pixels
                            // it still records the palette colour, as other decoders do.
                            if !clear {
                                d.copy_from_slice(&[cr, cg, cb, 255]);
                            } else if d.get(3) == Some(&0) {
                                d.copy_from_slice(&[cr, cg, cb, 0]);
                            }
                        }
                    }
                }
                let image = anim::canvas_image(cw as u32, ch as u32, canvas.clone())?;
                anim.frames.push(anim::Frame { image, delay_ms });
                pending = Some((disposal, (left, top, fw, fh), saved));
                transparent = None;
                disposal = 0;
                delay_ms = 0;
            }
            _ => return Err(Error::corrupt("GIF has an unknown block")),
        }
    }
    if anim.frames.is_empty() {
        return Err(Error::corrupt("GIF has no image"));
    }
    anim.loops = loops;
    Ok(anim)
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let anim = walk(data, limits, true, &mut notes)?;
    let img = anim
        .first_image()
        .ok_or_else(|| Error::corrupt("GIF has no image"))?;
    let target = if img.has_translucency() {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let (img, _) = ops::convert_layout(&img, target, [0, 0, 0]);
    Ok((img, notes))
}

/// Every frame of the file, composited onto the logical screen.
pub fn decode_animation(data: &[u8], limits: &Limits) -> Result<(Animation, Notes)> {
    let mut notes = Notes::new();
    let anim = walk(data, limits, false, &mut notes)?;
    Ok((anim, notes))
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    /// Dither when the image has more than 256 colours.
    pub dither: bool,
    /// Write rows in the four-pass interlaced order.
    pub interlace: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            dither: true,
            interlace: false,
        }
    }
}

fn sub_blocks(out: &mut Vec<u8>, data: &[u8]) {
    for chunk in data.chunks(255) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0);
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    let (rgba, n) = ops::convert_layout(&e8, Layout::Rgba, [255, 255, 255]);
    notes.extend(n);
    let (w, h) = (rgba.width as usize, rgba.height as usize);
    if w > 0xFFFF || h > 0xFFFF {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "GIF images are at most 65535 pixels on a side",
        ));
    }
    let q = quant::quantize(&rgba.data, w, h, opts.dither);
    if !q.exact {
        notes.push(Note::new(
            Severity::Loss,
            "gif.quantized",
            "the image has more than 256 colours and was reduced to a 256-colour palette",
        ));
    }
    if q.partial_alpha {
        notes.push(Note::new(
            Severity::Loss,
            "gif.alpha_binary",
            "GIF transparency is on or off; partly transparent pixels were rounded",
        ));
    }
    let entries = q.palette.len() + usize::from(q.transparent.is_some());
    let bits = (usize::BITS - entries.saturating_sub(1).leading_zeros()).clamp(1, 8);
    let table_size = 1usize << bits;

    let mut out = Vec::new();
    out.extend_from_slice(if q.transparent.is_some() {
        b"GIF89a"
    } else {
        b"GIF87a"
    });
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.push(0x80 | ((bits as u8 - 1) << 4) | (bits as u8 - 1));
    out.extend_from_slice(&[0, 0]);
    for i in 0..table_size {
        out.extend_from_slice(&q.palette.get(i).copied().unwrap_or([0, 0, 0]));
    }
    if let Some(t) = q.transparent {
        out.extend_from_slice(&[0x21, 0xF9, 4, 1, 0, 0, t, 0]);
    }
    out.push(0x2C);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.push(if opts.interlace { 0x40 } else { 0 });
    let min_code = bits.max(2);
    out.push(min_code as u8);
    let ordered: Vec<u8> = if opts.interlace {
        (0..h)
            .flat_map(|n| {
                let row = interlaced_row(n, h);
                q.indices
                    .get(row * w..(row + 1) * w)
                    .unwrap_or(&[])
                    .to_vec()
            })
            .collect()
    } else {
        q.indices
    };
    let packed = lzw::encode(&ordered, lzw::Config::gif(min_code))?;
    sub_blocks(&mut out, &packed);
    out.push(0x3B);
    Ok((out, notes))
}

/// Writes an animation. Each frame stores only the rectangle that changed since the previous
/// one, with unchanged pixels left transparent. A frame that must make pixels transparent again
/// is drawn on a cleared canvas, which the frame before it arranges with disposal method 2.
pub fn encode_animation(anim: &Animation, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let (w, h) = (anim.width as usize, anim.height as usize);
    if w == 0 || h == 0 || w > 0xFFFF || h > 0xFFFF {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "GIF images are at most 65535 pixels on a side",
        ));
    }
    // Binary alpha, with fully transparent pixels all alike so that equality means "same".
    let mut partial = false;
    let mut shown: Vec<Vec<u8>> = Vec::with_capacity(anim.frames.len());
    for f in &anim.frames {
        let (e8, _) = ops::convert_depth(&f.image, Depth::Eight);
        let (rgba, _) = ops::convert_layout(&e8, Layout::Rgba, [255, 255, 255]);
        if rgba.width as usize != w || rgba.height as usize != h {
            return Err(Error::new(
                ErrorCode::InvalidOptions,
                "animation frames differ in size",
            ));
        }
        let mut d = rgba.data;
        for p in d.as_chunks_mut::<4>().0 {
            partial |= p[3] != 0 && p[3] != 255;
            if p[3] < 128 {
                *p = [0; 4];
            } else {
                p[3] = 255;
            }
        }
        shown.push(d);
    }
    if partial {
        notes.push(Note::new(
            Severity::Loss,
            "gif.alpha_binary",
            "GIF transparency is on or off; partly transparent pixels were rounded",
        ));
    }
    let blank = anim::blank_canvas(anim.width, anim.height);
    let erases: Vec<bool> = (0..shown.len())
        .map(|i| {
            let (Some(cur), Some(prev)) =
                (shown.get(i), i.checked_sub(1).and_then(|j| shown.get(j)))
            else {
                return false;
            };
            prev.as_chunks::<4>()
                .0
                .iter()
                .zip(cur.as_chunks::<4>().0)
                .any(|(p, c)| p[3] != 0 && c[3] == 0)
        })
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(b"GIF89a");
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0]);
    if anim.loops != 1 {
        let n = if anim.loops == 0 {
            0
        } else {
            (anim.loops - 1).min(0xFFFF) as u16
        };
        out.extend_from_slice(&[0x21, 0xFF, 11]);
        out.extend_from_slice(b"NETSCAPE2.0");
        out.extend_from_slice(&[3, 1]);
        out.extend_from_slice(&n.to_le_bytes());
        out.push(0);
    }
    let mut lossy = false;
    for (i, cur) in shown.iter().enumerate() {
        let erase_next = erases.get(i + 1).copied().unwrap_or(false);
        let base = if erases.get(i).copied().unwrap_or(false) || i == 0 {
            &blank
        } else {
            shown.get(i - 1).unwrap_or(&blank)
        };
        let rect = if erase_next {
            Some((0, 0, w, h))
        } else {
            anim::changed_rect(base, cur, w, h)
        };
        let (rx, ry, rw, rh) = rect.unwrap_or((0, 0, 1, 1));
        // Unchanged pixels become transparent so they cost nothing and keep the canvas.
        let mut px = Vec::with_capacity(rw * rh * 4);
        for y in ry..ry + rh {
            for x in rx..rx + rw {
                let k = (y * w + x) * 4;
                let (c, b) = (
                    cur.get(k..k + 4).unwrap_or(&[0; 4]),
                    base.get(k..k + 4).unwrap_or(&[0; 4]),
                );
                if c == b {
                    px.extend_from_slice(&[0, 0, 0, 0]);
                } else {
                    px.extend_from_slice(c);
                }
            }
        }
        let q = quant::quantize(&px, rw, rh, opts.dither);
        lossy |= !q.exact;
        let entries = q.palette.len() + usize::from(q.transparent.is_some());
        let bits = (usize::BITS - entries.saturating_sub(1).leading_zeros()).clamp(1, 8);
        let disposal: u8 = if erase_next { 2 } else { 1 };
        let delay = ((anim.frames.get(i).map_or(0, |f| f.delay_ms) + 5) / 10).min(0xFFFF) as u16;
        out.extend_from_slice(&[
            0x21,
            0xF9,
            4,
            (disposal << 2) | u8::from(q.transparent.is_some()),
        ]);
        out.extend_from_slice(&delay.to_le_bytes());
        out.extend_from_slice(&[q.transparent.unwrap_or(0), 0]);
        out.push(0x2C);
        for v in [rx, ry, rw, rh] {
            out.extend_from_slice(&(v as u16).to_le_bytes());
        }
        out.push(0x80 | (bits as u8 - 1));
        for k in 0..1usize << bits {
            out.extend_from_slice(&q.palette.get(k).copied().unwrap_or([0, 0, 0]));
        }
        let min_code = bits.max(2);
        out.push(min_code as u8);
        let packed = lzw::encode(&q.indices, lzw::Config::gif(min_code))?;
        sub_blocks(&mut out, &packed);
    }
    out.push(0x3B);
    if lossy {
        notes.push(Note::new(
            Severity::Loss,
            "gif.quantized",
            "some frames have more than 256 colours and were reduced to a 256-colour palette",
        ));
    }
    Ok((out, notes))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rgba_of(img: &Image) -> Vec<u8> {
        ops::convert_layout(img, Layout::Rgba, [0, 0, 0]).0.data
    }

    #[test]
    fn pillow_variants() {
        let dir = format!("{}/tests/data/gif", env!("CARGO_MANIFEST_DIR"));
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|x| x.to_str()) != Some("gif") {
                continue;
            }
            let (img, _) = decode(&std::fs::read(&p).unwrap(), &Limits::default()).unwrap();
            let want = std::fs::read(p.with_extension("rgba")).unwrap();
            assert_eq!(rgba_of(&img), want, "{}", p.display());
            n += 1;
        }
        assert!(n >= 8, "expected the generated fixtures, found {n}");
    }

    #[test]
    fn interlace_order_is_a_permutation() {
        for h in [1usize, 2, 3, 5, 8, 9, 17, 100] {
            let mut rows: Vec<usize> = (0..h).map(|n| interlaced_row(n, h)).collect();
            rows.sort_unstable();
            assert_eq!(rows, (0..h).collect::<Vec<_>>(), "h={h}");
        }
    }

    fn picture(colours: u32) -> Image {
        let mut d = Vec::new();
        for y in 0..30u32 {
            for x in 0..40u32 {
                let k = (x + y * 3) % colours;
                let a = if x < 5 && y < 5 { 0 } else { 255 };
                d.extend_from_slice(&[(k * 37) as u8, ((k * 91) >> 1) as u8, (k * 11) as u8, a]);
            }
        }
        Image::new(40, 30, Layout::Rgba, Depth::Eight, d).unwrap()
    }

    #[test]
    fn exact_palettes_roundtrip_including_transparency() {
        let img = picture(20);
        for interlace in [false, true] {
            let (bytes, notes) = encode(
                &img,
                &EncodeOptions {
                    interlace,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(!notes.has_loss());
            let (back, _) = decode(&bytes, &Limits::default()).unwrap();
            let (a, b) = (rgba_of(&back), rgba_of(&img));
            for (x, y) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
                assert_eq!(x[3], y[3]);
                if y[3] != 0 {
                    assert_eq!(x, y);
                }
            }
        }
    }

    #[test]
    fn many_colours_are_quantised_with_a_loss_note() {
        let mut d = Vec::new();
        for y in 0..30u32 {
            for x in 0..40u32 {
                d.extend_from_slice(&[(x * 6) as u8, (y * 8) as u8, ((x + y) * 3) as u8]);
            }
        }
        let img = Image::new(40, 30, Layout::Rgb, Depth::Eight, d).unwrap();
        let (bytes, notes) = encode(&img, &EncodeOptions::default()).unwrap();
        assert!(notes.iter().any(|n| n.code == "gif.quantized"));
        let (back, _) = decode(&bytes, &Limits::default()).unwrap();
        assert_eq!((back.width, back.height), (40, 30));
    }

    #[test]
    fn solid_large_image_is_not_mistaken_for_a_bomb() {
        let img = Image::new(
            1000,
            1000,
            Layout::Rgb,
            Depth::Eight,
            vec![7; 1000 * 1000 * 3],
        )
        .unwrap();
        let (bytes, _) = encode(&img, &EncodeOptions::default()).unwrap();
        assert!(bytes.len() < 30_000);
        let (back, _) = decode(&bytes, &Limits::default()).unwrap();
        assert_eq!(back.data, img.data);
    }

    #[test]
    fn corrupt_input_errors() {
        let (good, _) = encode(&picture(20), &EncodeOptions::default()).unwrap();
        for cut in [0, 5, 12, 30, good.len() / 2, good.len() - 3] {
            assert!(
                decode(&good[..cut], &Limits::default()).is_err(),
                "cut {cut}"
            );
        }
        assert!(decode(b"GIF89a", &Limits::default()).is_err());
        let tight = Limits {
            max_pixels: 10,
            ..Limits::default()
        };
        assert!(decode(&good, &tight).is_err());
    }

    fn scene(i: u32, clear_bg: bool) -> Image {
        let mut d = Vec::new();
        for y in 0..24u32 {
            for x in 0..32u32 {
                let in_box = x >= 2 + i * 5 && x < 10 + i * 5 && (4..13).contains(&y);
                let px = if in_box {
                    [200, 30 + 40 * i as u8, 60, 255]
                } else if clear_bg {
                    [0, 0, 0, 0]
                } else {
                    [255, 255, 255, 255]
                };
                d.extend_from_slice(&px);
            }
        }
        Image::new(32, 24, Layout::Rgba, Depth::Eight, d).unwrap()
    }

    #[test]
    fn animations_roundtrip_with_delays_loops_and_erasing() {
        for clear_bg in [false, true] {
            let mut a = Animation::new(32, 24);
            a.loops = 3;
            for i in 0..5 {
                let image = scene(i, clear_bg);
                a.frames.push(anim::Frame {
                    image,
                    delay_ms: 40 * (i + 1),
                });
            }
            let (bytes, notes) = encode_animation(&a, &EncodeOptions::default()).unwrap();
            assert!(!notes.has_loss());
            let (back, _) = decode_animation(&bytes, &Limits::default()).unwrap();
            assert_eq!((back.loops, back.frames.len()), (3, 5));
            for (x, y) in back.frames.iter().zip(&a.frames) {
                assert_eq!(x.delay_ms, y.delay_ms);
                assert_eq!(x.image.data, y.image.data);
            }
            // Delta frames are smaller than five whole pictures.
            let (still, _) = encode(&scene(0, clear_bg), &EncodeOptions::default()).unwrap();
            if !clear_bg {
                assert!(
                    bytes.len() < still.len() * 4,
                    "{} vs {}",
                    bytes.len(),
                    still.len()
                );
            }
        }
    }

    #[test]
    fn first_frame_only_is_noted() {
        let mut a = Animation::new(32, 24);
        for i in 0..2 {
            a.frames.push(anim::Frame {
                image: scene(i, false),
                delay_ms: 100,
            });
        }
        let (bytes, _) = encode_animation(&a, &EncodeOptions::default()).unwrap();
        let (img, notes) = decode(&bytes, &Limits::default()).unwrap();
        assert!(notes.iter().any(|n| n.code == "gif.animation_first_frame"));
        assert_eq!(rgba_of(&img), scene(0, false).data);
    }
}
