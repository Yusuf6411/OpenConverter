//! zlib container (RFC 1950).
use crate::{deflate::deflate, inflate::inflate};
use oc_checksum::adler32;
use oc_core::{Error, ErrorCode, Limits, Progress, Result};
use oc_io::ByteReader;

pub fn compress(data: &[u8], level: u32) -> Vec<u8> {
    let hint: u8 = match level {
        0 | 1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let cmf = 0x78u8;
    let mut flg = hint << 6;
    flg += (31 - ((u16::from(cmf) << 8) + u16::from(flg)) % 31) as u8 % 31;
    let mut out = vec![cmf, flg];
    out.extend(deflate(data, level));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

pub fn decompress(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    let mut r = ByteReader::new(input);
    let cmf = r.u8()?;
    let flg = r.u8()?;
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
        return Err(Error::corrupt("bad zlib header").at(0));
    }
    if flg & 0x20 != 0 {
        return Err(
            Error::new(ErrorCode::UnsupportedVariant, "zlib preset dictionary")
                .with_detail("FDICT"),
        );
    }
    let body = input.get(2..).unwrap_or(&[]);
    let inflated = inflate(body, limits, progress)?;
    let mut t = ByteReader::new(body.get(inflated.consumed..).unwrap_or(&[]));
    let want = t
        .u32_be()
        .map_err(|e| e.at((2 + inflated.consumed) as u64))?;
    if adler32(&inflated.data) != want {
        return Err(Error::corrupt("zlib Adler-32 mismatch").at((2 + inflated.consumed) as u64));
    }
    Ok(inflated.data)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use oc_core::NoProgress;

    fn d(z: &[u8]) -> Result<Vec<u8>> {
        decompress(z, &Limits::default(), &NoProgress)
    }

    #[test]
    fn roundtrip_and_header_validity() {
        for level in [0, 1, 6, 9] {
            let z = compress(b"hello zlib hello zlib", level);
            assert_eq!(u16::from_be_bytes([z[0], z[1]]) % 31, 0);
            assert_eq!(d(&z).ok().as_deref(), Some(&b"hello zlib hello zlib"[..]));
        }
    }

    #[test]
    fn detects_corruption() {
        let mut z = compress(b"some data to protect", 6);
        let n = z.len();
        z[n - 1] ^= 1;
        assert!(d(&z).is_err());
        assert!(d(&[0x78]).is_err());
        assert!(d(&[0x79, 0x9C, 0]).is_err());
        assert_eq!(
            d(&[0x78, 0xBB, 0, 0]).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedVariant)
        );
    }
}
