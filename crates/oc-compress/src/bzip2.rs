//! bzip2 (Burrows-Wheeler block sorting, move-to-front, run-length and Huffman coding).
//! Reads concatenated streams; the obsolete "randomised" block mode is refused.
#![allow(clippy::indexing_slicing)] // table and buffer positions are bounded by the block and alphabet sizes checked above them
use crate::deflate::code_lengths;
use crate::msb::{MsbReader, MsbWriter};
use crate::sais::suffix_array;
use oc_core::{Error, ErrorCode, Limits, Progress, Result};

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;
const GROUP: usize = 50;
const MAX_LEN: u32 = 20;
const RUNA: u16 = 0;
const RUNB: u16 = 1;

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (i, e) in t.iter_mut().enumerate() {
        let mut c = (i as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
        *e = c;
    }
    t
}

struct Crc {
    t: [u32; 256],
}

impl Crc {
    fn new() -> Self {
        Self { t: crc_table() }
    }
    fn update(&self, mut c: u32, data: &[u8]) -> u32 {
        for &b in data {
            c = (c << 8)
                ^ self
                    .t
                    .get(usize::from((c >> 24) as u8 ^ b))
                    .copied()
                    .unwrap_or(0);
        }
        c
    }
}

// ---------------------------------------------------------------- decoder

struct Table {
    min: u32,
    max: u32,
    limit: [i32; 24],
    base: [i32; 24],
    perm: Vec<u16>,
}

impl Table {
    fn new(lens: &[u8]) -> Self {
        let min = u32::from(lens.iter().copied().min().unwrap_or(1));
        let max = u32::from(lens.iter().copied().max().unwrap_or(1));
        let mut perm = Vec::with_capacity(lens.len());
        for l in min..=max {
            for (s, &len) in lens.iter().enumerate() {
                if u32::from(len) == l {
                    perm.push(s as u16);
                }
            }
        }
        let mut base = [0i32; 24];
        let mut limit = [-1i32; 24];
        for &l in lens {
            if let Some(b) = base.get_mut(usize::from(l) + 1) {
                *b += 1;
            }
        }
        for i in 1..24 {
            base[i] += base[i - 1];
        }
        let mut vec = 0i32;
        for i in min as usize..=max as usize {
            vec += base[i + 1] - base[i];
            limit[i] = vec - 1;
            vec <<= 1;
        }
        for i in min as usize + 1..=max as usize {
            base[i] = ((limit[i - 1] + 1) << 1) - base[i];
        }
        Self {
            min,
            max,
            limit,
            base,
            perm,
        }
    }

    fn decode(&self, r: &mut MsbReader<'_>) -> Result<u16> {
        let mut n = self.min;
        let mut code = r.bits(n)? as i32;
        loop {
            if n > self.max {
                return Err(Error::corrupt("bzip2: invalid Huffman code"));
            }
            if code <= self.limit.get(n as usize).copied().unwrap_or(-1) {
                break;
            }
            n += 1;
            code = (code << 1) | r.bit()? as i32;
        }
        let idx = code - self.base.get(n as usize).copied().unwrap_or(0);
        self.perm
            .get(usize::try_from(idx).map_err(|_| Error::corrupt("bzip2: invalid Huffman code"))?)
            .copied()
            .ok_or_else(|| Error::corrupt("bzip2: invalid Huffman code"))
    }
}

