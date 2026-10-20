//! Finite State Entropy tables and the two bit-stream flavours Zstandard uses.
#![allow(clippy::indexing_slicing)] // table indices are masked or checked against sizes
use oc_core::{Error, Result};

/// Forward, least-significant-bit-first reader (used for table descriptions).
pub(crate) struct LsbReader<'a> {
    data: &'a [u8],
    pos: usize, // bits
}

impl<'a> LsbReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn read(&mut self, n: u32) -> Result<u32> {
        if self.pos + n as usize > self.data.len() * 8 {
            return Err(Error::truncated("zstd: table description ended"));
        }
        let mut v = 0u32;
        for i in 0..n {
            let p = self.pos + i as usize;
            v |= u32::from((self.data[p / 8] >> (p % 8)) & 1) << i;
        }
        self.pos += n as usize;
        Ok(v)
    }
    /// Bytes touched so far.
    pub fn bytes(&self) -> usize {
        self.pos.div_ceil(8)
    }
}

/// Backward reader: starts at the highest set bit of the last byte and moves toward the front.
pub(crate) struct BackReader<'a> {
    data: &'a [u8],
    pos: usize, // bits still available
    pub overflow: bool,
}

impl<'a> BackReader<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let last = *data
            .last()
            .ok_or_else(|| Error::corrupt("zstd: empty bit stream"))?;
        if last == 0 {
            return Err(Error::corrupt("zstd: bit stream has no end mark"));
        }
        let h = 7 - last.leading_zeros() as usize;
        Ok(Self {
            data,
            pos: (data.len() - 1) * 8 + h,
            overflow: false,
        })
    }

    pub fn remaining(&self) -> usize {
        self.pos
    }

    fn bits_at(&self, start: usize, n: u32) -> u64 {
        // `n` <= 32; gather the bytes that hold bits start..start+n.
        let first = start / 8;
        let mut acc = 0u64;
        for k in 0..6 {
            acc |= u64::from(self.data.get(first + k).copied().unwrap_or(0)) << (8 * k);
        }
        (acc >> (start % 8)) & ((1u64 << n) - 1)
    }

    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let n_us = n as usize;
        if self.pos >= n_us {
            self.pos -= n_us;
            self.bits_at(self.pos, n) as u32
        } else {
            self.overflow = true;
            let have = self.pos;
            let v = self.bits_at(0, have as u32) << (n_us - have);
            self.pos = 0;
            v as u32
        }
    }

    /// The next `n` bits without consuming them; missing bits read as zero.
    pub fn peek(&self, n: u32) -> u32 {
        let n_us = n as usize;
        if self.pos >= n_us {
            self.bits_at(self.pos - n_us, n) as u32
        } else {
            (self.bits_at(0, self.pos as u32) << (n_us - self.pos)) as u32
        }
    }

    pub fn consume(&mut self, n: u32) {
        let n = n as usize;
        if n > self.pos {
            self.overflow = true;
            self.pos = 0;
        } else {
            self.pos -= n;
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Cell {
    pub symbol: u8,
    pub nb: u8,
    pub base: u16,
}

#[derive(Clone)]
pub(crate) struct Table {
    pub al: u32,
    pub cells: Vec<Cell>,
}

impl Table {
    pub fn rle(symbol: u8) -> Self {
        Self {
            al: 0,
            cells: vec![Cell {
                symbol,
                nb: 0,
                base: 0,
            }],
        }
    }

    /// Builds the decoding table from normalised counts (-1 = "less than one").
    pub fn from_counts(counts: &[i16], al: u32) -> Result<Self> {
        let size = 1usize << al;
        let mut cells = vec![Cell::default(); size];
        let mut next = vec![0u16; counts.len()];
        let mut high = size - 1;
        for (s, &c) in counts.iter().enumerate() {
            if c == -1 {
                cells[high].symbol = s as u8;
                high = high.saturating_sub(1);
                next[s] = 1;
            } else {
                next[s] = c.max(0) as u16;
            }
        }
        let step = (size >> 1) + (size >> 3) + 3;
        let mask = size - 1;
        let mut pos = 0usize;
        for (s, &c) in counts.iter().enumerate() {
            for _ in 0..c.max(0) {
                cells[pos].symbol = s as u8;
                pos = (pos + step) & mask;
                while pos > high {
                    pos = (pos + step) & mask;
                }
            }
        }
        if pos != 0 {
            return Err(Error::corrupt("zstd: inconsistent FSE counts"));
        }
        for cell in &mut cells {
            let s = usize::from(cell.symbol);
            let ns = next[s];
            next[s] += 1;
            let nb = al - (31 - u32::from(ns).leading_zeros());
            cell.nb = nb as u8;
            cell.base = ((u32::from(ns) << nb) - size as u32) as u16;
        }
        Ok(Self { al, cells })
    }
}

/// Reads an FSE table description; returns the normalised counts, the accuracy log and the
/// number of bytes it took.
pub(crate) fn read_counts(
    data: &[u8],
    max_symbol: usize,
    max_al: u32,
) -> Result<(Vec<i16>, u32, usize)> {
    let mut r = LsbReader::new(data);
    let al = r.read(4)? + 5;
    if al > max_al {
        return Err(Error::corrupt("zstd: FSE accuracy log too large"));
    }
    let mut remaining = (1i32 << al) + 1;
    let mut threshold = 1i32 << al;
    let mut nb = al + 1;
    let mut counts: Vec<i16> = Vec::new();
    let mut prev_zero = false;
    while remaining > 1 && counts.len() <= max_symbol {
        if prev_zero {
            loop {
                let rep = r.read(2)?;
                counts.resize(counts.len() + rep as usize, 0);
                if counts.len() > max_symbol + 1 {
                    return Err(Error::corrupt("zstd: FSE symbol overflow"));
                }
                if rep != 3 {
                    break;
                }
            }
            if counts.len() > max_symbol {
                break;
            }
        }
        let max = (2 * threshold - 1) - remaining;
        // Read nb-1 bits first; one more only if the value is not below `max`.
        let low = r.read(nb - 1)? as i32;
        let count = if low < max {
            low
        } else {
            let hi = r.read(1)? as i32;
            let v = low | (hi << (nb - 1));
            if v >= threshold {
                v - max
            } else {
                v
            }
        };
        let c = count - 1;
        remaining -= c.abs();
        counts.push(c as i16);
        prev_zero = c == 0;
        while remaining < threshold {
            nb -= 1;
            threshold >>= 1;
        }
    }
    if remaining != 1 || counts.len() > max_symbol + 1 {
        return Err(Error::corrupt("zstd: FSE counts do not add up"));
    }
    Ok((counts, al, r.bytes()))
}
