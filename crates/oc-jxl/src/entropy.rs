//! The JPEG XL entropy decoder: prefix (Huffman) codes or rANS with alias tables, hybrid
//! integers, LZ77 and the clustering of contexts into distributions.
use crate::bits::{bad, Br};
use oc_core::Result;

/// `ceil(log2(x + 1))`.
fn add_log2_ceil(x: u32) -> u32 {
    if x >= 0x8000_0000 {
        32
    } else {
        (x + 1).next_power_of_two().trailing_zeros()
    }
}

#[derive(Clone, Debug)]
struct IntConfig {
    split_exponent: u32,
    split: u32,
    msb_in_token: u32,
    lsb_in_token: u32,
}

impl IntConfig {
    fn parse(br: &mut Br, log_alphabet_size: u32) -> Result<Self> {
        let split_exponent = br.read(add_log2_ceil(log_alphabet_size))?;
        let (msb, lsb) = if split_exponent != log_alphabet_size {
            let msb = br.read(add_log2_ceil(split_exponent))?;
            if msb > split_exponent {
                return Err(bad("invalid hybrid integer configuration"));
            }
            let lsb = br.read(add_log2_ceil(split_exponent - msb))?;
            (msb, lsb)
        } else {
            (0, 0)
        };
        if lsb + msb > split_exponent {
            return Err(bad("invalid hybrid integer configuration"));
        }
        Ok(Self {
            split_exponent,
            split: 1 << split_exponent,
            msb_in_token: msb,
            lsb_in_token: lsb,
        })
    }

    fn read_uint(&self, br: &mut Br, token: u32) -> Result<u32> {
        if token < self.split {
            return Ok(token);
        }
        let low = self.msb_in_token + self.lsb_in_token;
        let n = (self.split_exponent - low + ((token - self.split) >> low)) & 31;
        let rest = u64::from(br.read(n)?);
        let low_bits = u64::from(token & ((1 << self.lsb_in_token) - 1));
        let t = (token >> self.lsb_in_token) & ((1 << self.msb_in_token) - 1);
        let t = u64::from(t | (1 << self.msb_in_token));
        Ok(((((t << n) | rest) << self.lsb_in_token) | low_bits) as u32)
    }
}

// ---- prefix codes ----

#[derive(Clone, Debug)]
struct Prefix {
    /// Number of codes of each length 1..=15.
    count: [u16; 16],
    /// First canonical code of each length.
    first: [u32; 16],
    /// Index into `symbols` of the first code of each length.
    offset: [u16; 16],
    symbols: Vec<u16>,
    single: Option<u16>,
}

