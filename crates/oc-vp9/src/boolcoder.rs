//! The boolean (arithmetic) decoder of VP9 (spec 9.2).

const LOTS_OF_BITS: i32 = 0x4000_0000;

pub struct BoolDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    value: u64,
    count: i32,
    range: u32,
}

impl<'a> BoolDecoder<'a> {
    /// Start decoding `data`; the first bit is the marker, which must be zero.
    pub fn new(data: &'a [u8]) -> Option<Self> {
        if data.is_empty() {
            return None;
        }
        let mut d = Self {
            data,
            pos: 0,
            value: 0,
            count: -8,
            range: 255,
        };
        d.fill();
        if d.read(128) {
            return None;
        }
        Some(d)
    }

    fn fill(&mut self) {
        let mut shift = 64 - 8 - (self.count + 8);
        while shift >= 0 {
            match self.data.get(self.pos) {
                Some(&b) => {
                    self.count += 8;
                    self.value |= u64::from(b) << shift;
                    self.pos += 1;
                    shift -= 8;
                }
                None => {
                    self.count += LOTS_OF_BITS;
                    break;
                }
            }
        }
    }

    #[inline]
    pub fn read(&mut self, prob: u8) -> bool {
        let split = (self.range * u32::from(prob) + (256 - u32::from(prob))) >> 8;
        if self.count < 0 {
            self.fill();
        }
        let big = u64::from(split) << 56;
        let bit = if self.value >= big {
            self.range -= split;
            self.value -= big;
            true
        } else {
            self.range = split;
            false
        };
        let shift = self.range.leading_zeros().saturating_sub(24);
        self.range <<= shift;
        self.value <<= shift;
        self.count -= shift as i32;
        bit
    }

    pub fn literal(&mut self, n: u32) -> u32 {
        let mut x = 0;
        for _ in 0..n {
            x = 2 * x + u32::from(self.read(128));
        }
        x
    }
}
