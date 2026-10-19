//! LZMA and LZMA2 encoder: hash-chain match finder, repeat-distance aware greedy parse with
//! one step of lazy evaluation. Compression levels 0 to 9 trade chain depth and window size.
use super::{unsupported, Model, LenModel, ALIGN_BITS, END_POS_MODEL, MATCH_MIN};
use oc_core::Result;

const MATCH_MAX: usize = 273;

#[derive(Debug, Clone, Copy)]
pub struct Level(pub u32);

struct Params {
    dict: usize,
    nice: usize,
    depth: usize,
    lazy: bool,
}

fn params(level: Level) -> Params {
    let l = level.0.min(9);
    let dict = [1 << 18, 1 << 20, 1 << 21, 1 << 22, 1 << 22, 1 << 23, 1 << 23, 1 << 24, 1 << 24, 1 << 24][l as usize];
    let (nice, depth) = [(16, 4), (24, 10), (32, 20), (48, 40), (64, 80), (96, 160), (128, 320), (192, 640), (273, 1280), (273, 2560)][l as usize];
    Params {
        dict,
        nice,
        depth,
        lazy: l >= 3,
    }
}

// ---------------------------------------------------------------- range encoder

struct RangeEnc {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl RangeEnc {
    fn new() -> Self {
        Self {
            low: 0,
            range: 0xFFFF_FFFF,
            cache: 0,
            cache_size: 1,
            out: Vec::new(),
        }
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut tmp = self.cache;
            loop {
                self.out.push(tmp.wrapping_add(carry));
                tmp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    #[inline]
    fn bit(&mut self, p: &mut u16, bit: u32) {
        let bound = (self.range >> 11) * u32::from(*p);
        if bit == 0 {
            self.range = bound;
            *p += (2048 - *p) >> 5;
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
            *p -= *p >> 5;
        }
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn direct(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.range >>= 1;
            if (v >> i) & 1 == 1 {
                self.low += u64::from(self.range);
            }
            while self.range < (1 << 24) {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }

    fn tree(&mut self, probs: &mut [u16], bits: u32, v: u32) {
        let mut m = 1usize;
        for i in (0..bits).rev() {
            let b = (v >> i) & 1;
            self.bit(&mut probs[m], b);
            m = (m << 1) | b as usize;
        }
    }

    fn rev_tree(&mut self, probs: &mut [u16], bits: u32, v: u32) {
        let mut m = 1usize;
        for i in 0..bits {
            let b = (v >> i) & 1;
            self.bit(&mut probs[m], b);
            m = (m << 1) | b as usize;
        }
    }

    /// Bytes that will certainly be in the output once finished (upper bound of what is held).
    fn size_estimate(&self) -> usize {
        self.out.len() + self.cache_size as usize + 5
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

// ---------------------------------------------------------------- match finder

struct Finder {
    head4: Vec<u32>,
    head3: Vec<u32>,
    head2: Vec<u32>,
    prev: Vec<u32>,
    mask: usize,
    bits: u32,
    depth: usize,
    nice: usize,
    dict: usize,
    next: usize,
}

const NONE: u32 = u32::MAX;

#[inline]
fn word(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])
}

impl Finder {
    fn new(len: usize, p: &Params) -> Self {
        let window = p.dict.min(len.max(4096).next_power_of_two());
        let bits = (window.trailing_zeros() + 1).clamp(12, 22);
        Self {
            head4: vec![NONE; 1 << bits],
            head3: vec![NONE; 1 << 16],
            head2: vec![NONE; 1 << 16],
            prev: vec![NONE; window],
            mask: window - 1,
            bits,
            depth: p.depth,
            nice: p.nice,
            dict: window,
            next: 0,
        }
    }

    fn window(&self) -> usize {
        self.dict
    }

    #[inline]
    fn h4(&self, d: &[u8], i: usize) -> usize {
        (word(d, i).wrapping_mul(2_654_435_761) >> (32 - self.bits)) as usize
    }
    #[inline]
    fn h3(d: &[u8], i: usize) -> usize {
        ((u32::from(d[i]) | u32::from(d[i + 1]) << 8 | u32::from(d[i + 2]) << 16).wrapping_mul(2_654_435_761) >> 16) as usize
    }
    #[inline]
    fn h2(d: &[u8], i: usize) -> usize {
        usize::from(d[i]) | usize::from(d[i + 1]) << 8
    }

    /// Records position `i` so later positions can match it.
    fn insert(&mut self, d: &[u8], i: usize) {
        if i < self.next {
            return;
        }
        self.next = i + 1;
        if i + 4 <= d.len() {
            let h = self.h4(d, i);
            self.prev[i & self.mask] = self.head4[h];
            self.head4[h] = i as u32;
            self.head3[Self::h3(d, i)] = i as u32;
            self.head2[Self::h2(d, i)] = i as u32;
        } else if i + 3 <= d.len() {
            self.head3[Self::h3(d, i)] = i as u32;
            self.head2[Self::h2(d, i)] = i as u32;
        } else if i + 2 <= d.len() {
            self.head2[Self::h2(d, i)] = i as u32;
        }
    }

    fn match_len(d: &[u8], a: usize, b: usize, max: usize) -> usize {
        let mut n = 0;
        while n < max && d[a + n] == d[b + n] {
            n += 1;
        }
        n
    }

    /// Longest match for position `i` (not yet inserted): (length, distance - 1).
    fn find(&mut self, d: &[u8], i: usize, max: usize) -> (usize, usize) {
        let mut best = (0usize, 0usize);
        if max < 2 {
            return best;
        }
        let consider = |cand: u32, best: &mut (usize, usize), dict: usize| {
            if cand == NONE {
                return;
            }
            let c = cand as usize;
            if c >= i || i - c > dict {
                return;
            }
            let l = Self::match_len(d, c, i, max);
            if l > best.0 || (l == best.0 && l >= 2 && i - c - 1 < best.1) {
                *best = (l, i - c - 1);
            }
        };
        if i + 2 <= d.len() {
            consider(self.head2[Self::h2(d, i)], &mut best, self.dict);
        }
        if i + 3 <= d.len() {
            consider(self.head3[Self::h3(d, i)], &mut best, self.dict);
        }
        if i + 4 <= d.len() {
            let mut cand = self.head4[self.h4(d, i)];
            let mut steps = self.depth;
            while cand != NONE && steps > 0 {
                let c = cand as usize;
                if c >= i || i - c > self.dict {
                    break;
                }
                // The chain is only valid while the slot has not been overwritten.
                if d[c + best.0.min(max - 1)] == d[i + best.0.min(max - 1)] || best.0 == 0 {
                    let l = Self::match_len(d, c, i, max);
                    if l > best.0 {
                        best = (l, i - c - 1);
                        if l >= self.nice || l == max {
                            break;
                        }
                    }
                }
                let nxt = self.prev[c & self.mask];
                if nxt != NONE && (nxt as usize) >= c {
                    break;
                }
                cand = nxt;
                steps -= 1;
            }
        }
        self.insert(d, i);
        best
    }
}

// ---------------------------------------------------------------- symbol coding

struct Coder {
    m: Model,
    state: usize,
    reps: [usize; 4],
}

fn enc_len(rc: &mut RangeEnc, m: &mut LenModel, len: usize, pos_state: usize) {
    let l = (len - MATCH_MIN) as u32;
    if l < 8 {
        rc.bit(&mut m.choice, 0);
        rc.tree(&mut m.low[pos_state], 3, l);
    } else if l < 16 {
        rc.bit(&mut m.choice, 1);
        rc.bit(&mut m.choice2, 0);
        rc.tree(&mut m.mid[pos_state], 3, l - 8);
    } else {
        rc.bit(&mut m.choice, 1);
        rc.bit(&mut m.choice2, 1);
        rc.tree(&mut m.high, 8, l - 16);
    }
}

impl Coder {
    fn new(lc: u32, lp: u32, pb: u32) -> Self {
        Self {
            m: Model::new(lc, lp, pb),
            state: 0,
            reps: [0; 4],
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.m.lc, self.m.lp, self.m.pb);
    }

    fn pos_state(&self, pos: usize) -> usize {
        pos & ((1 << self.m.pb) - 1)
    }

    fn literal(&mut self, rc: &mut RangeEnc, d: &[u8], pos: usize) {
        let ps = self.pos_state(pos);
        rc.bit(&mut self.m.is_match[self.state][ps], 0);
        let prev = if pos > 0 { d[pos - 1] } else { 0 };
        let lit_state = ((pos & ((1 << self.m.lp) - 1)) << self.m.lc) + (usize::from(prev) >> (8 - self.m.lc));
        let probs = &mut self.m.literal[0x300 * lit_state..0x300 * (lit_state + 1)];
        let byte = usize::from(d[pos]);
        let mut sym = 1usize;
        if self.state >= 7 {
            let mut mb = usize::from(d[pos - self.reps[0] - 1]);
            let mut offs = 0x100usize;
            for i in (0..8).rev() {
                mb <<= 1;
                let mbit = mb & offs;
                let bit = (byte >> i) & 1;
                rc.bit(&mut probs[offs + mbit + sym], bit as u32);
                sym = (sym << 1) | bit;
                if bit == 0 {
                    offs &= !mbit;
                } else {
                    offs &= mbit;
                }
            }
        } else {
            for i in (0..8).rev() {
                let bit = (byte >> i) & 1;
                rc.bit(&mut probs[sym], bit as u32);
                sym = (sym << 1) | bit;
            }
        }
        self.state = if self.state < 4 {
            0
        } else if self.state < 10 {
            self.state - 3
        } else {
            self.state - 6
        };
    }

    fn encode_dist(&mut self, rc: &mut RangeEnc, len: usize, d: usize) {
        let len_state = (len - MATCH_MIN).min(3);
        let slot = if d < 4 {
            d
        } else {
            let n = 31 - (d as u32).leading_zeros();
            ((n as usize) << 1) | ((d >> (n - 1)) & 1)
        };
        rc.tree(&mut self.m.pos_slot[len_state], 6, slot as u32);
        if slot >= 4 {
            let n = (slot >> 1) as u32 - 1;
            let base = (2 | (slot & 1)) << n;
            let rem = (d - base) as u32;
            if slot < END_POS_MODEL {
                let off = base - slot;
                rc.rev_tree(&mut self.m.pos_special[off..], n, rem);
            } else {
                rc.direct(rem >> ALIGN_BITS, n - ALIGN_BITS);
                rc.rev_tree(&mut self.m.align, ALIGN_BITS, rem & ((1 << ALIGN_BITS) - 1));
            }
        }
    }

    fn new_match(&mut self, rc: &mut RangeEnc, pos: usize, len: usize, d: usize) {
        let ps = self.pos_state(pos);
        rc.bit(&mut self.m.is_match[self.state][ps], 1);
        rc.bit(&mut self.m.is_rep[self.state], 0);
        enc_len(rc, &mut self.m.len, len, ps);
        self.encode_dist(rc, len, d);
        self.reps = [d, self.reps[0], self.reps[1], self.reps[2]];
        self.state = if self.state < 7 { 7 } else { 10 };
    }

    fn rep_match(&mut self, rc: &mut RangeEnc, pos: usize, idx: usize, len: usize) {
        let ps = self.pos_state(pos);
        let st = self.state;
        rc.bit(&mut self.m.is_match[st][ps], 1);
        rc.bit(&mut self.m.is_rep[st], 1);
        if idx == 0 {
            rc.bit(&mut self.m.is_rep_g0[st], 0);
            rc.bit(&mut self.m.is_rep0_long[st][ps], 1);
        } else {
            rc.bit(&mut self.m.is_rep_g0[st], 1);
            if idx == 1 {
                rc.bit(&mut self.m.is_rep_g1[st], 0);
            } else {
                rc.bit(&mut self.m.is_rep_g1[st], 1);
                rc.bit(&mut self.m.is_rep_g2[st], (idx - 2) as u32);
            }
            let dist = self.reps[idx];
            for k in (1..=idx).rev() {
                self.reps[k] = self.reps[k - 1];
            }
            self.reps[0] = dist;
        }
        enc_len(rc, &mut self.m.rep_len, len, ps);
        self.state = if st < 7 { 8 } else { 11 };
    }

    fn short_rep(&mut self, rc: &mut RangeEnc, pos: usize) {
        let ps = self.pos_state(pos);
        let st = self.state;
        rc.bit(&mut self.m.is_match[st][ps], 1);
        rc.bit(&mut self.m.is_rep[st], 1);
        rc.bit(&mut self.m.is_rep_g0[st], 0);
        rc.bit(&mut self.m.is_rep0_long[st][ps], 0);
        self.state = if st < 7 { 9 } else { 11 };
    }

    fn end_marker(&mut self, rc: &mut RangeEnc, pos: usize) {
        let ps = self.pos_state(pos);
        rc.bit(&mut self.m.is_match[self.state][ps], 1);
        rc.bit(&mut self.m.is_rep[self.state], 0);
        enc_len(rc, &mut self.m.len, MATCH_MIN, ps);
        self.encode_dist(rc, MATCH_MIN, 0xFFFF_FFFF);
    }
}

// ---------------------------------------------------------------- parsing

struct Parser<'a> {
    d: &'a [u8],
    mf: Finder,
    c: Coder,
    p: Params,
}

impl Parser<'_> {
    fn rep_candidate(&self, pos: usize, max: usize) -> (usize, usize) {
        let mut best = (0usize, 0usize);
        for (i, &r) in self.c.reps.iter().enumerate() {
            if r >= pos {
                continue;
            }
            let l = Finder::match_len(self.d, pos - r - 1, pos, max);
            if l > best.0 {
                best = (l, i);
            }
        }
        best
    }

    /// Encodes `pos..` until `end` or until the range coder output nears `byte_budget`.
    /// Returns the position reached.
    fn run(&mut self, rc: &mut RangeEnc, mut pos: usize, end: usize, byte_budget: usize) -> usize {
        let d = self.d;
        let nice = self.p.nice;
        while pos < end && rc.size_estimate() < byte_budget {
            let max = (end - pos).min(MATCH_MAX);
            let (rlen, ridx) = if pos > 0 { self.rep_candidate(pos, max) } else { (0, 0) };
            let (mut mlen, mut mdist) = self.mf.find(d, pos, max);
            if mlen == 2 && mdist >= 128 {
                mlen = 0;
            }
            // Take a repeat when it is nearly as long as the best new match.
            let use_rep = rlen >= 2
                && (rlen + 1 >= mlen || (rlen + 2 >= mlen && mdist >= 1 << 9) || (rlen + 3 >= mlen && mdist >= 1 << 15));
            let (len, is_rep) = if rlen >= nice.min(max) && rlen >= 2 {
                (rlen, true)
            } else if mlen >= nice.min(max) && mlen >= 2 && !use_rep {
                (mlen, false)
            } else if use_rep {
                (rlen, true)
            } else {
                (mlen, false)
            };
            if len < 2 {
                // Literal, or a one-byte repeat of the last distance.
                if pos > self.c.reps[0] && d[pos] == d[pos - self.c.reps[0] - 1] && self.c.state >= 7 && pos > 0 {
                    self.c.short_rep(rc, pos);
                } else {
                    self.c.literal(rc, d, pos);
                }
                pos += 1;
                continue;
            }
            // One step of lazy matching: is the next position clearly better?
            if self.p.lazy && len < nice && pos + 1 < end {
                let max2 = (end - pos - 1).min(MATCH_MAX);
                let (rlen2, _) = self.rep_candidate(pos + 1, max2);
                let probe = self.mf_probe(pos + 1, max2);
                let better = |l2: usize, d2: usize| {
                    if is_rep {
                        l2 > len + 1
                    } else {
                        l2 > len || (l2 == len && d2 < mdist)
                    }
                };
                if (rlen2 >= 2 && rlen2 > len + 1 && is_rep) || (rlen2 >= len + 2 && !is_rep) || better(probe.0, probe.1) && probe.0 >= 3 && (probe.0 > len + 1 || probe.1 < mdist / 4) {
                    self.c.literal(rc, d, pos);
                    pos += 1;
                    continue;
                }
            }
            if is_rep {
                self.c.rep_match(rc, pos, ridx, len);
            } else {
                // A new match whose distance is already a repeat is cheaper as one.
                if let Some(i) = self.c.reps.iter().position(|&r| r == mdist) {
                    self.c.rep_match(rc, pos, i, len);
                } else {
                    self.c.new_match(rc, pos, len, mdist);
                }
            }
            let _ = &mut mdist;
            for i in pos + 1..pos + len {
                self.mf.insert(d, i);
            }
            pos += len;
        }
        pos
    }

    /// The best match at `pos` without recording it (the chain walk does not mutate).
    fn mf_probe(&mut self, pos: usize, max: usize) -> (usize, usize) {
        let saved = self.mf.next;
        // `find` inserts the position; pretend it was already inserted so the real pass
        // at `pos` does not insert twice.
        let r = self.mf.find(self.d, pos, max);
        self.mf.next = saved.max(pos + 1);
        r
    }
}

fn setup(data: &[u8], level: Level) -> Parser<'_> {
    let p = params(level);
    let mf = Finder::new(data.len(), &p);
    Parser {
        d: data,
        mf,
        c: Coder::new(3, 0, 2),
        p,
    }
}

/// Raw LZMA stream; returns the properties byte, the dictionary size and the stream.
pub fn encode_lzma(data: &[u8], level: Level, end_marker: bool) -> (u8, u32, Vec<u8>) {
    let mut ps = setup(data, level);
    let mut rc = RangeEnc::new();
    let mut pos = 0;
    while pos < data.len() {
        pos = ps.run(&mut rc, pos, data.len(), usize::MAX);
    }
    if end_marker {
        ps.c.end_marker(&mut rc, pos);
    }
    let dict = ps.mf.window().max(4096) as u32;
    ((2 * 5 + 0) * 9 + 3, dict, rc.finish())
}

/// LZMA2 chunk stream; returns the dictionary-size property byte and the data.
pub fn encode_lzma2(data: &[u8], level: Level) -> Result<(u8, Vec<u8>)> {
    let mut ps = setup(data, level);
    let dict = ps.mf.window().max(4096);
    let dict_prop = dict_prop(dict);
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut first = true;
    let mut need_state_reset = true;
    let props_byte = ((2 * 5 + 0) * 9 + 3) as u8;
    while pos < data.len() {
        let start = pos;
        let end = (start + (1 << 21)).min(data.len());
        let saved_coder = Coder {
            m: ps.c.m.clone(),
            state: ps.c.state,
            reps: ps.c.reps,
        };
        if need_state_reset {
            ps.c.reset();
        }
        let mut rc = RangeEnc::new();
        let reached = ps.run(&mut rc, start, end, 65_000);
        let comp = rc.finish();
        let n = reached - start;
        if comp.len() >= n || comp.len() > 65_536 || n == 0 {
            // Not worth coding: store the bytes and forget what this chunk did to the model.
            ps.c = saved_coder;
            ps.c.reset();
            pos = start;
            let mut left = (end - start).min(1 << 16);
            if reached > start && comp.len() < n {
                left = left.min(n);
            }
            let body = &data[pos..pos + left];
            out.push(if first { 1 } else { 2 });
            out.extend_from_slice(&((left - 1) as u16).to_be_bytes());
            out.extend_from_slice(body);
            pos += left;
            first = false;
            need_state_reset = true;
            // The match finder already saw these positions; make sure it saw all of them.
            for i in start..pos {
                ps.mf.insert(data, i);
            }
            continue;
        }
        let ctl = if first {
            0xE0
        } else if need_state_reset {
            0xC0
        } else {
            0x80
        };
        let u = n - 1;
        out.push(ctl | ((u >> 16) as u8 & 0x1F));
        out.push((u >> 8) as u8);
        out.push(u as u8);
        out.extend_from_slice(&((comp.len() - 1) as u16).to_be_bytes());
        if ctl >= 0xC0 {
            out.push(props_byte);
        }
        out.extend_from_slice(&comp);
        pos = reached;
        first = false;
        need_state_reset = false;
    }
    out.push(0);
    let _ = unsupported;
    Ok((dict_prop, out))
}

/// The LZMA2 dictionary size property for at least `dict` bytes.
pub fn dict_prop(dict: usize) -> u8 {
    for p in 0u8..40 {
        let size = (2usize | usize::from(p & 1)) << (p / 2 + 11);
        if size >= dict {
            return p;
        }
    }
    40
}
