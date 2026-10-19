//! LZ4 block and frame formats (read, including the legacy frame; write).
#![allow(clippy::indexing_slicing)] // positions are checked against the buffer ends before use
use oc_checksum::xxhash32;
use oc_core::{Error, ErrorCode, Limits, Progress, Result};

const MAGIC: u32 = 0x184D_2204;
const LEGACY: u32 = 0x184C_2102;
const MIN_MATCH: usize = 4;
const MF_LIMIT: usize = 12;
const LAST_LITERALS: usize = 5;

/// Decodes one block, appending to `out`. Back references reach no further than `floor`.
pub fn decode_block(src: &[u8], out: &mut Vec<u8>, floor: usize, limit: usize) -> Result<()> {
    let mut i = 0usize;
    let start = out.len();
    while i < src.len() {
        let token = src[i];
        i += 1;
        let mut ll = usize::from(token >> 4);
        if ll == 15 {
            loop {
                let b = *src.get(i).ok_or_else(|| Error::truncated("lz4: literal length"))?;
                i += 1;
                ll += usize::from(b);
                if b != 255 {
                    break;
                }
                if ll > limit {
                    return Err(Error::corrupt("lz4: literal run too long"));
                }
            }
        }
        let lits = src
            .get(i..i + ll)
            .ok_or_else(|| Error::truncated("lz4: literals"))?;
        out.extend_from_slice(lits);
        i += ll;
        if out.len() - start > limit {
            return Err(Error::corrupt("lz4: block larger than its declared maximum"));
        }
        if i == src.len() {
            return Ok(());
        }
        let off = src
            .get(i..i + 2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
            .ok_or_else(|| Error::truncated("lz4: match offset"))?;
        i += 2;
        if off == 0 || off > out.len() - floor {
            return Err(Error::corrupt("lz4: match offset out of range").at(i as u64));
        }
        let mut ml = usize::from(token & 15);
        if ml == 15 {
            loop {
                let b = *src.get(i).ok_or_else(|| Error::truncated("lz4: match length"))?;
                i += 1;
                ml += usize::from(b);
                if b != 255 {
                    break;
                }
                if ml > limit {
                    return Err(Error::corrupt("lz4: match too long"));
                }
            }
        }
        ml += MIN_MATCH;
        if out.len() - start + ml > limit {
            return Err(Error::corrupt("lz4: block larger than its declared maximum"));
        }
        let from = out.len() - off;
        if off >= ml {
            out.extend_from_within(from..from + ml);
        } else {
            for k in 0..ml {
                let b = out[from + k];
                out.push(b);
            }
        }
    }
    Err(Error::truncated("lz4: block ends inside a sequence"))
}

pub fn decompress(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut frames = 0;
    let u32_at = |p: usize| -> Result<u32> {
        input
            .get(p..p + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| Error::truncated("lz4: header"))
    };
    while pos < input.len() {
        progress.check_cancelled()?;
        let magic = u32_at(pos)?;
        pos += 4;
        if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
            let n = u32_at(pos)? as usize;
            pos += 4 + n;
            if pos > input.len() {
                return Err(Error::truncated("lz4: skippable frame"));
            }
            continue;
        }
        if magic == LEGACY {
            while pos < input.len() {
                if input.len() - pos >= 4 && matches!(u32_at(pos)?, MAGIC | LEGACY) {
                    break;
                }
                let n = u32_at(pos)? as usize;
                pos += 4;
                let body = input
                    .get(pos..pos + n)
                    .ok_or_else(|| Error::truncated("lz4: legacy block"))?;
                let floor = out.len();
                decode_block(body, &mut out, floor, 8 << 20)?;
                pos += n;
                limits.check_expansion(input.len() as u64, out.len() as u64)?;
            }
            frames += 1;
            continue;
        }
        if magic != MAGIC {
            return Err(if frames == 0 {
                Error::corrupt("not an LZ4 stream")
            } else {
                Error::corrupt("lz4: unexpected data after the last frame").at(pos as u64 - 4)
            });
        }
        let desc_start = pos;
        let flg = *input.get(pos).ok_or_else(|| Error::truncated("lz4: descriptor"))?;
        let bd = *input.get(pos + 1).ok_or_else(|| Error::truncated("lz4: descriptor"))?;
        pos += 2;
        if flg >> 6 != 1 {
            return Err(Error::new(ErrorCode::UnsupportedVariant, "lz4: unknown frame version"));
        }
        if flg & 2 != 0 || bd & 0x8F != 0 {
            return Err(Error::corrupt("lz4: reserved bits set"));
        }
        let indep = flg & 0x20 != 0;
        let block_sum = flg & 0x10 != 0;
        let has_size = flg & 8 != 0;
        let content_sum = flg & 4 != 0;
        let has_dict = flg & 1 != 0;
        let max_block = match (bd >> 4) & 7 {
            4 => 64 << 10,
            5 => 256 << 10,
            6 => 1 << 20,
            7 => 4 << 20,
            _ => return Err(Error::corrupt("lz4: bad block size code")),
        };
        let mut content_size = None;
        if has_size {
            let b = input.get(pos..pos + 8).ok_or_else(|| Error::truncated("lz4: content size"))?;
            content_size = Some(u64::from_le_bytes(b.try_into().unwrap_or([0; 8])));
            pos += 8;
        }
        if has_dict {
            return Err(Error::new(ErrorCode::UnsupportedVariant, "lz4: frames with a dictionary are not supported"));
        }
        let hc = *input.get(pos).ok_or_else(|| Error::truncated("lz4: header checksum"))?;
        if ((xxhash32(&input[desc_start..pos], 0) >> 8) & 0xFF) as u8 != hc {
            return Err(Error::corrupt("lz4: header checksum mismatch"));
        }
        pos += 1;
        if let Some(s) = content_size {
            limits.check_alloc(s)?;
        }
        let frame_start = out.len();
        loop {
            let raw = u32_at(pos)?;
            pos += 4;
            if raw == 0 {
                break;
            }
            let stored = raw & 0x8000_0000 != 0;
            let n = (raw & 0x7FFF_FFFF) as usize;
            if n > max_block + 16 {
                return Err(Error::corrupt("lz4: block larger than the declared maximum"));
            }
            let body = input.get(pos..pos + n).ok_or_else(|| Error::truncated("lz4: block"))?;
            pos += n;
            if block_sum {
                if xxhash32(body, 0) != u32_at(pos)? {
                    return Err(Error::corrupt("lz4: block checksum mismatch"));
                }
                pos += 4;
            }
            if stored {
                out.extend_from_slice(body);
            } else {
                // Linked blocks may reach up to 64 KiB back into earlier blocks.
                let floor = if indep { out.len() } else { out.len().saturating_sub(65_536).max(frame_start) };
                decode_block(body, &mut out, floor, max_block)?;
            }
            limits.check_expansion(input.len() as u64, out.len() as u64)?;
        }
        if content_sum {
            let want = u32_at(pos)?;
            pos += 4;
            if xxhash32(&out[frame_start..], 0) != want {
                return Err(Error::corrupt("lz4: content checksum mismatch"));
            }
        }
        if content_size.is_some_and(|s| s != (out.len() - frame_start) as u64) {
            return Err(Error::corrupt("lz4: content size mismatch"));
        }
        frames += 1;
        progress.report(pos as u64, input.len() as u64, "lz4");
    }
    Ok(out)
}

