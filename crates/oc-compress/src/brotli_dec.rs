//! Brotli decompression (RFC 7932). The static dictionary and the word transforms come from
//! the RFC's appendices (see `tools/gen_brotli_tables.py`, which checks them against the CRCs
//! the RFC states).
#![allow(clippy::indexing_slicing)] // indices are bounded by alphabet sizes and checked lengths

use crate::brotli::{COPY_BASE, COPY_EXTRA, INS_BASE, INS_EXTRA};
use crate::brotli_transforms::{LUT0, LUT1, LUT2, TRANSFORMS};
use oc_core::limits::Limits;
use oc_core::progress::Progress;
use oc_core::{Error, Result};

static DICT: &[u8] = include_bytes!("brotli_dict.bin");
const NDBITS: [u32; 25] = [
    0, 0, 0, 0, 10, 10, 11, 11, 10, 10, 10, 10, 10, 9, 9, 8, 7, 7, 8, 7, 7, 6, 6, 5, 5,
];

fn bad(msg: &'static str) -> Error {
    Error::corrupt(msg)
}

/// Bit source, least significant bit first.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize, // in bits
}

impl<'a> Bits<'a> {
    fn peek(&self, n: u32) -> u32 {
        let byte = self.pos / 8;
        let mut v = 0u64;
        for k in 0..6 {
            v |= u64::from(self.data.get(byte + k).copied().unwrap_or(0)) << (8 * k);
        }
        ((v >> (self.pos % 8)) & ((1u64 << n) - 1)) as u32
    }
    fn skip(&mut self, n: u32) -> Result<()> {
        self.pos += n as usize;
        if self.pos > self.data.len() * 8 {
            return Err(Error::truncated("brotli: stream ends early"));
        }
        Ok(())
    }
    fn read(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.peek(n);
        self.skip(n)?;
        Ok(v)
    }
    /// Skips to the next byte boundary; the skipped bits must be zero.
    fn align(&mut self) -> Result<()> {
        let n = (8 - self.pos % 8) % 8;
        if self.read(n as u32)? != 0 {
            return Err(bad("brotli: padding bits are not zero"));
        }
        Ok(())
    }
}

/// A canonical prefix code, decoded one bit at a time (codes are packed most significant bit
/// first inside the least-significant-bit-first stream).
#[derive(Default, Clone)]
struct Code {
    count: [u16; 16],
    symbols: Vec<u16>,
    single: Option<u16>,
}

impl Code {
    fn from_lengths(lens: &[u8]) -> Result<Self> {
        let mut c = Code::default();
        for &l in lens {
            c.count[usize::from(l)] += 1;
        }
        c.count[0] = 0;
        let used: usize = c.count.iter().map(|&n| usize::from(n)).sum();
        if used == 1 {
            let s = lens.iter().position(|&l| l != 0).unwrap_or(0);
            c.single = Some(s as u16);
            return Ok(c);
        }
        let mut left = 1i32;
        for l in 1..16 {
            left = (left << 1) - i32::from(c.count[l]);
            if left < 0 {
                return Err(bad("brotli: prefix code is over-subscribed"));
            }
        }
        if left != 0 {
            return Err(bad("brotli: prefix code is incomplete"));
        }
        let mut offs = [0usize; 17];
        for l in 1..16 {
            offs[l + 1] = offs[l] + usize::from(c.count[l]);
        }
        c.symbols = vec![0; used];
        for (s, &l) in lens.iter().enumerate() {
            if l != 0 {
                c.symbols[offs[usize::from(l)]] = s as u16;
                offs[usize::from(l)] += 1;
            }
        }
        Ok(c)
    }

    fn decode(&self, r: &mut Bits) -> Result<usize> {
        if let Some(s) = self.single {
            return Ok(usize::from(s));
        }
        let bits = r.peek(15);
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for l in 1..16 {
            code |= ((bits >> (l - 1)) & 1) as i32;
            let n = i32::from(self.count[l]);
            if code - first < n {
                r.skip(l as u32)?;
                return Ok(usize::from(self.symbols[(index + code - first) as usize]));
            }
            index += n;
            first = (first + n) << 1;
            code <<= 1;
        }
        Err(bad("brotli: invalid prefix code"))
    }
}