impl Prefix {
    fn from_lengths(lengths: &[u8]) -> Result<Self> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        count[0] = 0;
        let mut first = [0u32; 16];
        let mut offset = [0u16; 16];
        let mut code = 0u32;
        let mut off = 0u16;
        for len in 1..16 {
            code = (code + u32::from(count[len - 1])) << 1;
            first[len] = code;
            offset[len] = off;
            off += count[len];
        }
        let mut symbols = vec![0u16; usize::from(off)];
        let mut next = offset;
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[usize::from(next[usize::from(l)])] = sym as u16;
                next[usize::from(l)] += 1;
            }
        }
        // The code must be complete.
        let mut kraft = 0u32;
        for len in 1..16 {
            kraft += u32::from(count[len]) << (15 - len);
        }
        if kraft != 1 << 15 {
            return Err(bad("prefix code is not complete"));
        }
        Ok(Self {
            count,
            first,
            offset,
            symbols,
            single: None,
        })
    }

    fn single(sym: u16) -> Self {
        Self {
            count: [0; 16],
            first: [0; 16],
            offset: [0; 16],
            symbols: Vec::new(),
            single: Some(sym),
        }
    }

    fn parse(br: &mut Br, alphabet_size: u32) -> Result<Self> {
        if alphabet_size == 1 {
            return Ok(Self::single(0));
        }
        if alphabet_size > 1 << 15 {
            return Err(bad("prefix alphabet too large"));
        }
        let hskip = br.read(2)?;
        if hskip == 1 {
            Self::parse_simple(br, alphabet_size)
        } else {
            Self::parse_complex(br, alphabet_size, hskip)
        }
    }

    fn parse_simple(br: &mut Br, alphabet_size: u32) -> Result<Self> {
        let abits = alphabet_size.next_power_of_two().trailing_zeros();
        let nsym = br.read(2)? + 1;
        let mut syms = [0usize; 4];
        let lens: [u8; 4];
        match nsym {
            1 => {
                let s = br.read(abits)?;
                if s >= alphabet_size {
                    return Err(bad("prefix symbol out of range"));
                }
                return Ok(Self::single(s as u16));
            }
            2 => {
                syms[2] = br.read(abits)? as usize;
                syms[3] = br.read(abits)? as usize;
                lens = [0, 0, 1, 1];
            }
            3 => {
                for s in &mut syms[1..] {
                    *s = br.read(abits)? as usize;
                }
                lens = [0, 1, 2, 2];
            }
            _ => {
                for s in &mut syms {
                    *s = br.read(abits)? as usize;
                }
                lens = if br.bool()? {
                    [1, 2, 3, 3]
                } else {
                    [2, 2, 2, 2]
                };
            }
        }
        let mut code_lengths = vec![0u8; alphabet_size as usize];
        let used = match nsym {
            2 => 2,
            3 => 3,
            _ => 4,
        };
        for i in 4 - used..4 {
            let out = code_lengths
                .get_mut(syms[i])
                .ok_or_else(|| bad("prefix symbol out of range"))?;
            *out = lens[i];
        }
        Self::from_lengths(&code_lengths)
    }

    fn parse_complex(br: &mut Br, alphabet_size: u32, hskip: u32) -> Result<Self> {
        const ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let mut cl = [0u8; 18];
        let mut bitacc = 0usize;
        let mut nonzero = 0;
        let mut nonzero_sym = 0;
        for &idx in ORDER.iter().skip(hskip as usize) {
            let base = br.u32([(0, 0), (4, 0), (3, 0), (8, 0)])? as u8;
            let len = if base == 8 {
                if br.bool()? {
                    if br.bool()? {
                        5
                    } else {
                        1
                    }
                } else {
                    2
                }
            } else {
                base
            };
            cl[idx] = len;
            if len != 0 {
                nonzero += 1;
                nonzero_sym = idx;
                bitacc += 32 >> len;
                match bitacc.cmp(&32) {
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Equal => break,
                    std::cmp::Ordering::Greater => return Err(bad("invalid prefix code lengths")),
                }
            }
        }
        let lenp = if nonzero == 1 {
            Self::single(nonzero_sym as u16)
        } else if bitacc != 32 {
            return Err(bad("invalid prefix code lengths"));
        } else {
            Self::from_lengths_partial(&cl)?
        };
        let mut lengths = vec![0u8; alphabet_size as usize];
        let mut bitacc = 0usize;
        let mut prev_sym = 8u8;
        let mut last_nonzero = 8u8;
        let mut last_repeat = 0usize;
        let mut repeat = 0usize;
        let mut repeat_sym = 0u8;
        for len in &mut lengths {
            if repeat > 0 {
                *len = repeat_sym;
                repeat -= 1;
            } else {
                let sym = lenp.read(br)? as u8;
                match sym {
                    0 => {}
                    1..=15 => {
                        *len = sym;
                        last_nonzero = sym;
                    }
                    16 => {
                        repeat = br.read(2)? as usize + 3;
                        if prev_sym == 16 {
                            repeat += last_repeat * 3 - 8;
                            last_repeat += repeat;
                        } else {
                            last_repeat = repeat;
                        }
                        repeat_sym = last_nonzero;
                        *len = repeat_sym;
                        repeat -= 1;
                    }
                    _ => {
                        repeat = br.read(3)? as usize + 3;
                        if prev_sym == 17 {
                            repeat += last_repeat * 7 - 16;
                            last_repeat += repeat;
                        } else {
                            last_repeat = repeat;
                        }
                        repeat_sym = 0;
                        *len = 0;
                        repeat -= 1;
                    }
                }
                prev_sym = sym;
            }
            if *len != 0 {
                bitacc += 1 << 15usize.saturating_sub(usize::from(*len));
                if bitacc > 1 << 15 {
                    return Err(bad("prefix code lengths overflow"));
                } else if bitacc == 1 << 15 && repeat == 0 {
                    break;
                }
            }
        }
        if bitacc != 1 << 15 || repeat > 0 {
            return Err(bad("invalid prefix code lengths"));
        }
        Self::from_lengths(&lengths)
    }

    /// A complete code over at most 18 symbols (the code length alphabet).
    fn from_lengths_partial(lengths: &[u8]) -> Result<Self> {
        Self::from_lengths(lengths)
    }

    fn read(&self, br: &mut Br) -> Result<u32> {
        if let Some(s) = self.single {
            return Ok(u32::from(s));
        }
        let mut code = 0u32;
        for len in 1..16 {
            code = (code << 1) | br.read(1)?;
            let c = u32::from(self.count[len]);
            if code.wrapping_sub(self.first[len]) < c {
                let idx = usize::from(self.offset[len]) + (code - self.first[len]) as usize;
                return Ok(u32::from(self.symbols[idx]));
            }
        }
        Err(bad("invalid prefix code"))
    }
}