// ---------------------------------------------------------------- encoder

struct Params {
    depth: usize,
    skip: bool,
}

fn params(level: u32) -> Params {
    match level {
        0..=2 => Params { depth: 1, skip: true },
        3 => Params { depth: 8, skip: false },
        4..=6 => Params { depth: 32, skip: false },
        7..=9 => Params { depth: 128, skip: false },
        _ => Params { depth: 1024, skip: false },
    }
}

fn put_len(out: &mut Vec<u8>, mut n: usize) {
    while n >= 255 {
        out.push(255);
        n -= 255;
    }
    out.push(n as u8);
}

fn emit(out: &mut Vec<u8>, lits: &[u8], m: Option<(usize, usize)>) {
    let ll = lits.len();
    let ml = m.map_or(0, |(_, l)| l - MIN_MATCH);
    out.push(((ll.min(15) as u8) << 4) | if m.is_some() { ml.min(15) as u8 } else { 0 });
    if ll >= 15 {
        put_len(out, ll - 15);
    }
    out.extend_from_slice(lits);
    if let Some((dist, _)) = m {
        out.extend_from_slice(&(dist as u16).to_le_bytes());
        if ml >= 15 {
            put_len(out, ml - 15);
        }
    }
}

/// Compresses one independent block.
pub fn compress_block(src: &[u8], level: u32) -> Vec<u8> {
    let p = params(level);
    let mut out = Vec::with_capacity(src.len() / 2 + 16);
    let n = src.len();
    if n < MF_LIMIT + 1 {
        emit(&mut out, src, None);
        return out;
    }
    let hash_bits = 16;
    let mut head = vec![u32::MAX; 1 << hash_bits];
    let mut prev = if p.depth > 1 { vec![u32::MAX; n] } else { Vec::new() };
    let hash = |i: usize| -> usize {
        let w = u32::from_le_bytes([src[i], src[i + 1], src[i + 2], src[i + 3]]);
        (w.wrapping_mul(2_654_435_761) >> (32 - hash_bits)) as usize
    };
    let last_match_start = n - MF_LIMIT;
    let match_end = n - LAST_LITERALS;
    let mut anchor = 0usize;
    let mut i = 0usize;
    let mut miss = 0usize;
    while i <= last_match_start {
        let h = hash(i);
        let mut cand = head[h];
        let mut best = (0usize, 0usize);
        let mut steps = p.depth;
        while cand != u32::MAX && steps > 0 {
            let c = cand as usize;
            let dist = i - c;
            if dist > 65_535 {
                break;
            }
            let max = match_end - i;
            let mut l = 0;
            while l < max && src[c + l] == src[i + l] {
                l += 1;
            }
            if l >= MIN_MATCH && l > best.1 {
                best = (dist, l);
                if l == max {
                    break;
                }
            }
            if p.depth == 1 {
                break;
            }
            cand = prev[c];
            steps -= 1;
        }
        if p.depth > 1 {
            prev[i] = head[h];
        }
        head[h] = i as u32;
        if best.1 < MIN_MATCH {
            miss += 1;
            i += if p.skip { 1 + (miss >> 6) } else { 1 };
            continue;
        }
        miss = 0;
        // Extend backwards over literals that also match.
        let (dist, mut len) = best;
        let mut start = i;
        while start > anchor && start > dist && src[start - 1] == src[start - 1 - dist] {
            start -= 1;
            len += 1;
        }
        emit(&mut out, &src[anchor..start], Some((dist, len)));
        let end = start + len;
        // Index a few positions inside the match.
        if p.depth > 1 {
            let mut k = i + 1;
            while k < end && k <= last_match_start {
                let hk = hash(k);
                prev[k] = head[hk];
                head[hk] = k as u32;
                k += 1;
            }
        } else if end >= 2 && end - 2 <= last_match_start {
            head[hash(end - 2)] = (end - 2) as u32;
        }
        i = end;
        anchor = end;
    }
    emit(&mut out, &src[anchor..], None);
    out
}

