//! DEFLATE compression (RFC 1951): hash-chain LZ77 with lazy matching, then the
//! smallest of stored, fixed-Huffman and dynamic-Huffman encodings per block.
#![allow(clippy::indexing_slicing)] // all indices are bounded by table sizes or loop ranges

use crate::inflate::{CL_ORDER, DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA};
use oc_io::{BitOrder, BitWriter};

const WINDOW: usize = 32768;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
const TOKENS_PER_BLOCK: usize = 1 << 15;

#[derive(Clone, Copy)]
enum Token {
    Lit(u8),
    Match { len: u16, dist: u16 },
}

struct Params {
    max_chain: usize,
    good: usize,
    lazy: bool,
}

fn params(level: u32) -> Params {
    match level {
        1 => Params {
            max_chain: 4,
            good: 8,
            lazy: false,
        },
        2 => Params {
            max_chain: 8,
            good: 16,
            lazy: false,
        },
        3 => Params {
            max_chain: 16,
            good: 32,
            lazy: false,
        },
        4 => Params {
            max_chain: 16,
            good: 16,
            lazy: true,
        },
        5 => Params {
            max_chain: 32,
            good: 32,
            lazy: true,
        },
        6 => Params {
            max_chain: 128,
            good: 128,
            lazy: true,
        },
        7 => Params {
            max_chain: 256,
            good: 128,
            lazy: true,
        },
        8 => Params {
            max_chain: 1024,
            good: 258,
            lazy: true,
        },
        _ => Params {
            max_chain: 4096,
            good: 258,
            lazy: true,
        },
    }
}

fn hash3(d: &[u8], i: usize) -> usize {
    let v = u32::from(d[i]) | (u32::from(d[i + 1]) << 8) | (u32::from(d[i + 2]) << 16);
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

struct Matcher<'a> {
    data: &'a [u8],
    head: Vec<i32>,
    prev: Vec<i32>,
    p: Params,
}

impl<'a> Matcher<'a> {
    fn new(data: &'a [u8], level: u32) -> Self {
        Self {
            data,
            head: vec![-1; 1 << HASH_BITS],
            prev: vec![-1; data.len().max(1)],
            p: params(level),
        }
    }

    fn insert(&mut self, i: usize) {
        if i + MIN_MATCH <= self.data.len() {
            let h = hash3(self.data, i);
            self.prev[i] = self.head[h];
            self.head[h] = i as i32;
        }
    }

    fn find(&self, i: usize, min_len: usize) -> Option<(usize, usize)> {
        if i + MIN_MATCH > self.data.len() {
            return None;
        }
        let max_len = MAX_MATCH.min(self.data.len() - i);
        let mut best_len = min_len.max(MIN_MATCH - 1);
        let mut best_dist = 0;
        let mut cand = self.head[hash3(self.data, i)];
        let mut chain = self.p.max_chain;
        while cand >= 0 && chain > 0 {
            let c = cand as usize;
            if i - c > WINDOW {
                break;
            }
            if self.data[c + best_len.min(max_len - 1)] == self.data[i + best_len.min(max_len - 1)]
            {
                let mut l = 0;
                while l < max_len && self.data[c + l] == self.data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - c;
                    if l >= self.p.good || l == max_len {
                        break;
                    }
                }
            }
            cand = self.prev[c];
            chain -= 1;
        }
        (best_dist != 0 && best_len >= MIN_MATCH).then_some((best_len, best_dist))
    }
}

fn tokenize(data: &[u8], level: u32) -> Vec<Token> {
    let mut m = Matcher::new(data, level);
    let mut toks = Vec::with_capacity(data.len() / 2 + 16);
    let mut i = 0;
    while i < data.len() {
        let here = m.find(i, 0);
        match here {
            None => {
                toks.push(Token::Lit(data[i]));
                m.insert(i);
                i += 1;
            }
            Some((len, dist)) => {
                // Lazy matching: prefer a strictly longer match starting one byte later.
                if m.p.lazy && len < m.p.good {
                    m.insert(i);
                    if let Some((l2, _)) = m.find(i + 1, len) {
                        if l2 > len {
                            toks.push(Token::Lit(data[i]));
                            i += 1;
                            continue;
                        }
                    }
                    toks.push(Token::Match {
                        len: len as u16,
                        dist: dist as u16,
                    });
                    for k in i + 1..i + len {
                        m.insert(k);
                    }
                } else {
                    toks.push(Token::Match {
                        len: len as u16,
                        dist: dist as u16,
                    });
                    for k in i..i + len {
                        m.insert(k);
                    }
                }
                i += len;
            }
        }
    }
    toks
}

