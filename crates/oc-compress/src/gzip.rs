//! gzip container (RFC 1952), including concatenated members.
use crate::{deflate::deflate, inflate::inflate};
use oc_checksum::crc32;
use oc_core::{Error, Limits, Progress, Result};
use oc_io::ByteReader;

pub fn compress(data: &[u8], level: u32) -> Vec<u8> {
    let xfl = match level {
        9 => 2,
        1 => 4,
        _ => 0,
    };
    let mut out = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, xfl, 255];
    out.extend(deflate(data, level));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

pub fn decompress(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let mut r = ByteReader::new(input.get(pos..).unwrap_or(&[]));
        let (id1, id2, cm, flg) = (r.u8()?, r.u8()?, r.u8()?, r.u8()?);
        if id1 != 0x1F || id2 != 0x8B || cm != 8 {
            return Err(Error::corrupt("not a gzip member").at(pos as u64));
        }
        r.skip(6)?; // MTIME, XFL, OS
        if flg & 4 != 0 {
            let n = usize::from(r.u16_le()?);
            r.skip(n)?;
        }
        if flg & 8 != 0 {
            r.until(0)?;
        }
        if flg & 16 != 0 {
            r.until(0)?;
        }
        if flg & 2 != 0 {
            r.skip(2)?;
        }
        let p = pos + r.position();
        let body = input.get(p..).unwrap_or(&[]);
        let inflated = inflate(body, limits, progress)?;
        let at = (p + inflated.consumed) as u64;
        let mut t = ByteReader::new(body.get(inflated.consumed..).unwrap_or(&[]));
        let crc = t.u32_le().map_err(|e| e.at(at))?;
        let isize = t.u32_le().map_err(|e| e.at(at))?;
        if crc32(&inflated.data) != crc {
            return Err(Error::corrupt("gzip CRC-32 mismatch").at(at));
        }
        if inflated.data.len() as u32 != isize {
            return Err(Error::corrupt("gzip ISIZE mismatch").at(at));
        }
        out.extend_from_slice(&inflated.data);
        limits.check_expansion(input.len() as u64, out.len() as u64)?;
        pos = p + inflated.consumed + 8;
        if pos >= input.len() {
            return Ok(out);
        }
    }
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
    fn roundtrip_and_concatenated_members() {
        let a = compress(b"first ", 6);
        let b = compress(b"second", 1);
        let mut both = a.clone();
        both.extend(&b);
        assert_eq!(d(&a).ok().as_deref(), Some(&b"first "[..]));
        assert_eq!(d(&both).ok().as_deref(), Some(&b"first second"[..]));
    }

    #[test]
    fn header_fields_are_skipped() {
        let body = compress(b"x", 6);
        let mut z = vec![0x1F, 0x8B, 8, 8 | 16, 0, 0, 0, 0, 0, 3];
        z.extend(b"name\0comment\0");
        z.extend(&body[10..]);
        assert_eq!(d(&z).ok().as_deref(), Some(&b"x"[..]));
    }

    #[test]
    fn detects_corruption() {
        let mut z = compress(b"integrity", 6);
        let n = z.len();
        z[n - 5] ^= 0xFF;
        assert!(d(&z).is_err());
        assert!(d(&[0x1F, 0x8B]).is_err());
        assert!(d(b"not gzip at all").is_err());
    }
}
