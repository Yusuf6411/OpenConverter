//! The .xz container (XZ file format 1.1): blocks, index, footer, CRC-32/CRC-64/SHA-256
//! checks, LZMA2 with the delta and branch filters.
use crate::bcj::{self, Arch};
use crate::lzma::{self, Level};
use oc_checksum::{crc32, crc64_xz};
use oc_core::{Error, ErrorCode, Limits, Progress, Result};
use sha2::{Digest, Sha256};

const MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0];
const FOOTER_MAGIC: [u8; 2] = [b'Y', b'Z'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    None,
    Crc32,
    Crc64,
    Sha256,
}

impl Check {
    fn id(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Crc32 => 1,
            Self::Crc64 => 4,
            Self::Sha256 => 10,
        }
    }
    fn size(self) -> usize {
        match self {
            Self::None => 0,
            Self::Crc32 => 4,
            Self::Crc64 => 8,
            Self::Sha256 => 32,
        }
    }
    fn from_id(id: u8) -> Result<Self> {
        match id {
            0 => Ok(Self::None),
            1 => Ok(Self::Crc32),
            4 => Ok(Self::Crc64),
            10 => Ok(Self::Sha256),
            2 | 3 | 5..=9 | 11..=15 => Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!("xz: integrity check type {id} is not supported"),
            )),
            _ => Err(Error::corrupt("xz: bad check type")),
        }
    }
    fn compute(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::None => Vec::new(),
            Self::Crc32 => crc32(data).to_le_bytes().to_vec(),
            Self::Crc64 => crc64_xz(data).to_le_bytes().to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
        }
    }
}

/// An optional pre-filter for the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    None,
    Delta(u32),
    Branch(Arch),
}

fn varint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for i in 0..9 {
        let b = *buf
            .get(*pos)
            .ok_or_else(|| Error::truncated("xz: variable-length integer"))?;
        *pos += 1;
        v |= u64::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            if b == 0 && i > 0 {
                return Err(Error::corrupt("xz: non-minimal integer"));
            }
            return Ok(v);
        }
    }
    Err(Error::corrupt("xz: integer too long"))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

enum Step {
    Delta(usize),
    Branch(Arch),
}

pub fn decompress(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut streams = 0;
    loop {
        // Stream padding: zero bytes in multiples of four between streams.
        let mut p = pos;
        while input.get(p) == Some(&0) {
            p += 1;
        }
        if streams > 0 && p >= input.len() {
            if (p - pos) % 4 != 0 {
                return Err(Error::corrupt("xz: stream padding is not a multiple of four"));
            }
            break;
        }
        if streams > 0 && (p - pos) % 4 != 0 {
            return Err(Error::corrupt("xz: stream padding is not a multiple of four"));
        }
        pos = p;
        let used = stream(input.get(pos..).unwrap_or(&[]), &mut out, limits, progress)
            .map_err(|e| match e.offset {
                Some(o) => e.at(o + pos as u64),
                None => e,
            })?;
        pos += used;
        streams += 1;
        if pos >= input.len() {
            break;
        }
    }
    Ok(out)
}

