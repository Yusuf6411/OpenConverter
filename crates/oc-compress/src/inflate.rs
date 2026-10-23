//! DEFLATE decompression (RFC 1951). Bounded by [`Limits`], cancellable via [`Progress`].
#![allow(clippy::indexing_slicing)]
use oc_core::{Error, Limits, Progress, Result};
use oc_io::ByteReader;

pub(crate) const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
pub(crate) const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
pub(crate) const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
pub(crate) const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Order in which code-length code lengths are stored.
pub(crate) const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buf: 0,
            cnt: 0,
        }
    }
    fn need(&mut self, n: u32) -> Result<()> {
        while self.cnt < n {
            let b = *self
                .data
                .get(self.pos)
                .ok_or_else(|| Error::truncated("deflate stream ended").at(self.pos as u64))?;
            self.pos += 1;
            self.buf |= u64::from(b) << self.cnt;
            self.cnt += 8;
        }
        Ok(())
    }
    /// Top the buffer up to at least 56 bits, or to the end of the input.
    #[inline(always)]
    fn refill(&mut self) {
        if self.cnt >= 32 {
            return;
        }
        if let Some(w) = self.data.get(self.pos..self.pos + 8) {
            let mut a = [0u8; 8];
            a.copy_from_slice(w);
            self.buf |= u64::from_le_bytes(a) << self.cnt;
            self.pos += ((63 - self.cnt) >> 3) as usize;
            self.cnt |= 56;
            return;
        }
        while self.cnt <= 56 {
            let Some(&b) = self.data.get(self.pos) else {
                break;
            };
            self.pos += 1;
            self.buf |= u64::from(b) << self.cnt;
            self.cnt += 8;
        }
    }
    fn bits(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.cnt -= n;
        Ok(v)
    }
    /// Drop buffered bits up to the byte boundary and give back whole unread bytes.
    fn align(&mut self) {
        let drop = self.cnt % 8;
        self.buf >>= drop;
        self.cnt -= drop;
        // Return fully buffered bytes to the input.
        self.pos -= (self.cnt / 8) as usize;
        self.buf = 0;
        self.cnt = 0;
    }
}

const FAST: u32 = 10;

struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
    /// Indexed by the next `FAST` input bits: `symbol << 4 | length`, or 0 when the code is
    /// longer than `FAST` bits (or unused) and the canonical walk decides.
    fast: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self> {
        let mut count = [0u16; 16];
        for &l in lengths {
            let c = count
                .get_mut(usize::from(l))
                .ok_or_else(|| Error::corrupt("code length out of range"))?;
            *c += 1;
        }
        if usize::from(count.first().copied().unwrap_or(0)) == lengths.len() {
            return Ok(Self {
                count,
                symbol: Vec::new(),
                fast: Vec::new(),
            });
        }
        let mut left: i32 = 1;
        for &c in count.iter().skip(1) {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return Err(Error::corrupt("over-subscribed Huffman code"));
            }
        }
        // offs[len] = index of the first symbol with code length `len`.
        let mut offs = [0u16; 16];
        let mut running = 0u16;
        for (o, &c) in offs.iter_mut().zip(count.iter()).skip(1) {
            *o = running;
            running += c;
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let o = offs
                    .get_mut(usize::from(l))
                    .ok_or_else(|| Error::corrupt("bad length"))?;
                if let Some(s) = symbol.get_mut(usize::from(*o)) {
                    *s = sym as u16;
                }
                *o += 1;
            }
        }
        let mut fast = vec![0u16; 1 << FAST];
        let mut next = [0u32; 17];
        let mut code = 0u32;
        for l in 1..16 {
            let prev = if l > 1 { u32::from(count[l - 1]) } else { 0 };
            code = (code + prev) << 1;
            next[l] = code;
        }
        for (sym, &l) in lengths.iter().enumerate() {
            let l = u32::from(l);
            if l == 0 || l > FAST {
                if l != 0 {
                    if let Some(n) = next.get_mut(l as usize) {
                        *n += 1;
                    }
                }
                continue;
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            let rev = c.reverse_bits() >> (32 - l);
            let entry = ((sym as u16) << 4) | l as u16;
            let mut i = rev as usize;
            while i < fast.len() {
                fast[i] = entry;
                i += 1 << l;
            }
        }
        Ok(Self {
            count,
            symbol,
            fast,
        })
    }

    #[inline(always)]
    fn decode(&self, b: &mut Bits<'_>) -> Result<u16> {
        if !self.fast.is_empty() {
            b.refill();
            let e = self.fast[(b.buf & ((1 << FAST) - 1)) as usize];
            let l = u32::from(e & 15);
            if l != 0 && l <= b.cnt {
                b.buf >>= l;
                b.cnt -= l;
                return Ok(e >> 4);
            }
        }
        self.decode_slow(b)
    }

    fn decode_slow(&self, b: &mut Bits<'_>) -> Result<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for &count in self.count.iter().skip(1) {
            code |= b.bits(1)? as i32;
            let count = i32::from(count);
            if code - count < first {
                return self
                    .symbol
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or_else(|| Error::corrupt("bad Huffman symbol"));
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(Error::corrupt("invalid Huffman code"))
    }
}