// ---- rANS ----

#[derive(Clone, Copy, Debug, Default)]
struct Bucket {
    alias_symbol: u16,
    alias_cutoff: u16,
    dist: u16,
    alias_offset: u16,
    alias_dist: u16,
}

#[derive(Clone, Debug)]
struct Ans {
    buckets: Vec<Bucket>,
    log_bucket_size: u32,
    single: Option<u32>,
}

fn read_u8_field(br: &mut Br) -> Result<u32> {
    Ok(if br.bool()? {
        let n = br.read(3)?;
        (1 << n) + br.read(n)?
    } else {
        0
    })
}

fn read_ans_prefix(br: &mut Br) -> Result<u16> {
    Ok(match br.read(3)? {
        0 => 10,
        1 => {
            for val in [4, 0, 11, 13] {
                if br.bool()? {
                    return Ok(val);
                }
            }
            12
        }
        2 => 7,
        3 => {
            if br.bool()? {
                1
            } else {
                3
            }
        }
        4 => 6,
        5 => 8,
        6 => 9,
        _ => {
            if br.bool()? {
                2
            } else {
                5
            }
        }
    })
}

impl Ans {
    fn parse(br: &mut Br, log_alphabet_size: u32) -> Result<Self> {
        let table_size = 1usize << log_alphabet_size;
        let log_bucket_size = 12 - log_alphabet_size;
        let bucket_size = 1u16 << log_bucket_size;
        let mut dist = vec![0u16; table_size];
        let alphabet_size;
        if br.bool()? {
            if br.bool()? {
                let v0 = read_u8_field(br)? as usize;
                let v1 = read_u8_field(br)? as usize;
                if v0 == v1 {
                    return Err(bad("invalid ANS histogram"));
                }
                alphabet_size = v0.max(v1) + 1;
                if alphabet_size > table_size {
                    return Err(bad("invalid ANS histogram"));
                }
                let prob = br.read(12)? as u16;
                dist[v0] = prob;
                dist[v1] = (1 << 12) - prob;
            } else {
                let val = read_u8_field(br)? as usize;
                alphabet_size = val + 1;
                if alphabet_size > table_size {
                    return Err(bad("invalid ANS histogram"));
                }
                dist[val] = 1 << 12;
            }
        } else if br.bool()? {
            alphabet_size = read_u8_field(br)? as usize + 1;
            if alphabet_size > table_size {
                return Err(bad("invalid ANS histogram"));
            }
            let base = (1usize << 12) / alphabet_size;
            let leftover = (1usize << 12) % alphabet_size;
            dist[..leftover].fill(base as u16 + 1);
            dist[leftover..alphabet_size].fill(base as u16);
        } else {
            let mut len = 0u32;
            while len < 3 && br.bool()? {
                len += 1;
            }
            let shift = (br.read(len)? + (1 << len) - 1) as i32;
            if shift > 13 {
                return Err(bad("invalid ANS histogram"));
            }
            alphabet_size = read_u8_field(br)? as usize + 3;
            if alphabet_size > table_size {
                return Err(bad("invalid ANS histogram"));
            }
            let mut repeat_ranges: Vec<std::ops::Range<usize>> = Vec::new();
            let mut omit: Option<(u16, usize)> = None;
            let mut idx = 0;
            while idx < alphabet_size {
                dist[idx] = read_ans_prefix(br)?;
                if dist[idx] == 13 {
                    let rc = read_u8_field(br)? as usize + 4;
                    if idx + rc > alphabet_size {
                        return Err(bad("invalid ANS histogram"));
                    }
                    repeat_ranges.push(idx..idx + rc);
                    idx += rc;
                    continue;
                }
                match &mut omit {
                    Some((log, pos)) => {
                        if dist[idx] > *log {
                            *log = dist[idx];
                            *pos = idx;
                        }
                    }
                    None => omit = Some((dist[idx], idx)),
                }
                idx += 1;
            }
            let Some((_, omit_pos)) = omit else {
                return Err(bad("invalid ANS histogram"));
            };
            if dist.get(omit_pos + 1) == Some(&13) {
                return Err(bad("invalid ANS histogram"));
            }
            let mut ri = 0usize;
            let mut acc = 0u32;
            let mut prev = 0u16;
            for (idx, code) in dist.iter_mut().enumerate() {
                if ri < repeat_ranges.len() && repeat_ranges[ri].start <= idx {
                    if repeat_ranges[ri].end == idx {
                        ri += 1;
                    } else {
                        *code = prev;
                        acc += u32::from(*code);
                        if acc > 1 << 12 {
                            return Err(bad("invalid ANS histogram"));
                        }
                        continue;
                    }
                }
                if *code == 0 {
                    prev = 0;
                    continue;
                }
                if idx == omit_pos {
                    prev = 0;
                    continue;
                }
                if *code > 1 {
                    let zeros = i32::from(*code) - 1;
                    let bitcount = (shift - ((12 - zeros) >> 1)).clamp(0, zeros);
                    *code = (1u16 << zeros)
                        + ((br.read(bitcount as u32)? as u16) << (zeros - bitcount));
                }
                prev = *code;
                acc += u32::from(*code);
                if acc > 1 << 12 {
                    return Err(bad("invalid ANS histogram"));
                }
            }
            dist[omit_pos] = ((1u32 << 12) - acc) as u16;
        }
        if let Some(single) = dist.iter().position(|&d| d == 1 << 12) {
            let buckets = dist
                .iter()
                .enumerate()
                .map(|(i, &d)| Bucket {
                    alias_symbol: single as u16,
                    alias_cutoff: 0,
                    dist: d,
                    alias_offset: bucket_size.wrapping_mul(i as u16),
                    alias_dist: 1 << 12,
                })
                .collect();
            return Ok(Self {
                buckets,
                log_bucket_size,
                single: Some(single as u32),
            });
        }
        // Alias table construction.
        struct Work {
            dist: u16,
            alias_symbol: u16,
            alias_offset: u16,
            alias_cutoff: u16,
        }
        let mut work: Vec<Work> = dist
            .iter()
            .enumerate()
            .map(|(i, &d)| Work {
                dist: d,
                alias_symbol: if i < alphabet_size { i as u16 } else { 0 },
                alias_offset: 0,
                alias_cutoff: d,
            })
            .collect();
        let mut under = Vec::new();
        let mut over = Vec::new();
        for (i, w) in work.iter().enumerate() {
            match w.dist.cmp(&bucket_size) {
                std::cmp::Ordering::Less => under.push(i),
                std::cmp::Ordering::Equal => {}
                std::cmp::Ordering::Greater => over.push(i),
            }
        }
        while let (Some(o), Some(u)) = (over.pop(), under.pop()) {
            let by = bucket_size - work[u].alias_cutoff;
            work[o].alias_cutoff -= by;
            work[u].alias_symbol = o as u16;
            work[u].alias_offset = work[o].alias_cutoff;
            match work[o].alias_cutoff.cmp(&bucket_size) {
                std::cmp::Ordering::Less => under.push(o),
                std::cmp::Ordering::Equal => {}
                std::cmp::Ordering::Greater => over.push(o),
            }
        }
        let buckets = work
            .iter()
            .enumerate()
            .map(|(i, w)| {
                if w.alias_cutoff == bucket_size {
                    Bucket {
                        alias_symbol: i as u16,
                        alias_cutoff: bucket_size,
                        dist: w.dist,
                        alias_offset: 0,
                        alias_dist: w.dist,
                    }
                } else {
                    Bucket {
                        alias_symbol: w.alias_symbol,
                        alias_cutoff: w.alias_cutoff,
                        dist: w.dist,
                        alias_offset: w.alias_offset,
                        alias_dist: work[usize::from(w.alias_symbol)].dist,
                    }
                }
            })
            .collect();
        Ok(Self {
            buckets,
            log_bucket_size,
            single: None,
        })
    }

