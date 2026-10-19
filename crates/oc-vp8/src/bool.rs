//! The boolean entropy coder of VP8 (RFC 6386 section 7), reader and writer.

/// Reads bits whose probability of being zero is given in 1/256 steps.
pub struct BoolReader<'a> {
    data: &'a [u8],
    pos: usize,
    value: u64,
    /// Range minus one, kept in 127..=254.
    range: u32,
    bits: i32,
    eof: bool,
}

impl<'a> BoolReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut r = Self {
            data,
            pos: 0,
            value: 0,
            range: 254,
            bits: -8,
            eof: false,
        };
        r.load();
        r
    }

    /// True once the reader had to invent bytes past the end of its data.
    pub fn eof(&self) -> bool {
        self.eof
    }

    fn load(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 7) {
            let mut v = 0u64;
            for &b in chunk {
                v = (v << 8) | u64::from(b);
            }
            self.value = (self.value << 56) | v;
            self.bits += 56;
            self.pos += 7;
        } else if let Some(&b) = self.data.get(self.pos) {
            self.value = (self.value << 8) | u64::from(b);
            self.bits += 8;
            self.pos += 1;
        } else if !self.eof {
            self.value <<= 8;
            self.bits += 8;
            self.eof = true;
        } else {
            self.bits = 0;
        }
    }

    pub fn get(&mut self, prob: u8) -> bool {
        if self.bits < 0 {
            self.load();
        }
        let pos = self.bits as u32;
        let split = (self.range * u32::from(prob)) >> 8;
        let value = (self.value >> pos) as u32;
        let bit = value > split;
        let mut range;
        if bit {
            range = self.range - split;
            self.value -= u64::from(split + 1) << pos;
        } else {
            range = split + 1;
        }
        let shift = 7 ^ (31 - range.leading_zeros());
        range <<= shift;
        self.bits -= shift as i32;
        self.range = range - 1;
        bit
    }

    pub fn get_bit(&mut self) -> bool {
        self.get(0x80)
    }

    /// An unsigned `n`-bit value, most significant bit first.
    pub fn value(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for i in (0..n).rev() {
            v |= u32::from(self.get_bit()) << i;
        }
        v
    }

    /// A magnitude of `n` bits followed by a sign bit.
    pub fn signed(&mut self, n: u32) -> i32 {
        let v = self.value(n) as i32;
        self.sign(v)
    }

    /// A sign applied to `v`, coded with probability one half.
    pub fn sign(&mut self, v: i32) -> i32 {
        if self.get_bit() {
            -v
        } else {
            v
        }
    }
}

/// Writes bits with the matching probabilities; `finish` flushes the coder.
pub struct BoolWriter {
    out: Vec<u8>,
    /// Range minus one.
    range: u32,
    value: u32,
    /// Bits accumulated beyond the next complete byte, minus eight.
    nb_bits: i32,
    /// Pending 0xff bytes whose carry is not known yet.
    run: u32,
}

impl Default for BoolWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BoolWriter {
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            range: 254,
            value: 0,
            nb_bits: -8,
            run: 0,
        }
    }

    fn flush(&mut self) {
        let s = 8 + self.nb_bits;
        let bits = self.value >> s;
        self.value -= bits << s;
        self.nb_bits -= 8;
        if (bits & 0xff) != 0xff {
            let carry = bits & 0x100 != 0;
            if carry {
                if let Some(last) = self.out.last_mut() {
                    *last = last.wrapping_add(1);
                }
            }
            let fill = if carry { 0x00 } else { 0xff };
            self.out
                .extend(std::iter::repeat_n(fill, self.run as usize));
            self.run = 0;
            self.out.push((bits & 0xff) as u8);
        } else {
            self.run += 1;
        }
    }

    pub fn put(&mut self, bit: bool, prob: u8) {
        let split = (self.range * u32::from(prob)) >> 8;
        if bit {
            self.value += split + 1;
            self.range -= split + 1;
        } else {
            self.range = split;
        }
        if self.range < 127 {
            let shift = 7 ^ (31 - (self.range + 1).leading_zeros());
            self.range = ((self.range + 1) << shift) - 1;
            self.value <<= shift;
            self.nb_bits += shift as i32;
            if self.nb_bits > 0 {
                self.flush();
            }
        }
    }

    pub fn put_bit(&mut self, bit: bool) {
        self.put(bit, 0x80);
    }

    pub fn put_value(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.put_bit((v >> i) & 1 != 0);
        }
    }

    pub fn put_signed(&mut self, v: i32, n: u32) {
        self.put_value(v.unsigned_abs(), n);
        self.put_bit(v < 0);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.put_value(0, (9 - self.nb_bits) as u32);
        self.nb_bits = 0;
        self.flush();
        self.out
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn writer_and_reader_agree() {
        let mut state = 12345u32;
        let mut rnd = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state >> 8
        };
        let items: Vec<(bool, u8)> = (0..20_000)
            .map(|_| {
                let prob = (rnd() % 255 + 1) as u8;
                // Mostly follow the probability so long 0xff runs and carries occur.
                let bit = rnd() % 256 >= u32::from(prob);
                (bit, prob)
            })
            .collect();
        let mut w = BoolWriter::new();
        for &(b, p) in &items {
            w.put(b, p);
        }
        w.put_value(0x2a5, 10);
        w.put_signed(-37, 7);
        let bytes = w.finish();
        let mut r = BoolReader::new(&bytes);
        for &(b, p) in &items {
            assert_eq!(r.get(p), b);
        }
        assert_eq!(r.value(10), 0x2a5);
        assert_eq!(r.signed(7), -37);
        assert!(!r.eof());
    }

    #[test]
    fn reader_flags_running_dry() {
        let mut r = BoolReader::new(&[0x12]);
        for _ in 0..64 {
            r.get(128);
        }
        assert!(r.eof());
    }
}