fn stream(data: &[u8], out: &mut Vec<u8>, limits: &Limits, progress: &dyn Progress) -> Result<usize> {
    if data.len() < 12 || data.get(..6) != Some(&MAGIC[..]) {
        return Err(Error::corrupt("not an xz stream"));
    }
    let flags = data.get(6..8).ok_or_else(|| Error::truncated("xz: header"))?;
    let hcrc = u32::from_le_bytes(data.get(8..12).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]));
    if crc32(flags) != hcrc {
        return Err(Error::corrupt("xz: stream header CRC mismatch"));
    }
    if flags.first() != Some(&0) || flags.get(1).is_some_and(|b| b & 0xF0 != 0) {
        return Err(Error::corrupt("xz: reserved stream flags set"));
    }
    let check = Check::from_id(flags.get(1).copied().unwrap_or(0) & 0x0F)?;
    let mut pos = 12usize;
    let mut records: Vec<(u64, u64)> = Vec::new();
    loop {
        progress.check_cancelled()?;
        let first = *data
            .get(pos)
            .ok_or_else(|| Error::truncated("xz: stream ends before its index").at(pos as u64))?;
        if first == 0 {
            break;
        }
        let hsize = (usize::from(first) + 1) * 4;
        let header = data
            .get(pos..pos + hsize)
            .ok_or_else(|| Error::truncated("xz: block header"))?;
        let want = u32::from_le_bytes(header[hsize - 4..].try_into().unwrap_or([0; 4]));
        if crc32(&header[..hsize - 4]) != want {
            return Err(Error::corrupt("xz: block header CRC mismatch").at(pos as u64));
        }
        let bf = header[1];
        if bf & 0x3C != 0 {
            return Err(Error::corrupt("xz: reserved block flags set"));
        }
        let n_filters = usize::from(bf & 3) + 1;
        let mut hp = 2usize;
        let h = &header[..hsize - 4];
        let comp_size = if bf & 0x40 != 0 { Some(varint(h, &mut hp)?) } else { None };
        let uncomp_size = if bf & 0x80 != 0 { Some(varint(h, &mut hp)?) } else { None };
        let mut steps: Vec<Step> = Vec::new();
        let mut dict = 0u32;
        for i in 0..n_filters {
            let id = varint(h, &mut hp)?;
            let psize = varint(h, &mut hp)? as usize;
            let props = h
                .get(hp..hp + psize)
                .ok_or_else(|| Error::corrupt("xz: filter properties overrun the header"))?;
            hp += psize;
            let last = i + 1 == n_filters;
            match id {
                0x21 => {
                    if !last || psize != 1 {
                        return Err(Error::corrupt("xz: LZMA2 must be the last filter"));
                    }
                    let b = props[0];
                    if b > 40 {
                        return Err(Error::corrupt("xz: bad LZMA2 dictionary size"));
                    }
                    dict = if b == 40 { u32::MAX } else { (2 | u32::from(b & 1)) << (b / 2 + 11) };
                }
                0x03 => {
                    if last || psize != 1 {
                        return Err(Error::corrupt("xz: bad delta filter"));
                    }
                    steps.push(Step::Delta(usize::from(props[0]) + 1));
                }
                _ => {
                    if last {
                        return Err(Error::corrupt("xz: the last filter must be LZMA2"));
                    }
                    if psize != 0 {
                        return Err(Error::new(
                            ErrorCode::UnsupportedVariant,
                            "xz: branch filters with a start offset are not supported",
                        ));
                    }
                    steps.push(Step::Branch(Arch::from_filter_id(id)?));
                }
            }
        }
        if h.get(hp..).is_some_and(|rest| rest.iter().any(|&b| b != 0)) {
            return Err(Error::corrupt("xz: block header padding is not zero"));
        }
        let body_start = pos + hsize;
        let body = data.get(body_start..).unwrap_or(&[]);
        let (mut block, used) = lzma::decode_lzma2_counted(body, dict, limits, progress)?;
        if comp_size.is_some_and(|c| c != used as u64) {
            return Err(Error::corrupt("xz: block compressed size mismatch"));
        }
        if uncomp_size.is_some_and(|u| u != block.len() as u64) {
            return Err(Error::corrupt("xz: block uncompressed size mismatch"));
        }
        for st in steps.iter().rev() {
            match st {
                Step::Delta(d) => bcj::delta(&mut block, *d, false),
                Step::Branch(a) => bcj::convert(*a, &mut block, false),
            }
        }
        let padded = used.div_ceil(4) * 4;
        let pad = body
            .get(used..padded)
            .ok_or_else(|| Error::truncated("xz: block padding"))?;
        if pad.iter().any(|&b| b != 0) {
            return Err(Error::corrupt("xz: block padding is not zero"));
        }
        let cs = check.size();
        let stored = body
            .get(padded..padded + cs)
            .ok_or_else(|| Error::truncated("xz: block check"))?;
        if check.compute(&block) != stored {
            return Err(Error::corrupt("xz: integrity check failed").at((body_start + padded) as u64));
        }
        records.push(((hsize + used + cs) as u64, block.len() as u64));
        pos = body_start + padded + cs;
        out.extend_from_slice(&block);
        limits.check_expansion(data.len() as u64, out.len() as u64)?;
        progress.report(pos as u64, data.len() as u64, "xz");
    }
    // Index.
    let idx_start = pos;
    pos += 1;
    let n = varint(data, &mut pos)?;
    if n != records.len() as u64 {
        return Err(Error::corrupt("xz: index record count mismatch"));
    }
    for r in &records {
        let unpadded = varint(data, &mut pos)?;
        let size = varint(data, &mut pos)?;
        if (unpadded, size) != *r {
            return Err(Error::corrupt("xz: index does not match the blocks"));
        }
    }
    while (pos - idx_start) % 4 != 0 {
        if data.get(pos) != Some(&0) {
            return Err(Error::corrupt("xz: index padding is not zero"));
        }
        pos += 1;
    }
    let want = u32::from_le_bytes(
        data.get(pos..pos + 4)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| Error::truncated("xz: index CRC"))?,
    );
    if crc32(data.get(idx_start..pos).unwrap_or(&[])) != want {
        return Err(Error::corrupt("xz: index CRC mismatch"));
    }
    let index_size = pos + 4 - idx_start;
    pos += 4;
    // Footer.
    let f = data
        .get(pos..pos + 12)
        .ok_or_else(|| Error::truncated("xz: stream footer"))?;
    let fcrc = u32::from_le_bytes(f[..4].try_into().unwrap_or([0; 4]));
    if crc32(&f[4..10]) != fcrc {
        return Err(Error::corrupt("xz: footer CRC mismatch"));
    }
    let backward = (u32::from_le_bytes(f[4..8].try_into().unwrap_or([0; 4])) as usize + 1) * 4;
    if backward != index_size || f[8..10] != *flags || f[10..12] != FOOTER_MAGIC {
        return Err(Error::corrupt("xz: stream footer does not match the header"));
    }
    Ok(pos + 12)
}

