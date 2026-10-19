use oc_core::{Error, Result};

/// A cursor over a byte slice with checked, endian-aware reads.
#[derive(Debug, Clone)]
pub struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

macro_rules! int_reads {
    ($($name:ident, $ty:ty, $from:ident;)*) => {$(
        pub fn $name(&mut self) -> Result<$ty> {
            const N: usize = std::mem::size_of::<$ty>();
            let b = self.take(N)?;
            let mut a = [0u8; N];
            a.copy_from_slice(b);
            Ok(<$ty>::$from(a))
        }
    )*};
}

impl<'a> ByteReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn position(&self) -> usize {
        self.pos
    }
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Take `n` bytes, or fail with `truncated_file` at the current offset.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n);
        match end.and_then(|e| self.data.get(self.pos..e).map(|s| (s, e))) {
            Some((s, e)) => {
                self.pos = e;
                Ok(s)
            }
            None => Err(
                Error::truncated(format!("need {n} bytes, {} left", self.remaining()))
                    .at(self.pos as u64),
            ),
        }
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return Err(Error::corrupt("seek past end").at(pos as u64));
        }
        self.pos = pos;
        Ok(())
    }
    pub fn peek(&self, n: usize) -> Result<&'a [u8]> {
        self.clone().take(n)
    }
    /// A sub-reader over the next `n` bytes; the parent advances past them.
    pub fn sub(&mut self, n: usize) -> Result<ByteReader<'a>> {
        self.take(n).map(ByteReader::new)
    }
    pub fn u8(&mut self) -> Result<u8> {
        self.take(1).map(|b| b.first().copied().unwrap_or(0))
    }

    int_reads! {
        u16_le, u16, from_le_bytes;
        u16_be, u16, from_be_bytes;
        u32_le, u32, from_le_bytes;
        u32_be, u32, from_be_bytes;
        u64_le, u64, from_le_bytes;
        u64_be, u64, from_be_bytes;
        i16_le, i16, from_le_bytes;
        i16_be, i16, from_be_bytes;
        i32_le, i32, from_le_bytes;
        i32_be, i32, from_be_bytes;
    }

    pub fn f32_le(&mut self) -> Result<f32> {
        self.u32_le().map(f32::from_bits)
    }
    pub fn f32_be(&mut self) -> Result<f32> {
        self.u32_be().map(f32::from_bits)
    }

    /// Read bytes up to (not including) `delim`, consuming the delimiter.
    pub fn until(&mut self, delim: u8) -> Result<&'a [u8]> {
        let rest = self.data.get(self.pos..).unwrap_or(&[]);
        match rest.iter().position(|&b| b == delim) {
            Some(i) => {
                let s = rest.get(..i).unwrap_or(&[]);
                self.pos += i + 1;
                Ok(s)
            }
            None => Err(Error::truncated("missing delimiter").at(self.pos as u64)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_core::ErrorCode;

    #[test]
    fn endian_reads() {
        let mut r = ByteReader::new(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(r.u16_be().ok(), Some(0x0102));
        assert_eq!(r.u16_le().ok(), Some(0x0403));
        assert_eq!(r.u32_be().ok(), Some(0x0506_0708));
        assert!(r.is_empty());
    }

    #[test]
    fn truncation_reports_offset_and_does_not_advance_past_end() {
        let mut r = ByteReader::new(&[1, 2, 3]);
        r.skip(2).ok();
        let e = r.u32_le().err();
        assert_eq!(e.as_ref().map(|e| e.code), Some(ErrorCode::TruncatedFile));
        assert_eq!(e.and_then(|e| e.offset), Some(2));
        assert_eq!(r.position(), 2);
    }

    #[test]
    fn huge_take_does_not_overflow() {
        let mut r = ByteReader::new(&[1, 2, 3]);
        r.skip(1).ok();
        assert!(r.take(usize::MAX).is_err());
    }

    #[test]
    fn until_and_sub() {
        let mut r = ByteReader::new(b"abc\0defg");
        assert_eq!(r.until(0).ok(), Some(&b"abc"[..]));
        let mut s = r.sub(3).ok().unwrap_or_else(|| ByteReader::new(&[]));
        assert_eq!(s.take(3).ok(), Some(&b"def"[..]));
        assert_eq!(r.remaining(), 1);
        assert!(r.until(0).is_err());
        assert!(r.seek(100).is_err());
    }
}
