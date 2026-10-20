//! Huffman-coded literals (RFC 8878 section 4.2).
#![allow(clippy::indexing_slicing)]
use super::fse::{read_counts, BackReader, Table};
use oc_core::{Error, Result};

#[derive(Clone)]
pub(crate) struct HufTable {
    pub max_bits: u32,
    /// (symbol, bit length) for every `max_bits`-bit prefix.
    pub cells: Vec<(u8, u8)>,
}

impl HufTable {
    pub fn from_weights(weights: &[u8]) -> Result<Self> {
        let mut total: u32 = 0;
        for &w in weights {
            if w > 11 {
                return Err(Error::corrupt("zstd: Huffman weight above 11"));
            }
            if w > 0 {
                total += 1 << (w - 1);
            }
        }
        if total == 0 {
            return Err(Error::corrupt("zstd: empty Huffman tree"));
        }
        let max_bits = 32 - total.leading_zeros(); // floor(log2(total)) + 1
        if max_bits > 11 {
            return Err(Error::corrupt("zstd: Huffman code too long"));
        }
        let rest = (1u32 << max_bits) - total;
        if !rest.is_power_of_two() {
            return Err(Error::corrupt("zstd: Huffman weights do not fill the tree"));
        }
        let last = rest.trailing_zeros() as u8 + 1;
        let mut all: Vec<u8> = weights.to_vec();
        all.push(last);
        let size = 1usize << max_bits;
        let mut cells = vec![(0u8, 0u8); size];
        let mut pos = 0usize;
        for w in 1..=max_bits as u8 {
            for (s, &sw) in all.iter().enumerate() {
                if sw == w {
                    let len = 1usize << (w - 1);
                    for c in &mut cells[pos..pos + len] {
                        *c = (s as u8, (max_bits as u8) + 1 - w);
                    }
                    pos += len;
                }
            }
        }
        if pos != size || all.len() > 256 {
            return Err(Error::corrupt("zstd: Huffman table does not fill"));
        }
        Ok(Self { max_bits, cells })
    }

    pub fn decode_stream(&self, data: &[u8], count: usize, out: &mut Vec<u8>) -> Result<()> {
        let mut r = BackReader::new(data)?;
        for _ in 0..count {
            let (sym, nb) = self.cells[r.peek(self.max_bits) as usize];
            r.consume(u32::from(nb));
            out.push(sym);
        }
        if r.overflow || r.remaining() != 0 {
            return Err(Error::corrupt("zstd: Huffman stream not used exactly"));
        }
        Ok(())
    }
}

/// Reads the tree description at the start of `data`; returns the table and bytes used.
pub(crate) fn read_tree(data: &[u8]) -> Result<(HufTable, usize)> {
    let hb = *data
        .first()
        .ok_or_else(|| Error::truncated("zstd: Huffman header"))?;
    let mut weights: Vec<u8> = Vec::new();
    let used = if hb >= 128 {
        let n = usize::from(hb) - 127;
        let bytes = n.div_ceil(2);
        let body = data
            .get(1..1 + bytes)
            .ok_or_else(|| Error::truncated("zstd: Huffman weights"))?;
        for i in 0..n {
            let b = body[i / 2];
            weights.push(if i % 2 == 0 { b >> 4 } else { b & 15 });
        }
        1 + bytes
    } else {
        let size = usize::from(hb);
        let body = data
            .get(1..1 + size)
            .ok_or_else(|| Error::truncated("zstd: Huffman weights"))?;
        let (counts, al, desc) = read_counts(body, 12, 6)?;
        let table = Table::from_counts(&counts, al)?;
        let mut r = BackReader::new(body.get(desc..).unwrap_or(&[]))?;
        let mut s1 = r.read(al) as usize;
        let mut s2 = r.read(al) as usize;
        loop {
            if weights.len() > 255 {
                return Err(Error::corrupt("zstd: too many Huffman weights"));
            }
            let c = table.cells[s1];
            weights.push(c.symbol);
            s1 = usize::from(c.base) + r.read(u32::from(c.nb)) as usize;
            if r.overflow {
                weights.push(table.cells[s2].symbol);
                break;
            }
            let c = table.cells[s2];
            weights.push(c.symbol);
            s2 = usize::from(c.base) + r.read(u32::from(c.nb)) as usize;
            if r.overflow {
                weights.push(table.cells[s1].symbol);
                break;
            }
        }
        1 + size
    };
    if weights.len() > 255 {
        return Err(Error::corrupt("zstd: too many Huffman weights"));
    }
    Ok((HufTable::from_weights(&weights)?, used))
}
