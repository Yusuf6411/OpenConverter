//! Radiance HDR (`.hdr`, `.pic`): RGBE and XYZE pixels, flat, old and new run-length coding.
//!
//! The format holds linear radiance with a shared exponent. [`Image`] has no floating
//! point depth, so reading clips to the displayable range and encodes sRGB, and writing
//! takes sRGB samples back to linear radiance. Both directions say so in the notes.
// Scanline buffers are `len` long and every index is checked against `len` first; the
// four-byte pixel reads come from a slice taken with `get(pos..pos + 4)`.
#![allow(clippy::indexing_slicing)]
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

const MIN_RLE: usize = 8;
const MAX_RLE: usize = 32767;

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(b"#?RADIANCE") || data.starts_with(b"#?RGBE")
}

#[derive(Clone, Copy, PartialEq)]
enum Primaries {
    Rgb,
    Xyz,
}

/// Linear to sRGB code values, indexed by the linear value in 1/4095 steps.
pub(crate) fn srgb_table() -> Vec<u8> {
    (0..=4095u32)
        .map(|i| {
            let v = f64::from(i) / 4095.0;
            let s = if v <= 0.003_130_8 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (s * 255.0 + 0.5) as u8
        })
        .collect()
}

pub(crate) fn srgb_to_linear(s: f64) -> f64 {
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn to_code(table: &[u8], v: f32) -> u8 {
    // NaN fails both comparisons and lands on 0.
    let v = if v >= 1.0 {
        1.0
    } else if v > 0.0 {
        v
    } else {
        0.0
    };
    table
        .get((v * 4095.0 + 0.5) as usize)
        .copied()
        .unwrap_or(255)
}

/// What the header says: pixel encoding and the scanline geometry.
struct Header {
    primaries: Primaries,
    /// Number of scanlines and length of each, as written.
    scans: usize,
    len: usize,
    /// First axis is Y (scanlines are rows).
    rows: bool,
    /// Scanlines run from the top (Y) or left (X) when true.
    forward_scan: bool,
    /// Pixels in a scanline run left to right (X) or top to bottom (Y) when true.
    forward_pixel: bool,
    start: usize,
}

fn line(data: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let rest = data.get(at..)?;
    let n = rest.iter().position(|&b| b == b'\n')?;
    Some((rest.get(..n)?, at + n + 1))
}

fn parse_header(data: &[u8]) -> Result<Header> {
    if !sniff(data) && !data.starts_with(b"#?") {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "not a Radiance file",
        ));
    }
    let mut at = 0;
    let mut primaries = Primaries::Rgb;
    // The program line, then variables, then a blank line.
    let (_, next) = line(data, at).ok_or_else(|| Error::corrupt("header is cut short"))?;
    at = next;
    loop {
        let (l, next) = line(data, at).ok_or_else(|| Error::corrupt("header is cut short"))?;
        at = next;
        if l.is_empty() {
            break;
        }
        if let Some(f) = l.strip_prefix(b"FORMAT=") {
            match f {
                b"32-bit_rle_rgbe" => primaries = Primaries::Rgb,
                b"32-bit_rle_xyze" => primaries = Primaries::Xyz,
                _ => {
                    return Err(Error::new(
                        ErrorCode::UnsupportedVariant,
                        "unknown Radiance pixel format",
                    ))
                }
            }
        }
        if at > 1 << 16 {
            return Err(Error::corrupt("header is too long"));
        }
    }
    let (res, next) = line(data, at).ok_or_else(|| Error::corrupt("resolution line is missing"))?;
    let text =
        std::str::from_utf8(res).map_err(|_| Error::corrupt("resolution line is not text"))?;
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [a1, n1, a2, n2] = parts[..] else {
        return Err(Error::corrupt("bad resolution line"));
    };
    let n1: usize = n1
        .parse()
        .map_err(|_| Error::corrupt("bad resolution line"))?;
    let n2: usize = n2
        .parse()
        .map_err(|_| Error::corrupt("bad resolution line"))?;
    let axis = |s: &str| -> Result<(u8, bool)> {
        match s {
            "-Y" => Ok((b'Y', true)),
            "+Y" => Ok((b'Y', false)),
            "+X" => Ok((b'X', true)),
            "-X" => Ok((b'X', false)),
            _ => Err(Error::corrupt("bad resolution line")),
        }
    };
    let (ax1, f1) = axis(a1)?;
    let (ax2, f2) = axis(a2)?;
    if ax1 == ax2 {
        return Err(Error::corrupt("bad resolution line"));
    }
    Ok(Header {
        primaries,
        scans: n1,
        len: n2,
        rows: ax1 == b'Y',
        forward_scan: f1,
        forward_pixel: f2,
        start: next,
    })
}

