use oc_core::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOrder {
    /// Most significant bit first (JPEG, PNG filters, CCITT).
    MsbFirst,
    /// Least significant bit first (DEFLATE, LZW in GIF).
    LsbFirst,
}

/// Reads up to 32 bits at a time.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: u64,
    order: BitOrder,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8], order: BitOrder) -> Self {
        Self {
            data,
            pos: 0,
            order,
        }
    }
    pub fn bit_position(&self) -> u64 {
        self.pos
    }
    pub fn bits_left(&self) -> u64 {
        (self.data.len() as u64 * 8).saturating_sub(self.pos)
    }

    fn bit_at(&self, pos: u64) -> Option<u32> {
        let byte = *self.data.get(usize::try_from(pos / 8).ok()?)?;
        let i = (pos % 8) as u32;
        Some(match self.order {
            BitOrder::MsbFirst => u32::from(byte >> (7 - i)) & 1,
            BitOrder::LsbFirst => u32::from(byte >> i) & 1,
        })
    }

    /// Read `n` (0..=32) bits. In LSB-first mode the first bit read is the lowest bit
    /// of the result; in MSB-first mode it is the highest.
    pub fn read(&mut self, n: u32) -> Result<u32> {
        if n > 32 {
            return Err(Error::new(
                oc_core::ErrorCode::InternalError,
                "bit read wider than 32",
            ));
        }
        if u64::from(n) > self.bits_left() {
            return Err(Error::truncated("bit stream ended").at(self.pos / 8));
        }
        let mut v: u64 = 0;
        for k in 0..n {
            let b = u64::from(self.bit_at(self.pos + u64::from(k)).unwrap_or(0));
            match self.order {
                BitOrder::MsbFirst => v = (v << 1) | b,
                BitOrder::LsbFirst => v |= b << k,
            }
        }
        self.pos += u64::from(n);
        Ok(v as u32)
    }

    pub fn read_bit(&mut self) -> Result<bool> {
        self.read(1).map(|b| b == 1)
    }

    /// Skip to the next byte boundary.
    pub fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// Writes bits into a growing buffer.
#[derive(Debug, Clone)]
pub struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    nbits: u32,
    order: BitOrder,
}

impl BitWriter {
    pub fn new(order: BitOrder) -> Self {
        Self {
            out: Vec::new(),
            cur: 0,
            nbits: 0,
            order,
        }
    }

    /// Write the low `n` (0..=32) bits of `value`, following the writer's bit order
    /// (the mirror image of [`BitReader::read`]).
    pub fn write(&mut self, value: u32, n: u32) {
        for k in 0..n.min(32) {
            let bit = match self.order {
                BitOrder::MsbFirst => (value >> (n - 1 - k)) & 1,
                BitOrder::LsbFirst => (value >> k) & 1,
            } as u8;
            match self.order {
                BitOrder::MsbFirst => self.cur |= bit << (7 - self.nbits),
                BitOrder::LsbFirst => self.cur |= bit << self.nbits,
            }
            self.nbits += 1;
            if self.nbits == 8 {
                self.out.push(self.cur);
                self.cur = 0;
                self.nbits = 0;
            }
        }
    }

    /// Pad the current byte with zero bits.
    pub fn align(&mut self) {
        if self.nbits > 0 {
            self.out.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msb_and_lsb_read_orders() {
        let mut m = BitReader::new(&[0b1011_0000], BitOrder::MsbFirst);
        assert_eq!(m.read(4).ok(), Some(0b1011));
        let mut l = BitReader::new(&[0b0000_1011], BitOrder::LsbFirst);
        assert_eq!(l.read(4).ok(), Some(0b1011));
    }

    #[test]
    fn reading_past_end_fails() {
        let mut r = BitReader::new(&[0xFF], BitOrder::LsbFirst);
        assert!(r.read(9).is_err());
        assert_eq!(r.read(8).ok(), Some(0xFF));
        assert!(r.read(1).is_err());
        assert!(r.read(33).is_err());
    }

    #[test]
    fn write_read_roundtrip_both_orders() {
        for order in [BitOrder::MsbFirst, BitOrder::LsbFirst] {
            let fields: [(u32, u32); 6] = [
                (5, 3),
                (0x1FF, 9),
                (0, 1),
                (0xDEAD_BEEF, 32),
                (1, 1),
                (0x2A, 6),
            ];
            let mut w = BitWriter::new(order);
            for (v, n) in fields {
                w.write(v, n);
            }
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes, order);
            for (v, n) in fields {
                assert_eq!(r.read(n).ok(), Some(v), "{order:?} width {n}");
            }
        }
    }

    #[test]
    fn deflate_style_lsb_packing() {
        // Writing 1 (1 bit), 2 (2 bits), 5 (3 bits) LSB-first packs to 0b00_101_10_1.
        let mut w = BitWriter::new(BitOrder::LsbFirst);
        w.write(1, 1);
        w.write(2, 2);
        w.write(5, 3);
        assert_eq!(w.finish(), vec![0b0010_1101]);
    }

    #[test]
    fn align_skips_to_boundary() {
        let mut r = BitReader::new(&[0xAA, 0x55], BitOrder::MsbFirst);
        r.read(3).ok();
        r.align();
        assert_eq!(r.read(8).ok(), Some(0x55));
    }
}
