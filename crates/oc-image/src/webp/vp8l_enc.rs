//! VP8L (WebP lossless) encoder: subtract-green, predictor, cross-colour and palette
//! transforms, LZ77 with a colour cache, and length-limited prefix codes.
use super::vp8l::{cache_hash, predict, predict_all, sub_size, DISTANCE_MAP};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

const NUM_LITERAL: usize = 256;
const NUM_LENGTH: usize = 24;
const NUM_DISTANCE: usize = 40;
const MAX_LEN_BITS: u8 = 15;
const WINDOW: usize = (1 << 20) - 120;
const MAX_MATCH: usize = 4096;
const MIN_MATCH: usize = 4;
const CODE_LENGTH_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

/// Knobs that scale with the `effort` option (0 fastest .. 6 smallest).
#[derive(Debug, Clone)]
pub(crate) struct Tune {
    effort: u8,
    modes: Vec<u8>,
    cross_color: bool,
    chain: usize,
    lazy: bool,
    dp: bool,
    max_groups: usize,
    cache_tries: Vec<u32>,
}

impl Tune {
    pub fn new(effort: u8) -> Self {
        let all: Vec<u8> = (0..14).collect();
        match effort {
            0 => Self {
                effort,
                modes: vec![11],
                cross_color: false,
                chain: 4,
                lazy: false,
                dp: false,
                max_groups: 1,
                cache_tries: vec![0],
            },
            1 => Self {
                effort,
                modes: vec![1, 2, 11, 12],
                cross_color: false,
                chain: 8,
                lazy: false,
                dp: false,
                max_groups: 1,
                cache_tries: vec![0],
            },
            2 => Self {
                effort,
                modes: all,
                cross_color: false,
                chain: 16,
                lazy: false,
                dp: false,
                max_groups: 1,
                cache_tries: vec![0, 6],
            },
            3 => Self {
                effort,
                modes: all,
                cross_color: true,
                chain: 24,
                lazy: false,
                dp: true,
                max_groups: 128,
                cache_tries: vec![0, 4, 8],
            },
            4 => Self {
                effort,
                modes: all,
                cross_color: true,
                chain: 48,
                lazy: true,
                dp: true,
                max_groups: 128,
                cache_tries: vec![0, 2, 4, 6, 8, 10],
            },
            5 => Self {
                effort,
                modes: all,
                cross_color: true,
                chain: 128,
                lazy: true,
                dp: true,
                max_groups: 128,
                cache_tries: vec![0, 2, 4, 6, 8, 10],
            },
            _ => Self {
                effort: 6,
                modes: all,
                cross_color: true,
                chain: 192,
                lazy: true,
                dp: true,
                max_groups: 128,
                cache_tries: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            },
        }
    }
}

// ---------------------------------------------------------------------------------
// Bit writer

#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    /// Append the low `n` (0..=32) bits of `v`.
    #[inline]
    fn put(&mut self, v: u32, n: u32) {
        self.acc |= u64::from(v) << self.nbits;
        self.nbits += n;
        if self.nbits >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.nbits -= 32;
        }
    }
    fn finish(mut self) -> Vec<u8> {
        while self.nbits > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.nbits = self.nbits.saturating_sub(8);
        }
        self.out
    }
}

// ---------------------------------------------------------------------------------
// Prefix codes

/// Optimal code lengths limited to `limit` bits. A lone symbol gets length 1.
fn code_lengths(counts: &[u32], limit: u8) -> Vec<u8> {
    let used: Vec<usize> = counts
        .iter()
        .enumerate()
        .filter(|(_, &c)| c > 0)
        .map(|(i, _)| i)
        .collect();
    let mut lengths = vec![0u8; counts.len()];
    if used.len() <= 1 {
        if let Some(&s) = used.first() {
            if let Some(l) = lengths.get_mut(s) {
                *l = 1;
            }
        }
        return lengths;
    }
    let mut count_min = 1u64;
    loop {
        // Nodes 0..n are leaves, later ones are internal. `children` holds (left, right).
        let n = used.len();
        let mut heap: BinaryHeap<Reverse<(u64, usize)>> = used
            .iter()
            .enumerate()
            .map(|(k, &s)| {
                Reverse((
                    u64::from(counts.get(s).copied().unwrap_or(1)).max(count_min),
                    k,
                ))
            })
            .collect();
        let mut children: Vec<(usize, usize)> = Vec::with_capacity(n);
        let mut next = n;
        while heap.len() > 1 {
            let (Some(Reverse((wa, a))), Some(Reverse((wb, b)))) = (heap.pop(), heap.pop()) else {
                break;
            };
            children.push((a, b));
            heap.push(Reverse((wa + wb, next)));
            next += 1;
        }
        // Depths, walking the internal nodes from the root down.
        let mut depth = vec![0u8; next];
        let mut max_depth = 0u8;
        for (k, &(a, b)) in children.iter().enumerate().rev() {
            let d = depth.get(n + k).copied().unwrap_or(0).saturating_add(1);
            for c in [a, b] {
                if let Some(slot) = depth.get_mut(c) {
                    *slot = d;
                }
            }
            max_depth = max_depth.max(d);
        }
        if max_depth <= limit {
            for (k, &s) in used.iter().enumerate() {
                if let Some(l) = lengths.get_mut(s) {
                    *l = depth.get(k).copied().unwrap_or(1);
                }
            }
            return lengths;
        }
        count_min *= 2;
    }
}

/// A canonical code ready for writing. Codes are bit-reversed for LSB-first output.
struct Code {
    lengths: Vec<u8>,
    /// `length << 16 | reversed code`; zero length for a lone symbol.
    packed: Vec<u32>,
}

impl Code {
    #[allow(clippy::indexing_slicing)] // code lengths are at most 15, arrays have 16/17 slots
    fn new(lengths: Vec<u8>) -> Self {
        let used = lengths.iter().filter(|&&l| l != 0).count();
        let mut count = [0u32; 16];
        for &l in &lengths {
            if let Some(c) = count.get_mut(usize::from(l)) {
                *c += 1;
            }
        }
        count[0] = 0;
        let mut next = [0u32; 17];
        let mut code = 0u32;
        for l in 1..=15 {
            code = (code + count[l - 1]) << 1;
            next[l] = code;
        }
        let mut codes = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let c = next[usize::from(l)];
                next[usize::from(l)] += 1;
                if let Some(o) = codes.get_mut(s) {
                    *o = (c.reverse_bits() >> (32 - u32::from(l))) as u16;
                }
            }
        }
        let packed = if used <= 1 {
            vec![0; lengths.len()]
        } else {
            lengths
                .iter()
                .zip(&codes)
                .map(|(&l, &c)| (u32::from(l) << 16) | u32::from(c))
                .collect()
        };
        Self { lengths, packed }
    }

    /// The (bits, length) pair for `sym`, packed as `length << 16 | bits`.
    #[inline]
    fn get(&self, sym: usize) -> u32 {
        self.packed.get(sym).copied().unwrap_or(0)
    }

    #[inline]
    fn put(&self, bw: &mut BitWriter, sym: usize) {
        let e = self.get(sym);
        bw.put(e & 0xffff, e >> 16);
    }
}

/// Run-length tokens for a list of code lengths: (code length symbol, extra bits value).
fn length_tokens(lengths: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut prev = 8u8;
    let mut i = 0;
    while i < lengths.len() {
        let value = lengths.get(i).copied().unwrap_or(0);
        let mut reps = 1;
        while lengths.get(i + reps) == Some(&value) {
            reps += 1;
        }
        i += reps;
        if value == 0 {
            while reps >= 1 {
                if reps < 3 {
                    out.extend(std::iter::repeat_n((0u8, 0u8), reps));
                    break;
                } else if reps < 11 {
                    out.push((17, (reps - 3) as u8));
                    break;
                } else if reps < 139 {
                    out.push((18, (reps - 11) as u8));
                    break;
                }
                out.push((18, 127));
                reps -= 138;
            }
        } else {
            if value != prev {
                out.push((value, 0));
                reps -= 1;
            }
            while reps >= 1 {
                if reps < 3 {
                    out.extend(std::iter::repeat_n((value, 0u8), reps));
                    break;
                } else if reps < 7 {
                    out.push((16, (reps - 3) as u8));
                    break;
                }
                out.push((16, 3));
                reps -= 6;
            }
            prev = value;
        }
    }
    out
}