/// Reads one block (after its magic) and appends the decoded bytes to `out`.
/// Returns the CRC the block stored and the CRC of what was decoded.
fn read_block(
    r: &mut MsbReader<'_>,
    max_block: usize,
    crc: &Crc,
    out: &mut Vec<u8>,
) -> Result<(u32, u32)> {
    let stored = r.bits(32)?;
    if r.bit()? == 1 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "bzip2: randomised blocks (bzip2 0.9.0 and older) are not supported",
        ));
    }
    let orig_ptr = r.bits(24)? as usize;
    // Symbol map.
    let groups = r.bits(16)?;
    let mut used = Vec::new();
    for g in 0..16 {
        if groups & (0x8000 >> g) != 0 {
            let bits = r.bits(16)?;
            for k in 0..16 {
                if bits & (0x8000 >> k) != 0 {
                    used.push((g * 16 + k) as u8);
                }
            }
        }
    }
    if used.is_empty() {
        return Err(Error::corrupt("bzip2: block uses no symbols"));
    }
    let alpha = used.len() + 2;
    let eob = (used.len() + 1) as u16;
    let n_groups = r.bits(3)? as usize;
    if !(2..=6).contains(&n_groups) {
        return Err(Error::corrupt("bzip2: bad Huffman table count"));
    }
    let n_sel = r.bits(15)? as usize;
    if n_sel == 0 {
        return Err(Error::corrupt("bzip2: no selectors"));
    }
    let mut mtf_sel: Vec<u8> = (0..n_groups as u8).collect();
    let mut selectors = Vec::with_capacity(n_sel);
    for _ in 0..n_sel {
        let mut j = 0usize;
        while r.bit()? == 1 {
            j += 1;
            if j >= n_groups {
                return Err(Error::corrupt("bzip2: bad selector"));
            }
        }
        let v = mtf_sel.remove(j);
        mtf_sel.insert(0, v);
        selectors.push(v);
    }
    let mut tables = Vec::with_capacity(n_groups);
    for _ in 0..n_groups {
        let mut lens = Vec::with_capacity(alpha);
        let mut cur = r.bits(5)? as i32;
        for _ in 0..alpha {
            loop {
                if !(1..=MAX_LEN as i32).contains(&cur) {
                    return Err(Error::corrupt("bzip2: bad code length"));
                }
                if r.bit()? == 0 {
                    break;
                }
                if r.bit()? == 0 {
                    cur += 1;
                } else {
                    cur -= 1;
                }
            }
            lens.push(cur as u8);
        }
        tables.push(Table::new(&lens));
    }
    // Huffman + MTF + zero-run decoding into `ll`.
    let mut mtf: Vec<u8> = used.clone();
    let mut ll: Vec<u8> = Vec::new();
    let mut counts = [0usize; 256];
    let mut run: usize = 0;
    let mut run_w: usize = 1;
    let mut sel_i = 0usize;
    let mut left = 0usize;
    let mut table = tables
        .first()
        .ok_or_else(|| Error::corrupt("bzip2: no tables"))?;
    loop {
        if left == 0 {
            let s = *selectors
                .get(sel_i)
                .ok_or_else(|| Error::corrupt("bzip2: ran out of selectors"))?;
            table = tables
                .get(usize::from(s))
                .ok_or_else(|| Error::corrupt("bzip2: bad selector"))?;
            sel_i += 1;
            left = GROUP;
        }
        left -= 1;
        let sym = table.decode(r)?;
        if sym == RUNA || sym == RUNB {
            if run_w > 1 << 21 {
                return Err(Error::corrupt("bzip2: run too long"));
            }
            run += run_w * (usize::from(sym) + 1);
            run_w <<= 1;
            continue;
        }
        if run > 0 {
            let b = mtf.first().copied().unwrap_or(0);
            if ll.len() + run > max_block {
                return Err(Error::corrupt("bzip2: block overflows its declared size"));
            }
            ll.resize(ll.len() + run, b);
            counts[usize::from(b)] += run;
            run = 0;
            run_w = 1;
        }
        if sym == eob {
            break;
        }
        if ll.len() >= max_block {
            return Err(Error::corrupt("bzip2: block overflows its declared size"));
        }
        let idx = usize::from(sym) - 1;
        if idx >= mtf.len() {
            return Err(Error::corrupt("bzip2: bad MTF index"));
        }
        let b = mtf.remove(idx);
        mtf.insert(0, b);
        ll.push(b);
        counts[usize::from(b)] += 1;
    }
    let n = ll.len();
    if n == 0 || orig_ptr >= n {
        return Err(Error::corrupt("bzip2: bad block start pointer"));
    }
    // Inverse BWT.
    let mut cf = [0usize; 257];
    for i in 0..256 {
        cf[i + 1] = cf[i] + counts[i];
    }
    let mut tt = vec![0u32; n];
    for (i, &b) in ll.iter().enumerate() {
        let slot = cf
            .get_mut(usize::from(b))
            .ok_or_else(|| Error::corrupt("bzip2: bad symbol"))?;
        if let Some(t) = tt.get_mut(*slot) {
            *t = i as u32;
        }
        *slot += 1;
    }
    let mut pos = tt.get(orig_ptr).copied().unwrap_or(0) as usize;
    let start = out.len();
    // Undo the initial run-length coding (four equal bytes, then a repeat count).
    let mut same = 0u32;
    let mut last: i32 = -1;
    let mut i = 0;
    while i < n {
        let b = ll.get(pos).copied().unwrap_or(0);
        pos = tt.get(pos).copied().unwrap_or(0) as usize;
        i += 1;
        if same == 4 {
            let rep = usize::from(b);
            if rep > 0 {
                let c = last as u8;
                out.resize(out.len() + rep, c);
            }
            same = 0;
            last = -1;
            continue;
        }
        if i32::from(b) == last {
            same += 1;
        } else {
            same = 1;
            last = i32::from(b);
        }
        out.push(b);
    }
    let got = !crc.update(0xFFFF_FFFF, out.get(start..).unwrap_or(&[]));
    Ok((stored, got))
}

