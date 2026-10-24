//! LSB-first bit reader and canonical prefix codes as used by VP8L.
use oc_core::{Error, Result};

pub(crate) const MAX_CODE_LEN: usize = 15;
const ROOT_BITS: u32 = 8;

pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    nbits: u32,
    consumed: u64,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            acc: 0,
            nbits: 0,
            consumed: 0,
        }
    }

    #[inline]
    fn fill(&mut self) {
        while self.nbits <= 56 {
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.acc |= u64::from(b) << self.nbits;
            self.nbits += 8;
        }
    }

    /// Read `n` (0..=32) bits; reading past the end yields zeros and sets [`Bits::eos`].
    #[inline]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.nbits < n {
            self.fill();
        }
        let v = (self.acc & ((1u64 << n) - 1)) as u32;
        self.acc >>= n;
        self.nbits -= n;
        self.consumed += u64::from(n);
        v
    }

    #[inline]
    pub fn peek(&mut self, n: u32) -> u32 {
        if self.nbits < n {
            self.fill();
        }
        (self.acc & ((1u64 << n) - 1)) as u32
    }

    #[inline]
    pub fn skip(&mut self, n: u32) {
        self.acc >>= n;
        self.nbits -= n;
        self.consumed += u64::from(n);
    }

    /// True once more bits were consumed than the data holds.
    #[inline]
    pub fn eos(&self) -> bool {
        self.consumed > self.data.len() as u64 * 8
    }

    pub fn check(&self) -> Result<()> {
        if self.eos() {
            Err(Error::truncated("VP8L data ended early").at(self.consumed / 8))
        } else {
            Ok(())
        }
    }
}

/// A canonical prefix code. The first bit read is the first (most significant) bit of the code.
#[derive(Debug, Clone)]
pub(crate) struct Huffman {
    /// A code with one symbol takes no bits.
    single: Option<u16>,
    /// `len << 16 | symbol` indexed by the next `ROOT_BITS` bits; 0 when the code is longer.
    root: Vec<u32>,
    count: [u16; MAX_CODE_LEN + 1],
    symbols: Vec<u16>,
}

impl Huffman {
    /// Build from code lengths. The code must be complete, except that a single used
    /// symbol is allowed (it takes no bits).
    #[allow(clippy::indexing_slicing)] // every index is bounded by MAX_CODE_LEN above
    pub fn new(lengths: &[u8]) -> Result<Self> {
        let mut count = [0u16; MAX_CODE_LEN + 1];
        for &l in lengths {
            if usize::from(l) > MAX_CODE_LEN {
                return Err(Error::corrupt("prefix code length above 15"));
            }
            if let Some(c) = count.get_mut(usize::from(l)) {
                *c += 1;
            }
        }
        let used = lengths.len() - usize::from(count[0]);
        if used == 0 {
            return Err(Error::corrupt("prefix code without symbols"));
        }
        if used == 1 {
            let sym = lengths.iter().position(|&l| l != 0).unwrap_or(0);
            return Ok(Self {
                single: Some(sym as u16),
                root: Vec::new(),
                count,
                symbols: Vec::new(),
            });
        }
        let mut left: i32 = 1;
        for &c in count.iter().skip(1) {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return Err(Error::corrupt("oversubscribed prefix code"));
            }
        }
        if left != 0 {
            return Err(Error::corrupt("incomplete prefix code"));
        }
        // Sort symbols by (length, value).
        let mut offs = [0u16; MAX_CODE_LEN + 2];
        for l in 1..=MAX_CODE_LEN {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut symbols = vec![0u16; used];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let slot = &mut offs[usize::from(l)];
                if let Some(o) = symbols.get_mut(usize::from(*slot)) {
                    *o = s as u16;
                }
                *slot += 1;
            }
        }
        // Fill the root table with bit-reversed codes.
        let mut root = vec![0u32; 1 << ROOT_BITS];
        let mut code = 0u32;
        let mut idx = 0usize;
        for (l, &cnt) in count.iter().enumerate().skip(1) {
            for _ in 0..cnt {
                let sym = symbols.get(idx).copied().unwrap_or(0);
                idx += 1;
                if l as u32 <= ROOT_BITS {
                    let rev = code.reverse_bits() >> (32 - l as u32);
                    let entry = ((l as u32) << 16) | u32::from(sym);
                    let mut j = rev as usize;
                    while j < root.len() {
                        if let Some(r) = root.get_mut(j) {
                            *r = entry;
                        }
                        j += 1 << l;
                    }
                }
                code += 1;
            }
            code <<= 1;
        }
        Ok(Self {
            single: None,
            root,
            count,
            symbols,
        })
    }

    #[inline]
    pub fn read(&self, br: &mut Bits) -> u32 {
        if let Some(s) = self.single {
            return u32::from(s);
        }
        let v = br.peek(MAX_CODE_LEN as u32);
        let e = self
            .root
            .get((v & ((1 << ROOT_BITS) - 1)) as usize)
            .copied()
            .unwrap_or(0);
        if e != 0 {
            br.skip(e >> 16);
            return e & 0xffff;
        }
        self.read_slow(br, v)
    }

    #[cold]
    fn read_slow(&self, br: &mut Bits, v: u32) -> u32 {
        let (mut code, mut first, mut index) = (0u32, 0u32, 0u32);
        for len in 1..=MAX_CODE_LEN {
            code |= (v >> (len - 1)) & 1;
            let c = u32::from(self.count.get(len).copied().unwrap_or(0));
            if code < first + c {
                br.skip(len as u32);
                return self
                    .symbols
                    .get((index + code - first) as usize)
                    .map_or(0, |&s| u32::from(s));
            }
            index += c;
            first = (first + c) << 1;
            code <<= 1;
        }
        // Unreachable for a complete code; consume everything to flag the stream.
        br.skip(MAX_CODE_LEN as u32);
        0
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn short_and_long_codes_decode() {
        // Lengths 1,2,3,4,...,15,15 form a complete code.
        let mut lengths: Vec<u8> = (1..=15).collect();
        lengths.push(15);
        let h = Huffman::new(&lengths).unwrap();
        // Symbol 0 is "0", symbol 1 is "10", symbol 14 is 14 ones then "0"; the last two
        // symbols are 15 ones... build the stream: 0, 10, 110, then symbol 15 (all ones).
        let bits_in = [0u8, 1, 0, 1, 1, 0].iter().chain([1u8; 15].iter()).copied();
        let mut bytes = vec![0u8; 4];
        for (i, b) in bits_in.enumerate() {
            bytes[i / 8] |= b << (i % 8);
        }
        let mut br = Bits::new(&bytes);
        assert_eq!(h.read(&mut br), 0);
        assert_eq!(h.read(&mut br), 1);
        assert_eq!(h.read(&mut br), 2);
        assert_eq!(h.read(&mut br), 15);
        assert!(!br.eos());
    }

    #[test]
    fn single_symbol_takes_no_bits() {
        let mut lengths = vec![0u8; 10];
        lengths[7] = 3;
        let h = Huffman::new(&lengths).unwrap();
        let mut br = Bits::new(&[0xff]);
        assert_eq!(h.read(&mut br), 7);
        assert_eq!(br.read(8), 0xff);
    }

    #[test]
    fn bad_codes_are_rejected() {
        assert!(Huffman::new(&[1, 1, 1]).is_err());
        assert!(Huffman::new(&[1, 2]).is_err());
        assert!(Huffman::new(&[0, 0]).is_err());
        assert!(Huffman::new(&[16, 1]).is_err());
    }
}