/// One-block .xz with LZMA2 at preset `level` (0 to 9).
pub fn compress(data: &[u8], level: u32, check: Check) -> Vec<u8> {
    compress_filtered(data, level, check, Filter::None)
}

pub fn compress_filtered(data: &[u8], level: u32, check: Check, filter: Filter) -> Vec<u8> {
    let mut work = data.to_vec();
    match filter {
        Filter::None => {}
        Filter::Delta(d) => bcj::delta(&mut work, d.clamp(1, 256) as usize, true),
        Filter::Branch(a) => bcj::convert(a, &mut work, true),
    }
    let (dict_prop, body) = lzma::encode_lzma2(&work, Level(level)).unwrap_or((0, vec![0]));
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    let flags = [0u8, check.id()];
    out.extend_from_slice(&flags);
    out.extend_from_slice(&crc32(&flags).to_le_bytes());
    // Block header.
    let mut h = vec![0u8, 0u8];
    let n_filters = if filter == Filter::None { 1 } else { 2 };
    h[1] = (n_filters - 1) as u8;
    match filter {
        Filter::None => {}
        Filter::Delta(d) => {
            put_varint(&mut h, 3);
            put_varint(&mut h, 1);
            h.push((d.clamp(1, 256) - 1) as u8);
        }
        Filter::Branch(a) => {
            put_varint(&mut h, a.filter_id());
            put_varint(&mut h, 0);
        }
    }
    put_varint(&mut h, 0x21);
    put_varint(&mut h, 1);
    h.push(dict_prop);
    while (h.len() + 4) % 4 != 0 {
        h.push(0);
    }
    h[0] = ((h.len() + 4) / 4 - 1) as u8;
    let c = crc32(&h);
    h.extend_from_slice(&c.to_le_bytes());
    let header_len = h.len();
    out.extend(h);
    out.extend_from_slice(&body);
    let padded = body.len().div_ceil(4) * 4;
    out.resize(out.len() + padded - body.len(), 0);
    out.extend(check.compute(data));
    // Index.
    let mut idx = vec![0u8];
    put_varint(&mut idx, 1);
    put_varint(&mut idx, (header_len + body.len() + check.size()) as u64);
    put_varint(&mut idx, data.len() as u64);
    while idx.len() % 4 != 0 {
        idx.push(0);
    }
    let c = crc32(&idx);
    idx.extend_from_slice(&c.to_le_bytes());
    let backward = (idx.len() / 4 - 1) as u32;
    out.extend(&idx);
    let mut footer = Vec::new();
    footer.extend_from_slice(&backward.to_le_bytes());
    footer.extend_from_slice(&flags);
    out.extend_from_slice(&crc32(&footer).to_le_bytes());
    out.extend(footer);
    out.extend_from_slice(&FOOTER_MAGIC);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
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
    fn roundtrips_all_levels_and_checks() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            b"x".to_vec(),
            b"hello hello hello hello".to_vec(),
            lcg(40_000, 3, 256),
            lcg(200_000, 4, 6),
            b"0123456789".repeat(30_000),
        ];
        for c in &cases {
            for (level, check) in [(0, Check::Crc32), (3, Check::Crc64), (6, Check::Sha256), (9, Check::None)] {
                let z = compress(c, level, check);
                assert_eq!(&d(&z).unwrap(), c, "len {} level {level}", c.len());
            }
        }
    }

    #[test]
    fn filters_roundtrip() {
        let data = lcg(50_000, 8, 255);
        for f in [Filter::Delta(4), Filter::Branch(Arch::X86), Filter::Branch(Arch::Arm64)] {
            assert_eq!(d(&compress_filtered(&data, 4, Check::Crc64, f)).unwrap(), data, "{f:?}");
        }
    }

    #[test]
    fn two_streams_and_padding() {
        let mut z = compress(b"one ", 1, Check::Crc32);
        z.extend([0u8; 8]);
        z.extend(compress(b"two", 1, Check::Crc64));
        assert_eq!(d(&z).unwrap(), b"one two");
    }

    #[test]
    fn corruption_is_an_error() {
        let z = compress(&lcg(5000, 1, 20), 2, Check::Crc64);
        assert!(d(&z[..z.len() - 5]).is_err());
        let mut x = 5u32;
        for _ in 0..400 {
            let mut bad = z.clone();
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let p = (x as usize >> 3) % bad.len();
            bad[p] ^= 1 << (x % 8);
            let _ = d(&bad);
        }
    }
}