fn len_symbol(len: u16) -> usize {
    LEN_BASE.iter().rposition(|&b| b <= len).unwrap_or(0)
}
fn dist_symbol(dist: u16) -> usize {
    DIST_BASE.iter().rposition(|&b| b <= dist).unwrap_or(0)
}

/// Code lengths limited to `max_bits`, assigned so frequent symbols get short codes.
pub(crate) fn code_lengths(freq: &[u32], max_bits: usize) -> Vec<u8> {
    let used: Vec<usize> = (0..freq.len()).filter(|&i| freq[i] > 0).collect();
    let mut lens = vec![0u8; freq.len()];
    match used.len() {
        0 => return lens,
        1 => {
            lens[used[0]] = 1;
            return lens;
        }
        _ => {}
    }
    // Plain Huffman via a simple two-queue merge on sorted weights.
    let mut sorted = used.clone();
    sorted.sort_by_key(|&i| (freq[i], i));
    struct Node {
        w: u64,
        kids: Option<(usize, usize)>,
    }
    let mut nodes: Vec<Node> = sorted
        .iter()
        .map(|&i| Node {
            w: u64::from(freq[i]),
            kids: None,
        })
        .collect();
    let n = nodes.len();
    let (mut a, mut b) = (0usize, n);
    let pick = |nodes: &Vec<Node>, a: &mut usize, b: &mut usize| -> usize {
        let take_leaf = *a < n && (*b >= nodes.len() || nodes[*a].w <= nodes[*b].w);
        if take_leaf {
            *a += 1;
            *a - 1
        } else {
            *b += 1;
            *b - 1
        }
    };
    for _ in 0..n - 1 {
        let x = pick(&nodes, &mut a, &mut b);
        let y = pick(&nodes, &mut a, &mut b);
        let w = nodes[x].w + nodes[y].w;
        nodes.push(Node {
            w,
            kids: Some((x, y)),
        });
    }
    // Depth of each leaf.
    let mut depth = vec![0usize; nodes.len()];
    for idx in (n..nodes.len()).rev() {
        if let Some((x, y)) = nodes[idx].kids {
            depth[x] = depth[idx] + 1;
            depth[y] = depth[idx] + 1;
        }
    }
    // Count by length and clamp to max_bits (miniz-style enforcement).
    let mut num = vec![0i64; max_bits.max(depth.iter().take(n).copied().max().unwrap_or(0)) + 1];
    for d in depth.iter().take(n) {
        num[*d] += 1;
    }
    let top = num.len() - 1;
    for i in max_bits + 1..=top {
        num[max_bits] += num[i];
        num[i] = 0;
    }
    let mut total: i64 = (1..=max_bits).map(|i| num[i] << (max_bits - i)).sum();
    while total > 1i64 << max_bits {
        num[max_bits] -= 1;
        for i in (1..max_bits).rev() {
            if num[i] != 0 {
                num[i] -= 1;
                num[i + 1] += 2;
                break;
            }
        }
        total -= 1;
    }
    // Assign: sorted ascending by frequency, so the rarest symbols get the longest codes.
    let mut idx = 0;
    for len in (1..=max_bits).rev() {
        for _ in 0..num[len] {
            lens[sorted[idx]] = len as u8;
            idx += 1;
        }
    }
    lens
}

/// Canonical codes from lengths, bit-reversed for LSB-first output.
fn canonical_codes(lens: &[u8]) -> Vec<u32> {
    let mut count = [0u32; 16];
    for &l in lens {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0;
    for bits in 1..16 {
        code = (code + count[bits - 1]) << 1;
        next[bits] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next[usize::from(l)];
            next[usize::from(l)] += 1;
            c.reverse_bits() >> (32 - u32::from(l))
        })
        .collect()
}

fn fixed_lit_lens() -> Vec<u8> {
    let mut l = vec![8u8; 288];
    l[144..256].fill(9);
    l[256..280].fill(7);
    l
}

struct Block<'a> {
    toks: &'a [Token],
    lit_freq: [u32; 286],
    dist_freq: [u32; 30],
}

fn analyse(toks: &[Token]) -> Block<'_> {
    let mut lit_freq = [0u32; 286];
    let mut dist_freq = [0u32; 30];
    for t in toks {
        match *t {
            Token::Lit(b) => lit_freq[usize::from(b)] += 1,
            Token::Match { len, dist } => {
                lit_freq[257 + len_symbol(len)] += 1;
                dist_freq[dist_symbol(dist)] += 1;
            }
        }
    }
    lit_freq[256] = 1;
    Block {
        toks,
        lit_freq,
        dist_freq,
    }
}

