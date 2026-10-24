//! Most-significant-bit-first bit reader (spec 5.2). Reads past the end give zeros and set a
//! flag, so one check at the end of a packet is enough.
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    over: bool,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            over: false,
        }
    }

    pub fn overrun(&self) -> bool {
        self.over
    }

    #[inline]
    pub fn bit(&mut self) -> u32 {
        let byte = self.pos >> 3;
        let Some(&b) = self.data.get(byte) else {
            self.over = true;
            return 0;
        };
        let v = (b >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        u32::from(v)
    }

    /// An unsigned integer of `n` bits (0 to 32).
    pub fn read(&mut self, n: u32) -> u32 {
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | u64::from(self.bit());
        }
        v as u32
    }
}
