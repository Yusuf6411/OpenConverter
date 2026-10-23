//! Brotli compression (RFC 7932), encoder only: hash-chain LZ77 and one prefix code per
//! alphabet in each meta-block. The static dictionary is never referenced, so no dictionary
//! is needed to write; reading Brotli (which needs it) is not built.
#![allow(clippy::indexing_slicing)] // indices are bounded by alphabet sizes and loop ranges

use crate::deflate::{canonical_codes, code_lengths};

/// Bit sink, least significant bit first.
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, v: u64, bits: u32) {
        debug_assert!(bits <= 32);
        self.acc |= (v & ((1u64 << bits) - 1)) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

const INS_BASE: [u32; 24] = [
    0, 1, 2, 3, 4, 5, 6, 8, 10, 14, 18, 26, 34, 50, 66, 98, 130, 194, 322, 578, 1090, 2114, 6210,
    22594,
];
const INS_EXTRA: [u32; 24] = [
    0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 12, 14, 24,
];
const COPY_BASE: [u32; 24] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70, 102, 134, 198, 326, 582, 1094, 2118,
];
const COPY_EXTRA: [u32; 24] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 24,
];

const CL_ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];
/// The code for code-length-code lengths 0 to 5: (bits, count), least significant first.
const CL_LEN_CODE: [(u32, u32); 6] = [
    (0b00, 2),
    (0b0111, 4),
    (0b011, 3),
    (0b10, 2),
    (0b01, 2),
    (0b1111, 4),
];

fn code_for(table: &[u32], v: u32) -> usize {
    table.iter().rposition(|&b| b <= v).unwrap_or(0)
}

struct Cmd {
    ins: u32,
    copy: u32,
    dist: u32,
}

struct Level {
    chain: usize,
    nice: usize,
}

fn level_params(level: u32) -> Level {
    match level {
        0..=1 => Level { chain: 4, nice: 32 },
        2..=4 => Level {
            chain: 16,
            nice: 64,
        },
        5..=8 => Level {
            chain: 64,
            nice: 128,
        },
        _ => Level {
            chain: 256,
            nice: 258,
        },
    }
}

fn parse(data: &[u8], start: usize, end: usize, window: usize, lv: &Level) -> Vec<Cmd> {
    const HB: u32 = 16;
    let mut head = vec![u32::MAX; 1 << HB];
    let mut prev = vec![u32::MAX; data.len().max(1)];
    let hash = |i: usize| -> usize {
        let w = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        (w.wrapping_mul(2_654_435_761) >> (32 - HB)) as usize
    };
    let mut cmds = Vec::new();
    // Seed the tables with the bytes before this meta-block so it can refer back to them.
    let mut insert_from = start.saturating_sub(window);
    let mut i = start;
    let mut anchor = start;
    let insert = |upto: usize, head: &mut Vec<u32>, prev: &mut Vec<u32>, from: &mut usize| {
        while *from < upto {
            if *from + 4 <= data.len() {
                let h = hash(*from);
                prev[*from] = head[h];
                head[h] = *from as u32;
            }
            *from += 1;
        }
    };
    while i + 4 <= end {
        insert(i, &mut head, &mut prev, &mut insert_from);
        let h = hash(i);
        let mut cand = head[h];
        let mut best = (0usize, 0usize);
        let mut steps = lv.chain;
        let max = (end - i).min(1 << 24);
        let limit = i.min(window.saturating_sub(16));
        while cand != u32::MAX && steps > 0 {
            let c = cand as usize;
            if i - c > limit {
                break;
            }
            if best.0 == 0 || data[c + best.0.min(max - 1)] == data[i + best.0.min(max - 1)] {
                let mut l = 0;
                while l < max && data[c + l] == data[i + l] {
                    l += 1;
                }
                if l > best.0 {
                    best = (l, i - c);
                    if l >= lv.nice || l == max {
                        break;
                    }
                }
            }
            let n = prev[c];
            if n != u32::MAX && n as usize >= c {
                break;
            }
            cand = n;
            steps -= 1;
        }
        if best.0 >= 4 {
            cmds.push(Cmd {
                ins: (i - anchor) as u32,
                copy: best.0 as u32,
                dist: best.1 as u32,
            });
            i += best.0;
            anchor = i;
        } else {
            i += 1;
        }
    }
    if anchor < end {
        cmds.push(Cmd {
            ins: (end - anchor) as u32,
            copy: 0,
            dist: 0,
        });
    }
    cmds
}

