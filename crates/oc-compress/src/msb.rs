//! Fast most-significant-bit-first bit reader and writer (bzip2, Zstandard's forward streams
//! use the LSB flavour in their own modules).
use oc_core::{Error, Result};

pub(crate) struct MsbReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> MsbReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buf: 0,
            cnt: 0,
        }
    }

    fn fill(&mut self, n: u32) -> Result<()> {
        while self.cnt < n {
            let b = *self
                .data
                .get(self.pos)
                .ok_or_else(|| Error::truncated("bit stream ended").at(self.pos as u64))?;
            self.pos += 1;
            self.buf = (self.buf << 8) | u64::from(b);
            self.cnt += 8;
        }
        Ok(())
    }

    /// Read `n` (0..=32) bits.
    pub fn bits(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.fill(n)?;
        self.cnt -= n;
        Ok(((self.buf >> self.cnt) & ((1u64 << n) - 1)) as u32)
    }

    pub fn bit(&mut self) -> Result<u32> {
        self.bits(1)
    }

    /// Bytes consumed so far, counting a partly read byte as consumed.
    pub fn byte_pos(&self) -> usize {
        self.pos
    }

    /// Drop the bits left in the current byte.
    pub fn align(&mut self) {
        self.cnt -= self.cnt % 8;
    }
}

#[derive(Default)]
pub(crate) struct MsbWriter {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl MsbWriter {
    /// Append the low `n` (0..=32) bits of `v`.
    pub fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.acc = (self.acc << n) | (u64::from(v) & ((1u64 << n) - 1));
        self.n += n;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(0, pad);
        }
        self.out
    }
}