/// One LZ4 frame with independent 4 MiB blocks, content size and content checksum.
pub fn compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC.to_le_bytes());
    let desc = {
        let mut d = vec![0x40 | 0x20 | 0x08 | 0x04, 0x70];
        d.extend_from_slice(&(data.len() as u64).to_le_bytes());
        d
    };
    out.extend_from_slice(&desc);
    out.push(((xxhash32(&desc, 0) >> 8) & 0xFF) as u8);
    for chunk in data.chunks(4 << 20) {
        let c = compress_block(chunk, level);
        if c.len() >= chunk.len() {
            out.extend_from_slice(&(chunk.len() as u32 | 0x8000_0000).to_le_bytes());
            out.extend_from_slice(chunk);
        } else {
            out.extend_from_slice(&(c.len() as u32).to_le_bytes());
            out.extend(c);
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&xxhash32(data, 0).to_le_bytes());
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

    fn data(n: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        let mut v = Vec::new();
        while v.len() < n {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let r = (s >> 16) as usize;
            if r % 3 == 0 && v.len() > 20 {
                let from = r % (v.len() - 10);
                let len = 4 + r % 40;
                for k in 0..len {
                    v.push(v[from + k % (v.len() - from)]);
                }
            } else {
                v.push(b'a' + (r % 20) as u8);
            }
        }
        v.truncate(n);
        v
    }

    #[test]
    fn roundtrips() {
        for n in [0usize, 1, 11, 12, 13, 17, 100, 1000, 70_000, 300_000] {
            let src = data(n, n as u32 + 1);
            for level in [1, 3, 9, 12] {
                let z = compress(&src, level);
                assert_eq!(d(&z).unwrap(), src, "n {n} level {level}");
            }
        }
    }

    #[test]
    fn corruption_is_an_error_not_a_panic() {
        let z = compress(&data(5000, 3), 3);
        let mut x = 11u32;
        for _ in 0..500 {
            let mut bad = z.clone();
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let p = (x as usize >> 4) % bad.len();
            bad[p] ^= 1 << (x % 8);
            let _ = d(&bad);
        }
        assert!(d(&z[..z.len() - 3]).is_err());
    }
}