fn dist_symbol(d: u32) -> (usize, u32, u32) {
    // distance d >= 1 with NPOSTFIX = NDIRECT = 0
    let x = d + 3;
    let nb = 31 - x.leading_zeros() - 1;
    let h = (x >> nb) & 1;
    (
        16 + 2 * (nb as usize - 1) + h as usize,
        nb,
        x & ((1 << nb) - 1),
    )
}

fn cell_of(ic: usize, cc: usize) -> usize {
    match (ic >> 3, cc >> 3) {
        (0, 0) => 2,
        (0, 1) => 3,
        (1, 0) => 4,
        (1, 1) => 5,
        (0, 2) => 6,
        (2, 0) => 7,
        (1, 2) => 8,
        (2, 1) => 9,
        _ => 10,
    }
}

/// A prefix code ready for writing: code lengths and bit-reversed codes.
struct Code {
    lens: Vec<u8>,
    codes: Vec<u32>,
}

fn make_code(freq: &[u32]) -> Code {
    let lens = code_lengths(freq, 15);
    let codes = canonical_codes(&lens);
    Code { lens, codes }
}

impl Code {
    fn put(&self, w: &mut Bits, sym: usize) {
        let l = u32::from(self.lens[sym]);
        if l > 0 {
            w.put(u64::from(self.codes[sym]), l);
        }
    }
}

fn write_simple(w: &mut Bits, freq: &[u32], alpha_bits: u32, c: &mut Code) {
    // Up to four used symbols: a simple prefix code.
    let mut used: Vec<usize> = (0..freq.len()).filter(|&i| freq[i] > 0).collect();
    used.sort_by(|&a, &b| freq[b].cmp(&freq[a]).then(a.cmp(&b)));
    let n = used.len().max(1);
    w.put(1, 2);
    w.put(n as u64 - 1, 2);
    if used.is_empty() {
        used.push(0);
    }
    let mut lens = vec![0u8; freq.len()];
    let order: Vec<usize> = match n {
        1 => {
            lens[used[0]] = 0;
            used.clone()
        }
        2 => {
            let mut v = used.clone();
            v.sort();
            lens[v[0]] = 1;
            lens[v[1]] = 1;
            v
        }
        3 => {
            // first symbol gets 1 bit, the other two 2 bits, sorted
            let mut rest = used[1..].to_vec();
            rest.sort();
            lens[used[0]] = 1;
            lens[rest[0]] = 2;
            lens[rest[1]] = 2;
            let mut v = vec![used[0]];
            v.extend(rest);
            v
        }
        _ => {
            let l = code_lengths(freq, 15);
            let mut sorted = used.clone();
            sorted.sort_by_key(|&s| (l[s], s));
            if sorted.iter().all(|&s| l[s] == 2) {
                for &s in &sorted {
                    lens[s] = 2;
                }
                w.put(0, 0);
                let v = sorted.clone();
                for &s in &v {
                    w.put(s as u64, alpha_bits);
                }
                w.put(0, 1);
                c.lens = lens;
                c.codes = canonical_codes(&c.lens);
                return;
            }
            // lengths 1, 2, 3, 3: the most frequent first, then in code order
            let first = used[0];
            let second = used[1];
            let mut last = [used[2], used[3]];
            last.sort();
            lens[first] = 1;
            lens[second] = 2;
            lens[last[0]] = 3;
            lens[last[1]] = 3;
            for s in [first, second, last[0], last[1]] {
                w.put(s as u64, alpha_bits);
            }
            w.put(1, 1);
            c.lens = lens;
            c.codes = canonical_codes(&c.lens);
            return;
        }
    };
    for &s in &order {
        w.put(s as u64, alpha_bits);
    }
    c.lens = lens;
    c.codes = canonical_codes(&c.lens);
}