fn fixed_tables(d64: bool) -> Result<(Huffman, Huffman)> {
    let mut l = [8u8; 288];
    l[144..256].fill(9);
    l[256..280].fill(7);
    let nd = if d64 { 32 } else { 30 };
    Ok((Huffman::new(&l)?, Huffman::new(&vec![5u8; nd])?))
}

/// Result of inflating one DEFLATE stream.
pub struct Inflated {
    pub data: Vec<u8>,
    /// Number of input bytes the stream occupied.
    pub consumed: usize,
}

/// Inflate a raw DEFLATE stream, ignoring any trailing input.
pub fn inflate(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Inflated> {
    inflate_impl(input, limits, progress, false, &[])
}

/// Inflate a raw DEFLATE stream whose matches may reach into `dict` (the preceding output
/// of an earlier block, as in Microsoft CAB "MSZIP"). Only the new bytes are returned.
pub fn inflate_with_dict(
    input: &[u8],
    dict: &[u8],
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Inflated> {
    inflate_impl(input, limits, progress, false, dict)
}

/// Inflate a Deflate64 ("enhanced deflate") stream: 64 KiB window, 16-bit extra length
/// bits on symbol 285 and two more distance codes.
pub fn inflate64(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Inflated> {
    inflate_impl(input, limits, progress, true, &[])
}

fn inflate_impl(
    input: &[u8],
    limits: &Limits,
    progress: &dyn Progress,
    d64: bool,
    dict: &[u8],
) -> Result<Inflated> {
    let mut b = Bits::new(input);
    let mut out: Vec<u8> = dict.to_vec();
    let mut checked = out.len();
    loop {
        let last = b.bits(1)? == 1;
        match b.bits(2)? {
            0 => {
                b.align();
                let mut hdr = ByteReader::new(input.get(b.pos..).unwrap_or(&[]));
                let len = hdr.u16_le().map_err(|e| e.at(b.pos as u64))?;
                let nlen = hdr.u16_le().map_err(|e| e.at(b.pos as u64))?;
                if len != !nlen {
                    return Err(Error::corrupt("stored block length check failed").at(b.pos as u64));
                }
                b.pos += 4;
                let end = b.pos + usize::from(len);
                let body = input
                    .get(b.pos..end)
                    .ok_or_else(|| Error::truncated("stored block body").at(b.pos as u64))?;
                out.extend_from_slice(body);
                b.pos = end;
            }
            1 => {
                let (l, d) = fixed_tables(d64)?;
                codes(
                    &mut b,
                    &mut out,
                    &l,
                    &d,
                    limits,
                    progress,
                    input.len(),
                    &mut checked,
                    d64,
                )?;
            }
            2 => {
                let nlen = b.bits(5)? as usize + 257;
                let ndist = b.bits(5)? as usize + 1;
                let ncode = b.bits(4)? as usize + 4;
                if nlen > 286 || ndist > if d64 { 32 } else { 30 } {
                    return Err(Error::corrupt("too many length or distance codes"));
                }
                let mut cl = [0u8; 19];
                for &o in CL_ORDER.iter().take(ncode) {
                    if let Some(slot) = cl.get_mut(o) {
                        *slot = b.bits(3)? as u8;
                    }
                }
                let clh = Huffman::new(&cl)?;
                let mut lengths = vec![0u8; nlen + ndist];
                let mut i = 0;
                while i < nlen + ndist {
                    let sym = clh.decode(&mut b)?;
                    if sym < 16 {
                        let slot = lengths
                            .get_mut(i)
                            .ok_or_else(|| Error::corrupt("table overrun"))?;
                        *slot = sym as u8;
                        i += 1;
                    } else {
                        let (val, rep) = match sym {
                            16 => {
                                if i == 0 {
                                    return Err(Error::corrupt("repeat with no previous length"));
                                }
                                let prev = lengths.get(i - 1).copied().unwrap_or(0);
                                (prev, 3 + b.bits(2)? as usize)
                            }
                            17 => (0, 3 + b.bits(3)? as usize),
                            _ => (0, 11 + b.bits(7)? as usize),
                        };
                        if i + rep > nlen + ndist {
                            return Err(Error::corrupt("code length repeat overruns table"));
                        }
                        if let Some(run) = lengths.get_mut(i..i + rep) {
                            run.fill(val);
                        }
                        i += rep;
                    }
                }
                if lengths.get(256).copied().unwrap_or(0) == 0 {
                    return Err(Error::corrupt("missing end-of-block code"));
                }
                let (ll, dl) = lengths.split_at(nlen);
                let lh = Huffman::new(ll)?;
                let dh = Huffman::new(dl)?;
                codes(
                    &mut b,
                    &mut out,
                    &lh,
                    &dh,
                    limits,
                    progress,
                    input.len(),
                    &mut checked,
                    d64,
                )?;
            }
            _ => return Err(Error::corrupt("reserved block type").at(b.pos as u64)),
        }
        limits.check_expansion(input.len() as u64, out.len() as u64)?;
        if last {
            break;
        }
    }
    b.align();
    out.drain(..dict.len().min(out.len()));
    Ok(Inflated {
        data: out,
        consumed: b.pos,
    })
}

#[allow(clippy::too_many_arguments)]
fn codes(
    b: &mut Bits<'_>,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
    limits: &Limits,
    progress: &dyn Progress,
    input_len: usize,
    checked: &mut usize,
    d64: bool,
) -> Result<()> {
    loop {
        let sym = usize::from(lit.decode(b)?);
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            _ => {
                let li = sym - 257;
                let base = *LEN_BASE
                    .get(li)
                    .ok_or_else(|| Error::corrupt("bad length symbol"))?;
                let (base, extra) = if d64 && li == 28 {
                    (3, 16)
                } else {
                    (base, LEN_EXTRA.get(li).copied().unwrap_or(0))
                };
                let len = usize::from(base) + b.bits(u32::from(extra))? as usize;
                let ds = usize::from(dist.decode(b)?);
                let (dbase, dextra) = match ds {
                    30 if d64 => (32769usize, 14u8),
                    31 if d64 => (49153usize, 14u8),
                    _ => (
                        usize::from(
                            *DIST_BASE
                                .get(ds)
                                .ok_or_else(|| Error::corrupt("bad distance symbol"))?,
                        ),
                        DIST_EXTRA.get(ds).copied().unwrap_or(0),
                    ),
                };
                let d = dbase + b.bits(u32::from(dextra))? as usize;
                if d > out.len() {
                    return Err(
                        Error::corrupt("distance reaches before start of output").at(b.pos as u64)
                    );
                }
                let start = out.len() - d;
                if d >= len {
                    out.extend_from_within(start..start + len);
                } else {
                    out.reserve(len);
                    for k in 0..len {
                        let v = out.get(start + k).copied().unwrap_or(0);
                        out.push(v);
                    }
                }
            }
        }
        if out.len() - *checked >= 1 << 16 {
            *checked = out.len();
            progress.check_cancelled()?;
            limits.check_expansion(input_len as u64, out.len() as u64)?;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use oc_core::{ErrorCode, NoProgress};

    fn run(d: &[u8]) -> Result<Inflated> {
        inflate(d, &Limits::default(), &NoProgress)
    }

    #[test]
    fn stored_block() {
        let r = run(&[0x01, 0x03, 0x00, 0xFC, 0xFF, b'a', b'b', b'c']).ok();
        assert_eq!(r.as_ref().map(|r| r.data.as_slice()), Some(&b"abc"[..]));
        assert_eq!(r.map(|r| r.consumed), Some(8));
    }

    #[test]
    fn fixed_block_known_stream() {
        // zlib.compress(b"hello hello hello hello", 9)[2:-4] from CPython.
        let raw = [0xCB, 0x48, 0xCD, 0xC9, 0xC9, 0x57, 0xC8, 0x40, 0x27, 0x01];
        assert_eq!(
            run(&raw).ok().map(|r| r.data),
            Some(b"hello hello hello hello".to_vec())
        );
    }

    #[test]
    fn corrupt_inputs_error_not_panic() {
        assert_eq!(
            run(&[]).err().map(|e| e.code),
            Some(ErrorCode::TruncatedFile)
        );
        assert!(run(&[0x07]).is_err()); // reserved block type 3
        assert!(run(&[0x01, 0x03, 0x00, 0x00, 0x00]).is_err()); // bad NLEN
    }

    #[test]
    fn dynamic_block_from_cpython() {
        // zlib level 9 raw stream of 20x a sentence followed by all byte values.
        let z = include_bytes!("../tests/data/dynamic.deflate");
        let mut want = b"The quick brown fox jumps over the lazy dog. ".repeat(20);
        want.extend(0..=255u8);
        let r = run(z).ok();
        assert_eq!(r.as_ref().map(|r| r.data.as_slice()), Some(&want[..]));
        assert_eq!(r.map(|r| r.consumed), Some(z.len()));
    }

    #[test]
    fn deflate64_long_match_and_far_distance_codes() {
        // Fixed block: literal 'a', length symbol 285 with 16 extra bits (997 -> 1000), distance 1.
        let mut bits: Vec<bool> = Vec::new();
        let mut push = |v: u32, n: u32, msb_first: bool| {
            for i in 0..n {
                let sh = if msb_first { n - 1 - i } else { i };
                bits.push(v >> sh & 1 == 1);
            }
        };
        push(1, 1, false); // BFINAL
        push(1, 2, false); // fixed
        push(0x30 + 0x61, 8, true); // literal 'a'
        push(0xC0 + 5, 8, true); // symbol 285
        push(997, 16, false);
        push(0, 5, true); // distance code 0
        push(0, 7, true); // end of block
        let mut raw = vec![0u8; bits.len().div_ceil(8)];
        for (i, b) in bits.iter().enumerate() {
            if *b {
                raw[i / 8] |= 1 << (i % 8);
            }
        }
        let r = inflate64(&raw, &Limits::default(), &oc_core::NoProgress).ok();
        assert_eq!(r.map(|r| r.data), Some(vec![b'a'; 1001]));
        // The same bytes are not valid plain DEFLATE output of that length.
        let plain = inflate(&raw, &Limits::default(), &oc_core::NoProgress).ok();
        assert_ne!(plain.map(|r| r.data.len()), Some(1001));
    }

    #[test]
    fn garbage_never_panics() {
        let mut s = 12345u32;
        for len in 0..200usize {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (s >> 24) as u8
                })
                .collect();
            let _ = run(&junk);
        }
    }

    #[test]
    fn bomb_is_refused() {
        // A long run of zeros compresses ~1000:1; a tight limit must stop it.
        let z = crate::deflate::deflate(&vec![0u8; 5_000_000], 6);
        let limits = Limits {
            max_output: 1 << 20,
            ..Limits::default()
        };
        let e = inflate(&z, &limits, &NoProgress).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::LimitExceeded));
    }

    #[test]
    fn cancellation_is_honoured() {
        let data = vec![7u8; 1_000_000];
        let z = crate::deflate::deflate(&data, 1);
        let t = oc_core::CancelToken::new();
        t.cancel();
        let p = oc_core::progress::TokenProgress::new(t, |_, _, _| {});
        let e = inflate(&z, &Limits::default(), &p).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::Cancelled));
    }
}
