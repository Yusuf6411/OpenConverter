//! PackBits run-length coding (TIFF 6.0 section 9, also used in PSD and MacPaint).
use oc_core::{Error, Result};
use oc_io::ByteReader;

/// Decode PackBits, producing exactly `expected` bytes when given, otherwise until input ends.
pub fn decode(input: &[u8], expected: Option<usize>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut r = ByteReader::new(input);
    while !r.is_empty() {
        if expected.is_some_and(|e| out.len() >= e) {
            break;
        }
        let n = r.u8()? as i8;
        if n >= 0 {
            out.extend_from_slice(r.take(n as usize + 1)?);
        } else if n != -128 {
            let b = r.u8()?;
            out.resize(out.len() + (1 - i16::from(n)) as usize, b);
        }
    }
    if let Some(e) = expected {
        if out.len() < e {
            return Err(Error::truncated("PackBits data shorter than expected"));
        }
        out.truncate(e);
    }
    Ok(out)
}

#[allow(clippy::indexing_slicing)] // all indices are bounded by the loop conditions
pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let mut run = 1;
        while i + run < data.len() && run < 128 && data[i + run] == data[i] {
            run += 1;
        }
        if run >= 2 {
            out.push((1 - run as i16) as i8 as u8);
            out.push(data[i]);
            i += run;
        } else {
            let start = i;
            while i < data.len() && i - start < 128 {
                if i + 2 < data.len() && data[i] == data[i + 1] && data[i] == data[i + 2] {
                    break;
                }
                i += 1;
            }
            out.push((i - start - 1) as u8);
            out.extend_from_slice(&data[start..i]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn tiff_spec_example() {
        // From the TIFF 6.0 specification.
        let enc = [
            0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A, 0xFD, 0xAA, 0x03, 0x80, 0x00, 0x2A, 0x22, 0xF7,
            0xAA,
        ];
        let dec = [
            0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0x22,
            0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA,
        ];
        assert_eq!(decode(&enc, None).ok().as_deref(), Some(&dec[..]));
    }

    #[test]
    fn roundtrip_and_errors() {
        let mut d: Vec<u8> = (0..=255u8).collect();
        d.extend(std::iter::repeat_n(9, 300));
        d.extend([1, 1, 2, 2, 2]);
        assert_eq!(decode(&encode(&d), Some(d.len())).ok(), Some(d));
        assert!(decode(&[5, 1, 2], None).is_err());
        assert!(decode(&[0xFF], None).is_err());
        assert!(decode(&[0, 1], Some(5)).is_err());
        assert_eq!(decode(&[0x80, 0, 5], None).ok(), Some(vec![5]));
    }
}
