//! Apple icon images (ICNS).
//!
//! Read: the largest image in the file, whether it is stored as PNG (`icp4`-`icp6`,
//! `ic07`-`ic14`) or in the older RLE-compressed RGB form (`is32`, `il32`, `ih32`, `it32`
//! with their 8-bit masks). Icons stored as JPEG 2000 and the palette-based 4 and 8-bit
//! forms are named as unsupported. Write: PNG-compressed entries at the standard sizes.
use crate::image::{Depth, Image, Layout};
use crate::ops;
use crate::png;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(b"icns")
}

/// `(type, edge in pixels)` of the entries that hold a PNG or JPEG 2000 stream.
const MODERN: [(&[u8; 4], u32); 11] = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"icp6", 64),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
    (b"ic11", 32),
    (b"ic12", 64),
    (b"ic13", 256),
    (b"ic14", 512),
];

/// RLE-compressed RGB entries, their mask entries and their edge.
const LEGACY: [(&[u8; 4], &[u8; 4], u32); 4] = [
    (b"is32", b"s8mk", 16),
    (b"il32", b"l8mk", 32),
    (b"ih32", b"h8mk", 48),
    (b"it32", b"t8mk", 128),
];

struct Element<'a> {
    kind: [u8; 4],
    data: &'a [u8],
}

fn be32(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(i)?,
        *d.get(i + 1)?,
        *d.get(i + 2)?,
        *d.get(i + 3)?,
    ]))
}

fn elements<'a>(data: &'a [u8], limits: &Limits) -> Result<Vec<Element<'a>>> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not an ICNS file"));
    }
    let total = be32(data, 4).ok_or_else(|| Error::truncated("ICNS header"))? as usize;
    let end = total.clamp(8, data.len());
    let mut pos = 8;
    let mut out = Vec::new();
    while pos + 8 <= end {
        let kind = data
            .get(pos..pos + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .ok_or_else(|| Error::truncated("ICNS element"))?;
        let len = be32(data, pos + 4).ok_or_else(|| Error::truncated("ICNS element"))? as usize;
        if len < 8 {
            return Err(Error::corrupt("ICNS element is shorter than its header").at(pos as u64));
        }
        let body = data
            .get(pos + 8..pos + len)
            .ok_or_else(|| Error::truncated("ICNS element data").at(pos as u64))?;
        limits.check_entries(out.len() as u64 + 1)?;
        out.push(Element { kind, data: body });
        pos += len;
    }
    Ok(out)
}

/// Undoes the ICNS run-length coding of one colour plane of `n` bytes.
fn unpack_plane(src: &[u8], pos: &mut usize, n: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let b = *src
            .get(*pos)
            .ok_or_else(|| Error::truncated("ICNS pixel data"))?;
        *pos += 1;
        if b & 0x80 != 0 {
            let run = usize::from(b) - 125;
            let v = *src
                .get(*pos)
                .ok_or_else(|| Error::truncated("ICNS pixel data"))?;
            *pos += 1;
            out.resize((out.len() + run).min(n), v);
        } else {
            let run = usize::from(b) + 1;
            let lit = src
                .get(*pos..*pos + run)
                .ok_or_else(|| Error::truncated("ICNS pixel data"))?;
            *pos += run;
            out.extend_from_slice(lit.get(..run.min(n - out.len())).unwrap_or(&[]));
        }
    }
    Ok(out)
}