pub fn decompress(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    let crc = Crc::new();
    let mut out = Vec::new();
    let mut r = MsbReader::new(input);
    let mut streams = 0;
    loop {
        // Stream header.
        if r.bits(8).ok() != Some(u32::from(b'B'))
            || r.bits(8).ok() != Some(u32::from(b'Z'))
            || r.bits(8).ok() != Some(u32::from(b'h'))
        {
            if streams > 0 {
                break; // trailing bytes after the last stream are ignored
            }
            return Err(Error::corrupt("not a bzip2 stream"));
        }
        let level = r.bits(8)?;
        if !(u32::from(b'1')..=u32::from(b'9')).contains(&level) {
            return Err(Error::corrupt("bzip2: bad block size"));
        }
        let max_block = (level - u32::from(b'0')) as usize * 100_000;
        let mut combined = 0u32;
        loop {
            progress.check_cancelled()?;
            let magic = (u64::from(r.bits(24)?) << 24) | u64::from(r.bits(24)?);
            if magic == END_MAGIC {
                let stored = r.bits(32)?;
                if stored != combined {
                    return Err(Error::corrupt("bzip2: stream CRC mismatch"));
                }
                r.align();
                break;
            }
            if magic != BLOCK_MAGIC {
                return Err(Error::corrupt("bzip2: bad block marker").at(r.byte_pos() as u64));
            }
            let (stored, got) = read_block(&mut r, max_block, &crc, &mut out)?;
            if stored != got {
                return Err(Error::corrupt("bzip2: block CRC mismatch").at(r.byte_pos() as u64));
            }
            combined = combined.rotate_left(1) ^ got;
            limits.check_expansion(input.len() as u64, out.len() as u64)?;
            progress.report(r.byte_pos() as u64, input.len() as u64, "bzip2");
        }
        streams += 1;
        // r is byte aligned again; the loop's header check sees what follows.
        if r.byte_pos() >= input.len() {
            break;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- encoder

fn encode_groups(nmtf: usize) -> usize {
    match nmtf {
        0..=199 => 2,
        200..=599 => 3,
        600..=1199 => 4,
        1200..=2399 => 5,
        _ => 6,
    }
}

/// Canonical MSB-first codes (shorter first, then by symbol).
fn codes(lens: &[u8]) -> Vec<u32> {
    let min = lens.iter().copied().min().unwrap_or(1);
    let max = lens.iter().copied().max().unwrap_or(1);
    let mut out = vec![0u32; lens.len()];
    let mut v = 0u32;
    for n in min..=max {
        for (i, &l) in lens.iter().enumerate() {
            if l == n {
                out[i] = v;
                v += 1;
            }
        }
        v <<= 1;
    }
    out
}

/// Sorts the rotations of `block`; returns the last column and the row of rotation 0.
fn bwt(block: &[u8]) -> (Vec<u8>, usize) {
    let n = block.len();
    let mut s: Vec<usize> = Vec::with_capacity(2 * n);
    s.extend(block.iter().map(|&b| usize::from(b)));
    s.extend(block.iter().map(|&b| usize::from(b)));
    let sa = suffix_array(&s, 255);
    let mut last = Vec::with_capacity(n);
    let mut orig = 0;
    for &i in sa.iter().filter(|&&i| i < n) {
        if i == 0 {
            orig = last.len();
        }
        last.push(block.get((i + n - 1) % n).copied().unwrap_or(0));
    }
    (last, orig)
}

fn write_block(w: &mut MsbWriter, block: &[u8], block_crc: u32) {
    let (last, orig) = bwt(block);
    // Move-to-front with zero-run coding.
    let mut present = [false; 256];
    for &b in &last {
        present[usize::from(b)] = true;
    }
    let used: Vec<u8> = (0..=255u8).filter(|&b| present[usize::from(b)]).collect();
    let n_in_use = used.len();
    let eob = (n_in_use + 1) as u16;
    let mut yy = used.clone();
    let mut syms: Vec<u16> = Vec::with_capacity(last.len() / 2 + 8);
    let mut pend: u32 = 0;
    let flush = |pend: &mut u32, syms: &mut Vec<u16>| {
        if *pend > 0 {
            let mut z = *pend - 1;
            loop {
                syms.push(if z & 1 == 1 { RUNB } else { RUNA });
                if z < 2 {
                    break;
                }
                z = (z - 2) / 2;
            }
            *pend = 0;
        }
    };
    for &b in &last {
        let j = yy.iter().position(|&x| x == b).unwrap_or(0);
        if j == 0 {
            pend += 1;
        } else {
            flush(&mut pend, &mut syms);
            let v = yy.remove(j);
            yy.insert(0, v);
            syms.push(j as u16 + 1);
        }
    }
    flush(&mut pend, &mut syms);
    syms.push(eob);
    let alpha = n_in_use + 2;
    let n_groups = encode_groups(syms.len());
    let mut freq = vec![0u32; alpha];
    for &s in &syms {
        if let Some(f) = freq.get_mut(usize::from(s)) {
            *f += 1;
        }
    }
    // Initial tables: contiguous slices of the alphabet with equal share of the symbols.
    let mut lens: Vec<Vec<u8>> = vec![vec![15u8; alpha]; n_groups];
    {
        let mut rem: u64 = syms.len() as u64;
        let mut gs = 0usize;
        let mut part = n_groups;
        while part > 0 {
            let target = rem / part as u64;
            let mut ge = gs as i64 - 1;
            let mut acc = 0u64;
            while acc < target && ge < alpha as i64 - 1 {
                ge += 1;
                acc += u64::from(freq[ge as usize]);
            }
            if ge > gs as i64 && part != n_groups && part != 1 && (n_groups - part) % 2 == 1 {
                acc -= u64::from(freq[ge as usize]);
                ge -= 1;
            }
            for (v, l) in lens[part - 1].iter_mut().enumerate().take(alpha) {
                *l = if (gs as i64..=ge).contains(&(v as i64)) {
                    0
                } else {
                    15
                };
            }
            // 0 is not a legal length; bump below once real frequencies exist.
            part -= 1;
            gs = (ge + 1) as usize;
            rem = rem.saturating_sub(acc);
        }
        for t in &mut lens {
            for l in t.iter_mut() {
                if *l == 0 {
                    *l = 1;
                }
            }
        }
    }
    let n_sel = syms.len().div_ceil(GROUP);
    let mut selectors = vec![0u8; n_sel];
    for _ in 0..4 {
        let mut rfreq = vec![vec![0u32; alpha]; n_groups];
        for (gi, chunk) in syms.chunks(GROUP).enumerate() {
            let mut best = 0usize;
            let mut best_cost = u32::MAX;
            for (t, tl) in lens.iter().enumerate() {
                let cost: u32 = chunk
                    .iter()
                    .map(|&s| u32::from(tl.get(usize::from(s)).copied().unwrap_or(15)))
                    .sum();
                if cost < best_cost {
                    best_cost = cost;
                    best = t;
                }
            }
            selectors[gi] = best as u8;
            for &s in chunk {
                rfreq[best][usize::from(s)] += 1;
            }
        }
        for (t, f) in rfreq.iter().enumerate() {
            let w: Vec<u32> = f.iter().map(|&x| x.max(1)).collect();
            lens[t] = code_lengths(&w, 17);
        }
    }
    let table_codes: Vec<Vec<u32>> = lens.iter().map(|l| codes(l)).collect();

    w.put((BLOCK_MAGIC >> 24) as u32, 24);
    w.put((BLOCK_MAGIC & 0xFF_FFFF) as u32, 24);
    w.put(block_crc, 32);
    w.put(0, 1);
    w.put(orig as u32, 24);
    // Symbol map.
    let mut groups = 0u32;
    for g in 0..16 {
        if (0..16).any(|k| present[g * 16 + k]) {
            groups |= 0x8000 >> g;
        }
    }
    w.put(groups, 16);
    for g in 0..16 {
        if groups & (0x8000 >> g) != 0 {
            let mut bits = 0u32;
            for k in 0..16 {
                if present[g * 16 + k] {
                    bits |= 0x8000 >> k;
                }
            }
            w.put(bits, 16);
        }
    }
    w.put(n_groups as u32, 3);
    w.put(n_sel as u32, 15);
    let mut order: Vec<u8> = (0..n_groups as u8).collect();
    for &s in &selectors {
        let j = order.iter().position(|&x| x == s).unwrap_or(0);
        for _ in 0..j {
            w.put(1, 1);
        }
        w.put(0, 1);
        let v = order.remove(j);
        order.insert(0, v);
    }
    for tl in &lens {
        let mut cur = i32::from(tl.first().copied().unwrap_or(1));
        w.put(cur as u32, 5);
        for &l in tl {
            let l = i32::from(l);
            while cur < l {
                w.put(0b10, 2);
                cur += 1;
            }
            while cur > l {
                w.put(0b11, 2);
                cur -= 1;
            }
            w.put(0, 1);
        }
    }
    for (gi, chunk) in syms.chunks(GROUP).enumerate() {
        let t = usize::from(selectors[gi]);
        for &s in chunk {
            w.put(
                table_codes[t][usize::from(s)],
                u32::from(lens[t][usize::from(s)]),
            );
        }
    }
}

/// Compresses `data` with 100k * `level` byte blocks (level 1 to 9).
pub fn compress(data: &[u8], level: u32) -> Vec<u8> {
    let level = level.clamp(1, 9);
    let limit = level as usize * 100_000 - 19;
    let crc = Crc::new();
    let mut w = MsbWriter::default();
    for b in [b'B', b'Z', b'h', b'0' + level as u8] {
        w.put(u32::from(b), 8);
    }
    let mut combined = 0u32;
    let mut i = 0usize;
    while i < data.len() {
        let mut block: Vec<u8> = Vec::with_capacity(limit + 8);
        let start = i;
        // Initial run-length coding: runs of 4 to 259 become four bytes and a count.
        while i < data.len() {
            let b = data[i];
            let mut run = 1;
            while i + run < data.len() && data[i + run] == b && run < 255 + 4 {
                run += 1;
            }
            let cost = if run >= 4 { 5 } else { run };
            if block.len() + cost > limit {
                break;
            }
            if run >= 4 {
                block.extend_from_slice(&[b, b, b, b, (run - 4) as u8]);
            } else {
                block.resize(block.len() + run, b);
            }
            i += run;
        }
        if i == start {
            break; // cannot happen: a fresh block always has room
        }
        let block_crc = !crc.update(0xFFFF_FFFF, &data[start..i]);
        combined = combined.rotate_left(1) ^ block_crc;
        write_block(&mut w, &block, block_crc);
    }
    w.put((END_MAGIC >> 24) as u32, 24);
    w.put((END_MAGIC & 0xFF_FFFF) as u32, 24);
    w.put(combined, 32);
    w.finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn d(z: &[u8]) -> Result<Vec<u8>> {
        decompress(z, &Limits::default(), &NoProgress)
    }

    fn lcg(n: usize, mut s: u32, modulo: u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((s >> 16) % modulo) as u8
            })
            .collect()
    }

    #[test]
    fn roundtrips() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            b"a".to_vec(),
            b"abracadabra abracadabra".to_vec(),
            vec![7; 1000],
            vec![0; 300_000],
            lcg(50_000, 1, 256),
            lcg(250_000, 2, 3),
            b"banana".repeat(40_000),
        ];
        for c in cases {
            for level in [1, 9] {
                let z = compress(&c, level);
                assert_eq!(d(&z).unwrap(), c, "len {} level {level}", c.len());
            }
        }
    }

    #[test]
    fn concatenated_streams() {
        let mut z = compress(b"hello ", 9);
        z.extend(compress(b"world", 1));
        assert_eq!(d(&z).unwrap(), b"hello world");
    }

    #[test]
    fn corruption_is_an_error_not_a_panic() {
        let data = lcg(30_000, 5, 40);
        let z = compress(&data, 1);
        assert!(d(&z[..z.len() / 2]).is_err());
        let mut x = 99u32;
        for _ in 0..300 {
            let mut bad = z.clone();
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let p = 4 + (x as usize >> 4) % (bad.len() - 4);
            bad[p] ^= 1 << (x % 8);
            let _ = d(&bad);
        }
        assert!(d(b"BZx9").is_err());
        assert!(d(b"").is_err());
    }
}
