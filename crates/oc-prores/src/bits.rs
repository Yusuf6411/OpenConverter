//! MSB-first bit reader and the Golomb-Rice/exponential-Golomb combination codes (RDD 36 7.1.1.1).

pub struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    pub fn overrun(&self) -> bool {
        self.pos > self.data.len() * 8
    }

    pub fn bit(&mut self) -> u32 {
        let b = self
            .data
            .get(self.pos >> 3)
            .map_or(0, |&v| (v >> (7 - (self.pos & 7))) & 1);
        self.pos += 1;
        u32::from(b)
    }

    /// Up to 32 bits; bits past the end read as zero and set the overrun flag.
    pub fn read(&mut self, n: u32) -> u32 {
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | u64::from(self.bit());
        }
        v as u32
    }

    /// Whether the bits not yet read amount to 31 or fewer, all zero (`endOfData`).
    pub fn end_of_data(&self) -> bool {
        let left = self.bits_left();
        if left >= 32 {
            return false;
        }
        let mut p = self.pos;
        let total = self.data.len() * 8;
        while p < total {
            if (self.data[p >> 3] >> (7 - (p & 7))) & 1 == 1 {
                return false;
            }
            p += 1;
        }
        true
    }

    /// Count zero bits up to and including the next one bit; `None` for an absurd run.
    fn zeros(&mut self) -> Option<u32> {
        let mut q = 0;
        while self.bit() == 0 {
            q += 1;
            if q > 32 || self.overrun() {
                return None;
            }
        }
        Some(q)
    }

    fn exp_tail(&mut self, q: u32, k: u32) -> Option<u32> {
        let n = q + k;
        if n > 31 {
            return None;
        }
        Some((((1u64 << n) | u64::from(self.read(n))) - (1u64 << k)) as u32)
    }

    /// Exponential-Golomb code of order `k`.
    pub fn exp_golomb(&mut self, k: u32) -> Option<u32> {
        let q = self.zeros()?;
        self.exp_tail(q, k)
    }

    /// Rice/exponential-Golomb combination code.
    pub fn combo(&mut self, last_rice_q: u32, k_rice: u32, k_exp: u32) -> Option<u32> {
        let q = self.zeros()?;
        if q <= last_rice_q {
            Some((q << k_rice) | self.read(k_rice))
        } else {
            let base = (last_rice_q + 1) << k_rice;
            Some(
                self.exp_tail(q - (last_rice_q + 1), k_exp)?
                    .saturating_add(base),
            )
        }
    }
}

/// The inverse of the signed integer-to-symbol mapping S(n).
pub fn unsigned_to_signed(s: u32) -> i32 {
    if s & 1 == 0 {
        (s >> 1) as i32
    } else {
        -(((s >> 1) + 1) as i32)
    }
}