fn alphabet_bits(size: usize) -> u32 {
    if size <= 1 {
        0
    } else {
        usize::BITS - (size - 1).leading_zeros()
    }
}

fn read_code(r: &mut Bits, alphabet: usize) -> Result<Code> {
    let hskip = r.read(2)?;
    let mut lens = vec![0u8; alphabet];
    if hskip == 1 {
        let nsym = r.read(2)? as usize + 1;
        let ab = alphabet_bits(alphabet);
        let mut syms = [0usize; 4];
        for i in 0..nsym {
            let s = r.read(ab)? as usize;
            if s >= alphabet || syms[..i].contains(&s) {
                return Err(bad("brotli: bad symbol in a simple prefix code"));
            }
            syms[i] = s;
        }
        let ls: &[u8] = match nsym {
            1 => &[1],
            2 => &[1, 1],
            3 => &[1, 2, 2],
            _ => {
                if r.read(1)? == 1 {
                    &[1, 2, 3, 3]
                } else {
                    &[2, 2, 2, 2]
                }
            }
        };
        for (i, &l) in ls.iter().enumerate() {
            lens[syms[i]] = l;
        }
        return Code::from_lengths(&lens);
    }
    const ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    const CL_LEN: [u8; 16] = [2, 2, 2, 3, 2, 2, 2, 4, 2, 2, 2, 3, 2, 2, 2, 4];
    const CL_VAL: [u8; 16] = [0, 4, 3, 2, 0, 4, 3, 1, 0, 4, 3, 2, 0, 4, 3, 5];
    let mut cl = [0u8; 18];
    let mut space = 32i32;
    let mut nonzero = 0;
    for &o in ORDER.iter().skip(hskip as usize) {
        let i = r.peek(4) as usize;
        r.skip(u32::from(CL_LEN[i]))?;
        let l = CL_VAL[i];
        cl[o] = l;
        if l != 0 {
            space -= 32 >> l;
            nonzero += 1;
            if space <= 0 {
                break;
            }
        }
    }
    if nonzero != 1 && space != 0 {
        return Err(bad("brotli: bad code-length code"));
    }
    let clc = Code::from_lengths(&cl)?;
    let (mut sym, mut space) = (0usize, 32768i32);
    let (mut prev, mut repeat, mut repeat_len) = (8u8, 0u32, 0u8);
    while sym < alphabet && space > 0 {
        let c = clc.decode(r)?;
        if c < 16 {
            repeat = 0;
            lens[sym] = c as u8;
            sym += 1;
            if c != 0 {
                prev = c as u8;
                space -= 32768 >> c;
            }
        } else {
            let extra = if c == 16 { 2 } else { 3 };
            let new_len = if c == 16 { prev } else { 0 };
            if repeat_len != new_len {
                repeat = 0;
                repeat_len = new_len;
            }
            let old = repeat;
            if repeat > 0 {
                repeat = (repeat - 2) << extra;
            }
            repeat += r.read(extra)? + 3;
            let delta = (repeat - old) as usize;
            if sym + delta > alphabet {
                return Err(bad("brotli: code-length repeat overflows the alphabet"));
            }
            for l in &mut lens[sym..sym + delta] {
                *l = new_len;
            }
            sym += delta;
            if new_len != 0 {
                space -= (delta as i32) * (32768 >> new_len);
            }
        }
    }
    if space != 0 {
        return Err(bad("brotli: code lengths do not fill the code"));
    }
    Code::from_lengths(&lens)
}

fn read_nbl(r: &mut Bits) -> Result<usize> {
    if r.read(1)? == 0 {
        return Ok(1);
    }
    let n = r.read(3)?;
    Ok((1usize << n) + 1 + r.read(n)? as usize)
}

const BLOCK_BASE: [u32; 26] = [
    1, 5, 9, 13, 17, 25, 33, 41, 49, 65, 81, 97, 113, 145, 177, 209, 241, 305, 369, 497, 753, 1265,
    2289, 4337, 8433, 16625,
];
const BLOCK_EXTRA: [u32; 26] = [
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 24,
];

/// Block-switch state of one category (literals, commands or distances).
struct Blocks {
    n_types: usize,
    types: Option<Code>,
    counts: Option<Code>,
    cur: usize,
    prev: usize,
    left: u32,
}