    fn read(&self, br: &mut Br, state: &mut u32) -> Result<u32> {
        let idx = *state & 0xfff;
        let i = (idx >> self.log_bucket_size) as usize;
        let pos = idx & ((1 << self.log_bucket_size) - 1);
        let b = &self.buckets[i];
        let (symbol, offset, dist) = if self.single.is_some() {
            // Every position maps to the one symbol and the state does not change.
            (
                u32::from(b.alias_symbol),
                (u32::from(b.alias_offset)).wrapping_add(pos),
                1u32 << 12,
            )
        } else if pos >= u32::from(b.alias_cutoff) {
            (
                u32::from(b.alias_symbol),
                u32::from(b.alias_offset) + pos - u32::from(b.alias_cutoff),
                u32::from(b.alias_dist),
            )
        } else {
            (i as u32, pos, u32::from(b.dist))
        };
        let next = (*state >> 12).wrapping_mul(dist).wrapping_add(offset);
        if next < 1 << 16 {
            *state = (next << 16) | br.peek(16);
            br.skip(16)?;
        } else {
            *state = next;
        }
        Ok(symbol)
    }
}

// ---- the decoder ----

#[derive(Clone, Debug)]
enum Code {
    Prefix(Vec<Prefix>),
    Ans {
        dist: Vec<Ans>,
        state: u32,
        initial: bool,
    },
}

