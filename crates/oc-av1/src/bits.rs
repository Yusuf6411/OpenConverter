//! MSB-first bit reader for the AV1 header syntax (spec section 4.10).
use oc_core::{Error, Result};

pub struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn bit(&mut self) -> Result<u32> {
        let byte = self
            .data
            .get(self.pos >> 3)
            .ok_or_else(|| Error::truncated("AV1 header ends early"))?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(u32::from(b))
    }

    pub fn flag(&mut self) -> Result<bool> {
        Ok(self.bit()? != 0)
    }

    /// f(n), n <= 32
    pub fn f(&mut self, n: u32) -> Result<u32> {
        let mut x = 0u64;
        for _ in 0..n {
            x = (x << 1) | u64::from(self.bit()?);
        }
        Ok(x as u32)
    }

    /// su(n): signed integer from n bits
    pub fn su(&mut self, n: u32) -> Result<i32> {
        let v = self.f(n)? as i32;
        let sign = 1i32 << (n - 1);
        Ok(if v & sign != 0 { v - 2 * sign } else { v })
    }

    /// ns(n): non-symmetric unsigned
    pub fn ns(&mut self, n: u32) -> Result<u32> {
        let w = 32 - n.leading_zeros(); // FloorLog2(n) + 1
        let m = (1u32 << w) - n;
        let v = self.f(w - 1)?;
        if v < m {
            return Ok(v);
        }
        let extra = self.f(1)?;
        Ok((v << 1) - m + extra)
    }

    pub fn le(&mut self, n: u32) -> Result<u32> {
        let mut t = 0u32;
        for i in 0..n {
            t += self.f(8)? << (i * 8);
        }
        Ok(t)
    }

    pub fn leb128(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for i in 0..8 {
            let b = self.f(8)?;
            v |= u64::from(b & 0x7f) << (i * 7);
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(v)
    }

    pub fn uvlc(&mut self) -> Result<u32> {
        let mut lz = 0;
        loop {
            if self.flag()? {
                break;
            }
            lz += 1;
            if lz >= 32 {
                return Ok(u32::MAX);
            }
        }
        Ok(self.f(lz)? + ((1u64 << lz) - 1) as u32)
    }

    pub fn byte_align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
}

/// Read a leb128 value from a byte slice, returning the value and its length.
pub fn leb128_bytes(data: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..8 {
        let b = *data.get(i)?;
        v |= u64::from(b & 0x7f) << (i * 7);
        if b & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}