/// Run-length symbols for a list of code lengths: (symbol, extra value, extra bits).
fn rle_lengths(lens: &[u8]) -> Vec<(usize, u32, u32)> {
    let end = lens.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1);
    let lens = &lens[..end];
    let mut out: Vec<(usize, u32, u32)> = Vec::new();
    let mut prev = 8u8;
    let mut i = 0;
    while i < lens.len() {
        let v = lens[i];
        let mut run = 1;
        while i + run < lens.len() && lens[i + run] == v {
            run += 1;
        }
        i += run;
        let mut reps = run;
        if v == 0 {
            if reps == 11 {
                out.push((0, 0, 0));
                reps -= 1;
            }
            if reps < 3 {
                for _ in 0..reps {
                    out.push((0, 0, 0));
                }
            } else {
                let start = out.len();
                reps -= 3;
                loop {
                    out.push((17, (reps & 7) as u32, 3));
                    reps >>= 3;
                    if reps == 0 {
                        break;
                    }
                    reps -= 1;
                }
                out[start..].reverse();
            }
        } else {
            if v != prev {
                out.push((usize::from(v), 0, 0));
                reps -= 1;
            }
            if reps < 3 {
                for _ in 0..reps {
                    out.push((usize::from(v), 0, 0));
                }
            } else {
                let start = out.len();
                reps -= 3;
                loop {
                    out.push((16, (reps & 3) as u32, 2));
                    reps >>= 2;
                    if reps == 0 {
                        break;
                    }
                    reps -= 1;
                }
                out[start..].reverse();
            }
            prev = v;
        }
    }
    out
}

fn write_complex(w: &mut Bits, c: &Code) {
    let rle = rle_lengths(&c.lens);
    let mut cf = [0u32; 18];
    for &(s, _, _) in &rle {
        cf[s] += 1;
    }
    if cf.iter().filter(|&&f| f > 0).count() == 1 {
        // a code with one symbol cannot be written; give it a partner that is never used
        let used = cf.iter().position(|&f| f > 0).unwrap_or(0);
        cf[if used == 0 { 1 } else { 0 }] = 1;
    }
    let cl = code_lengths(&cf, 5);
    let ccodes = canonical_codes(&cl);
    w.put(0, 2); // HSKIP = 0
    let last = CL_ORDER.iter().rposition(|&s| cl[s] != 0).unwrap_or(0);
    for &s in &CL_ORDER[..=last] {
        let (bits, n) = CL_LEN_CODE[usize::from(cl[s])];
        w.put(u64::from(bits), n);
    }
    for &(s, extra, nb) in &rle {
        w.put(u64::from(ccodes[s]), u32::from(cl[s]));
        w.put(u64::from(extra), nb);
    }
}

fn write_code(w: &mut Bits, freq: &[u32], alpha_bits: u32) -> Code {
    let used = freq.iter().filter(|&&f| f > 0).count();
    let mut c = make_code(freq);
    if used <= 4 {
        write_simple(w, freq, alpha_bits, &mut c);
    } else {
        write_complex(w, &c);
    }
    c
}

fn alpha_bits(size: usize) -> u32 {
    32 - ((size - 1) as u32).leading_zeros()
}