#[derive(Clone, Debug)]
struct Lz77 {
    min_symbol: u32,
    min_length: u32,
    len_conf: IntConfig,
    window: Vec<u32>,
    num_to_copy: u32,
    copy_pos: u32,
    num_decoded: u32,
}

#[derive(Clone, Debug)]
pub struct Decoder {
    lz77: Option<Lz77>,
    clusters: Vec<u8>,
    configs: Vec<IntConfig>,
    code: Code,
}

const SPECIAL_DISTANCES: [[i8; 2]; 120] = [
    [0, 1],
    [1, 0],
    [1, 1],
    [-1, 1],
    [0, 2],
    [2, 0],
    [1, 2],
    [-1, 2],
    [2, 1],
    [-2, 1],
    [2, 2],
    [-2, 2],
    [0, 3],
    [3, 0],
    [1, 3],
    [-1, 3],
    [3, 1],
    [-3, 1],
    [2, 3],
    [-2, 3],
    [3, 2],
    [-3, 2],
    [0, 4],
    [4, 0],
    [1, 4],
    [-1, 4],
    [4, 1],
    [-4, 1],
    [3, 3],
    [-3, 3],
    [2, 4],
    [-2, 4],
    [4, 2],
    [-4, 2],
    [0, 5],
    [3, 4],
    [-3, 4],
    [4, 3],
    [-4, 3],
    [5, 0],
    [1, 5],
    [-1, 5],
    [5, 1],
    [-5, 1],
    [2, 5],
    [-2, 5],
    [5, 2],
    [-5, 2],
    [4, 4],
    [-4, 4],
    [3, 5],
    [-3, 5],
    [5, 3],
    [-5, 3],
    [0, 6],
    [6, 0],
    [1, 6],
    [-1, 6],
    [6, 1],
    [-6, 1],
    [2, 6],
    [-2, 6],
    [6, 2],
    [-6, 2],
    [4, 5],
    [-4, 5],
    [5, 4],
    [-5, 4],
    [3, 6],
    [-3, 6],
    [6, 3],
    [-6, 3],
    [0, 7],
    [7, 0],
    [1, 7],
    [-1, 7],
    [5, 5],
    [-5, 5],
    [7, 1],
    [-7, 1],
    [4, 6],
    [-4, 6],
    [6, 4],
    [-6, 4],
    [2, 7],
    [-2, 7],
    [7, 2],
    [-7, 2],
    [3, 7],
    [-3, 7],
    [7, 3],
    [-7, 3],
    [5, 6],
    [-5, 6],
    [6, 5],
    [-6, 5],
    [8, 0],
    [4, 7],
    [-4, 7],
    [7, 4],
    [-7, 4],
    [8, 1],
    [8, 2],
    [6, 6],
    [-6, 6],
    [8, 3],
    [5, 7],
    [-5, 7],
    [7, 5],
    [-7, 5],
    [8, 4],
    [6, 7],
    [-6, 7],
    [7, 6],
    [-7, 6],
    [8, 5],
    [7, 7],
    [-7, 7],
    [8, 6],
    [8, 7],
];

