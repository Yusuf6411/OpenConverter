//! Netpbm: PBM/PGM/PPM (P1-P6) and PAM (P7).
use crate::image::{Depth, Image, Layout, SampleWriter};
use crate::ops;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    matches!(data, [b'P', b'1'..=b'7', c, ..] if c.is_ascii_whitespace())
}

struct Cursor<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn skip_ws(&mut self) {
        while let Some(&b) = self.d.get(self.p) {
            if b == b'#' {
                while self
                    .d
                    .get(self.p)
                    .is_some_and(|&c| c != b'\n' && c != b'\r')
                {
                    self.p += 1;
                }
            } else if b.is_ascii_whitespace() {
                self.p += 1;
            } else {
                break;
            }
        }
    }
    fn token(&mut self) -> Option<&'a [u8]> {
        self.skip_ws();
        let start = self.p;
        while self
            .d
            .get(self.p)
            .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'#')
        {
            self.p += 1;
        }
        (self.p > start).then(|| self.d.get(start..self.p).unwrap_or(&[]))
    }
    fn number(&mut self, what: &str) -> Result<u32> {
        let t = self
            .token()
            .ok_or_else(|| Error::truncated(format!("missing {what}")).at(self.p as u64))?;
        std::str::from_utf8(t)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| Error::corrupt(format!("bad {what}")).at(self.p as u64))
    }
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let mut c = Cursor { d: data, p: 0 };
    let magic = c.token().ok_or_else(|| Error::truncated("empty file"))?;
    let kind = match magic {
        [b'P', k @ b'1'..=b'7'] => *k - b'0',
        _ => {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                "not a Netpbm file",
            ))
        }
    };
    let (width, height, maxval, layout) = if kind == 7 {
        decode_pam_header(&mut c)?
    } else {
        let w = c.number("width")?;
        let h = c.number("height")?;
        let (m, l) = match kind {
            1 | 4 => (1, Layout::Gray),
            2 | 5 => (c.number("maxval")?, Layout::Gray),
            _ => (c.number("maxval")?, Layout::Rgb),
        };
        (w, h, m, l)
    };
    if width == 0 || height == 0 {
        return Err(Error::corrupt("zero image size"));
    }
    if maxval == 0 || maxval > 65535 {
        return Err(Error::corrupt("maxval out of range"));
    }
    limits.check_pixels(width, height)?;
    let ch = layout.channels();
    let count = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(ch))
        .ok_or_else(|| Error::limit("max_pixels", "image too large"))?;
    limits.check_alloc((count * 2) as u64)?;
    let depth = if maxval > 255 {
        Depth::Sixteen
    } else {
        Depth::Eight
    };
    let rescale = maxval != 255 && maxval != 65535 && kind != 1 && kind != 4;
    let target = u32::from(depth.max() as u16);
    let scale = |v: u32| -> u16 {
        if rescale {
            ((v * target + maxval / 2) / maxval) as u16
        } else {
            v as u16
        }
    };
    let mut w = SampleWriter::new(depth, count);
    match kind {
        1 => {
            for _ in 0..count {
                c.skip_ws();
                // P1 digits may be written without separators.
                let b =
                    *c.d.get(c.p)
                        .ok_or_else(|| Error::truncated("P1 bitmap data").at(c.p as u64))?;
                c.p += 1;
                match b {
                    b'0' => w.push(255),
                    b'1' => w.push(0),
                    _ => return Err(Error::corrupt("P1 data must be 0 or 1").at(c.p as u64)),
                }
            }
        }
        2 | 3 => {
            for _ in 0..count {
                let v = c.number("sample")?;
                if v > maxval {
                    return Err(Error::corrupt("sample exceeds maxval").at(c.p as u64));
                }
                w.push(scale(v));
            }
        }
        4 => {
            // Exactly one whitespace byte follows the header.
            c.p += 1;
            let row = (width as usize).div_ceil(8);
            let body =
                c.d.get(c.p..c.p + row * height as usize)
                    .ok_or_else(|| Error::truncated("P4 bitmap data").at(c.p as u64))?;
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let byte = body.get(y * row + x / 8).copied().unwrap_or(0);
                    w.push(if (byte >> (7 - x % 8)) & 1 == 1 {
                        0
                    } else {
                        255
                    });
                }
            }
        }
        _ => {
            c.p += 1;
            let bps = if maxval > 255 { 2 } else { 1 };
            let body =
                c.d.get(c.p..c.p + count * bps)
                    .ok_or_else(|| Error::truncated("pixel data").at(c.p as u64))?;
            for i in 0..count {
                let v = if bps == 2 {
                    u32::from(u16::from_be_bytes([
                        body.get(2 * i).copied().unwrap_or(0),
                        body.get(2 * i + 1).copied().unwrap_or(0),
                    ]))
                } else {
                    u32::from(body.get(i).copied().unwrap_or(0))
                };
                if v > maxval {
                    return Err(Error::corrupt("sample exceeds maxval").at((c.p + i * bps) as u64));
                }
                w.push(scale(v));
            }
        }
    }
    if rescale {
        notes.push(Note::new(
            Severity::Info,
            "maxval_rescaled",
            format!("samples rescaled from maxval {maxval}"),
        ));
    }
    Ok((Image::new(width, height, layout, depth, w.data)?, notes))
}