fn write_meta_block(w: &mut Bits, data: &[u8], start: usize, end: usize, last: bool, cmds: &[Cmd]) {
    let mlen = end - start;
    w.put(u64::from(last), 1);
    if last {
        w.put(0, 1); // not empty
    }
    let nibbles = if mlen <= 1 << 16 {
        4
    } else if mlen <= 1 << 20 {
        5
    } else {
        6
    };
    w.put(nibbles as u64 - 4, 2);
    w.put(mlen as u64 - 1, nibbles * 4);
    if !last {
        w.put(0, 1); // compressed
    }
    w.put(0, 3); // one block type each for literals, commands, distances
    w.put(0, 2 + 4); // NPOSTFIX, NDIRECT
    w.put(0, 2); // literal context mode
    w.put(0, 1); // one literal tree
    w.put(0, 1); // one distance tree

    // Symbols.
    let mut lit_f = [0u32; 256];
    let mut cmd_f = [0u32; 704];
    let mut dist_f = [0u32; 64];
    let mut pos = start;
    struct Plan {
        cmd: usize,
        ie: (u32, u32),
        ce: (u32, u32),
        dist: Option<(usize, u32, u32)>,
        lit_at: usize,
        ins: usize,
    }
    let mut plans = Vec::with_capacity(cmds.len());
    for c in cmds {
        let ic = code_for(&INS_BASE, c.ins);
        let (cc, copy_len) = if c.copy == 0 {
            (0, 2)
        } else {
            (code_for(&COPY_BASE, c.copy), c.copy)
        };
        let cmd = cell_of(ic, cc) * 64 + ((ic & 7) << 3) + (cc & 7);
        cmd_f[cmd] += 1;
        for &b in &data[pos..pos + c.ins as usize] {
            lit_f[usize::from(b)] += 1;
        }
        let dist = if c.copy == 0 {
            None
        } else {
            let d = dist_symbol(c.dist);
            dist_f[d.0] += 1;
            Some(d)
        };
        plans.push(Plan {
            cmd,
            ie: (c.ins - INS_BASE[ic], INS_EXTRA[ic]),
            ce: (copy_len - COPY_BASE[cc], COPY_EXTRA[cc]),
            dist,
            lit_at: pos,
            ins: c.ins as usize,
        });
        pos += c.ins as usize + c.copy as usize;
    }
    let lit = write_code(w, &lit_f, 8);
    let cmd = write_code(w, &cmd_f, alpha_bits(704));
    let dist = write_code(w, &dist_f, alpha_bits(64));
    for p in &plans {
        cmd.put(w, p.cmd);
        w.put(u64::from(p.ie.0), p.ie.1);
        w.put(u64::from(p.ce.0), p.ce.1);
        for &b in &data[p.lit_at..p.lit_at + p.ins] {
            lit.put(w, usize::from(b));
        }
        if let Some((sym, nb, extra)) = p.dist {
            dist.put(w, sym);
            w.put(u64::from(extra), nb);
        }
    }
}

/// Compress `data` into a Brotli stream. `level` 0..=11 trades speed for size.
pub fn compress(data: &[u8], level: u32) -> Vec<u8> {
    let wbits = 22u32;
    let window = (1usize << wbits) - 16;
    let mut w = Bits {
        out: Vec::new(),
        acc: 0,
        n: 0,
    };
    // WBITS 22: a set bit, then 3 bits holding 5
    w.put(1, 1);
    w.put(5, 3);
    if data.is_empty() {
        w.put(1, 1); // ISLAST
        w.put(1, 1); // ISLASTEMPTY
        return w.finish();
    }
    let lv = level_params(level);
    let mut start = 0;
    while start < data.len() {
        let end = (start + (1 << 22)).min(data.len());
        let cmds = parse(data, start, end, window, &lv);
        write_meta_block(&mut w, data, start, end, end == data.len(), &cmds);
        start = end;
    }
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_symbols_invert() {
        for d in [1u32, 2, 3, 4, 5, 6, 100, 65535, 1 << 20] {
            let (sym, nb, extra) = dist_symbol(d);
            let ndistbits = 1 + ((sym - 16) >> 1);
            let hcode = (sym - 16) & 1;
            assert_eq!(ndistbits as u32, nb);
            let offset = ((2 + hcode) << ndistbits) - 4;
            assert_eq!(offset as u32 + extra + 1, d);
        }
    }
}