impl Decoder {
    /// Read the distributions of `num_dist` contexts.
    pub fn parse(br: &mut Br, num_dist: u32) -> Result<Self> {
        let lz77 = if br.bool()? {
            let min_symbol = br.u32([(224, 0), (512, 0), (4096, 0), (8, 15)])?;
            let min_length = br.u32([(3, 0), (4, 0), (5, 2), (9, 8)])?;
            let len_conf = IntConfig::parse(br, 8)?;
            Some(Lz77 {
                min_symbol,
                min_length,
                len_conf,
                window: Vec::new(),
                num_to_copy: 0,
                copy_pos: 0,
                num_decoded: 0,
            })
        } else {
            None
        };
        let num_dist = if lz77.is_some() {
            num_dist + 1
        } else {
            num_dist
        };
        Self::parse_inner(br, num_dist, lz77)
    }

    fn parse_no_lz77(br: &mut Br, num_dist: u32) -> Result<Self> {
        if br.bool()? {
            return Err(bad("LZ77 is not allowed here"));
        }
        Self::parse_inner(br, num_dist, None)
    }

    fn parse_inner(br: &mut Br, num_dist: u32, lz77: Option<Lz77>) -> Result<Self> {
        let (num_clusters, clusters) = read_clusters(br, num_dist)?;
        let use_prefix = br.bool()?;
        let log_alphabet_size = if use_prefix { 15 } else { br.read(2)? + 5 };
        let configs = (0..num_clusters)
            .map(|_| IntConfig::parse(br, log_alphabet_size))
            .collect::<Result<Vec<_>>>()?;
        let code = if use_prefix {
            let counts = (0..num_clusters)
                .map(|_| -> Result<u32> {
                    let count = if br.bool()? {
                        let n = br.read(4)?;
                        1 + (1 << n) + br.read(n)?
                    } else {
                        1
                    };
                    if count > 1 << 15 {
                        return Err(bad("prefix alphabet too large"));
                    }
                    Ok(count)
                })
                .collect::<Result<Vec<_>>>()?;
            let dist = counts
                .into_iter()
                .map(|c| Prefix::parse(br, c))
                .collect::<Result<Vec<_>>>()?;
            Code::Prefix(dist)
        } else {
            let dist = (0..num_clusters)
                .map(|_| Ans::parse(br, log_alphabet_size))
                .collect::<Result<Vec<_>>>()?;
            Code::Ans {
                dist,
                state: 0,
                initial: true,
            }
        };
        Ok(Self {
            lz77,
            clusters,
            configs,
            code,
        })
    }

