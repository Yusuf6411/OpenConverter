//! The bit reader and the primitive field types of the JPEG XL header syntax (U32, U64, F16,
//! enums), least significant bit first.
use oc_core::{Error, Result};

#[derive(Clone)]
pub struct Br<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

pub fn eof() -> Error {
    Error::truncated("JPEG XL data ends early")
}

pub fn bad(msg: &str) -> Error {
    Error::corrupt(format!("JPEG XL: {msg}"))
}

impl<'a> Br<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn bits_read(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    /// The next `n <= 32` bits without consuming them; bits past the end read as zero.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        let byte = self.pos >> 3;
        let mut w = [0u8; 8];
        if let Some(s) = self.data.get(byte..) {
            let k = s.len().min(8);
            w[..k].copy_from_slice(&s[..k]);
        }
        let v = u64::from_le_bytes(w) >> (self.pos & 7);
        (v & ((1u64 << n) - 1)) as u32
    }

    #[inline]
    pub fn skip(&mut self, n: usize) -> Result<()> {
        if self.pos + n > self.data.len() * 8 {
            self.pos = self.data.len() * 8;
            return Err(eof());
        }
        self.pos += n;
        Ok(())
    }

    #[inline]
    pub fn read(&mut self, n: u32) -> Result<u32> {
        let v = self.peek(n);
        self.skip(n as usize)?;
        Ok(v)
    }

    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.read(1)? != 0)
    }

    /// `U32(d0, d1, d2, d3)`: each descriptor is `(offset, bits)`; zero bits is a constant.
    pub fn u32(&mut self, d: [(u32, u32); 4]) -> Result<u32> {
        let (offset, bits) = d[self.read(2)? as usize];
        Ok(offset.wrapping_add(self.read(bits)?))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(match self.read(2)? {
            0 => 0,
            1 => u64::from(self.read(4)?) + 1,
            2 => u64::from(self.read(8)?) + 17,
            _ => {
                let mut value = u64::from(self.read(12)?);
                let mut shift = 12u32;
                while self.read(1)? == 1 {
                    if shift == 60 {
                        value |= u64::from(self.read(4)?) << shift;
                        break;
                    }
                    value |= u64::from(self.read(8)?) << shift;
                    shift += 8;
                }
                value
            }
        })
    }

    pub fn f16(&mut self) -> Result<f32> {
        let v = self.read(16)?;
        let neg = (v & 0x8000) << 16;
        if v & 0x7fff == 0 {
            return Ok(f32::from_bits(neg));
        }
        let mantissa = v & 0x3ff;
        let exponent = (v >> 10) & 0x1f;
        if exponent == 0x1f {
            return Err(bad("half-precision NaN or infinity"));
        }
        if exponent == 0 {
            let val = (1.0 / 16384.0) * (mantissa as f32 / 1024.0);
            return Ok(if neg != 0 { -val } else { val });
        }
        Ok(f32::from_bits(
            (mantissa << 13) | ((exponent + 112) << 23) | neg,
        ))
    }

    /// An enumeration value: `U32(0, 1, 2 + u(4), 18 + u(6))`.
    pub fn enum_value(&mut self) -> Result<u32> {
        self.u32([(0, 0), (1, 0), (2, 4), (18, 6)])
    }

    pub fn zero_pad(&mut self) -> Result<()> {
        let n = (8 - (self.pos & 7)) & 7;
        if self.read(n as u32)? != 0 {
            return Err(bad("padding bits are not zero"));
        }
        Ok(())
    }

    /// Skip to the next byte boundary without checking the padding.
    pub fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// `UnpackSigned`: 0, 1, 2, 3, ... map to 0, -1, 1, -2, ...
pub fn unpack_signed(v: u32) -> i32 {
    if v & 1 == 0 {
        (v >> 1) as i32
    } else {
        -(((v + 1) >> 1) as i32)
    }
}
