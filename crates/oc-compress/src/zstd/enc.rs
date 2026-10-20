//! Zstandard encoder: hash-chain LZ77 (greedy or lazy by level), Huffman-coded literals with
//! one or four streams, FSE-coded sequences with predefined, RLE or custom tables.
#![allow(clippy::indexing_slicing)] // sizes are derived from the data being encoded
use super::dec::{default_tables, LL_BASE, LL_BITS, ML_BASE, ML_BITS};
use super::fse::Table;
use crate::deflate::code_lengths;
use oc_checksum::xxhash64;

const MAX_BLOCK: usize = 128 * 1024;
const MIN_MATCH: usize = 3;

// ---------------------------------------------------------------- bit writer

/// Least-significant-bit-first writer; backward streams end with a sentinel bit.
#[derive(Default)]
struct BitW {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitW {
    fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.acc |= u64::from(v & (((1u64 << n) - 1) as u32)) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// Close a backward stream: sentinel one bit, then zero padding.
    fn close(mut self) -> Vec<u8> {
        self.put(1, 1);
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
    /// Close a forward stream (table descriptions): zero padding only.
    fn flush(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

// ---------------------------------------------------------------- FSE encoding

fn highbit(v: u32) -> u32 {
    31 - v.leading_zeros()
}

/// FSE table log for `src` symbols with symbols up to `max_symbol` (libzstd's rule).
fn optimal_log(max_log: u32, src: usize, max_symbol: usize) -> u32 {
    let min_bits_src = highbit(src as u32) + 1;
    let min_bits_sym = highbit(max_symbol as u32) + 2;
    let min_bits = min_bits_src.min(min_bits_sym);
    let max_bits_src = highbit((src as u32).saturating_sub(1).max(1)).saturating_sub(2);
    let mut tl = max_log;
    if max_bits_src < tl {
        tl = max_bits_src;
    }
    if min_bits > tl {
        tl = min_bits;
    }
    tl.clamp(5, max_log)
}

/// Scales a histogram to `1 << tl`, every used symbol keeping at least one cell.
/// `None` when a single symbol makes up everything.
fn normalize(hist: &[u32], tl: u32) -> Option<Vec<i16>> {
    let total: u64 = hist.iter().map(|&h| u64::from(h)).sum();
    if total == 0 || hist.iter().any(|&h| u64::from(h) == total) {
        return None;
    }
    let size = 1i64 << tl;
    let used = hist.iter().filter(|&&h| h > 0).count() as i64;
    if used > size {
        return None;
    }
    let mut norm = vec![0i64; hist.len()];
    let mut sum = 0i64;
    for (s, &h) in hist.iter().enumerate() {
        if h > 0 {
            let p = ((u64::from(h) * size as u64 + total / 2) / total).max(1) as i64;
            norm[s] = p;
            sum += p;
        }
    }
    while sum != size {
        if sum < size {
            // Give a cell to the symbol that gains the most from it.
            let best = (0..norm.len()).filter(|&s| norm[s] > 0).max_by(|&a, &b| {
                let ga = f64::from(hist[a]) / norm[a] as f64;
                let gb = f64::from(hist[b]) / norm[b] as f64;
                ga.total_cmp(&gb)
            })?;
            norm[best] += 1;
            sum += 1;
        } else {
            let best = (0..norm.len()).filter(|&s| norm[s] > 1).min_by(|&a, &b| {
                let la = f64::from(hist[a]) / (norm[a] - 1) as f64;
                let lb = f64::from(hist[b]) / (norm[b] - 1) as f64;
                la.total_cmp(&lb)
            })?;
            norm[best] -= 1;
            sum -= 1;
        }
    }
    Some(norm.into_iter().map(|v| v as i16).collect())
}

/// Serialises a normalised distribution the way [`super::fse::read_counts`] reads it.
fn write_counts(norm: &[i16], al: u32) -> Vec<u8> {
    let mut w = BitW::default();
    w.put(al - 5, 4);
    let mut remaining = (1i32 << al) + 1;
    let mut threshold = 1i32 << al;
    let mut nb = al + 1;
    // Trailing zero-probability symbols are not described.
    let last = norm.iter().rposition(|&c| c != 0).unwrap_or(0);
    let mut s = 0usize;
    let mut prev_zero = false;
    while remaining > 1 && s <= last {
        if prev_zero {
            let start = s;
            while s <= last && norm[s] == 0 {
                s += 1;
            }
            let mut n0 = s - start;
            while n0 >= 3 {
                w.put(3, 2);
                n0 -= 3;
            }
            w.put(n0 as u32, 2);
            if s > last {
                break;
            }
        }
        let c = i32::from(norm[s]);
        s += 1;
        let max = (2 * threshold - 1) - remaining;
        remaining -= c.abs();
        let mut count = c + 1;
        if count >= threshold {
            count += max;
        }
        // Values below `max` need one bit less.
        w.put(count as u32, if count < max { nb - 1 } else { nb });
        prev_zero = c == 0;
        while remaining < threshold {
            nb -= 1;
            threshold >>= 1;
        }
    }
    w.flush()
}

/// Per-symbol list of (base, bits, cell) used to find the cell a state transition goes through.
struct EncTable {
    al: u32,
    by_symbol: Vec<Vec<(u32, u32, u32)>>,
}

impl EncTable {
    fn new(t: &Table, symbols: usize) -> Self {
        let mut by_symbol = vec![Vec::new(); symbols.max(256)];
        for (i, c) in t.cells.iter().enumerate() {
            by_symbol[usize::from(c.symbol)].push((u32::from(c.base), u32::from(c.nb), i as u32));
        }
        for v in &mut by_symbol {
            v.sort_unstable();
        }
        Self {
            al: t.al,
            by_symbol,
        }
    }

    /// A cell that decodes `sym` (the state the encoder starts from for the last symbol).
    /// The cell with the most state bits is chosen: a decoder that reads weights relies on the
    /// update after the second-to-last symbol running out of bits, which needs at least one.
    fn start(&self, sym: usize) -> u32 {
        self.by_symbol[sym]
            .iter()
            .max_by_key(|c| c.1)
            .map_or(0, |c| c.2)
    }

    /// Writes the bits that lead the decoder from the cell of `sym` to `state`; returns that cell.
    fn encode(&self, w: &mut BitW, state: u32, sym: usize) -> u32 {
        let cells = &self.by_symbol[sym];
        let i = cells.partition_point(|c| c.0 <= state).saturating_sub(1);
        let (base, nb, cell) = cells[i];
        w.put(state - base, nb);
        cell
    }
}

// ---------------------------------------------------------------- literals

struct HufEnc {
    lens: Vec<u8>,
    codes: Vec<u32>,
    max_bits: u32,
    last: usize,
}

fn build_huffman(lits: &[u8]) -> Option<HufEnc> {
    let mut hist = [0u32; 256];
    for &b in lits {
        hist[usize::from(b)] += 1;
    }
    let used = hist.iter().filter(|&&h| h > 0).count();
    if used < 2 {
        return None;
    }
    let lens = code_lengths(&hist, 11);
    let max_bits = u32::from(*lens.iter().max()?);
    let last = lens.iter().rposition(|&l| l > 0)?;
    // Canonical order: longest codes first; the table is filled from index zero upward.
    let mut codes = vec![0u32; 256];
    let mut pos = 0u32;
    for len in (1..=max_bits as u8).rev() {
        for (s, &l) in lens.iter().enumerate() {
            if l == len {
                codes[s] = pos >> (max_bits - u32::from(len));
                pos += 1 << (max_bits - u32::from(len));
            }
        }
    }
    Some(HufEnc {
        lens,
        codes,
        max_bits,
        last,
    })
}

impl HufEnc {
    fn weight(&self, s: usize) -> u8 {
        if self.lens[s] == 0 {
            0
        } else {
            (self.max_bits + 1 - u32::from(self.lens[s])) as u8
        }
    }

    /// Tree description: direct 4-bit weights or FSE-compressed, whichever is shorter.
    fn describe(&self) -> Vec<u8> {
        let weights: Vec<u8> = (0..self.last).map(|s| self.weight(s)).collect();
        let mut best: Vec<u8> = Vec::new();
        if weights.len() <= 128 {
            best.push((127 + weights.len()) as u8);
            for pair in weights.chunks(2) {
                best.push((pair[0] << 4) | pair.get(1).copied().unwrap_or(0));
            }
        }
        if weights.len() >= 3 {
            if let Some(c) = fse_weights(&weights) {
                if c.len() < 128 && (best.is_empty() || c.len() + 1 < best.len()) {
                    best = std::iter::once(c.len() as u8).chain(c).collect();
                }
            }
        }
        best
    }

    fn stream(&self, data: &[u8]) -> Vec<u8> {
        let mut w = BitW::default();
        for &b in data.iter().rev() {
            w.put(
                self.codes[usize::from(b)],
                u32::from(self.lens[usize::from(b)]),
            );
        }
        w.close()
    }
}

/// FSE-compressed Huffman weights (accuracy up to 6), or `None` if they do not compress.
fn fse_weights(weights: &[u8]) -> Option<Vec<u8>> {
    let mut hist = [0u32; 13];
    for &w in weights {
        hist[usize::from(w)] += 1;
    }
    let max_sym = hist.iter().rposition(|&h| h > 0)?;
    let tl = optimal_log(6, weights.len(), max_sym);
    let norm = normalize(&hist[..=max_sym], tl)?;
    let table = Table::from_counts(&norm, tl).ok()?;
    let enc = EncTable::new(&table, 13);
    let n = weights.len();
    let mut w = BitW::default();
    // The last two weights only set the starting states.
    let mut st = [0u32; 2];
    st[(n - 1) % 2] = enc.start(usize::from(weights[n - 1]));
    st[(n - 2) % 2] = enc.start(usize::from(weights[n - 2]));
    for k in (0..n - 2).rev() {
        st[k % 2] = enc.encode(&mut w, st[k % 2], usize::from(weights[k]));
    }
    w.put(st[1], tl);
    w.put(st[0], tl);
    let mut out = write_counts(&norm, tl);
    out.extend(w.close());
    Some(out)
}

/// Returns the literals section bytes.
fn literals_section(lits: &[u8]) -> Vec<u8> {
    let n = lits.len();
    let raw = |lits: &[u8], kind: u8| -> Vec<u8> {
        let n = lits.len();
        let mut out = Vec::new();
        if n < 32 {
            out.push(kind | ((n as u8) << 3));
        } else if n < 4096 {
            let v = u32::from(kind) | (1 << 2) | ((n as u32) << 4);
            out.extend_from_slice(&v.to_le_bytes()[..2]);
        } else {
            let v = u32::from(kind) | (3 << 2) | ((n as u32) << 4);
            out.extend_from_slice(&v.to_le_bytes()[..3]);
        }
        out
    };
    if n == 0 {
        return raw(lits, 0);
    }
    if lits.iter().all(|&b| b == lits[0]) && n > 1 {
        let mut h = raw(lits, 1);
        h.push(lits[0]);
        return h;
    }
    let plain = |lits: &[u8]| {
        let mut h = raw(lits, 0);
        h.extend_from_slice(lits);
        h
    };
    if n < 32 {
        return plain(lits);
    }
    let Some(huf) = build_huffman(lits) else {
        return plain(lits);
    };
    let tree = huf.describe();
    if tree.is_empty() {
        return plain(lits);
    }
    let four = n >= 256;
    let mut body = tree;
    if four {
        let per = n.div_ceil(4);
        let parts: Vec<Vec<u8>> = [
            &lits[..per],
            &lits[per..2 * per],
            &lits[2 * per..3 * per],
            &lits[3 * per..],
        ]
        .iter()
        .map(|p| huf.stream(p))
        .collect();
        if parts.iter().take(3).any(|p| p.len() > 65_535) {
            return plain(lits);
        }
        for p in parts.iter().take(3) {
            body.extend_from_slice(&(p.len() as u16).to_le_bytes());
        }
        for p in &parts {
            body.extend_from_slice(p);
        }
    } else {
        body.extend(huf.stream(lits));
    }
    let comp = body.len();
    if comp + 4 > n || comp >= 1 << 18 {
        return plain(lits);
    }
    // Header: type 2 (compressed), size format by size, regenerated and compressed sizes.
    let (fmt, hdr_bytes, bits) = if !four && n < 1024 && comp < 1024 {
        (0u64, 3, 10)
    } else if n < 1024 && comp < 1024 {
        (1, 3, 10)
    } else if n < 16_384 && comp < 16_384 {
        (2, 4, 14)
    } else {
        (3, 5, 18)
    };
    let v: u64 = 2 | (fmt << 2) | ((n as u64) << 4) | ((comp as u64) << (4 + bits));
    let mut out = v.to_le_bytes()[..hdr_bytes].to_vec();
    out.extend(body);
    out
}

// ---------------------------------------------------------------- sequences

#[derive(Clone, Copy)]
struct Seq {
    ll: u32,
    ml: u32,
    /// Offset value as Zstandard codes it: 1..=3 repeat codes, otherwise distance + 3.
    ov: u32,
}

fn code_of(table: &[u32], v: u32) -> usize {
    table.partition_point(|&b| b <= v) - 1
}

enum Mode {
    Predefined,
    Rle(u8),
    Custom(Vec<i16>, u32),
}

/// Cost in bits of coding `hist` with a distribution given as normalised counts.
fn cost_bits(hist: &[u32], norm: &[i16], al: u32) -> f64 {
    let mut bits = 0.0;
    for (s, &h) in hist.iter().enumerate() {
        if h == 0 {
            continue;
        }
        let c = match norm.get(s).copied() {
            Some(c) if c != 0 => f64::from(c.unsigned_abs()),
            _ => return f64::INFINITY,
        };
        bits += f64::from(h) * (f64::from(al) - c.log2());
    }
    bits
}

fn choose_mode(hist: &[u32], default: &[i16], default_al: u32, max_al: u32, nseq: usize) -> Mode {
    if let Some(only) = hist.iter().position(|&h| h as usize == nseq) {
        return Mode::Rle(only as u8);
    }
    let max_sym = hist.iter().rposition(|&h| h > 0).unwrap_or(0);
    let predefined = cost_bits(hist, default, default_al);
    let tl = optimal_log(max_al, nseq, max_sym);
    if let Some(norm) = normalize(&hist[..=max_sym], tl) {
        let desc = write_counts(&norm, tl).len() as f64 * 8.0;
        let custom = cost_bits(hist, &norm, tl) + desc;
        if custom < predefined {
            return Mode::Custom(norm, tl);
        }
    }
    Mode::Predefined
}

fn sequences_section(seqs: &[Seq]) -> Vec<u8> {
    let n = seqs.len();
    let mut out = Vec::new();
    if n == 0 {
        out.push(0);
        return out;
    }
    if n < 128 {
        out.push(n as u8);
    } else if n < 0x7F00 {
        out.push(((n >> 8) + 128) as u8);
        out.push(n as u8);
    } else {
        out.push(255);
        out.extend_from_slice(&((n - 0x7F00) as u16).to_le_bytes());
    }
    let ll_codes: Vec<usize> = seqs.iter().map(|s| code_of(&LL_BASE, s.ll)).collect();
    let ml_codes: Vec<usize> = seqs.iter().map(|s| code_of(&ML_BASE, s.ml)).collect();
    let of_codes: Vec<usize> = seqs.iter().map(|s| highbit(s.ov) as usize).collect();
    let mut hl = vec![0u32; 36];
    let mut hm = vec![0u32; 53];
    let mut ho = vec![0u32; 32];
    for i in 0..n {
        hl[ll_codes[i]] += 1;
        hm[ml_codes[i]] += 1;
        ho[of_codes[i]] += 1;
    }
    let (dl, dof, dm) = (
        super::dec::LL_DEFAULT.as_slice(),
        super::dec::OF_DEFAULT.as_slice(),
        super::dec::ML_DEFAULT.as_slice(),
    );
    let ml_mode = choose_mode(&hm, dm, 6, 9, n);
    let ll_mode = choose_mode(&hl, dl, 6, 9, n);
    let of_mode = choose_mode(&ho, dof, 5, 8, n);
    let code = |m: &Mode| match m {
        Mode::Predefined => 0u8,
        Mode::Rle(_) => 1,
        Mode::Custom(..) => 2,
    };
    out.push((code(&ll_mode) << 6) | (code(&of_mode) << 4) | (code(&ml_mode) << 2));
    let (def_ll, def_of, def_ml) = default_tables();
    let build = |m: &Mode, def: Table, out: &mut Vec<u8>| -> Table {
        match m {
            Mode::Predefined => def,
            Mode::Rle(s) => {
                out.push(*s);
                Table::rle(*s)
            }
            Mode::Custom(norm, al) => {
                out.extend(write_counts(norm, *al));
                Table::from_counts(norm, *al).unwrap_or(def)
            }
        }
    };
    let t_ll = build(&ll_mode, def_ll, &mut out);
    let t_of = build(&of_mode, def_of, &mut out);
    let t_ml = build(&ml_mode, def_ml, &mut out);
    let (e_ll, e_of, e_ml) = (
        EncTable::new(&t_ll, 36),
        EncTable::new(&t_of, 32),
        EncTable::new(&t_ml, 53),
    );
    let mut w = BitW::default();
    let last = n - 1;
    let mut s_ll = e_ll.start(ll_codes[last]);
    let mut s_of = e_of.start(of_codes[last]);
    let mut s_ml = e_ml.start(ml_codes[last]);
    let extras = |w: &mut BitW, i: usize| {
        let s = &seqs[i];
        w.put(s.ll - LL_BASE[ll_codes[i]], u32::from(LL_BITS[ll_codes[i]]));
        w.put(s.ml - ML_BASE[ml_codes[i]], u32::from(ML_BITS[ml_codes[i]]));
        w.put(s.ov - (1 << of_codes[i]), of_codes[i] as u32);
    };
    extras(&mut w, last);
    for i in (0..last).rev() {
        s_of = e_of.encode(&mut w, s_of, of_codes[i]);
        s_ml = e_ml.encode(&mut w, s_ml, ml_codes[i]);
        s_ll = e_ll.encode(&mut w, s_ll, ll_codes[i]);
        extras(&mut w, i);
    }
    w.put(s_ml, e_ml.al);
    w.put(s_of, e_of.al);
    w.put(s_ll, e_ll.al);
    out.extend(w.close());
    out
}

// ---------------------------------------------------------------- parsing

#[derive(Clone, Copy)]
pub struct Params {
    depth: usize,
    nice: usize,
    lazy: u8,
    hash_bits: u32,
}

fn params(level: i32) -> Params {
    let (depth, nice, lazy) = match level {
        i32::MIN..=0 => (1, 8, 0),
        1 => (2, 16, 0),
        2 => (4, 24, 0),
        3 => (8, 32, 0),
        4 => (12, 40, 1),
        5 => (24, 48, 1),
        6 => (32, 64, 1),
        7 => (48, 96, 1),
        8 => (64, 128, 1),
        9 => (96, 160, 2),
        10..=12 => (160, 256, 2),
        13..=16 => (320, 384, 2),
        _ => (640, 512, 2),
    };
    Params {
        depth,
        nice,
        lazy,
        hash_bits: 17,
    }
}

struct Matcher<'a> {
    d: &'a [u8],
    head: Vec<u32>,
    prev: Vec<u32>,
    mask: usize,
    window: usize,
    p: Params,
    next: usize,
}

const NIL: u32 = u32::MAX;

impl<'a> Matcher<'a> {
    fn new(d: &'a [u8], window: usize, p: Params) -> Self {
        let size = window.min(d.len().max(1)).next_power_of_two();
        Self {
            d,
            head: vec![NIL; 1 << p.hash_bits],
            prev: vec![NIL; size],
            mask: size - 1,
            window: size.min(window),
            p,
            next: 0,
        }
    }

    fn hash(&self, i: usize) -> usize {
        let w = u32::from_le_bytes([self.d[i], self.d[i + 1], self.d[i + 2], self.d[i + 3]]);
        (w.wrapping_mul(2_654_435_761) >> (32 - self.p.hash_bits)) as usize
    }

    fn insert(&mut self, i: usize) {
        if i < self.next || i + 4 > self.d.len() {
            return;
        }
        self.next = i + 1;
        let h = self.hash(i);
        self.prev[i & self.mask] = self.head[h];
        self.head[h] = i as u32;
    }

    fn len(&self, a: usize, b: usize, max: usize) -> usize {
        let mut n = 0;
        while n < max && self.d[a + n] == self.d[b + n] {
            n += 1;
        }
        n
    }

    /// Best chain match at `i`: (length, distance).
    fn find(&self, i: usize, max: usize) -> (usize, usize) {
        let mut best = (0, 0);
        if max < 4 || i + 4 > self.d.len() {
            return best;
        }
        let mut cand = self.head[self.hash(i)];
        let mut steps = self.p.depth;
        while cand != NIL && steps > 0 {
            let c = cand as usize;
            if c >= i || i - c > self.window {
                break;
            }
            if best.0 == 0 || self.d[c + best.0] == self.d[i + best.0] {
                let l = self.len(c, i, max);
                if l > best.0 || (l == best.0 && i - c < best.1) {
                    best = (l, i - c);
                    if l >= self.p.nice || l == max {
                        break;
                    }
                }
            }
            let n = self.prev[c & self.mask];
            if n != NIL && n as usize >= c {
                break;
            }
            cand = n;
            steps -= 1;
        }
        if best.0 < 4 {
            return (0, 0);
        }
        best
    }
}

/// The offset value for `dist` given the literal run and the repeat history; updates the history.
fn offset_value(dist: u32, ll: u32, reps: &mut [u32; 3]) -> u32 {
    if ll > 0 {
        if dist == reps[0] {
            return 1;
        }
        if dist == reps[1] {
            *reps = [dist, reps[0], reps[2]];
            return 2;
        }
        if dist == reps[2] {
            *reps = [dist, reps[0], reps[1]];
            return 3;
        }
    } else {
        if dist == reps[1] {
            *reps = [dist, reps[0], reps[2]];
            return 1;
        }
        if dist == reps[2] {
            *reps = [dist, reps[0], reps[1]];
            return 2;
        }
        if reps[0] > 1 && dist == reps[0] - 1 {
            *reps = [dist, reps[0], reps[1]];
            return 3;
        }
    }
    *reps = [dist, reps[0], reps[1]];
    dist + 3
}

/// Whether `dist` would be coded as a repeat given `ll` literals first.
fn is_repeat(dist: u32, ll: u32, reps: &[u32; 3]) -> bool {
    if ll > 0 {
        reps.contains(&dist)
    } else {
        dist == reps[1] || dist == reps[2] || (reps[0] > 1 && dist == reps[0] - 1)
    }
}

fn gain(len: usize, dist: usize, rep: bool) -> i64 {
    if rep {
        len as i64 * 4
    } else {
        len as i64 * 4 - i64::from(highbit(dist as u32 + 1))
    }
}

/// Splits `d[start..end]` into sequences and trailing literals.
fn parse(
    m: &mut Matcher<'_>,
    start: usize,
    end: usize,
    reps: &mut [u32; 3],
    literals: &mut Vec<u8>,
) -> Vec<Seq> {
    let d = m.d;
    let mut seqs = Vec::new();
    let mut i = start;
    let mut anchor = start;
    let nice = m.p.nice;
    while i < end {
        let max = end - i;
        let ll = (i - anchor) as u32;
        // Repeat candidates (three bytes are enough).
        let mut rep_best = (0usize, 0usize);
        for &r in reps.iter() {
            let r = r as usize;
            if r <= i && max >= MIN_MATCH {
                let l = m.len(i - r, i, max);
                if l >= MIN_MATCH && is_repeat(r as u32, ll, reps) && l > rep_best.0 {
                    rep_best = (l, r);
                }
            }
        }
        let (mlen, mdist) = m.find(i, max);
        m.insert(i);
        let g_main = if mlen >= 4 {
            gain(mlen, mdist, is_repeat(mdist as u32, ll, reps))
        } else {
            i64::MIN
        };
        let g_rep = if rep_best.0 >= MIN_MATCH {
            gain(rep_best.0, rep_best.1, true)
        } else {
            i64::MIN
        };
        let (mut len, mut dist, mut g) = if g_rep >= g_main && g_rep != i64::MIN {
            (rep_best.0, rep_best.1, g_rep)
        } else if g_main != i64::MIN {
            (mlen, mdist, g_main)
        } else {
            (0, 0, i64::MIN)
        };
        if len == 0 {
            i += 1;
            continue;
        }
        // Lazy matching: a clearly better match one or two bytes later wins.
        let mut depth = 0;
        while len < nice && m.p.lazy > depth && i + 1 < end {
            let (l2, d2) = m.find(i + 1, end - i - 1);
            let g2 = if l2 >= 4 {
                gain(l2, d2, is_repeat(d2 as u32, ll + 1, reps)) - 4
            } else {
                i64::MIN
            };
            if g2 > g {
                m.insert(i + 1);
                i += 1;
                len = l2;
                dist = d2;
                g = g2;
                depth += 1;
                continue;
            }
            break;
        }
        let ll = (i - anchor) as u32;
        literals.extend_from_slice(&d[anchor..i]);
        let ov = offset_value(dist as u32, ll, reps);
        seqs.push(Seq {
            ll,
            ml: len as u32,
            ov,
        });
        for k in i + 1..i + len {
            m.insert(k);
        }
        i += len;
        anchor = i;
    }
    literals.extend_from_slice(&d[anchor..end]);
    seqs
}

// ---------------------------------------------------------------- frames

/// Compresses `data` into one Zstandard frame. `level` 1..=19 (0 and below are the fast end).
pub fn compress(data: &[u8], level: i32, checksum: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xFD2F_B528u32.to_le_bytes());
    let n = data.len() as u64;
    let window_log = (64 - n.max(1).saturating_sub(1).leading_zeros()).clamp(10, 27);
    let single = n <= 1 << 23;
    let (fcs_flag, fcs_len): (u8, usize) = if single && n < 256 {
        (0, 1)
    } else if (256..65_536 + 256).contains(&n) {
        (1, 2)
    } else if n <= u64::from(u32::MAX) {
        (2, 4)
    } else {
        (3, 8)
    };
    let desc = (fcs_flag << 6) | (u8::from(single) << 5) | (u8::from(checksum) << 2);
    out.push(desc);
    if !single {
        out.push(((window_log - 10) << 3) as u8);
    }
    match fcs_len {
        1 => out.push(n as u8),
        2 => out.extend_from_slice(&((n - 256) as u16).to_le_bytes()),
        4 => out.extend_from_slice(&(n as u32).to_le_bytes()),
        _ => out.extend_from_slice(&n.to_le_bytes()),
    }
    let window = if single {
        n as usize
    } else {
        1usize << window_log
    };
    let mut m = Matcher::new(data, window.max(1024), params(level));
    let mut reps = [1u32, 4, 8];
    let mut pos = 0usize;
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00]);
    }
    while pos < data.len() {
        let end = (pos + MAX_BLOCK).min(data.len());
        let last = end == data.len();
        let block = &data[pos..end];
        let size = block.len();
        let hdr = |ty: u32, sz: usize| -> [u8; 3] {
            let v = u32::from(last) | (ty << 1) | ((sz as u32) << 3);
            [v as u8, (v >> 8) as u8, (v >> 16) as u8]
        };
        if block.iter().all(|&b| b == block[0]) {
            out.extend_from_slice(&hdr(1, size));
            out.push(block[0]);
            pos = end;
            continue;
        }
        let saved = reps;
        let mut lits = Vec::new();
        let seqs = parse(&mut m, pos, end, &mut reps, &mut lits);
        let mut body = literals_section(&lits);
        body.extend(sequences_section(&seqs));
        if body.len() >= size {
            // Incompressible: store it, and forget this block's effect on the repeat history.
            reps = saved;
            out.extend_from_slice(&hdr(0, size));
            out.extend_from_slice(block);
        } else {
            out.extend_from_slice(&hdr(2, body.len()));
            out.extend(body);
        }
        pos = end;
    }
    if checksum {
        out.extend_from_slice(&(xxhash64(data, 0) as u32).to_le_bytes());
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::zstd::fse::read_counts;

    #[test]
    fn count_descriptions_roundtrip() {
        let mut s = 7u32;
        let mut r = move || {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            s >> 16
        };
        for _ in 0..500 {
            let syms = 2 + (r() % 50) as usize;
            let mut hist = vec![0u32; syms];
            for h in hist.iter_mut() {
                if r() % 3 != 0 {
                    *h = 1 + r() % 200;
                }
            }
            if hist.iter().filter(|&&h| h > 0).count() < 2 {
                continue;
            }
            let total: usize = hist.iter().map(|&h| h as usize).sum();
            let max_sym = hist.iter().rposition(|&h| h > 0).unwrap();
            let tl = optimal_log(9, total, max_sym);
            let Some(norm) = normalize(&hist[..=max_sym], tl) else {
                continue;
            };
            let bytes = write_counts(&norm, tl);
            let (got, al, used) = read_counts(&bytes, max_sym.max(35), 9).unwrap();
            assert_eq!(al, tl);
            assert_eq!(used, bytes.len());
            let mut want = norm.clone();
            while want.last() == Some(&0) {
                want.pop();
            }
            let mut got = got;
            while got.last() == Some(&0) {
                got.pop();
            }
            assert_eq!(got, want, "hist {hist:?}");
        }
    }
}
