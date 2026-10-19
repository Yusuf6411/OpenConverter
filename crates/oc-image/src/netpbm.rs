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

#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeOptions {
    /// Write the ASCII variants (P1/P2/P3).
    pub ascii: bool,
    /// Background for flattening alpha when the image has some.
    pub background: [u8; 3],
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> (Vec<u8>, Notes) {
    let mut notes = Notes::new();
    // Netpbm has no alpha (except PAM, not written): flatten.
    let flat = if img.layout.has_alpha() {
        let target = if img.layout.is_gray() {
            Layout::Gray
        } else {
            Layout::Rgb
        };
        let (i, n) = ops::convert_layout(img, target, opts.background);
        notes.extend(n);
        i
    } else {
        img.clone()
    };
    let (kind, maxval) = if flat.layout == Layout::Rgb {
        (3, flat.depth.max())
    } else {
        (2, flat.depth.max())
    };
    let ch = flat.layout.channels();
    let mut out = format!(
        "P{}\n{} {}\n{}\n",
        if opts.ascii { kind } else { kind + 3 },
        flat.width,
        flat.height,
        maxval
    )
    .into_bytes();
    if opts.ascii {
        for y in 0..flat.height as usize {
            let line: Vec<String> = (0..flat.width as usize * ch)
                .map(|x| flat.sample(y * flat.width as usize * ch + x).to_string())
                .collect();
            out.extend_from_slice(line.join(" ").as_bytes());
            out.push(b'\n');
        }
    } else {
        out.extend_from_slice(&flat.data);
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
        // Writing flattens (no alpha in P6) and says so.
        let (_, notes) = encode(&i, &EncodeOptions::default());
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