/// Reads one scanline of `len` RGBE quadruples starting at `pos`; returns the new position.
fn read_scan(data: &[u8], mut pos: usize, len: usize, out: &mut [[u8; 4]]) -> Result<usize> {
    let cut = || Error::corrupt("pixel data is cut short").at(0);
    let head = data.get(pos..pos + 4);
    let new_rle = (MIN_RLE..=MAX_RLE).contains(&len)
        && matches!(head, Some(&[2, 2, hi, _]) if hi & 0x80 == 0);
    if new_rle {
        let hi = usize::from(*data.get(pos + 2).ok_or_else(cut)?);
        let lo = usize::from(*data.get(pos + 3).ok_or_else(cut)?);
        if (hi << 8 | lo) != len {
            return Err(Error::corrupt("scanline length disagrees with the header").at(pos as u64));
        }
        pos += 4;
        for c in 0..4 {
            let mut i = 0;
            while i < len {
                let n = usize::from(*data.get(pos).ok_or_else(cut)?);
                pos += 1;
                if n > 128 {
                    let n = n - 128;
                    let v = *data.get(pos).ok_or_else(cut)?;
                    pos += 1;
                    for px in out
                        .get_mut(i..i + n)
                        .ok_or_else(|| Error::corrupt("run passes the end of the scanline"))?
                    {
                        px[c] = v;
                    }
                    i += n;
                } else {
                    if n == 0 {
                        return Err(Error::corrupt("empty literal run").at(pos as u64));
                    }
                    let src = data.get(pos..pos + n).ok_or_else(cut)?;
                    let dst = out
                        .get_mut(i..i + n)
                        .ok_or_else(|| Error::corrupt("run passes the end of the scanline"))?;
                    for (px, &v) in dst.iter_mut().zip(src) {
                        px[c] = v;
                    }
                    pos += n;
                    i += n;
                }
            }
        }
        return Ok(pos);
    }
    // Flat pixels, with the old run marker (1, 1, 1, count).
    let mut i = 0;
    let mut shift = 0u32;
    while i < len {
        let q = data.get(pos..pos + 4).ok_or_else(cut)?;
        pos += 4;
        let px = [q[0], q[1], q[2], q[3]];
        if px[..3] == [1, 1, 1] && i > 0 {
            let n = usize::from(px[3]).checked_shl(shift).unwrap_or(usize::MAX);
            let prev = out[i - 1];
            let dst = out
                .get_mut(i..i.saturating_add(n))
                .ok_or_else(|| Error::corrupt("run passes the end of the scanline"))?;
            dst.fill(prev);
            i += n;
            shift = shift.saturating_add(8);
        } else {
            out[i] = px;
            i += 1;
            shift = 0;
        }
    }
    Ok(pos)
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let h = parse_header(data)?;
    if h.scans == 0 || h.len == 0 {
        return Err(Error::corrupt("zero image size"));
    }
    let (w, ht) = if h.rows {
        (h.len, h.scans)
    } else {
        (h.scans, h.len)
    };
    let (w32, h32) = (
        u32::try_from(w).map_err(|_| Error::corrupt("image is too wide"))?,
        u32::try_from(ht).map_err(|_| Error::corrupt("image is too tall"))?,
    );
    limits.check_pixels(w32, h32)?;
    limits.check_alloc((w * ht * 3) as u64)?;
    let table = srgb_table();
    let mut out = vec![0u8; w * ht * 3];
    let mut scan = vec![[0u8; 4]; h.len];
    let mut pos = h.start;
    for s in 0..h.scans {
        pos = read_scan(data, pos, h.len, &mut scan)?;
        for (i, q) in scan.iter().enumerate() {
            let (x, y) = if h.rows {
                (
                    if h.forward_pixel { i } else { w - 1 - i },
                    if h.forward_scan { s } else { ht - 1 - s },
                )
            } else {
                (
                    if h.forward_scan { s } else { w - 1 - s },
                    if h.forward_pixel { i } else { ht - 1 - i },
                )
            };
            let rgb = if q[3] == 0 {
                [0.0; 3]
            } else {
                let f = 2f64.powi(i32::from(q[3]) - 136);
                let c = [
                    f64::from(q[0]) * f,
                    f64::from(q[1]) * f,
                    f64::from(q[2]) * f,
                ];
                match h.primaries {
                    Primaries::Rgb => [c[0] as f32, c[1] as f32, c[2] as f32],
                    // CIE XYZ to linear Rec. 709 (D65).
                    Primaries::Xyz => [
                        (3.240_97 * c[0] - 1.537_383 * c[1] - 0.498_611 * c[2]) as f32,
                        (-0.969_244 * c[0] + 1.875_968 * c[1] + 0.041_555 * c[2]) as f32,
                        (0.055_63 * c[0] - 0.203_977 * c[1] + 1.056_972 * c[2]) as f32,
                    ],
                }
            };
            let o = (y * w + x) * 3;
            if let Some(dst) = out.get_mut(o..o + 3) {
                for (d, v) in dst.iter_mut().zip(rgb) {
                    *d = to_code(&table, v);
                }
            }
        }
    }
    let mut notes = Notes::new();
    notes.push(Note::new(
        Severity::Info,
        "hdr.tone",
        "Radiance HDR holds linear radiance; values above 1.0 were clipped and the rest encoded as sRGB",
    ));
    Ok((Image::new(w32, h32, Layout::Rgb, Depth::Eight, out)?, notes))
}