fn decode_legacy(rgb: &[u8], mask: Option<&[u8]>, edge: u32) -> Result<Image> {
    let n = (edge * edge) as usize;
    // The 128-pixel form starts with four zero bytes.
    let body = if edge == 128 && rgb.starts_with(&[0; 4]) {
        rgb.get(4..).unwrap_or(&[])
    } else {
        rgb
    };
    let mut pos = 0;
    let mut planes = Vec::with_capacity(n * 3);
    for _ in 0..3 {
        planes.extend(unpack_plane(body, &mut pos, n)?);
    }
    let mut data = Vec::with_capacity(n * 4);
    for i in 0..n {
        let c = |k: usize| planes.get(k * n + i).copied().unwrap_or(0);
        let a = match mask {
            Some(m) => m.get(i).copied().unwrap_or(255),
            None => 255,
        };
        data.extend_from_slice(&[c(0), c(1), c(2), a]);
    }
    Image::new(edge, edge, Layout::Rgba, Depth::Eight, data)
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let list = elements(data, limits)?;
    // Candidates by edge, largest first; PNG before the legacy form at the same size.
    let mut best: Option<(u32, bool, &Element<'_>)> = None;
    let mut jpeg2000 = false;
    let mut count = 0;
    for e in &list {
        let edge = MODERN
            .iter()
            .find(|(k, _)| **k == e.kind)
            .map(|&(_, s)| (s, true))
            .or_else(|| {
                LEGACY
                    .iter()
                    .find(|(k, _, _)| **k == e.kind)
                    .map(|&(_, _, s)| (s, false))
            });
        let Some((mut edge, modern)) = edge else {
            continue;
        };
        if modern {
            if !png::sniff(e.data) {
                jpeg2000 = true;
                continue;
            }
            // Retina entries hold twice the pixels their name suggests; trust the PNG header.
            edge = be32(e.data, 16).unwrap_or(edge);
        }
        count += 1;
        if best.is_none_or(|(s, m, _)| (edge, modern) > (s, m)) {
            best = Some((edge, modern, e));
        }
    }
    let Some((edge, modern, e)) = best else {
        return Err(if jpeg2000 {
            Error::new(
                ErrorCode::UnsupportedVariant,
                "this ICNS file stores its icons as JPEG 2000, which is not supported",
            )
        } else {
            Error::new(
                ErrorCode::UnsupportedVariant,
                "this ICNS file has no PNG or RGB icon images",
            )
        });
    };
    let img = if modern {
        let (img, n) = png::decode(e.data, limits, progress)?;
        notes.extend(n);
        img
    } else {
        limits.check_pixels(edge, edge)?;
        let mask_kind = LEGACY
            .iter()
            .find(|(k, _, _)| **k == e.kind)
            .map(|&(_, m, _)| m);
        let mask = mask_kind
            .and_then(|m| list.iter().find(|x| x.kind == *m))
            .map(|x| x.data);
        decode_legacy(e.data, mask, edge)?
    };
    if count > 1 {
        notes.push(Note::new(
            Severity::Info,
            "icns.largest_image",
            format!(
                "the file holds {count} images; the largest ({}x{}) was read",
                img.width, img.height
            ),
        ));
    }
    Ok((img, notes))
}

#[derive(Debug, Clone, Default)]
pub struct EncodeOptions {
    /// Square sizes to write, each one of 16, 32, 64, 128, 256, 512, 1024. Empty picks the
    /// standard sizes that fit inside the image.
    pub sizes: Vec<u32>,
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    let mut sizes = opts.sizes.clone();
    if sizes.is_empty() {
        let longest = e8.width.max(e8.height);
        sizes = MODERN
            .iter()
            .take(7)
            .map(|&(_, s)| s)
            .filter(|&s| s <= longest)
            .collect();
        if sizes.is_empty() {
            sizes.push(16);
        }
    }
    sizes.sort_unstable();
    sizes.dedup();
    let mut body = Vec::new();
    for s in sizes {
        let Some(&(kind, _)) = MODERN.iter().take(7).find(|(_, e)| *e == s) else {
            return Err(Error::invalid_option(
                "sizes",
                "ICNS sizes are 16, 32, 64, 128, 256, 512 and 1024",
            ));
        };
        let square = crate::ico::fit_square(&e8, s);
        let (png, _) = png::encode(&square, &png::EncodeOptions::default());
        body.extend_from_slice(kind);
        body.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(&png);
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&body);
    Ok((out, notes))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn picture(w: u32, h: u32) -> Image {
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                d.extend_from_slice(&[
                    (x * 7) as u8,
                    (y * 5) as u8,
                    90,
                    if x < 3 { 0 } else { 255 },
                ]);
            }
        }
        Image::new(w, h, Layout::Rgba, Depth::Eight, d).unwrap()
    }

    #[test]
    fn png_entries_roundtrip_and_the_largest_is_read() {
        let img = picture(300, 300);
        let (bytes, _) = encode(&img, &EncodeOptions::default()).unwrap();
        assert!(sniff(&bytes));
        let list = elements(&bytes, &Limits::default()).unwrap();
        let kinds: Vec<_> = list.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [*b"icp4", *b"icp5", *b"icp6", *b"ic07", *b"ic08"]);
        let (back, notes) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        assert_eq!((back.width, back.height), (256, 256));
        assert!(notes.iter().any(|n| n.code == "icns.largest_image"));
        let small = encode(&picture(40, 20), &EncodeOptions { sizes: vec![64] })
            .unwrap()
            .0;
        let (back, _) = decode(&small, &Limits::default(), &NoProgress).unwrap();
        assert_eq!((back.width, back.height), (64, 64));
        assert!(encode(&img, &EncodeOptions { sizes: vec![48] }).is_err());
    }

    #[test]
    fn rle_planes_decode() {
        // A run byte b >= 0x80 repeats the next byte b - 125 times, so 0xFF is 130 copies;
        // two of them cover the 256 pixels of a 16x16 plane.
        let mut data = Vec::new();
        for v in [10u8, 20, 30] {
            data.extend_from_slice(&[0xFF, v, 0xFF, v]);
        }
        let mask = vec![128u8; 256];
        let img = decode_legacy(&data, Some(&mask), 16).unwrap();
        assert_eq!(&img.data[..4], &[10, 20, 30, 128]);
        assert_eq!(&img.data[img.data.len() - 4..], &[10, 20, 30, 128]);
        // A literal byte b < 0x80 copies the next b + 1 bytes.
        let mut lit = Vec::new();
        for v in 0..3u8 {
            for chunk in 0..2u8 {
                lit.push(127);
                lit.extend((0..128u8).map(|i| v * 50 + chunk * 10 + i / 16));
            }
        }
        let img = decode_legacy(&lit, None, 16).unwrap();
        assert_eq!(&img.data[..4], &[0, 50, 100, 255]);
        assert_eq!(&img.data[4..8], &[0, 50, 100, 255]);
        assert_eq!(&img.data[8 * 4..8 * 4 + 4], &[0, 50, 100, 255]);
        assert_eq!(&img.data[128 * 4..128 * 4 + 4], &[10, 60, 110, 255]);
        assert!(decode_legacy(&lit[..lit.len() - 5], None, 16).is_err());
    }

    #[test]
    fn jpeg2000_and_junk_are_named_or_rejected() {
        let mut f = b"icns".to_vec();
        let jp2 = b"\0\0\0\x0cjP  \r\n\x87\n";
        f.extend_from_slice(&(8u32 + 8 + jp2.len() as u32).to_be_bytes());
        f.extend_from_slice(b"ic08");
        f.extend_from_slice(&(8 + jp2.len() as u32).to_be_bytes());
        f.extend_from_slice(jp2);
        let e = decode(&f, &Limits::default(), &NoProgress).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnsupportedVariant);
        assert!(e.to_string().contains("JPEG 2000"));
        assert!(decode(
            b"icns\0\0\0\x10ic08\0\0\0\x03",
            &Limits::default(),
            &NoProgress
        )
        .is_err());
        assert!(decode(b"nope", &Limits::default(), &NoProgress).is_err());
        let (good, _) = encode(&picture(32, 32), &EncodeOptions::default()).unwrap();
        for cut in (0..good.len()).step_by(11) {
            let _ = decode(&good[..cut], &Limits::default(), &NoProgress);
        }
    }
}