fn data_cost(b: &Block<'_>, lit: &[u8], dist: &[u8]) -> u64 {
    let mut bits = 0u64;
    for (s, &f) in b.lit_freq.iter().enumerate() {
        bits += u64::from(f) * u64::from(lit[s]);
        if s >= 257 {
            bits += u64::from(f) * u64::from(LEN_EXTRA[s - 257]);
        }
    }
    for (s, &f) in b.dist_freq.iter().enumerate() {
        bits += u64::from(f) * (u64::from(dist[s]) + u64::from(DIST_EXTRA[s]));
    }
    bits
}

fn write_tokens(w: &mut BitWriter, toks: &[Token], lit: &[u8], dist: &[u8]) {
    let lc = canonical_codes(lit);
    let dc = canonical_codes(dist);
    for t in toks {
        match *t {
            Token::Lit(b) => w.write(lc[usize::from(b)], u32::from(lit[usize::from(b)])),
            Token::Match { len, dist: d } => {
                let ls = len_symbol(len);
                w.write(lc[257 + ls], u32::from(lit[257 + ls]));
                w.write(u32::from(len - LEN_BASE[ls]), u32::from(LEN_EXTRA[ls]));
                let ds = dist_symbol(d);
                w.write(dc[ds], u32::from(dist[ds]));
                w.write(u32::from(d - DIST_BASE[ds]), u32::from(DIST_EXTRA[ds]));
            }
        }
    }
    w.write(lc[256], u32::from(lit[256]));
}

/// Run-length encode the concatenated code lengths with symbols 16/17/18.
fn rle_lengths(lens: &[u8]) -> Vec<(u8, u8, u8)> {
    // (symbol, extra bits count, extra value)
    let mut out = Vec::new();
    let mut i = 0;
    while i < lens.len() {
        let v = lens[i];
        let mut run = 1;
        while i + run < lens.len() && lens[i + run] == v {
            run += 1;
        }
        let mut left = run;
        if v == 0 {
            while left >= 11 {
                let n = left.min(138);
                out.push((18, 7, (n - 11) as u8));
                left -= n;
            }
            if left >= 3 {
                out.push((17, 3, (left - 3) as u8));
                left = 0;
            }
        } else {
            out.push((v, 0, 0));
            left -= 1;
            while left >= 3 {
                let n = left.min(6);
                out.push((16, 2, (n - 3) as u8));
                left -= n;
            }
        }
        for _ in 0..left {
            out.push((v, 0, 0));
        }
        i += run;
    }
    out
}

fn write_block(w: &mut BitWriter, raw: &[u8], b: &Block<'_>, last: bool) {
    let flit = fixed_lit_lens();
    let fdist = vec![5u8; 30];
    let fixed_bits = 3 + data_cost(b, &flit, &fdist);

    let mut lit = code_lengths(&b.lit_freq, 15);
    let mut dist = code_lengths(&b.dist_freq, 15);
    if dist.iter().all(|&l| l == 0) {
        dist[0] = 1;
    }
    let nlit = (257..=286)
        .rev()
        .find(|&n| lit[n - 1] != 0)
        .unwrap_or(257)
        .max(257);
    let ndist = (1..=30).rev().find(|&n| dist[n - 1] != 0).unwrap_or(1);
    lit.truncate(nlit);
    dist.truncate(ndist);
    let mut all = lit.clone();
    all.extend_from_slice(&dist);
    let rle = rle_lengths(&all);
    let mut cl_freq = [0u32; 19];
    for &(s, _, _) in &rle {
        cl_freq[usize::from(s)] += 1;
    }
    let cl_lens = code_lengths(&cl_freq, 7);
    let ncl = (4..=19)
        .rev()
        .find(|&n| cl_lens[CL_ORDER[n - 1]] != 0)
        .unwrap_or(4);
    let mut header_bits = 3 + 14 + 3 * ncl as u64;
    for &(s, eb, _) in &rle {
        header_bits += u64::from(cl_lens[usize::from(s)]) + u64::from(eb);
    }
    // Lengths for the cost model must cover the full alphabets.
    let mut lit_full = lit.clone();
    lit_full.resize(286, 0);
    let mut dist_full = dist.clone();
    dist_full.resize(30, 0);
    let dyn_bits = header_bits + data_cost(b, &lit_full, &dist_full);
    let stored_bits = 8 * (raw.len() as u64 + 5 * (raw.len() as u64 / 65535 + 1));

    if stored_bits < fixed_bits.min(dyn_bits) {
        let chunks: Vec<&[u8]> = if raw.is_empty() {
            vec![&[][..]]
        } else {
            raw.chunks(65535).collect()
        };
        let n = chunks.len();
        for (k, c) in chunks.into_iter().enumerate() {
            let final_chunk = last && k + 1 == n;
            w.write(u32::from(final_chunk), 1);
            w.write(0, 2);
            w.align();
            w.write(c.len() as u32, 16);
            w.write(!(c.len() as u16) as u32, 16);
            for &byte in c {
                w.write(u32::from(byte), 8);
            }
        }
    } else if fixed_bits <= dyn_bits {
        w.write(u32::from(last), 1);
        w.write(1, 2);
        write_tokens(w, b.toks, &flit, &fdist);
    } else {
        w.write(u32::from(last), 1);
        w.write(2, 2);
        w.write((nlit - 257) as u32, 5);
        w.write((ndist - 1) as u32, 5);
        w.write((ncl - 4) as u32, 4);
        for &o in CL_ORDER.iter().take(ncl) {
            w.write(u32::from(cl_lens[o]), 3);
        }
        let cc = canonical_codes(&cl_lens);
        for &(s, eb, ev) in &rle {
            w.write(cc[usize::from(s)], u32::from(cl_lens[usize::from(s)]));
            w.write(u32::from(ev), u32::from(eb));
        }
        write_tokens(w, b.toks, &lit_full, &dist_full);
    }
}

