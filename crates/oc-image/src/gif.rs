//! GIF 87a / 89a.
//!
//! Read: the first image of the file, drawn on a transparent logical screen; interlacing,
//! local and global colour tables and the transparency index are honoured. Further frames
//! are not decoded and a note says so (the image model has no frames yet).
//! Write: a single frame with an exact palette when the image has at most 256 colours,
//! otherwise median-cut quantisation with optional dithering; alpha is on/off.
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

fn skip_sub_blocks(r: &mut ByteReader<'_>) -> Result<()> {
    loop {
        let n = usize::from(r.u8()?);
        if n == 0 {
            return Ok(());
        }
        r.skip(n)?;
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

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
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

    let mut transparent: Option<u8> = None;
    let mut frame: Option<Image> = None;
    while let Ok(block) = r.u8() {
        match block {
            0x3B => break,
            0x21 => {
                let label = r.u8()?;
                if label == 0xF9 {
                    let body = read_sub_blocks(&mut r)?;
                    if let &[flags, _, _, index, ..] = body.as_slice() {
                        transparent = (flags & 1 != 0).then_some(index);
                    }
                } else {
                    skip_sub_blocks(&mut r)?;
                }
            }
            0x2C => {
                if frame.is_some() {
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
                let (cw, ch) = if screen_w == 0 || screen_h == 0 {
                    // Some writers leave the screen size at 0; fall back to the frame.
                    (fw as u32, fh as u32)
                } else {
                    (screen_w, screen_h)
                };
                limits.check_pixels(cw, ch)?;
                limits.check_alloc(u64::from(cw) * u64::from(ch) * 4)?;
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
                let mut rgba = vec![0u8; cw * ch * 4];
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
                        let a = if transparent == Some(idx) { 0 } else { 255 };
                        if let Some(d) = rgba.get_mut((py * cw + px) * 4..(py * cw + px) * 4 + 4) {
                            d.copy_from_slice(&[cr, cg, cb, a]);
                        }
                    }
                }
                let img = Image::new(cw as u32, ch as u32, Layout::Rgba, Depth::Eight, rgba)?;
                frame = Some(img);
            }
            _ => return Err(Error::corrupt("GIF has an unknown block")),
        }
    }
    let img = frame.ok_or_else(|| Error::corrupt("GIF has no image"))?;
    let target = if img.has_translucency() {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let (img, _) = ops::convert_layout(&img, target, [0, 0, 0]);
    Ok((img, notes))
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
}