/// Shared-exponent pixel from linear red, green and blue.
fn rgbe(c: [f32; 3]) -> [u8; 4] {
    let v = c[0].max(c[1]).max(c[2]);
    if v < 1e-32 {
        return [0; 4];
    }
    // v = f * 2^e with f in [0.5, 1).
    let e = ((v.to_bits() >> 23) & 0xFF) as i32 - 126;
    let scale = 256.0 * 2f32.powi(-e);
    let m = |x: f32| (x * scale + 0.5).clamp(0.0, 255.0) as u8;
    [m(c[0]), m(c[1]), m(c[2]), (e + 128).clamp(0, 255) as u8]
}

fn put_scan(out: &mut Vec<u8>, scan: &[[u8; 4]]) {
    let len = scan.len();
    if !(MIN_RLE..=MAX_RLE).contains(&len) {
        for q in scan {
            out.extend_from_slice(q);
        }
        return;
    }
    out.extend_from_slice(&[2, 2, (len >> 8) as u8, (len & 255) as u8]);
    let mut plane = vec![0u8; len];
    for c in 0..4 {
        for (p, q) in plane.iter_mut().zip(scan) {
            *p = q[c];
        }
        let mut i = 0;
        while i < len {
            let run = plane[i..]
                .iter()
                .take(127)
                .take_while(|&&b| b == plane[i])
                .count();
            if run >= 4 {
                out.extend_from_slice(&[128 + run as u8, plane[i]]);
                i += run;
                continue;
            }
            // Literal bytes until the next run of four or 128 bytes.
            let mut j = i;
            while j < len && j - i < 128 {
                let r = plane[j..]
                    .iter()
                    .take(4)
                    .take_while(|&&b| b == plane[j])
                    .count();
                if r >= 4 {
                    break;
                }
                j += 1;
            }
            out.push((j - i) as u8);
            out.extend_from_slice(&plane[i..j]);
            i = j;
        }
    }
}

pub fn encode(img: &Image) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let (rgb, n) = ops::convert_layout(img, Layout::Rgb, [255, 255, 255]);
    notes.extend(n);
    let w = rgb.width as usize;
    let table: Vec<f32> = match rgb.depth {
        Depth::Eight => (0..256u32)
            .map(|i| srgb_to_linear(f64::from(i) / 255.0) as f32)
            .collect(),
        Depth::Sixteen => (0..65536u32)
            .map(|i| srgb_to_linear(f64::from(i) / 65535.0) as f32)
            .collect(),
    };
    let bytes = rgb.depth.bytes();
    let mut out = Vec::with_capacity(rgb.data.len() / 2 + 64);
    out.extend_from_slice(b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n");
    out.extend_from_slice(format!("-Y {} +X {}\n", rgb.height, rgb.width).as_bytes());
    let mut scan = vec![[0u8; 4]; w];
    for row in rgb.data.chunks_exact(w * 3 * bytes) {
        for (q, px) in scan.iter_mut().zip(row.chunks_exact(3 * bytes)) {
            let mut c = [0f32; 3];
            for (k, v) in c.iter_mut().enumerate() {
                let idx = if bytes == 1 {
                    usize::from(px[k])
                } else {
                    usize::from(u16::from_be_bytes([px[2 * k], px[2 * k + 1]]))
                };
                *v = table[idx];
            }
            *q = rgbe(c);
        }
        put_scan(&mut out, &scan);
    }
    notes.push(Note::new(
        Severity::Info,
        "hdr.linear",
        "sRGB samples were converted to the linear radiance that Radiance HDR stores",
    ));
    Ok((out, notes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn rgbe_of_known_values() {
        assert_eq!(rgbe([1.0, 1.0, 1.0]), [128, 128, 128, 129]);
        assert_eq!(rgbe([0.0; 3]), [0; 4]);
        assert_eq!(rgbe([0.5, 0.25, 0.0]), [128, 64, 0, 128]);
    }

    #[test]
    fn round_trip_is_close() {
        let mut data = Vec::new();
        for y in 0..9u32 {
            for x in 0..40u32 {
                data.extend_from_slice(&[
                    (x * 6) as u8,
                    (255 - y * 20) as u8,
                    ((x * y) % 256) as u8,
                ]);
            }
        }
        let img = Image::new(40, 9, Layout::Rgb, Depth::Eight, data.clone()).unwrap();
        let (bytes, _) = encode(&img).unwrap();
        let (back, _) = decode(&bytes, &Limits::default()).unwrap();
        assert_eq!((back.width, back.height), (40, 9));
        let worst = data
            .iter()
            .zip(&back.data)
            .map(|(a, b)| i32::from(*a).abs_diff(i32::from(*b)))
            .max()
            .unwrap();
        assert!(worst <= 20, "{worst}");
    }
}
