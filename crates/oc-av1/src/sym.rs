//! The symbol (range) decoder of section 8.2.

pub struct Sym {
    data: Vec<u8>,
    pos: usize,
    value: u32,
    range: u32,
    max_bits: i64,
    update: bool,
}

impl Sym {
    /// `data` is exactly the `sz` bytes of the tile.
    pub fn new(data: &[u8], disable_cdf_update: bool) -> Sym {
        let mut s = Sym {
            data: data.to_vec(),
            pos: 0,
            value: 0,
            range: 1 << 15,
            max_bits: 8 * data.len() as i64 - 15,
            update: !disable_cdf_update,
        };
        let num_bits = (data.len() * 8).min(15) as u32;
        let buf = s.read(num_bits);
        let padded = buf << (15 - num_bits);
        s.value = ((1 << 15) - 1) ^ padded;
        s
    }

    fn read(&mut self, n: u32) -> u32 {
        let mut x = 0u32;
        let mut left = n;
        while left > 0 {
            let byte = u32::from(self.data.get(self.pos >> 3).copied().unwrap_or(0));
            let avail = 8 - (self.pos & 7) as u32;
            let take = avail.min(left);
            let v = (byte >> (avail - take)) & ((1 << take) - 1);
            x = (x << take) | v;
            self.pos += take as usize;
            left -= take;
        }
        x
    }

    pub fn symbol(&mut self, cdf: &mut [u16]) -> usize {
        let n = cdf.len() - 1;
        let mut cur = self.range;
        let mut symbol = 0usize;
        let mut prev;
        loop {
            prev = cur;
            let f = (1u32 << 15) - u32::from(cdf[symbol]);
            cur = ((self.range >> 8) * (f >> 6)) >> 1;
            cur += 4 * (n - symbol - 1) as u32;
            if self.value >= cur {
                break;
            }
            symbol += 1;
        }
        self.range = prev - cur;
        self.value -= cur;
        let bits = 15 - (31 - self.range.leading_zeros());
        self.range <<= bits;
        let num_bits = i64::from(bits).min(self.max_bits.max(0)) as u32;
        let new_data = self.read(num_bits);
        let padded = new_data << (bits - num_bits);
        self.value = padded ^ (((self.value + 1) << bits) - 1);
        self.max_bits -= i64::from(bits);
        if self.update {
            let c = u32::from(cdf[n]);
            let rate = 3
                + u32::from(c > 15)
                + u32::from(c > 31)
                + (31 - (n as u32).leading_zeros()).min(2);
            let mut tmp = 0u32;
            for i in 0..n - 1 {
                if i == symbol {
                    tmp = 1 << 15;
                }
                let v = u32::from(cdf[i]);
                if tmp < v {
                    cdf[i] = (v - ((v - tmp) >> rate)) as u16;
                } else {
                    cdf[i] = (v + ((tmp - v) >> rate)) as u16;
                }
            }
            cdf[n] += u16::from(cdf[n] < 32);
        }
        symbol
    }

    pub fn bool(&mut self) -> u32 {
        let mut cdf = [1u16 << 14, 1 << 15, 0];
        let saved = self.update;
        self.update = false;
        let s = self.symbol(&mut cdf);
        self.update = saved;
        s as u32
    }

    pub fn literal(&mut self, n: u32) -> u32 {
        let mut x = 0;
        for _ in 0..n {
            x = 2 * x + self.bool();
        }
        x
    }

    /// NS(n)
    pub fn ns(&mut self, n: u32) -> u32 {
        let w = 32 - n.leading_zeros();
        let m = (1u32 << w) - n;
        let v = self.literal(w - 1);
        if v < m {
            return v;
        }
        let extra = self.literal(1);
        (v << 1) - m + extra
    }
}