impl Blocks {
    fn read_header(r: &mut Bits) -> Result<Self> {
        let n_types = read_nbl(r)?;
        if n_types < 2 {
            return Ok(Self {
                n_types,
                types: None,
                counts: None,
                cur: 0,
                prev: 1,
                left: 1 << 24,
            });
        }
        let types = read_code(r, n_types + 2)?;
        let counts = read_code(r, 26)?;
        let mut b = Self {
            n_types,
            types: Some(types),
            counts: Some(counts),
            cur: 0,
            prev: 1,
            left: 0,
        };
        b.left = b.read_count(r)?;
        Ok(b)
    }
    fn read_count(&self, r: &mut Bits) -> Result<u32> {
        let c = match &self.counts {
            Some(c) => c.decode(r)?,
            None => return Ok(1 << 24),
        };
        Ok(BLOCK_BASE[c] + r.read(BLOCK_EXTRA[c])?)
    }
    /// Counts one symbol of this category, switching blocks first when the current one is used up.
    fn step(&mut self, r: &mut Bits) -> Result<()> {
        if self.left == 0 {
            let Some(t) = &self.types else {
                return Err(bad("brotli: block count ran out"));
            };
            let s = t.decode(r)?;
            let new = match s {
                0 => self.prev,
                1 => {
                    let n = self.cur + 1;
                    if n >= self.n_types {
                        n - self.n_types
                    } else {
                        n
                    }
                }
                _ => s - 2,
            };
            self.prev = self.cur;
            self.cur = new;
            self.left = self.read_count(r)?;
        }
        self.left -= 1;
        Ok(())
    }
}

fn read_context_map(r: &mut Bits, size: usize, n_trees: usize) -> Result<Vec<u8>> {
    let rle_max = if r.read(1)? == 0 { 0 } else { r.read(4)? + 1 } as usize;
    let code = read_code(r, n_trees + rle_max)?;
    let mut map = Vec::with_capacity(size);
    while map.len() < size {
        let s = code.decode(r)?;
        if s == 0 {
            map.push(0);
        } else if s <= rle_max {
            let n = (1usize << s) + r.read(s as u32)? as usize;
            if map.len() + n > size {
                return Err(bad("brotli: context map run is too long"));
            }
            map.resize(map.len() + n, 0);
        } else {
            map.push((s - rle_max) as u8);
        }
    }
    if r.read(1)? == 1 {
        let mut mtf: Vec<u8> = (0..=255).collect();
        for v in &mut map {
            let i = usize::from(*v);
            let x = mtf[i];
            *v = x;
            mtf.copy_within(0..i, 1);
            mtf[0] = x;
        }
    }
    if map.iter().any(|&v| usize::from(v) >= n_trees) {
        return Err(bad("brotli: context map names a missing tree"));
    }
    Ok(map)
}

fn ferment(word: &mut [u8], pos: usize) -> usize {
    let c = word[pos];
    if c < 192 {
        if (97..=122).contains(&c) {
            word[pos] ^= 32;
        }
        1
    } else if c < 224 {
        if pos + 1 < word.len() {
            word[pos + 1] ^= 32;
        }
        2
    } else {
        if pos + 2 < word.len() {
            word[pos + 2] ^= 5;
        }
        3
    }
}