fn decode_pam_header(c: &mut Cursor<'_>) -> Result<(u32, u32, u32, Layout)> {
    let (mut w, mut h, mut depth, mut maxval, mut tupl) = (0, 0, 0, 0, String::new());
    loop {
        let key = c.token().ok_or_else(|| Error::truncated("PAM header"))?;
        match key {
            b"ENDHDR" => break,
            b"WIDTH" => w = c.number("WIDTH")?,
            b"HEIGHT" => h = c.number("HEIGHT")?,
            b"DEPTH" => depth = c.number("DEPTH")?,
            b"MAXVAL" => maxval = c.number("MAXVAL")?,
            b"TUPLTYPE" => {
                // The value runs to the end of the line.
                c.skip_ws();
                let start = c.p;
                while c.d.get(c.p).is_some_and(|&b| b != b'\n') {
                    c.p += 1;
                }
                tupl = String::from_utf8_lossy(c.d.get(start..c.p).unwrap_or(&[]))
                    .trim()
                    .to_string();
            }
            _ => return Err(Error::corrupt("unknown PAM header field")),
        }
    }
    let layout = match (tupl.as_str(), depth) {
        ("BLACKANDWHITE" | "GRAYSCALE", 1) | ("", 1) => Layout::Gray,
        ("GRAYSCALE_ALPHA" | "BLACKANDWHITE_ALPHA", 2) | ("", 2) => Layout::GrayAlpha,
        ("RGB", 3) | ("", 3) => Layout::Rgb,
        ("RGB_ALPHA", 4) | ("", 4) => Layout::Rgba,
        _ => {
            return Err(
                Error::new(ErrorCode::UnsupportedVariant, "unsupported PAM tuple type")
                    .with_detail(format!("{tupl} depth {depth}")),
            )
        }
    };
    Ok((w, h, maxval, layout))
}

