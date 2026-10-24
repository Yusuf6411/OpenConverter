//! Tier-2: tag trees and the packet header bit reader (T.800 Annex B).
use oc_core::{Error, Result};

/// Bit reader for packet headers: a byte following 0xFF carries only seven bits.
pub struct HdrBits<'a> {
    d: &'a [u8],
    pub pos: usize,
    buf: u8,
    cnt: u8,
    last_ff: bool,
}

impl<'a> HdrBits<'a> {
    pub fn new(d: &'a [u8], pos: usize) -> Self {
        HdrBits {
            d,
            pos,
            buf: 0,
            cnt: 0,
            last_ff: false,
        }
    }

    pub fn bit(&mut self) -> Result<u32> {
        if self.cnt == 0 {
            let b = *self
                .d
                .get(self.pos)
                .ok_or_else(|| Error::truncated("JPEG 2000: packet header"))?;
            self.pos += 1;
            self.cnt = if self.last_ff { 7 } else { 8 };
            self.buf = b;
            self.last_ff = b == 0xFF;
        }
        self.cnt -= 1;
        Ok(u32::from((self.buf >> self.cnt) & 1))
    }

    pub fn bits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }

    /// Finish the header: a trailing 0xFF is followed by a stuffed byte.
    pub fn align(&mut self) {
        self.cnt = 0;
        if self.last_ff {
            self.pos += 1;
            self.last_ff = false;
        }
    }
}

#[derive(Clone)]
struct Node {
    value: u32,
    low: u32,
}

#[derive(Clone)]
pub struct TagTree {
    /// Levels from the leaves (index 0) to the root.
    levels: Vec<(usize, usize, Vec<Node>)>,
}

impl TagTree {
    pub fn new(w: usize, h: usize) -> TagTree {
        let mut levels = Vec::new();
        let (mut cw, mut ch) = (w.max(1), h.max(1));
        loop {
            levels.push((
                cw,
                ch,
                vec![
                    Node {
                        value: u32::MAX,
                        low: 0
                    };
                    cw * ch
                ],
            ));
            if cw == 1 && ch == 1 {
                break;
            }
            cw = cw.div_ceil(2);
            ch = ch.div_ceil(2);
        }
        TagTree { levels }
    }

    /// Reads bits until it is known whether the leaf's value is below `threshold`.
    pub fn decode(&mut self, hb: &mut HdrBits, x: usize, y: usize, threshold: u32) -> Result<bool> {
        let mut low = 0;
        for lvl in (0..self.levels.len()).rev() {
            let (w, _, nodes) = &mut self.levels[lvl];
            let n = &mut nodes[(y >> lvl) * *w + (x >> lvl)];
            if low > n.low {
                n.low = low;
            } else {
                low = n.low;
            }
            while low < threshold && low < n.value {
                if hb.bit()? == 1 {
                    n.value = low;
                } else {
                    low += 1;
                }
            }
            n.low = low;
        }
        let (w, _, nodes) = &self.levels[0];
        Ok(nodes[y * *w + x].value < threshold)
    }

    pub fn value(&self, x: usize, y: usize) -> u32 {
        let (w, _, nodes) = &self.levels[0];
        nodes[y * *w + x].value
    }
}