fn write_code(bw: &mut BitWriter, lengths: &[u8]) {
    let used: Vec<usize> = lengths
        .iter()
        .enumerate()
        .filter(|(_, &l)| l != 0)
        .map(|(i, _)| i)
        .collect();
    if used.len() <= 2 && used.iter().all(|&s| s < 256) {
        bw.put(1, 1);
        match used.as_slice() {
            [] => {
                // One symbol, value 0: costs no bits per use.
                bw.put(0, 1);
                bw.put(0, 1);
                bw.put(0, 1);
            }
            [a, rest @ ..] => {
                bw.put(rest.len() as u32, 1);
                if *a < 2 {
                    bw.put(0, 1);
                    bw.put(*a as u32, 1);
                } else {
                    bw.put(1, 1);
                    bw.put(*a as u32, 8);
                }
                if let Some(b) = rest.first() {
                    bw.put(*b as u32, 8);
                }
            }
        }
        return;
    }
    bw.put(0, 1);
    let tokens = length_tokens(lengths);
    let mut hist = [0u32; 19];
    for &(t, _) in &tokens {
        if let Some(h) = hist.get_mut(usize::from(t)) {
            *h += 1;
        }
    }
    let cl = Code::new(code_lengths(&hist, 7));
    let mut num = 19;
    while num > 4 {
        let slot = CODE_LENGTH_ORDER.get(num - 1).copied().unwrap_or(0);
        if cl.lengths.get(slot).copied().unwrap_or(0) != 0 {
            break;
        }
        num -= 1;
    }
    bw.put((num - 4) as u32, 4);
    for &slot in CODE_LENGTH_ORDER.iter().take(num) {
        bw.put(u32::from(cl.lengths.get(slot).copied().unwrap_or(0)), 3);
    }
    // Trailing zero lengths need no tokens when enough of them can be skipped.
    let mut trimmed = tokens.len();
    let mut trailing_bits = 0u32;
    while trimmed > 0 {
        match tokens.get(trimmed - 1) {
            Some(&(0, _)) => trailing_bits += 1,
            Some(&(17, _)) => trailing_bits += 3,
            Some(&(18, _)) => trailing_bits += 7,
            _ => break,
        }
        trimmed -= 1;
    }
    let trim = trimmed > 1 && trailing_bits > 12;
    let count = if trim { trimmed } else { tokens.len() };
    if trim {
        bw.put(1, 1);
        let nbits = if count <= 2 {
            0
        } else {
            u32::BITS - 1 - ((count - 2) as u32).leading_zeros()
        };
        let pairs = nbits / 2 + 1;
        bw.put(pairs - 1, 3);
        bw.put((count - 2) as u32, pairs * 2);
    } else {
        bw.put(0, 1);
    }
    for &(t, extra) in tokens.iter().take(count) {
        cl.put(bw, usize::from(t));
        match t {
            16 => bw.put(u32::from(extra), 2),
            17 => bw.put(u32::from(extra), 3),
            18 => bw.put(u32::from(extra), 7),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------
// Tokens and histograms

#[derive(Clone, Copy, Debug)]
enum Token {
    Literal(u32),
    Cache(u32),
    /// Length, distance code (already mapped through the 2-D neighbourhood table).
    Copy(u16, u32),
}

/// (prefix symbol, extra bit count, extra bits value) for a value >= 1.
#[inline]
fn prefix_encode(v: u32) -> (u32, u32, u32) {
    let d = v - 1;
    if d < 4 {
        return (d, 0, 0);
    }
    let hb = 31 - d.leading_zeros();
    let second = (d >> (hb - 1)) & 1;
    let extra = hb - 1;
    (2 * hb + second, extra, d & ((1 << extra) - 1))
}

struct PlaneCodes {
    lut: [u8; 128],
}

impl PlaneCodes {
    fn new() -> Self {
        let mut lut = [0u8; 128];
        for (i, &(dx, dy)) in DISTANCE_MAP.iter().enumerate() {
            let packed = (i32::from(dy) * 16 + 8 - i32::from(dx)) as usize;
            if let Some(slot) = lut.get_mut(packed) {
                *slot = i as u8;
            }
        }
        Self { lut }
    }

    fn code(&self, xsize: usize, dist: usize) -> u32 {
        let yoff = dist / xsize;
        let xoff = dist - yoff * xsize;
        let get = |i: usize| u32::from(self.lut.get(i).copied().unwrap_or(0)) + 1;
        if xoff <= 8 && yoff < 8 {
            get(yoff * 16 + 8 - xoff)
        } else if xoff + 8 > xsize && yoff < 7 {
            get((yoff + 1) * 16 + 8 + (xsize - xoff))
        } else {
            dist as u32 + 120
        }
    }
}

struct Histograms {
    green: Vec<u32>,
    red: Vec<u32>,
    blue: Vec<u32>,
    alpha: Vec<u32>,
    dist: Vec<u32>,
    extra_bits: u64,
}

impl Histograms {
    fn new(cache_bits: u32) -> Self {
        let cache = if cache_bits > 0 {
            1usize << cache_bits
        } else {
            0
        };
        Self {
            green: vec![0; NUM_LITERAL + NUM_LENGTH + cache],
            red: vec![0; NUM_LITERAL],
            blue: vec![0; NUM_LITERAL],
            alpha: vec![0; NUM_LITERAL],
            dist: vec![0; NUM_DISTANCE],
            extra_bits: 0,
        }
    }

    fn add(&mut self, t: Token) {
        fn bump(v: &mut [u32], i: usize) {
            if let Some(c) = v.get_mut(i) {
                *c += 1;
            }
        }
        match t {
            Token::Literal(p) => {
                bump(&mut self.green, ((p >> 8) & 0xff) as usize);
                bump(&mut self.red, ((p >> 16) & 0xff) as usize);
                bump(&mut self.blue, (p & 0xff) as usize);
                bump(&mut self.alpha, (p >> 24) as usize);
            }
            Token::Cache(i) => bump(&mut self.green, NUM_LITERAL + NUM_LENGTH + i as usize),
            Token::Copy(len, dcode) => {
                let (ls, le, _) = prefix_encode(u32::from(len));
                let (ds, de, _) = prefix_encode(dcode);
                bump(&mut self.green, NUM_LITERAL + ls as usize);
                bump(&mut self.dist, ds as usize);
                self.extra_bits += u64::from(le + de);
            }
        }
    }

    fn from_tokens(tokens: &[Token], cache_bits: u32) -> Self {
        let mut h = Self::new(cache_bits);
        for &t in tokens {
            h.add(t);
        }
        h
    }

    /// Shannon estimate of the coded size in bits.
    fn estimate(&self) -> f64 {
        [&self.green, &self.red, &self.blue, &self.alpha, &self.dist]
            .iter()
            .map(|h| entropy_bits(h))
            .sum::<f64>()
            + self.extra_bits as f64
    }
}

fn entropy_bits(h: &[u32]) -> f64 {
    let total: f64 = h.iter().map(|&c| f64::from(c)).sum();
    if total == 0.0 {
        return 0.0;
    }
    let sum: f64 = h
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| f64::from(c) * f64::from(c).log2())
        .sum();
    total * total.log2() - sum
}

// ---------------------------------------------------------------------------------
// LZ77

struct Matcher<'a> {
    argb: &'a [u32],
    head: Vec<u32>,
    prev: Vec<u32>,
    chain: usize,
}

const HASH_BITS: u32 = 18;
const NIL: u32 = u32::MAX;

impl<'a> Matcher<'a> {
    fn new(argb: &'a [u32], chain: usize) -> Self {
        Self {
            argb,
            head: vec![NIL; 1 << HASH_BITS],
            prev: vec![NIL; argb.len()],
            chain,
        }
    }

    #[inline]
    fn hash(&self, i: usize) -> usize {
        let a = self.argb.get(i).copied().unwrap_or(0);
        let b = self.argb.get(i + 1).copied().unwrap_or(0);
        ((a.wrapping_mul(0x1e35_a7bd) ^ b.wrapping_mul(0x9e37_79b1).rotate_left(11))
            .wrapping_mul(0x85eb_ca6b)
            >> (32 - HASH_BITS)) as usize
    }

    #[inline]
    fn insert(&mut self, i: usize) {
        if i + 1 < self.argb.len() {
            let h = self.hash(i);
            if let (Some(hd), Some(p)) = (self.head.get_mut(h), self.prev.get_mut(i)) {
                *p = *hd;
                *hd = i as u32;
            }
        }
    }

    fn match_len(&self, i: usize, j: usize, limit: usize) -> usize {
        let (Some(a), Some(b)) = (self.argb.get(i..), self.argb.get(j..)) else {
            return 0;
        };
        a.iter()
            .zip(b)
            .take(limit)
            .take_while(|(x, y)| x == y)
            .count()
    }

    /// Longest earlier match for position `i`; ties prefer the cheaper distance code.
    fn find(&self, i: usize, xsize: usize, planes: &PlaneCodes) -> (usize, usize) {
        let limit = (self.argb.len() - i).min(MAX_MATCH);
        if limit < MIN_MATCH {
            return (0, 0);
        }
        let mut best = (0usize, 0usize, u32::MAX);
        let consider = |len: usize, dist: usize, best: &mut (usize, usize, u32)| {
            if len < MIN_MATCH {
                return;
            }
            if len > best.0 || (len == best.0 && len > 0) {
                let code = planes.code(xsize, dist);
                if len > best.0 || code < best.2 {
                    *best = (len, dist, code);
                }
            }
        };
        // The two most common distances: the pixel to the left and the one above.
        if i >= 1 {
            let l = self.match_len(i, i - 1, limit);
            consider(l, 1, &mut best);
        }
        if i >= xsize {
            let l = self.match_len(i, i - xsize, limit);
            consider(l, xsize, &mut best);
        }
        if i + 1 < self.argb.len() {
            let mut cand = self.head.get(self.hash(i)).copied().unwrap_or(NIL);
            let mut steps = self.chain;
            while cand != NIL && steps > 0 {
                let c = cand as usize;
                let dist = i - c;
                if dist > WINDOW {
                    break;
                }
                // Only a longer match is interesting; check the byte that would extend it.
                if best.0 == 0
                    || self.argb.get(c + best.0.min(limit - 1))
                        == self.argb.get(i + best.0.min(limit - 1))
                {
                    let l = self.match_len(i, c, limit);
                    consider(l, dist, &mut best);
                    if l >= limit {
                        break;
                    }
                }
                cand = self.prev.get(c).copied().unwrap_or(NIL);
                steps -= 1;
            }
        }
        (best.0, best.1)
    }
}

/// The longest earlier match at every position (length 0 when there is none).
struct Matches {
    len: Vec<u16>,
    dist: Vec<u32>,
}

fn find_matches(argb: &[u32], xsize: usize, tune: &Tune, planes: &PlaneCodes) -> Matches {
    let n = argb.len();
    let mut m = Matcher::new(argb, tune.chain);
    let mut out = Matches {
        len: vec![0; n],
        dist: vec![0; n],
    };
    for i in 0..n {
        let (l, d) = m.find(i, xsize, planes);
        m.insert(i);
        if l >= MIN_MATCH {
            if let (Some(a), Some(b)) = (out.len.get_mut(i), out.dist.get_mut(i)) {
                *a = l as u16;
                *b = d as u32;
            }
        }
    }
    out
}

/// Greedy parse (optionally one step lazy) over the match table.
fn lz77(
    argb: &[u32],
    xsize: usize,
    tune: &Tune,
    planes: &PlaneCodes,
    matches: &Matches,
) -> Vec<Token> {
    let n = argb.len();
    let mut tokens = Vec::with_capacity(n / 2 + 16);
    let mut i = 0;
    while i < n {
        let len = usize::from(matches.len.get(i).copied().unwrap_or(0));
        if len >= MIN_MATCH {
            if tune.lazy && usize::from(matches.len.get(i + 1).copied().unwrap_or(0)) > len + 1 {
                tokens.push(Token::Literal(argb.get(i).copied().unwrap_or(0)));
                i += 1;
                continue;
            }
            let dist = matches.dist.get(i).copied().unwrap_or(1) as usize;
            tokens.push(Token::Copy(len as u16, planes.code(xsize, dist)));
            i += len;
        } else {
            tokens.push(Token::Literal(argb.get(i).copied().unwrap_or(0)));
            i += 1;
        }
    }
    tokens
}

/// Bit costs per symbol, derived from the histograms of an earlier parse.
struct CostModel {
    green: Vec<f32>,
    red: Vec<f32>,
    blue: Vec<f32>,
    alpha: Vec<f32>,
    dist: Vec<f32>,
    length: Vec<f32>,
}

fn symbol_costs(h: &[u32]) -> Vec<f32> {
    let total: u64 = h.iter().map(|&c| u64::from(c)).sum();
    let log_total = if total == 0 {
        0.0
    } else {
        (total as f32).log2()
    };
    h.iter()
        .map(|&c| {
            if c == 0 {
                log_total + 6.0
            } else {
                log_total - (c as f32).log2()
            }
        })
        .collect()
}

impl CostModel {
    fn new(h: &Histograms) -> Self {
        let green = symbol_costs(&h.green);
        let length = (0..=MAX_MATCH as u32)
            .map(|l| {
                if l == 0 {
                    return 0.0;
                }
                let (s, e, _) = prefix_encode(l);
                green.get(NUM_LITERAL + s as usize).copied().unwrap_or(0.0) + e as f32
            })
            .collect();
        Self {
            green,
            red: symbol_costs(&h.red),
            blue: symbol_costs(&h.blue),
            alpha: symbol_costs(&h.alpha),
            dist: symbol_costs(&h.dist),
            length,
        }
    }

    fn literal(&self, p: u32) -> f32 {
        let at = |v: &[f32], i: u32| v.get(i as usize).copied().unwrap_or(0.0);
        at(&self.green, (p >> 8) & 0xff)
            + at(&self.red, (p >> 16) & 0xff)
            + at(&self.blue, p & 0xff)
            + at(&self.alpha, p >> 24)
    }

    fn distance(&self, dcode: u32) -> f32 {
        let (s, e, _) = prefix_encode(dcode);
        self.dist.get(s as usize).copied().unwrap_or(0.0) + e as f32
    }
}

/// Cheapest parse under `model` by dynamic programming over positions. The colour cache
/// is path independent (every pixel enters it in order), so hits are known per position.
fn dp_parse(
    argb: &[u32],
    xsize: usize,
    planes: &PlaneCodes,
    matches: &Matches,
    cache_bits: u32,
    model: &CostModel,
) -> Vec<Token> {
    let n = argb.len();
    let mut cost = vec![f32::MAX; n + 1];
    // (token length ending here, distance code of that token or 0 for a literal)
    let mut step: Vec<(u16, u32)> = vec![(1, 0); n + 1];
    if let Some(c) = cost.first_mut() {
        *c = 0.0;
    }
    let mut cache = vec![0u32; if cache_bits > 0 { 1 << cache_bits } else { 0 }];
    for i in 0..n {
        let base = cost.get(i).copied().unwrap_or(f32::MAX);
        let px = argb.get(i).copied().unwrap_or(0);
        let mut lit = model.literal(px);
        if cache_bits > 0 {
            let k = cache_hash(px, cache_bits);
            if cache.get(k) == Some(&px) {
                lit = lit.min(
                    model
                        .green
                        .get(NUM_LITERAL + NUM_LENGTH + k)
                        .copied()
                        .unwrap_or(lit),
                );
            }
        }
        if let Some(c) = cost.get_mut(i + 1) {
            if base + lit < *c {
                *c = base + lit;
                if let Some(s) = step.get_mut(i + 1) {
                    *s = (1, 0);
                }
            }
        }
        let len = usize::from(matches.len.get(i).copied().unwrap_or(0));
        let dist = matches.dist.get(i).copied().unwrap_or(1) as usize;
        if cache_bits > 0 {
            if let Some(c) = cache.get_mut(cache_hash(px, cache_bits)) {
                *c = px;
            }
        }
        if len >= MIN_MATCH {
            let dcode = planes.code(xsize, dist);
            let dc = model.distance(dcode);
            // Every shorter length of the same match, then (for long ones) only the full one.
            let (lo, hi) = if len <= 160 {
                (MIN_MATCH, len)
            } else {
                (len, len)
            };
            for k in lo..=hi {
                let total = base + dc + model.length.get(k).copied().unwrap_or(0.0);
                if let Some(c) = cost.get_mut(i + k) {
                    if total < *c {
                        *c = total;
                        if let Some(s) = step.get_mut(i + k) {
                            *s = (k as u16, dcode);
                        }
                    }
                }
            }
        }
    }
    let mut tokens = Vec::new();
    let mut pos = n;
    while pos > 0 {
        let (len, dcode) = step.get(pos).copied().unwrap_or((1, 0));
        let len = usize::from(len).max(1).min(pos);
        if dcode == 0 {
            tokens.push(Token::Literal(argb.get(pos - 1).copied().unwrap_or(0)));
            pos -= 1;
        } else {
            tokens.push(Token::Copy(len as u16, dcode));
            pos -= len;
        }
    }
    tokens.reverse();
    tokens
}

/// Replace literals with colour cache hits.
fn apply_cache(argb: &[u32], tokens: &[Token], bits: u32) -> Vec<Token> {
    let mut cache = vec![0u32; 1 << bits];
    let mut out = Vec::with_capacity(tokens.len());
    let mut pos = 0usize;
    for &t in tokens {
        match t {
            Token::Literal(p) => {
                let k = cache_hash(p, bits);
                if cache.get(k) == Some(&p) {
                    out.push(Token::Cache(k as u32));
                } else {
                    out.push(Token::Literal(p));
                    if let Some(c) = cache.get_mut(k) {
                        *c = p;
                    }
                }
                pos += 1;
            }
            Token::Copy(len, _) => {
                if let Some(run) = argb.get(pos..pos + usize::from(len)) {
                    for &p in run {
                        if let Some(c) = cache.get_mut(cache_hash(p, bits)) {
                            *c = p;
                        }
                    }
                }
                pos += usize::from(len);
                out.push(t);
            }
            Token::Cache(_) => {
                pos += 1;
                out.push(t);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------
// Meta prefix image: several prefix code groups chosen per tile

/// Rough cost in bits of writing a prefix code with `nnz` used symbols.
fn header_bits(nnz: u32) -> f64 {
    match nnz {
        0 => 4.0,
        1 => 7.0,
        2 => 16.0,
        n => 30.0 + 3.8 * f64::from(n),
    }
}

/// Symbol index space shared by all components: green(+length+cache), red, blue, alpha, distance.
#[derive(Clone, Copy)]
struct Space {
    bounds: [usize; 6],
}

impl Space {
    fn new(cache_bits: u32) -> Self {
        let g = NUM_LITERAL + NUM_LENGTH + if cache_bits > 0 { 1 << cache_bits } else { 0 };
        Self {
            bounds: [0, g, g + 256, g + 512, g + 768, g + 768 + NUM_DISTANCE],
        }
    }
    fn size(&self) -> usize {
        self.bounds[5]
    }
    #[allow(clippy::indexing_slicing)] // `c` ranges over the six bounds
    fn comp(&self, sym: usize) -> usize {
        (1..6).find(|&c| sym < self.bounds[c]).map_or(4, |c| c - 1)
    }
}

/// Visit every symbol a token codes.
fn token_symbols(t: Token, sp: &Space, mut f: impl FnMut(usize)) {
    let g = sp.bounds[1];
    match t {
        Token::Literal(p) => {
            f(((p >> 8) & 0xff) as usize);
            f(g + ((p >> 16) & 0xff) as usize);
            f(g + 256 + (p & 0xff) as usize);
            f(g + 512 + (p >> 24) as usize);
        }
        Token::Cache(i) => f(NUM_LITERAL + NUM_LENGTH + i as usize),
        Token::Copy(len, dcode) => {
            f(NUM_LITERAL + prefix_encode(u32::from(len)).0 as usize);
            f(g + 768 + prefix_encode(dcode).0 as usize);
        }
    }
}

#[derive(Clone)]
struct Cluster {
    hist: Vec<u32>,
    tot: [u32; 5],
    nnz: [u32; 5],
    bits: f64,
}

impl Cluster {
    fn empty(sp: &Space) -> Self {
        Self {
            hist: vec![0; sp.size()],
            tot: [0; 5],
            nnz: [0; 5],
            bits: 0.0,
        }
    }

    #[allow(clippy::indexing_slicing, clippy::needless_range_loop)] // components are 0..5, the size of every array
    fn recompute(&mut self, sp: &Space, t: &NLogN) {
        self.tot = [0; 5];
        self.nnz = [0; 5];
        let mut flog = [0.0f64; 5];
        for c in 0..5 {
            for &n in self.hist.get(sp.bounds[c]..sp.bounds[c + 1]).unwrap_or(&[]) {
                if n > 0 {
                    self.tot[c] += n;
                    self.nnz[c] += 1;
                    flog[c] += t.get(n);
                }
            }
        }
        self.bits = (0..5)
            .map(|c| t.get(self.tot[c]) - flog[c] + header_bits(self.nnz[c]))
            .sum();
    }

    fn add_tile(&mut self, tile: &[(u32, u32)], sp: &Space) {
        for &(s, n) in tile {
            if let Some(h) = self.hist.get_mut(s as usize) {
                *h += n;
            }
            let _ = sp;
        }
    }
}

struct TileHist2 {
    syms: Vec<(u32, u32)>,
    tot: [u32; 5],
}

/// Bits added by merging a tile into a cluster.
#[allow(clippy::indexing_slicing, clippy::needless_range_loop)] // components are 0..5, the size of every array
fn add_cost(c: &Cluster, tile: &TileHist2, sp: &Space, t: &NLogN) -> f64 {
    let mut d = 0.0;
    let mut new_nnz = [0u32; 5];
    for &(s, n) in &tile.syms {
        let a = c.hist.get(s as usize).copied().unwrap_or(0);
        if a == 0 {
            new_nnz[sp.comp(s as usize)] += 1;
        }
        d -= t.get(a + n) - t.get(a);
    }
    for k in 0..5 {
        d += t.get(c.tot[k] + tile.tot[k]) - t.get(c.tot[k]);
        d += header_bits(c.nnz[k] + new_nnz[k]) - header_bits(c.nnz[k]);
    }
    d
}

/// Group the tiles' statistics into a few prefix code groups. Returns the group of every
/// tile and the number of groups; a single group means no meta prefix image is needed.
#[allow(clippy::indexing_slicing)] // components are 0..5, the size of every array
fn cluster_tiles(tiles: &[TileHist2], sp: &Space, max_groups: usize) -> (Vec<u32>, usize) {
    let t = NLogN::new();
    let n = tiles.len();
    let single = |_: ()| (vec![0u32; n], 1usize);
    if n < 2 {
        return single(());
    }
    // 1. Online assignment in raster order: join the cheapest cluster or start a new one.
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut assign = vec![0usize; n];
    for (i, tile) in tiles.iter().enumerate() {
        let mut best: Option<(usize, f64)> = None;
        for (k, c) in clusters.iter().enumerate() {
            let d = add_cost(c, tile, sp, &t);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((k, d));
            }
        }
        let mut alone = Cluster::empty(sp);
        alone.add_tile(&tile.syms, sp);
        alone.recompute(sp, &t);
        // A new cluster also costs a little in the entropy image.
        let start_new = match best {
            Some((_, d)) => alone.bits + 4.0 < d && clusters.len() < max_groups,
            None => true,
        };
        if start_new {
            if let Some(a) = assign.get_mut(i) {
                *a = clusters.len();
            }
            clusters.push(alone);
        } else if let Some((k, _)) = best {
            if let Some(a) = assign.get_mut(i) {
                *a = k;
            }
            if let Some(c) = clusters.get_mut(k) {
                c.add_tile(&tile.syms, sp);
                c.recompute(sp, &t);
            }
        }
    }
    // 2. Refinement: reassign tiles to the cluster whose statistics code them best.
    for _ in 0..2 {
        let costs: Vec<Vec<f32>> = clusters
            .iter()
            .map(|c| {
                let mut v = vec![0f32; sp.size()];
                for k in 0..5 {
                    let total = f64::from(c.tot[k].max(1));
                    for s in sp.bounds[k]..sp.bounds[k + 1] {
                        let a = f64::from(c.hist.get(s).copied().unwrap_or(0));
                        let bits = if a > 0.0 {
                            (total / a).log2()
                        } else {
                            total.log2() + 8.0
                        };
                        if let Some(x) = v.get_mut(s) {
                            *x = bits as f32;
                        }
                    }
                }
                v
            })
            .collect();
        let mut changed = false;
        for (i, tile) in tiles.iter().enumerate() {
            let mut best = (f32::MAX, 0usize);
            for (k, cost) in costs.iter().enumerate() {
                let mut bits = 0f32;
                for &(s, nn) in &tile.syms {
                    bits += cost.get(s as usize).copied().unwrap_or(0.0) * nn as f32;
                }
                if bits < best.0 {
                    best = (bits, k);
                }
            }
            if assign.get(i).copied() != Some(best.1) {
                changed = true;
                if let Some(a) = assign.get_mut(i) {
                    *a = best.1;
                }
            }
        }
        // Rebuild, dropping empty clusters.
        let mut rebuilt: Vec<Cluster> = (0..clusters.len()).map(|_| Cluster::empty(sp)).collect();
        for (i, tile) in tiles.iter().enumerate() {
            if let Some(c) = assign.get(i).and_then(|&k| rebuilt.get_mut(k)) {
                c.add_tile(&tile.syms, sp);
            }
        }
        let mut remap = vec![usize::MAX; rebuilt.len()];
        let mut kept = Vec::new();
        for (k, mut c) in rebuilt.into_iter().enumerate() {
            if c.hist.iter().any(|&h| h > 0) {
                c.recompute(sp, &t);
                if let Some(r) = remap.get_mut(k) {
                    *r = kept.len();
                }
                kept.push(c);
            }
        }
        for a in assign.iter_mut() {
            *a = remap.get(*a).copied().unwrap_or(0);
        }
        clusters = kept;
        if !changed {
            break;
        }
    }
    // 3. Greedy pairwise merging while it saves bits (pair costs are cached).
    let m = clusters.len();
    let mut alive: Vec<Option<Cluster>> = clusters.into_iter().map(Some).collect();
    let mut owner: Vec<usize> = (0..m).collect();
    let pair_delta = |ca: &Cluster, cb: &Cluster| -> f64 {
        let mut merged = ca.clone();
        for (h, &x) in merged.hist.iter_mut().zip(&cb.hist) {
            *h += x;
        }
        merged.recompute(sp, &t);
        // The entropy image gets slightly cheaper with fewer groups.
        merged.bits - ca.bits - cb.bits - 2.0
    };
    let mut delta = vec![f64::MAX; m * m];
    for a in 0..m {
        for b in a + 1..m {
            if let (Some(Some(ca)), Some(Some(cb))) = (alive.get(a), alive.get(b)) {
                if let Some(d) = delta.get_mut(a * m + b) {
                    *d = pair_delta(ca, cb);
                }
            }
        }
    }
    loop {
        let mut best: Option<(f64, usize, usize)> = None;
        for a in 0..m {
            for b in a + 1..m {
                let d = delta.get(a * m + b).copied().unwrap_or(f64::MAX);
                if d < 0.0 && best.is_none_or(|(bd, _, _)| d < bd) {
                    best = Some((d, a, b));
                }
            }
        }
        let Some((_, a, b)) = best else { break };
        let Some(Some(cb)) = alive.get_mut(b).map(Option::take) else {
            break;
        };
        if let Some(Some(ca)) = alive.get_mut(a) {
            for (h, &x) in ca.hist.iter_mut().zip(&cb.hist) {
                *h += x;
            }
            ca.recompute(sp, &t);
        }
        for o in owner.iter_mut() {
            if *o == b {
                *o = a;
            }
        }
        for x in 0..m {
            if x == b {
                continue;
            }
            if let Some(d) = delta.get_mut(x.min(b) * m + x.max(b)) {
                *d = f64::MAX;
            }
            if x != a {
                if let (Some(Some(ca)), Some(Some(cx))) = (alive.get(a), alive.get(x)) {
                    let d = pair_delta(ca, cx);
                    if let Some(slot) = delta.get_mut(a.min(x) * m + a.max(x)) {
                        *slot = d;
                    }
                }
            }
        }
    }
    // Number the surviving clusters.
    let mut number = vec![u32::MAX; alive.len()];
    let mut count = 0u32;
    for (i, c) in alive.iter().enumerate() {
        if c.is_some() {
            if let Some(x) = number.get_mut(i) {
                *x = count;
            }
            count += 1;
        }
    }
    let groups: Vec<u32> = assign
        .iter()
        .map(|&k| {
            let rep = owner.get(k).copied().unwrap_or(0);
            number.get(rep).copied().unwrap_or(0)
        })
        .collect();
    (groups, count as usize)
}

/// Per-tile symbol statistics of a token stream.
#[allow(clippy::indexing_slicing)] // components are 0..5, the size of every array
fn tile_histograms(tokens: &[Token], w: usize, h: usize, bits: u32, sp: &Space) -> Vec<TileHist2> {
    let tw = sub_size(w as u32, bits) as usize;
    let th = sub_size(h as u32, bits) as usize;
    let mut lists: Vec<Vec<u32>> = vec![Vec::new(); tw * th];
    let (mut x, mut y) = (0usize, 0usize);
    for &t in tokens {
        let idx = (y >> bits) * tw + (x >> bits);
        if let Some(l) = lists.get_mut(idx) {
            token_symbols(t, sp, |s| l.push(s as u32));
        }
        let len = match t {
            Token::Copy(l, _) => usize::from(l),
            _ => 1,
        };
        x += len;
        while x >= w {
            x -= w;
            y += 1;
        }
    }
    lists
        .into_iter()
        .map(|mut l| {
            l.sort_unstable();
            let mut syms: Vec<(u32, u32)> = Vec::new();
            let mut tot = [0u32; 5];
            for s in l {
                tot[sp.comp(s as usize)] += 1;
                match syms.last_mut() {
                    Some((ls, n)) if *ls == s => *n += 1,
                    _ => syms.push((s, 1)),
                }
            }
            TileHist2 { syms, tot }
        })
        .collect()
}

// ---------------------------------------------------------------------------------
// Entropy-coded image

/// Write one entropy-coded image (colour cache info, prefix codes, data). `meta_bits` > 0
/// allows several prefix code groups over tiles of `1 << meta_bits` pixels.
fn write_image(
    bw: &mut BitWriter,
    argb: &[u32],
    xsize: usize,
    tune: &Tune,
    planes: &PlaneCodes,
    meta_bits: u32,
    sub: bool,
) {
    let matches = find_matches(argb, xsize, tune, planes);
    let base = lz77(argb, xsize, tune, planes, &matches);
    let mut best: (u32, Vec<Token>, f64) = (0, base.clone(), f64::MAX);
    let tries: &[u32] = if sub { &[0] } else { &tune.cache_tries };
    for &bits in tries {
        let tokens = if bits == 0 {
            base.clone()
        } else {
            apply_cache(argb, &base, bits)
        };
        let est = Histograms::from_tokens(&tokens, bits).estimate()
            // The cache enlarges the green alphabet; charge its code header roughly.
            + if bits > 0 { f64::from(1u32 << bits) * 0.05 } else { 0.0 };
        if est < best.2 {
            best = (bits, tokens, est);
        }
    }
    let (cache_bits, mut tokens, est) = best;
    if tune.dp && !sub {
        let model = CostModel::new(&Histograms::from_tokens(&tokens, cache_bits));
        let parsed = dp_parse(argb, xsize, planes, &matches, cache_bits, &model);
        let parsed = if cache_bits > 0 {
            apply_cache(argb, &parsed, cache_bits)
        } else {
            parsed
        };
        if Histograms::from_tokens(&parsed, cache_bits).estimate() < est {
            tokens = parsed;
        }
    }
    if cache_bits > 0 {
        bw.put(1, 1);
        bw.put(cache_bits, 4);
    } else {
        bw.put(0, 1);
    }
    let height = argb.len() / xsize.max(1);
    // Several prefix code groups, when they pay for themselves.
    let mut groups: Option<(Vec<u32>, usize)> = None;
    if meta_bits > 0 && !sub {
        let sp = Space::new(cache_bits);
        let tiles = tile_histograms(&tokens, xsize, height, meta_bits, &sp);
        let (assign, k) = cluster_tiles(&tiles, &sp, tune.max_groups);
        if k > 1 {
            groups = Some((assign, k));
        }
    }
    let tw = sub_size(xsize as u32, meta_bits.max(1)) as usize;
    let group_at = |x: usize, y: usize| -> usize {
        match &groups {
            Some((assign, _)) => assign
                .get((y >> meta_bits) * tw + (x >> meta_bits))
                .copied()
                .unwrap_or(0) as usize,
            None => 0,
        }
    };
    let k = groups.as_ref().map_or(1, |g| g.1);
    if !sub {
        match &groups {
            Some((assign, _)) => {
                bw.put(1, 1);
                bw.put(meta_bits - 2, 3);
                let img: Vec<u32> = assign.iter().map(|&g| 0xff00_0000 | (g << 8)).collect();
                write_image(bw, &img, tw, tune, planes, 0, true);
            }
            None => bw.put(0, 1),
        }
    }
    // Histograms per group, from the tokens' start positions.
    let mut hists: Vec<Histograms> = (0..k).map(|_| Histograms::new(cache_bits)).collect();
    let (mut x, mut y) = (0usize, 0usize);
    for &t in &tokens {
        if let Some(h) = hists.get_mut(group_at(x, y)) {
            h.add(t);
        }
        let len = match t {
            Token::Copy(l, _) => usize::from(l),
            _ => 1,
        };
        x += len;
        while x >= xsize {
            x -= xsize;
            y += 1;
        }
    }
    struct Codes {
        green: Code,
        red: Code,
        blue: Code,
        alpha: Code,
        dist: Code,
    }
    let codes: Vec<Codes> = hists
        .iter()
        .map(|h| Codes {
            green: Code::new(code_lengths(&h.green, MAX_LEN_BITS)),
            red: Code::new(code_lengths(&h.red, MAX_LEN_BITS)),
            blue: Code::new(code_lengths(&h.blue, MAX_LEN_BITS)),
            alpha: Code::new(code_lengths(&h.alpha, MAX_LEN_BITS)),
            dist: Code::new(code_lengths(&h.dist, MAX_LEN_BITS)),
        })
        .collect();
    for c in &codes {
        for code in [&c.green, &c.red, &c.blue, &c.alpha, &c.dist] {
            write_code(bw, &code.lengths);
        }
    }
    let (mut x, mut y) = (0usize, 0usize);
    for t in tokens {
        let Some(c) = codes.get(group_at(x, y)) else {
            break;
        };
        let len = match t {
            Token::Literal(p) => {
                // Two codes of at most 15 bits fit one 32-bit write.
                let g = c.green.get(((p >> 8) & 0xff) as usize);
                let r = c.red.get(((p >> 16) & 0xff) as usize);
                bw.put(
                    (g & 0xffff) | ((r & 0xffff) << (g >> 16)),
                    (g >> 16) + (r >> 16),
                );
                let b = c.blue.get((p & 0xff) as usize);
                let a = c.alpha.get((p >> 24) as usize);
                bw.put(
                    (b & 0xffff) | ((a & 0xffff) << (b >> 16)),
                    (b >> 16) + (a >> 16),
                );
                1
            }
            Token::Cache(i) => {
                c.green.put(bw, NUM_LITERAL + NUM_LENGTH + i as usize);
                1
            }
            Token::Copy(len, dcode) => {
                let (ls, le, lv) = prefix_encode(u32::from(len));
                c.green.put(bw, NUM_LITERAL + ls as usize);
                bw.put(lv, le);
                let (ds, de, dv) = prefix_encode(dcode);
                c.dist.put(bw, ds as usize);
                bw.put(dv, de);
                usize::from(len)
            }
        };
        x += len;
        while x >= xsize {
            x -= xsize;
            y += 1;
        }
    }
}

// ---------------------------------------------------------------------------------
// Transforms

#[inline]
fn sub_px(a: u32, b: u32) -> u32 {
    let ag = (((a >> 8) & 0x00ff_00ff).wrapping_add(0x0100_0100))
        .wrapping_sub((b >> 8) & 0x00ff_00ff)
        & 0x00ff_00ff;
    let rb =
        ((a & 0x00ff_00ff).wrapping_add(0x0100_0100)).wrapping_sub(b & 0x00ff_00ff) & 0x00ff_00ff;
    (ag << 8) | rb
}

fn subtract_green(argb: &mut [u32]) {
    for p in argb {
        let g = (*p >> 8) & 0xff;
        let rb = (((*p & 0x00ff_00ff).wrapping_add(0x0100_0100)).wrapping_sub((g << 16) | g))
            & 0x00ff_00ff;
        *p = (*p & 0xff00_ff00) | rb;
    }
}

/// `n * log2(n)`, tabulated for the counts that occur in tile histograms.
struct NLogN(Vec<f64>);

impl NLogN {
    fn new() -> Self {
        Self(
            (0..1u32 << 16)
                .map(|n| {
                    if n == 0 {
                        0.0
                    } else {
                        f64::from(n) * f64::from(n).log2()
                    }
                })
                .collect(),
        )
    }
    #[inline]
    fn get(&self, n: u32) -> f64 {
        match self.0.get(n as usize) {
            Some(&v) => v,
            None => f64::from(n) * f64::from(n).log2(),
        }
    }
}

/// Extra bits needed to code a tile's residuals on top of everything chosen so far
/// (the entropy of accumulated+tile minus that of accumulated).
struct Accum {
    bins: [u32; 256],
    total: u32,
}

impl Accum {
    fn new() -> Self {
        Self {
            bins: [0; 256],
            total: 0,
        }
    }

    fn cost(&self, tile: &[u16; 256], touched: &[u8], count: u32, t: &NLogN) -> f64 {
        let mut s = t.get(self.total + count) - t.get(self.total);
        for &b in touched {
            let a = self.bins.get(usize::from(b)).copied().unwrap_or(0);
            let c = u32::from(tile.get(usize::from(b)).copied().unwrap_or(0));
            s -= t.get(a + c) - t.get(a);
        }
        s
    }

    fn add(&mut self, tile: &[u16; 256], touched: &[u8], count: u32) {
        for &b in touched {
            if let Some(a) = self.bins.get_mut(usize::from(b)) {
                *a += u32::from(tile.get(usize::from(b)).copied().unwrap_or(0));
            }
        }
        self.total += count;
    }
}

/// Histogram of one channel of a tile with the list of bins it touched.
struct TileHist {
    bins: [u16; 256],
    touched: Vec<u8>,
}

impl TileHist {
    fn new() -> Self {
        Self {
            bins: [0; 256],
            touched: Vec::new(),
        }
    }
    fn clear(&mut self) {
        for &b in &self.touched {
            if let Some(x) = self.bins.get_mut(usize::from(b)) {
                *x = 0;
            }
        }
        self.touched.clear();
    }
    #[inline]
    fn add(&mut self, v: u8) {
        if let Some(x) = self.bins.get_mut(usize::from(v)) {
            if *x == 0 {
                self.touched.push(v);
            }
            *x += 1;
        }
    }
}

/// libwebp's tile size: smaller tiles for slower efforts, but at most ~2600 tiles.
fn histo_bits(effort: u8, palette: bool, w: usize, h: usize) -> u32 {
    let mut bits = (if palette { 9 } else { 7 }) - i32::from(effort.min(6));
    loop {
        let size =
            sub_size(w as u32, bits as u32) as usize * sub_size(h as u32, bits as u32) as usize;
        if size <= 2600 {
            break;
        }
        bits += 1;
    }
    bits.clamp(2, 9) as u32
}

fn transform_bits(effort: u8, histo: u32) -> u32 {
    let max = match effort {
        0..=3 => 6,
        4 => 5,
        _ => 4,
    };
    histo.min(max)
}

/// Choose a predictor per tile, returning the modes and the residual image.
fn predictor_transform(
    src: &[u32],
    w: usize,
    h: usize,
    bits: u32,
    modes: &[u8],
) -> (Vec<u32>, Vec<u32>) {
    let bw = sub_size(w as u32, bits) as usize;
    let bh = sub_size(h as u32, bits) as usize;
    let tile = 1usize << bits;
    let nlogn = NLogN::new();
    let at = |x: usize, y: usize| src.get(y * w + x).copied().unwrap_or(0);
    let pred_at = |mode: u32, x: usize, y: usize| -> u32 {
        if y == 0 {
            return if x == 0 { 0xff00_0000 } else { at(x - 1, 0) };
        }
        if x == 0 {
            return at(0, y - 1);
        }
        let tr = src.get((y - 1) * w + x + 1).copied().unwrap_or(0);
        predict(mode, at(x - 1, y), at(x, y - 1), tr, at(x - 1, y - 1))
    };
    let mut acc: Vec<Accum> = (0..4).map(|_| Accum::new()).collect();
    // One histogram per (mode, channel), reused for every tile.
    let nm = modes.len();
    let mut hists: Vec<TileHist> = (0..nm * 4).map(|_| TileHist::new()).collect();
    let mut tile_modes = vec![0u32; bw * bh];
    for ty in 0..bh {
        for tx in 0..bw {
            let (x0, y0) = (tx * tile, ty * tile);
            let (x1, y1) = ((x0 + tile).min(w), (y0 + tile).min(h));
            for hst in hists.iter_mut() {
                hst.clear();
            }
            let mut count = 0u32;
            for y in y0.max(1)..y1 {
                for x in x0.max(1)..x1 {
                    let px = at(x, y);
                    let tr = src.get((y - 1) * w + x + 1).copied().unwrap_or(0);
                    let all = predict_all(at(x - 1, y), at(x, y - 1), tr, at(x - 1, y - 1));
                    for (mi, &m) in modes.iter().enumerate() {
                        let p = all.get(usize::from(m)).copied().unwrap_or(0xff00_0000);
                        let r = sub_px(px, p);
                        if let Some(hs) = hists.get_mut(mi * 4..mi * 4 + 4) {
                            for (hc, shift) in hs.iter_mut().zip([24u32, 16, 8, 0]) {
                                hc.add((r >> shift) as u8);
                            }
                        }
                    }
                    count += 1;
                }
            }
            let mut best_cost = f64::MAX;
            let mut best = 0usize;
            for mi in 0..nm {
                let Some(hs) = hists.get(mi * 4..mi * 4 + 4) else {
                    break;
                };
                let cost: f64 = acc
                    .iter()
                    .zip(hs)
                    .map(|(a, c)| {
                        a.cost(&c.bins, &c.touched, count, &nlogn) + near_zero_bonus(&c.bins)
                    })
                    .sum();
                if cost < best_cost {
                    best_cost = cost;
                    best = mi;
                }
            }
            if let Some(hs) = hists.get(best * 4..best * 4 + 4) {
                for (a, c) in acc.iter_mut().zip(hs) {
                    a.add(&c.bins, &c.touched, count);
                }
            }
            if let Some(t) = tile_modes.get_mut(ty * bw + tx) {
                *t = 0xff00_0000 | (u32::from(modes.get(best).copied().unwrap_or(0)) << 8);
            }
        }
    }
    let mut out = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mode = tile_modes
                .get((y >> bits) * bw + (x >> bits))
                .map_or(0, |m| (m >> 8) & 0xf);
            if let Some(o) = out.get_mut(y * w + x) {
                *o = sub_px(at(x, y), pred_at(mode, x, y));
            }
        }
    }
    (tile_modes, out)
}

/// libwebp's nudge towards residuals that cluster around zero. Without it the choice can
/// lock into modes whose residuals merely resemble the statistics gathered so far.
fn near_zero_bonus(bins: &[u16; 256]) -> f64 {
    let at = |i: usize| f64::from(bins.get(i).copied().unwrap_or(0));
    let mut weight = 0.94;
    let mut bits = at(0);
    for i in 1..16 {
        bits += weight * (at(i) + at(256 - i));
        weight *= 0.6;
    }
    -0.1 * bits
}

#[inline]
fn color_delta(t: i32, c: u32) -> i32 {
    (i32::from(t as i8) * i32::from(c as u8 as i8)) >> 5
}

/// Pick per-tile cross-colour multipliers and apply them. Returns None if all are zero.
fn cross_color_transform(data: &mut [u32], w: usize, h: usize, bits: u32) -> Option<Vec<u32>> {
    let bw = sub_size(w as u32, bits) as usize;
    let bh = sub_size(h as u32, bits) as usize;
    let tile = 1usize << bits;
    let nlogn = NLogN::new();
    let mut acc_red = Accum::new();
    let mut acc_blue = Accum::new();
    let mut hist = TileHist::new();
    let mut elems = vec![0xff00_0000u32; bw * bh];
    let mut chosen = vec![(0i32, 0i32, 0i32); bw * bh];
    let mut any = false;
    let mut px: Vec<(u32, u32, u32)> = Vec::with_capacity(tile * tile);
    for ty in 0..bh {
        for tx in 0..bw {
            px.clear();
            for y in ty * tile..((ty + 1) * tile).min(h) {
                for x in tx * tile..((tx + 1) * tile).min(w) {
                    let p = data.get(y * w + x).copied().unwrap_or(0);
                    px.push(((p >> 8) & 0xff, (p >> 16) & 0xff, p & 0xff));
                }
            }
            let count = px.len() as u32;
            let mut eval = |acc: &Accum, f: &dyn Fn(u32, u32, u32) -> i32| -> f64 {
                hist.clear();
                for &(g, r, b) in &px {
                    hist.add(f(g, r, b) as u8);
                }
                acc.cost(&hist.bins, &hist.touched, count, &nlogn)
            };
            let search = |eval: &mut dyn FnMut(i32) -> f64| -> i32 {
                let mut cur = 0i32;
                let mut cost = eval(0);
                for step in [64, 32, 16, 8, 4, 2, 1] {
                    for cand in [cur + step, cur - step] {
                        if (-128..=127).contains(&cand) {
                            let c = eval(cand);
                            if c < cost {
                                cost = c;
                                cur = cand;
                            }
                        }
                    }
                }
                cur
            };
            let g2r = search(&mut |t| eval(&acc_red, &|g, r, _| r as i32 - color_delta(t, g)));
            let g2b = search(&mut |t| eval(&acc_blue, &|g, _, b| b as i32 - color_delta(t, g)));
            let r2b = search(&mut |t| {
                eval(&acc_blue, &|g, r, b| {
                    b as i32 - color_delta(g2b, g) - color_delta(t, r)
                })
            });
            // Fold the winners into the running statistics.
            hist.clear();
            for &(g, r, _) in &px {
                hist.add((r as i32 - color_delta(g2r, g)) as u8);
            }
            acc_red.add(&hist.bins, &hist.touched, count);
            hist.clear();
            for &(g, r, b) in &px {
                hist.add((b as i32 - color_delta(g2b, g) - color_delta(r2b, r)) as u8);
            }
            acc_blue.add(&hist.bins, &hist.touched, count);
            if let Some(c) = chosen.get_mut(ty * bw + tx) {
                *c = (g2r, g2b, r2b);
            }
            if (g2r, g2b, r2b) != (0, 0, 0) {
                any = true;
            }
            if let Some(e) = elems.get_mut(ty * bw + tx) {
                *e = 0xff00_0000
                    | (((r2b & 0xff) as u32) << 16)
                    | (((g2b & 0xff) as u32) << 8)
                    | ((g2r & 0xff) as u32);
            }
        }
    }
    if !any {
        return None;
    }
    for y in 0..h {
        for x in 0..w {
            let (g2r, g2b, r2b) = chosen
                .get((y >> bits) * bw + (x >> bits))
                .copied()
                .unwrap_or((0, 0, 0));
            if let Some(p) = data.get_mut(y * w + x) {
                let a = *p >> 24;
                let green = (*p >> 8) & 0xff;
                let red = (*p >> 16) & 0xff;
                let blue = *p & 0xff;
                let new_red = (red as i32 - color_delta(g2r, green)) as u32 & 0xff;
                let new_blue =
                    (blue as i32 - color_delta(g2b, green) - color_delta(r2b, red)) as u32 & 0xff;
                *p = (a << 24) | (new_red << 16) | (green << 8) | new_blue;
            }
        }
    }
    Some(elems)
}

// ---------------------------------------------------------------------------------
// Driver

fn write_header(bw: &mut BitWriter, w: usize, h: usize, alpha: bool) {
    bw.put(0x2f, 8);
    bw.put((w - 1) as u32, 14);
    bw.put((h - 1) as u32, 14);
    bw.put(u32::from(alpha), 1);
    bw.put(0, 3);
}

fn encode_direct(
    argb: &[u32],
    w: usize,
    h: usize,
    alpha: bool,
    tune: &Tune,
    planes: &PlaneCodes,
) -> Vec<u8> {
    let mut bw = BitWriter::default();
    write_header(&mut bw, w, h, alpha);
    let mut data = argb.to_vec();
    // Subtract green.
    bw.put(1, 1);
    bw.put(2, 2);
    subtract_green(&mut data);
    // Predictor.
    let bits = transform_bits(tune.effort, histo_bits(tune.effort, false, w, h));
    let (modes, resid) = predictor_transform(&data, w, h, bits, &tune.modes);
    bw.put(1, 1);
    bw.put(0, 2);
    bw.put(bits - 2, 3);
    let bwid = sub_size(w as u32, bits) as usize;
    write_image(&mut bw, &modes, bwid, tune, planes, 0, true);
    data = resid;
    // Cross colour.
    if tune.cross_color {
        if let Some(elems) = cross_color_transform(&mut data, w, h, bits) {
            bw.put(1, 1);
            bw.put(1, 2);
            bw.put(bits - 2, 3);
            write_image(&mut bw, &elems, bwid, tune, planes, 0, true);
        }
    }
    bw.put(0, 1);
    let mb = if tune.max_groups > 1 {
        histo_bits(tune.effort, false, w, h)
    } else {
        0
    };
    write_image(&mut bw, &data, w, tune, planes, mb, false);
    bw.finish()
}

fn palette_of(argb: &[u32]) -> Option<Vec<u32>> {
    let mut seen = std::collections::HashSet::new();
    let mut order = Vec::new();
    for &p in argb {
        if seen.insert(p) {
            if seen.len() > 256 {
                return None;
            }
            order.push(p);
        }
    }
    Some(order)
}

fn encode_palette(
    argb: &[u32],
    w: usize,
    h: usize,
    alpha: bool,
    palette: &[u32],
    tune: &Tune,
    planes: &PlaneCodes,
) -> Vec<u8> {
    let mut bw = BitWriter::default();
    write_header(&mut bw, w, h, alpha);
    let n = palette.len();
    let index: std::collections::HashMap<u32, u32> = palette
        .iter()
        .enumerate()
        .map(|(i, &c)| (c, i as u32))
        .collect();
    let width_bits = match n {
        0..=2 => 3,
        3..=4 => 2,
        5..=16 => 1,
        _ => 0,
    };
    bw.put(1, 1);
    bw.put(3, 2);
    bw.put((n - 1) as u32, 8);
    let mut deltas = palette.to_vec();
    for i in (1..n).rev() {
        let prev = deltas.get(i - 1).copied().unwrap_or(0);
        if let Some(d) = deltas.get_mut(i) {
            *d = sub_px(*d, prev);
        }
    }
    write_image(&mut bw, &deltas, n, tune, planes, 0, true);
    bw.put(0, 1);
    let pw = sub_size(w as u32, width_bits) as usize;
    let bits_per = 8 >> width_bits;
    let mut packed = vec![0xff00_0000u32; pw * h];
    for y in 0..h {
        for x in 0..w {
            let idx = index
                .get(&argb.get(y * w + x).copied().unwrap_or(0))
                .copied()
                .unwrap_or(0);
            let shift = bits_per * (x as u32 & ((1 << width_bits) - 1));
            if let Some(p) = packed.get_mut(y * pw + (x >> width_bits)) {
                *p |= idx << (8 + shift);
            }
        }
    }
    let mb = if tune.max_groups > 1 {
        histo_bits(tune.effort, true, pw, h)
    } else {
        0
    };
    write_image(&mut bw, &packed, pw, tune, planes, mb, false);
    bw.finish()
}

/// Encode premultiplication-free ARGB pixels into a VP8L bitstream.
pub(crate) fn encode(argb: &[u32], w: usize, h: usize, alpha: bool, effort: u8) -> Vec<u8> {
    let tune = Tune::new(effort);
    let planes = PlaneCodes::new();
    let mut best: Option<Vec<u8>> = None;
    let mut consider = |v: Vec<u8>| {
        if best.as_ref().is_none_or(|b| v.len() < b.len()) {
            best = Some(v);
        }
    };
    if let Some(pal) = palette_of(argb) {
        let mut sorted = pal.clone();
        sorted.sort_unstable();
        consider(encode_palette(argb, w, h, alpha, &pal, &tune, &planes));
        if effort >= 2 {
            consider(encode_palette(argb, w, h, alpha, &sorted, &tune, &planes));
        }
        if effort >= 4 || pal.len() > 64 {
            consider(encode_direct(argb, w, h, alpha, &tune, &planes));
        }
    } else {
        consider(encode_direct(argb, w, h, alpha, &tune, &planes));
    }
    best.unwrap_or_default()
}