/// Which member of the family to write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Variant {
    /// PGM for gray, PPM for colour, PAM only when there is real transparency.
    #[default]
    Auto,
    /// Bilevel bitmap (P1/P4); anything else is thresholded.
    Pbm,
    Pgm,
    Ppm,
    /// Keeps alpha. Binary only.
    Pam,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeOptions {
    /// Write the ASCII variants (P1/P2/P3). PAM has no ASCII form and ignores this.
    pub ascii: bool,
    /// Background for flattening alpha when the chosen variant cannot hold it.
    pub background: [u8; 3],
    pub variant: Variant,
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> (Vec<u8>, Notes) {
    let mut notes = Notes::new();
    let translucent = img.layout.has_alpha() && img.has_translucency();
    let variant = match opts.variant {
        Variant::Auto if translucent => Variant::Pam,
        Variant::Auto if img.layout.is_gray() => Variant::Pgm,
        Variant::Auto => Variant::Ppm,
        v => v,
    };
    let target = match variant {
        Variant::Pbm | Variant::Pgm => Layout::Gray,
        Variant::Ppm => Layout::Rgb,
        _ => img.layout,
    };
    let (mut flat, n) = ops::convert_layout(img, target, opts.background);
    notes.extend(n);
    if variant == Variant::Pbm {
        let (g, n) = ops::convert_depth(&flat, Depth::Eight);
        notes.extend(n);
        flat = g;
        if flat.data.iter().any(|&v| v != 0 && v != 255) {
            notes.push(Note::new(
                Severity::Loss,
                "netpbm.bilevel",
                "the image was reduced to black and white at the 50% threshold",
            ));
        }
    }
    let (w, h) = (flat.width as usize, flat.height as usize);
    let ch = flat.layout.channels();
    let maxval = flat.depth.max();
    let ascii = opts.ascii && variant != Variant::Pam;
    if opts.ascii && variant == Variant::Pam {
        notes.push(Note::new(
            Severity::Info,
            "netpbm.pam_binary",
            "PAM has no text form; wrote the binary variant",
        ));
    }
    let mut out = match variant {
        Variant::Pam => {
            let tuple = match flat.layout {
                Layout::Gray => "GRAYSCALE",
                Layout::GrayAlpha => "GRAYSCALE_ALPHA",
                Layout::Rgb => "RGB",
                Layout::Rgba => "RGB_ALPHA",
            };
            format!(
                "P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH {ch}\nMAXVAL {maxval}\nTUPLTYPE {tuple}\nENDHDR\n"
            )
        }
        Variant::Pbm => format!("P{}\n{w} {h}\n", if ascii { 1 } else { 4 }),
        _ => {
            let kind = if variant == Variant::Ppm { 3 } else { 2 };
            format!(
                "P{}\n{w} {h}\n{maxval}\n",
                if ascii { kind } else { kind + 3 }
            )
        }
    }
    .into_bytes();
    match variant {
        Variant::Pbm => {
            // 1 is black in PBM, the opposite of a gray sample.
            for row in flat.data.chunks(w.max(1)) {
                if ascii {
                    let line: Vec<&str> = row
                        .iter()
                        .map(|&v| if v < 128 { "1" } else { "0" })
                        .collect();
                    out.extend_from_slice(line.join(" ").as_bytes());
                    out.push(b'\n');
                } else {
                    for byte in row.chunks(8) {
                        let mut b = 0u8;
                        for (i, &v) in byte.iter().enumerate() {
                            if v < 128 {
                                b |= 0x80 >> i;
                            }
                        }
                        out.push(b);
                    }
                }
            }
        }
        _ if ascii => {
            for y in 0..h {
                let line: Vec<String> = (0..w * ch)
                    .map(|x| flat.sample(y * w * ch + x).to_string())
                    .collect();
                out.extend_from_slice(line.join(" ").as_bytes());
                out.push(b'\n');
            }
        }
        _ => out.extend_from_slice(&flat.data),
    }
    (out, notes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::unwrap_used)]
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../tests/fixtures/images/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn decodes_the_1x_fixtures() {
        let (g, _) = decode(&fixture("grey.pgm"), &Limits::default()).unwrap();
        assert_eq!(g.layout, Layout::Gray);
        assert_eq!(g.data.len(), g.width as usize * g.height as usize);
        let (c, _) = decode(&fixture("gradient.ppm"), &Limits::default()).unwrap();
        assert_eq!(c.layout, Layout::Rgb);
        let (s, _) = decode(&fixture("rgb16.ppm"), &Limits::default()).unwrap();
        assert_eq!(s.depth, Depth::Sixteen);
        let (b, _) = decode(&fixture("bits.pbm"), &Limits::default()).unwrap();
        assert!(b.data.iter().all(|&v| v == 0 || v == 255));
    }

    #[test]
    fn binary_and_ascii_roundtrip_exactly() {
        for name in ["gradient.ppm", "grey.pgm", "rgb16.ppm"] {
            let (img, _) = decode(&fixture(name), &Limits::default()).unwrap();
            for ascii in [false, true] {
                let (bytes, notes) = encode(
                    &img,
                    &EncodeOptions {
                        ascii,
                        ..Default::default()
                    },
                );
                assert!(notes.is_empty());
                let (back, _) = decode(&bytes, &Limits::default()).unwrap();
                assert_eq!(back.data, img.data, "{name} ascii={ascii}");
            }
        }
    }

    #[test]
    fn headers_comments_and_odd_maxval() {
        let (i, n) = decode(b"P2\n# c\n2 1\n# d\n3\n0 3\n", &Limits::default()).unwrap();
        assert_eq!(i.data, vec![0, 255]);
        assert!(!n.is_empty()); // rescaled
        let (p1, _) = decode(b"P1 3 1 101", &Limits::default()).unwrap();
        assert_eq!(p1.data, vec![0, 255, 0]);
    }

    #[test]
    fn pam_with_alpha_decodes() {
        let pam = b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n\x01\x02\x03\x04";
        let (i, _) = decode(pam, &Limits::default()).unwrap();
        assert_eq!((i.layout, i.data.clone()), (Layout::Rgba, vec![1, 2, 3, 4]));
        // Auto keeps real transparency by writing PAM.
        let (bytes, notes) = encode(&i, &EncodeOptions::default());
        assert!(bytes.starts_with(b"P7"));
        assert!(!notes.has_loss());
        assert_eq!(decode(&bytes, &Limits::default()).unwrap().0.data, i.data);
        // PPM cannot, so it flattens and says so.
        let (ppm, notes) = encode(
            &i,
            &EncodeOptions {
                variant: Variant::Ppm,
                ..Default::default()
            },
        );
        assert!(ppm.starts_with(b"P6"));
        assert!(notes.has_loss());
    }

    #[test]
    fn pbm_writes_bilevel_in_both_forms() {
        let img = Image::new(
            10,
            2,
            Layout::Gray,
            Depth::Eight,
            (0..20).map(|i| if i % 3 == 0 { 0 } else { 255 }).collect(),
        )
        .unwrap();
        for ascii in [false, true] {
            let (bytes, notes) = encode(
                &img,
                &EncodeOptions {
                    ascii,
                    variant: Variant::Pbm,
                    ..Default::default()
                },
            );
            assert!(notes.is_empty());
            let (back, _) = decode(&bytes, &Limits::default()).unwrap();
            assert_eq!(back.data, img.data, "ascii={ascii}");
        }
        let grey = Image::new(2, 1, Layout::Gray, Depth::Eight, vec![100, 200]).unwrap();
        let (_, notes) = encode(
            &grey,
            &EncodeOptions {
                variant: Variant::Pbm,
                ..Default::default()
            },
        );
        assert!(notes.has_loss());
    }

    #[test]
    fn bad_input_errors() {
        for bad in [
            &b""[..],
            b"P6\n2 2\n255\nabc",
            b"P6\n0 5\n255\n",
            b"P3\n1 1\n10\n11 0 0",
            b"X6 1 1 1",
            b"P5 99999999 99999999 255\n",
        ] {
            assert!(decode(bad, &Limits::default()).is_err(), "{bad:?}");
        }
    }
}
