#![allow(clippy::indexing_slicing)] // table indices are bounded by the format's alphabet sizes
//! LZX decompression (Microsoft CAB / CHM flavour), written from the published format
//! description. Output frames are 32 KiB; the whole output doubles as the window.
use oc_core::{Error, Result};

const MIN_MATCH: usize = 2;
const NUM_CHARS: usize = 256;
const PRETREE_SYMS: usize = 20;
const ALIGNED_SYMS: usize = 8;
const SECONDARY_LENGTHS: usize = 249;
const FRAME: usize = 32768;

/// MSB-first reader over little-endian 16-bit words.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    left: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buf: 0,
            left: 0,
        }
    }
    fn ensure(&mut self, n: u32) {
        while self.left < n {
            let lo = self.data.get(self.pos).copied().unwrap_or(0);
            let hi = self.data.get(self.pos + 1).copied().unwrap_or(0);
            self.pos += 2;
            self.buf = (self.buf << 16) | (u64::from(hi) << 8) | u64::from(lo);
            self.left += 16;
        }
    }
    fn peek(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.ensure(n);
        ((self.buf >> (self.left - n)) & ((1u64 << n) - 1)) as u32
    }
    fn skip(&mut self, n: u32) {
        self.left -= n;
    }
    fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        if n > 0 {
            self.skip(n);
        }
        v
    }
    fn overrun(&self) -> bool {
        self.pos > self.data.len() + 4
    }
    /// Drop bits up to the next 16-bit boundary.
    fn align16(&mut self) {
        self.left -= self.left % 16;
    }
}

struct Huff {
    /// Lookup indexed by the next `bits` bits: (symbol << 5) | code length.
    table: Vec<u32>,
    bits: u32,
}

impl Huff {
    fn new(lengths: &[u8]) -> Result<Self> {
        let max = lengths.iter().copied().max().unwrap_or(0);
        if max == 0 {
            return Ok(Self { table: vec![0], bits: 0 });
        }
        if max > 16 {
            return Err(Error::corrupt("LZX: code length above 16"));
        }
        let bits = u32::from(max);
        let mut count = [0u32; 17];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        count[0] = 0;
        let mut next = [0u32; 18];
        let mut code = 0u32;
        for l in 1..=16usize {
            code = (code + count[l - 1]) << 1;
            next[l] = code;
        }
        let mut table = vec![0u32; 1 << bits];
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = u32::from(l);
            let c = next[l as usize];
            next[l as usize] += 1;
            if c >= 1 << l {
                return Err(Error::corrupt("LZX: over-subscribed Huffman code"));
            }
            let first = (c << (bits - l)) as usize;
            let n = 1usize << (bits - l);
            let entry = ((sym as u32) << 5) | l;
            for slot in table.get_mut(first..first + n).ok_or_else(|| Error::corrupt("LZX: bad code"))? {
                *slot = entry;
            }
        }
        Ok(Self { table, bits })
    }
    fn decode(&self, b: &mut Bits<'_>) -> Result<usize> {
        if self.bits == 0 {
            return Ok(0);
        }
        let v = b.peek(self.bits) as usize;
        let e = *self.table.get(v).ok_or_else(|| Error::corrupt("LZX: bad code"))?;
        let l = e & 31;
        if l == 0 {
            return Err(Error::corrupt("LZX: invalid Huffman code"));
        }
        b.skip(l);
        Ok((e >> 5) as usize)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    None,
    Verbatim,
    Aligned,
    Stored,
}

pub struct Lzx {
    window_bits: u32,
    r: [usize; 3],
    main_len: Vec<u8>,
    length_len: Vec<u8>,
    header_read: bool,
    intel_size: u32,
    intel_pos: i64,
    intel_started: bool,
    block: Block,
    remaining: usize,
    main: Option<Huff>,
    length: Option<Huff>,
    aligned: Option<Huff>,
    frames: u64,
    stored_odd: bool,
    extra: Vec<u8>,
    base: Vec<u32>,
}

impl Lzx {
    pub fn new(window_bits: u32) -> Result<Self> {
        if !(15..=21).contains(&window_bits) {
            return Err(Error::corrupt("LZX: window size out of range"));
        }
        let slots = match window_bits {
            20 => 42,
            21 => 50,
            b => (b * 2) as usize,
        };
        let mut extra = vec![0u8; 52];
        let mut j = 0u8;
        let mut i = 0usize;
        while i <= 50 {
            extra[i] = j;
            extra[i + 1] = j;
            if i != 0 && j < 17 {
                j += 1;
            }
            i += 2;
        }
        let mut base = vec![0u32; 52];
        let mut acc = 0u32;
        for i in 0..=50usize {
            base[i] = acc;
            acc = acc.wrapping_add(1u32 << extra[i]);
        }
        Ok(Self {
            window_bits,
            r: [1, 1, 1],
            main_len: vec![0; NUM_CHARS + slots * 8],
            length_len: vec![0; SECONDARY_LENGTHS],
            header_read: false,
            intel_size: 0,
            intel_pos: 0,
            intel_started: false,
            block: Block::None,
            remaining: 0,
            main: None,
            length: None,
            aligned: None,
            frames: 0,
            stored_odd: false,
            extra,
            base,
        })
    }