    /// Start reading an entropy-coded stream (reads the initial rANS state).
    pub fn begin(&mut self, br: &mut Br) -> Result<()> {
        if let Code::Ans { state, initial, .. } = &mut self.code {
            *state = br.read(32)?;
            *initial = false;
        }
        Ok(())
    }

    /// Check that an rANS stream ended in the state the format requires.
    pub fn finalize(&self) -> Result<()> {
        if let Code::Ans { state, .. } = &self.code {
            if *state != 0x13_0000 {
                return Err(bad("rANS stream has the wrong final state"));
            }
        }
        Ok(())
    }

    pub fn clusters(&self) -> &[u8] {
        &self.clusters
    }

    fn symbol(&mut self, br: &mut Br, cluster: u8) -> Result<u32> {
        match &mut self.code {
            Code::Prefix(d) => d[usize::from(cluster)].read(br),
            Code::Ans {
                dist,
                state,
                initial,
            } => {
                if *initial {
                    *state = br.read(32)?;
                    *initial = false;
                }
                dist[usize::from(cluster)].read(br, state)
            }
        }
    }

    pub fn single_symbol(&self, cluster: u8) -> Option<u32> {
        if self.lz77.is_some() {
            return None;
        }
        let s = match &self.code {
            Code::Prefix(d) => d[usize::from(cluster)].single.map(u32::from),
            Code::Ans { dist, .. } => dist[usize::from(cluster)].single,
        }?;
        (s < self.configs[usize::from(cluster)].split).then_some(s)
    }

    pub fn read(&mut self, br: &mut Br, ctx: u32) -> Result<u32> {
        self.read_mult(br, ctx, 0)
    }

    /// Read an integer in context `ctx`; `dist_multiplier` is the LZ77 distance multiplier (the
    /// image width for modular channels, 0 elsewhere).
    pub fn read_mult(&mut self, br: &mut Br, ctx: u32, dist_multiplier: u32) -> Result<u32> {
        let cluster = *self
            .clusters
            .get(ctx as usize)
            .ok_or_else(|| bad("context out of range"))?;
        self.read_clustered(br, cluster, dist_multiplier)
    }

    pub fn read_clustered(
        &mut self,
        br: &mut Br,
        cluster: u8,
        dist_multiplier: u32,
    ) -> Result<u32> {
        let Some(mut lz) = self.lz77.take() else {
            let token = self.symbol(br, cluster)?;
            return self.configs[usize::from(cluster)].read_uint(br, token);
        };
        let r = self.read_lz77(br, cluster, dist_multiplier, &mut lz);
        self.lz77 = Some(lz);
        r
    }