/// The dictionary word for `word_id` at `len`, with its transform applied.
fn dictionary_word(len: usize, word_id: usize) -> Result<Vec<u8>> {
    if !(4..=24).contains(&len) {
        return Err(bad("brotli: dictionary reference has an invalid length"));
    }
    let n_words = 1usize << NDBITS[len];
    let t = word_id >> NDBITS[len];
    if t >= TRANSFORMS.len() {
        return Err(bad("brotli: dictionary transform out of range"));
    }
    let offset: usize = (4..len).map(|l| l << NDBITS[l]).sum::<usize>() + (word_id % n_words) * len;
    let mut w = DICT
        .get(offset..offset + len)
        .ok_or_else(|| bad("brotli: dictionary word out of range"))?
        .to_vec();
    let (prefix, kind, suffix) = TRANSFORMS[t];
    match kind {
        0 => {}
        1 => {
            if !w.is_empty() {
                ferment(&mut w, 0);
            }
        }
        2 => {
            let mut i = 0;
            while i < w.len() {
                i += ferment(&mut w, i);
            }
        }
        3..=11 => {
            let k = usize::from(kind) - 2;
            w = w.get(k..).unwrap_or(&[]).to_vec();
        }
        _ => {
            let k = usize::from(kind) - 11;
            w.truncate(w.len().saturating_sub(k));
        }
    }
    let mut out = Vec::with_capacity(prefix.len() + w.len() + suffix.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(&w);
    out.extend_from_slice(suffix);
    Ok(out)
}

/// (insert code base, copy code base, distance is implicit zero) for each block of 64 symbols.
const CELLS: [(usize, usize, bool); 11] = [
    (0, 0, true),
    (0, 8, true),
    (0, 0, false),
    (0, 8, false),
    (8, 0, false),
    (8, 8, false),
    (0, 16, false),
    (16, 0, false),
    (8, 16, false),
    (16, 8, false),
    (16, 16, false),
];

fn context_id(mode: u8, p1: u8, p2: u8) -> usize {
    match mode {
        0 => usize::from(p1 & 63),
        1 => usize::from(p1 >> 2),
        2 => usize::from(LUT0[usize::from(p1)] | LUT1[usize::from(p2)]),
        _ => usize::from((LUT2[usize::from(p1)] << 3) | LUT2[usize::from(p2)]),
    }
}

/// Decompresses a Brotli stream.
pub fn decompress(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    let mut r = Bits {
        data: input,
        pos: 0,
    };
    let wbits = if r.read(1)? == 0 {
        16
    } else {
        let n = r.read(3)?;
        if n != 0 {
            17 + n
        } else {
            match r.read(3)? {
                0 => 17,
                1 => return Err(bad("brotli: invalid window size")),
                m => 8 + m,
            }
        }
    };
    let window = (1usize << wbits) - 16;
    let mut out: Vec<u8> = Vec::new();
    let mut ring = [16i64, 15, 11, 4];
    let mut ring_pos = 0usize;
    loop {
        progress.check_cancelled()?;
        let last = r.read(1)? == 1;
        if last && r.read(1)? == 1 {
            break;
        }
        let nibbles = match r.read(2)? {
            3 => 0,
            n => n + 4,
        };
        if nibbles == 0 {
            if r.read(1)? != 0 {
                return Err(bad("brotli: reserved bit is set"));
            }
            let nb = r.read(2)?;
            let mut skip = 0usize;
            for i in 0..nb {
                skip |= (r.read(8)? as usize) << (8 * i);
            }
            if nb > 0 {
                skip += 1;
            }
            r.align()?;
            r.pos += skip * 8;
            if r.pos > input.len() * 8 {
                return Err(Error::truncated("brotli: metadata ends early"));
            }
            if last {
                break;
            }
            continue;
        }
        let mlen = r.read(nibbles * 4)? as usize + 1;
        limits.check_alloc((out.len() + mlen) as u64)?;
        limits.check_expansion(input.len() as u64, (out.len() + mlen) as u64)?;
        if !last && r.read(1)? == 1 {
            r.align()?;
            let start = r.pos / 8;
            let body = input
                .get(start..start + mlen)
                .ok_or_else(|| Error::truncated("brotli: stored block ends early"))?;
            out.extend_from_slice(body);
            r.pos += mlen * 8;
            continue;
        }
        let mut lit_b = Blocks::read_header(&mut r)?;
        let mut cmd_b = Blocks::read_header(&mut r)?;
        let mut dist_b = Blocks::read_header(&mut r)?;
        let npostfix = r.read(2)?;
        let ndirect = (r.read(4)? as usize) << npostfix;
        let mut modes = Vec::with_capacity(lit_b.n_types);
        for _ in 0..lit_b.n_types {
            modes.push(r.read(2)? as u8);
        }
        let n_lit_trees = read_nbl(&mut r)?;
        let lit_map = if n_lit_trees >= 2 {
            read_context_map(&mut r, 64 * lit_b.n_types, n_lit_trees)?
        } else {
            vec![0; 64 * lit_b.n_types]
        };
        let n_dist_trees = read_nbl(&mut r)?;
        let dist_map = if n_dist_trees >= 2 {
            read_context_map(&mut r, 4 * dist_b.n_types, n_dist_trees)?
        } else {
            vec![0; 4 * dist_b.n_types]
        };
        let mut lit_codes = Vec::with_capacity(n_lit_trees);
        for _ in 0..n_lit_trees {
            lit_codes.push(read_code(&mut r, 256)?);
        }
        let mut cmd_codes = Vec::with_capacity(cmd_b.n_types);
        for _ in 0..cmd_b.n_types {
            cmd_codes.push(read_code(&mut r, 704)?);
        }
        let mut dist_codes = Vec::with_capacity(n_dist_trees);
        for _ in 0..n_dist_trees {
            dist_codes.push(read_code(&mut r, 16 + ndirect + (48 << npostfix))?);
        }
        let end = out.len() + mlen;
        while out.len() < end {
            cmd_b.step(&mut r)?;
            let sym = cmd_codes[cmd_b.cur].decode(&mut r)?;
            let (ib, cb, implicit) = CELLS[sym >> 6];
            let ic = ib + ((sym >> 3) & 7);
            let cc = cb + (sym & 7);
            let ins = INS_BASE[ic] as usize + r.read(INS_EXTRA[ic])? as usize;
            let copy = COPY_BASE[cc] as usize + r.read(COPY_EXTRA[cc])? as usize;
            if ins > end - out.len() {
                return Err(bad("brotli: insert runs past the end of the meta-block"));
            }
            for _ in 0..ins {
                lit_b.step(&mut r)?;
                let n = out.len();
                let p1 = if n >= 1 { out[n - 1] } else { 0 };
                let p2 = if n >= 2 { out[n - 2] } else { 0 };
                let mode = modes[lit_b.cur];
                let tree = lit_map[64 * lit_b.cur + context_id(mode, p1, p2)];
                let b = lit_codes[usize::from(tree)].decode(&mut r)?;
                out.push(b as u8);
            }
            if out.len() >= end {
                break;
            }
            let max_dist = window.min(out.len());
            let mut is_ref = true;
            let dist: i64 = if implicit {
                ring[(ring_pos + 3) & 3]
            } else {
                dist_b.step(&mut r)?;
                let ctx = copy.saturating_sub(2).min(3);
                let tree = dist_map[4 * dist_b.cur + ctx];
                let dcode = dist_codes[usize::from(tree)].decode(&mut r)?;
                if dcode < 16 {
                    let last = ring[(ring_pos + 3) & 3];
                    let second = ring[(ring_pos + 2) & 3];
                    const ADJ: [i64; 6] = [-1, 1, -2, 2, -3, 3];
                    let d = match dcode {
                        0 => last,
                        1 => second,
                        2 => ring[(ring_pos + 1) & 3],
                        3 => ring[ring_pos & 3],
                        4..=9 => last + ADJ[dcode - 4],
                        _ => second + ADJ[dcode - 10],
                    };
                    is_ref = dcode != 0;
                    d
                } else if dcode < 16 + ndirect {
                    (dcode - 15) as i64
                } else {
                    let k = dcode - ndirect - 16;
                    let nbits = 1 + (k >> (npostfix + 1)) as u32;
                    let hcode = k >> npostfix;
                    let lcode = k & ((1 << npostfix) - 1);
                    let offset = ((2 + (hcode & 1)) << nbits) - 4;
                    let extra = r.read(nbits)? as usize;
                    (((offset + extra) << npostfix) + lcode + ndirect + 1) as i64
                }
            };
            if dist <= 0 {
                return Err(bad("brotli: distance is not positive"));
            }
            if (dist as usize) <= max_dist {
                if is_ref && !implicit {
                    ring[ring_pos & 3] = dist;
                    ring_pos += 1;
                }
                if copy > end - out.len() {
                    return Err(bad("brotli: copy runs past the end of the meta-block"));
                }
                let d = dist as usize;
                let from = out.len() - d;
                if d >= copy {
                    out.extend_from_within(from..from + copy);
                } else {
                    for k in 0..copy {
                        out.push(out[from + k]);
                    }
                }
            } else {
                let word = dictionary_word(copy, dist as usize - max_dist - 1)?;
                if word.len() > end - out.len() {
                    return Err(bad(
                        "brotli: dictionary word runs past the end of the meta-block",
                    ));
                }
                out.extend_from_slice(&word);
            }
        }
        if last {
            break;
        }
    }
    Ok(out)
}