/// Compress `data` into a raw DEFLATE stream. `level` is 0 (stored) to 9.
pub fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut w = BitWriter::new(BitOrder::LsbFirst);
    if level == 0 || data.is_empty() {
        let b = analyse(&[]);
        if data.is_empty() {
            write_block(&mut w, data, &b, true);
        } else {
            let chunks: Vec<&[u8]> = data.chunks(65535).collect();
            let n = chunks.len();
            for (k, c) in chunks.into_iter().enumerate() {
                w.write(u32::from(k + 1 == n), 1);
                w.write(0, 2);
                w.align();
                w.write(c.len() as u32, 16);
                w.write(!(c.len() as u16) as u32, 16);
                for &byte in c {
                    w.write(u32::from(byte), 8);
                }
            }
        }
        return w.finish();
    }
    let toks = tokenize(data, level.min(9));
    // Walk tokens in blocks, tracking the raw bytes each block covers for the stored fallback.
    let mut pos = 0;
    let mut chunks = toks.chunks(TOKENS_PER_BLOCK).peekable();
    while let Some(c) = chunks.next() {
        let covered: usize = c
            .iter()
            .map(|t| match *t {
                Token::Lit(_) => 1,
                Token::Match { len, .. } => usize::from(len),
            })
            .sum();
        let block = analyse(c);
        write_block(
            &mut w,
            &data[pos..pos + covered],
            &block,
            chunks.peek().is_none(),
        );
        pos += covered;
    }
    w.finish()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use crate::inflate::inflate;
    use oc_core::{Limits, NoProgress};

    fn roundtrip(data: &[u8], level: u32) -> usize {
        let z = deflate(data, level);
        let back = inflate(&z, &Limits::default(), &NoProgress).map(|r| r.data);
        assert_eq!(
            back.ok().as_deref(),
            Some(data),
            "level {level}, {} bytes",
            data.len()
        );
        z.len()
    }

    fn lcg(n: usize, mut s: u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn roundtrips_all_levels() {
        let text: Vec<u8> = b"the quick brown fox jumps over the lazy dog. "
            .iter()
            .cycle()
            .take(50_000)
            .copied()
            .collect();
        let noise = lcg(70_000, 1);
        let mixed: Vec<u8> = noise
            .iter()
            .take(3000)
            .chain(text.iter())
            .copied()
            .collect();
        for level in 0..=9 {
            for d in [&b""[..], b"a", b"ab", b"abc", &text, &noise, &mixed] {
                roundtrip(d, level);
            }
        }
    }

    #[test]
    fn compresses_repetitive_data_well_and_noise_not_much() {
        let zeros = vec![0u8; 1_000_000];
        assert!(roundtrip(&zeros, 6) < 2_000);
        let noise = lcg(100_000, 7);
        assert!(roundtrip(&noise, 6) <= noise.len() + 100);
    }

    #[test]
    fn long_matches_across_window_edge() {
        let mut d = lcg(40_000, 3);
        let head: Vec<u8> = d[..1000].to_vec();
        d.extend_from_slice(&head);
        roundtrip(&d, 9);
    }

    #[test]
    fn skewed_alphabet_exercises_length_limiting() {
        // Fibonacci-like frequencies force deep Huffman trees that need clamping to 15 bits.
        let mut d = Vec::new();
        let (mut a, mut b) = (1usize, 1usize);
        for sym in 0..24u8 {
            d.extend(std::iter::repeat_n(sym, a.min(60_000)));
            let c = a + b;
            a = b;
            b = c;
        }
        roundtrip(&d, 6);
    }
}