    fn read_lz77(
        &mut self,
        br: &mut Br,
        cluster: u8,
        dist_multiplier: u32,
        st: &mut Lz77,
    ) -> Result<u32> {
        let r;
        if st.num_to_copy > 0 {
            r = st.window[(st.copy_pos & 0xf_ffff) as usize];
            st.copy_pos += 1;
            st.num_to_copy -= 1;
        } else {
            let token = self.symbol(br, cluster)?;
            if token >= st.min_symbol {
                if st.num_decoded == 0 {
                    return Err(bad("LZ77 repeat before any symbol"));
                }
                let lz_cluster = *self.clusters.last().ok_or_else(|| bad("no clusters"))?;
                let n = st.len_conf.read_uint(br, token - st.min_symbol)?;
                st.num_to_copy = n
                    .checked_add(st.min_length)
                    .ok_or_else(|| bad("LZ77 length overflow"))?;
                let token = self.symbol(br, lz_cluster)?;
                let distance = self.configs[usize::from(lz_cluster)].read_uint(br, token)?;
                let distance = if dist_multiplier == 0 {
                    distance
                } else if distance < 120 {
                    let [off, d] = SPECIAL_DISTANCES[distance as usize];
                    let v = i32::from(off) + dist_multiplier as i32 * i32::from(d);
                    (v - 1).max(0) as u32
                } else {
                    distance - 120
                };
                let distance = (((1u32 << 20) - 1).min(distance) + 1).min(st.num_decoded);
                st.copy_pos = st.num_decoded - distance;
                r = st.window[(st.copy_pos & 0xf_ffff) as usize];
                st.copy_pos += 1;
                st.num_to_copy -= 1;
            } else {
                r = self.configs[usize::from(cluster)].read_uint(br, token)?;
            }
        }
        let offset = (st.num_decoded & 0xf_ffff) as usize;
        if st.window.len() <= offset {
            st.window.push(r);
        } else {
            st.window[offset] = r;
        }
        st.num_decoded += 1;
        Ok(r)
    }
}

/// Read how contexts are clustered into distributions: `(number of clusters, cluster of each)`.
pub fn read_clusters(br: &mut Br, num_dist: u32) -> Result<(u32, Vec<u8>)> {
    if num_dist == 1 {
        return Ok((1, vec![0]));
    }
    let cluster = if br.bool()? {
        let nbits = br.read(2)?;
        (0..num_dist)
            .map(|_| br.read(nbits).map(|b| b as u8))
            .collect::<Result<Vec<_>>>()?
    } else {
        let use_mtf = br.bool()?;
        let mut dec = if num_dist <= 2 {
            Decoder::parse_no_lz77(br, 1)?
        } else {
            Decoder::parse(br, 1)?
        };
        dec.begin(br)?;
        let mut ret = (0..num_dist)
            .map(|_| -> Result<u8> {
                let b = dec.read(br, 0)?;
                u8::try_from(b).map_err(|_| bad("cluster index out of range"))
            })
            .collect::<Result<Vec<_>>>()?;
        dec.finalize()?;
        if use_mtf {
            let mut map = [0u8; 256];
            for (i, m) in map.iter_mut().enumerate() {
                *m = i as u8;
            }
            for c in &mut ret {
                let idx = usize::from(*c);
                *c = map[idx];
                map.copy_within(0..idx, 1);
                map[0] = *c;
            }
        }
        ret
    };
    let num_clusters = u32::from(*cluster.iter().max().unwrap_or(&0)) + 1;
    let mut seen = [false; 256];
    for &c in &cluster {
        seen[usize::from(c)] = true;
    }
    if seen.iter().filter(|&&s| s).count() as u32 != num_clusters {
        return Err(bad("distribution clusters have a hole"));
    }
    Ok((num_clusters, cluster))
}

/// Read a permutation of `size` entries, the first `skip` fixed.
pub fn read_permutation(
    br: &mut Br,
    dec: &mut Decoder,
    size: u32,
    skip: u32,
) -> Result<Vec<usize>> {
    let ctx = |x: u32| add_log2_ceil(x).min(7);
    let end = dec.read(br, ctx(size))?;
    if end > size - skip {
        return Err(bad("invalid permutation"));
    }
    let mut lehmer = vec![0u32; end as usize];
    let mut prev = 0u32;
    for (idx, v) in lehmer.iter_mut().enumerate() {
        *v = dec.read(br, ctx(prev))?;
        if *v >= size - skip - idx as u32 {
            return Err(bad("invalid permutation"));
        }
        prev = *v;
    }
    let mut temp: Vec<usize> = (skip as usize..size as usize).collect();
    let mut perm: Vec<usize> = (0..skip as usize).collect();
    for l in lehmer {
        perm.push(temp.remove(l as usize));
    }
    perm.extend(temp);
    Ok(perm)
}