    fn read_lengths(b: &mut Bits<'_>, lens: &mut [u8], first: usize, last: usize) -> Result<()> {
        let mut pre = [0u8; PRETREE_SYMS];
        for p in &mut pre {
            *p = b.read(4) as u8;
        }
        let pt = Huff::new(&pre)?;
        let mut i = first;
        while i < last {
            let sym = pt.decode(b)?;
            let step = |prev: u8, sym: usize| -> u8 {
                let z = i32::from(prev) - sym as i32;
                (if z < 0 { z + 17 } else { z }) as u8
            };
            match sym {
                17 | 18 => {
                    let n = if sym == 17 { b.read(4) + 4 } else { b.read(5) + 20 } as usize;
                    let end = (i + n).min(last);
                    for l in lens.get_mut(i..end).ok_or_else(|| Error::corrupt("LZX: length overrun"))? {
                        *l = 0;
                    }
                    i = end;
                }
                19 => {
                    let n = b.read(1) as usize + 4;
                    let s2 = pt.decode(b)?;
                    let prev = lens.get(i).copied().unwrap_or(0);
                    let v = step(prev, s2);
                    let end = (i + n).min(last);
                    for l in lens.get_mut(i..end).ok_or_else(|| Error::corrupt("LZX: length overrun"))? {
                        *l = v;
                    }
                    i = end;
                }
                s => {
                    let prev = lens.get(i).copied().unwrap_or(0);
                    *lens.get_mut(i).ok_or_else(|| Error::corrupt("LZX: length overrun"))? = step(prev, s);
                    i += 1;
                }
            }
        }
        Ok(())
    }

    /// Decode one CAB data frame (`out_len` bytes, at most 32768) appended to `out`.
    /// `input` is that frame's compressed payload. `out` holds all earlier output.
    pub fn decode_frame(&mut self, input: &[u8], out_len: usize, out: &mut Vec<u8>) -> Result<()> {
        if out_len > FRAME {
            return Err(Error::corrupt("LZX: frame larger than 32 KiB"));
        }
        let start = out.len();
        let target = start + out_len;
        let mut b = Bits::new(input);
        if !self.header_read {
            if b.read(1) == 1 {
                let hi = b.read(16);
                let lo = b.read(16);
                self.intel_size = (hi << 16) | lo;
            }
            self.header_read = true;
        }
        while out.len() < target {
            if self.remaining == 0 {
                if self.block == Block::Stored {
                    // Padding to an even length after an odd-sized stored block.
                    if self.stored_odd {
                        b.pos = b.pos.saturating_add(1);
                    }
                    b.left = 0;
                    b.buf = 0;
                }
                self.begin_block(&mut b)?;
            }
            let n = self.remaining.min(target - out.len());
            match self.block {
                Block::Stored => {
                    let a = b.pos;
                    let e = a + n;
                    let chunk = b
                        .data
                        .get(a..e)
                        .ok_or_else(|| Error::truncated("LZX: stored data cut short"))?;
                    out.extend_from_slice(chunk);
                    b.pos = e;
                    self.remaining -= n;
                }
                Block::Verbatim | Block::Aligned => {
                    let produced = self.run(&mut b, out, out.len() + n)?;
                    self.remaining -= produced;
                }
                Block::None => return Err(Error::corrupt("LZX: no active block")),
            }
            if b.overrun() {
                return Err(Error::truncated("LZX: input ended early"));
            }
        }
        if out.len() != target {
            return Err(Error::corrupt("LZX: match crosses a frame boundary"));
        }
        // Frames restart on a 16-bit boundary (each frame has its own input buffer).
        b.align16();
        if self.remaining == 0 {
            self.stored_odd = false;
        }
        // E8 call translation.
        if self.intel_started && self.intel_size != 0 && self.frames < 32768 && out_len > 10 {
            let size = i64::from(self.intel_size);
            let mut cur = self.intel_pos;
            let end = start + out_len - 10;
            let mut i = start;
            while i < end {
                if out[i] != 0xE8 {
                    i += 1;
                    cur += 1;
                    continue;
                }
                i += 1;
                let abs = i32::from_le_bytes([out[i], out[i + 1], out[i + 2], out[i + 3]]);
                let abs64 = i64::from(abs);
                if abs64 >= -cur && abs64 < size {
                    let rel = if abs64 >= 0 { abs64 - cur } else { abs64 + size };
                    out[i..i + 4].copy_from_slice(&(rel as i32).to_le_bytes());
                }
                i += 4;
                cur += 5;
            }
        }
        self.intel_pos += out_len as i64;
        self.frames += 1;
        Ok(())
    }

    fn begin_block(&mut self, b: &mut Bits<'_>) -> Result<()> {
        let kind = b.read(3);
        let hi = b.read(16) as usize;
        let lo = b.read(8) as usize;
        self.remaining = (hi << 8) | lo;
        self.stored_odd = false;
        match kind {
            1 | 2 => {
                self.block = if kind == 1 { Block::Verbatim } else { Block::Aligned };
                if kind == 2 {
                    let mut al = [0u8; ALIGNED_SYMS];
                    for a in &mut al {
                        *a = b.read(3) as u8;
                    }
                    self.aligned = Some(Huff::new(&al)?);
                }
                let mut main = std::mem::take(&mut self.main_len);
                let total = main.len();
                Self::read_lengths(b, &mut main, 0, NUM_CHARS)?;
                if main.get(0xE8).copied().unwrap_or(0) != 0 {
                    self.intel_started = true;
                }
                Self::read_lengths(b, &mut main, NUM_CHARS, total)?;
                self.main = Some(Huff::new(&main)?);
                self.main_len = main;
                let mut len = std::mem::take(&mut self.length_len);
                Self::read_lengths(b, &mut len, 0, SECONDARY_LENGTHS)?;
                self.length = Some(Huff::new(&len)?);
                self.length_len = len;
            }
            3 => {
                self.block = Block::Stored;
                self.intel_started = true;
                // The encoder pads the logical bit position to the next 16-bit boundary,
                // adding a whole word when it is already aligned.
                let logical = b.pos * 8 - b.left as usize;
                b.pos = (logical / 16 + 1) * 2;
                b.left = 0;
                b.buf = 0;
                let raw = b
                    .data
                    .get(b.pos..b.pos + 12)
                    .ok_or_else(|| Error::truncated("LZX: stored block header cut short"))?;
                for k in 0..3 {
                    self.r[k] = u32::from_le_bytes([raw[k * 4], raw[k * 4 + 1], raw[k * 4 + 2], raw[k * 4 + 3]]) as usize;
                }
                b.pos += 12;
                self.stored_odd = self.remaining & 1 == 1;
            }
            _ => return Err(Error::corrupt("LZX: invalid block type")),
        }
        Ok(())
    }

    /// Decode symbols until `limit` output bytes exist. Returns bytes produced.
    fn run(&mut self, b: &mut Bits<'_>, out: &mut Vec<u8>, limit: usize) -> Result<usize> {
        let begin = out.len();
        let window = 1usize << self.window_bits;
        let main = self.main.as_ref().ok_or_else(|| Error::corrupt("LZX: missing main tree"))?;
        let length = self.length.as_ref().ok_or_else(|| Error::corrupt("LZX: missing length tree"))?;
        let aligned_block = self.block == Block::Aligned;
        while out.len() < limit {
            let sym = main.decode(b)?;
            if sym < NUM_CHARS {
                out.push(sym as u8);
                continue;
            }
            let m = sym - NUM_CHARS;
            let mut len = m & 7;
            let slot = m >> 3;
            if len == 7 {
                len += length.decode(b)?;
            }
            len += MIN_MATCH;
            let off: usize = match slot {
                0 => self.r[0],
                1 => {
                    self.r.swap(0, 1);
                    self.r[0]
                }
                2 => {
                    self.r.swap(0, 2);
                    self.r[0]
                }
                s => {
                    let eb = u32::from(self.extra.get(s).copied().unwrap_or(0));
                    let base = self.base.get(s).copied().unwrap_or(0) as usize;
                    let o = if aligned_block && eb >= 3 {
                        let verb = b.read(eb - 3) as usize;
                        let al = self
                            .aligned
                            .as_ref()
                            .ok_or_else(|| Error::corrupt("LZX: missing aligned tree"))?
                            .decode(b)?;
                        base + (verb << 3) + al - 2
                    } else if eb > 0 {
                        base + b.read(eb) as usize - 2
                    } else {
                        base - 2
                    };
                    self.r[2] = self.r[1];
                    self.r[1] = self.r[0];
                    self.r[0] = o;
                    o
                }
            };
            if off == 0 || off > out.len() || off > window {
                return Err(Error::corrupt("LZX: match distance out of range"));
            }
            if out.len() + len > limit {
                return Err(Error::corrupt("LZX: match crosses a frame boundary"));
            }
            let from = out.len() - off;
            for k in 0..len {
                let v = out[from + k];
                out.push(v);
            }
            if b.overrun() {
                return Err(Error::truncated("LZX: input ended early"));
            }
        }
        Ok(out.len() - begin)
    }
}
