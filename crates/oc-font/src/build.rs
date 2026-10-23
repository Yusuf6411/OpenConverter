//! Writing sfnt files and WOFF wrappers.
use crate::reader::{put_u16, put_u32, R};
use crate::sfnt::Tag;
use oc_core::limits::Limits;
use oc_core::progress::NoProgress;
use oc_core::{Error, Result};

pub fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    for c in data.chunks(4) {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        sum = sum.wrapping_add(u32::from_be_bytes(w));
    }
    sum
}

/// Assemble an sfnt from tables, with a correct directory, checksums and `head` adjustment.
pub fn build_sfnt(flavor: [u8; 4], tables: &mut [(Tag, Vec<u8>)]) -> Vec<u8> {
    tables.sort_by_key(|t| t.0);
    let n = tables.len();
    let mut entry_sel = 0u16;
    while (1usize << (entry_sel + 1)) <= n {
        entry_sel += 1;
    }
    let search_range = (1u16 << entry_sel) * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&flavor);
    put_u16(&mut out, n as u16);
    put_u16(&mut out, search_range);
    put_u16(&mut out, entry_sel);
    put_u16(&mut out, (n as u16).wrapping_mul(16).wrapping_sub(search_range));
    let mut offset = 12 + 16 * n;
    let mut head_pos = None;
    for (tag, data) in tables.iter_mut() {
        if tag == b"head" && data.len() >= 12 {
            data[8..12].fill(0);
        }
        out.extend_from_slice(tag);
        put_u32(&mut out, checksum(data));
        put_u32(&mut out, offset as u32);
        put_u32(&mut out, data.len() as u32);
        if tag == b"head" {
            head_pos = Some(offset);
        }
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in tables.iter() {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    if let Some(p) = head_pos {
        let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        if let Some(dst) = out.get_mut(p + 8..p + 12) {
            dst.copy_from_slice(&adj.to_be_bytes());
        }
    }
    out
}

/// The tables of an sfnt file as owned buffers.
pub fn tables_of(data: &[u8]) -> Result<([u8; 4], Vec<(Tag, Vec<u8>)>)> {
    let font = crate::Font::parse(data)?;
    let flavor = R::new(data)
        .tag(0)
        .ok_or_else(|| Error::truncated("font header"))?;
    Ok((flavor, tables_of_font(&font).1))
}

/// The tables of a parsed font as owned buffers (the flavor is not known here).
pub fn tables_of_font(font: &crate::Font) -> ((), Vec<(Tag, Vec<u8>)>) {
    let tabs = font
        .tables
        .iter()
        .filter_map(|t| {
            font.data
                .get(t.offset..t.offset + t.length)
                .map(|d| (t.tag, d.to_vec()))
        })
        .collect();
    ((), tabs)
}

pub fn is_woff(data: &[u8]) -> bool {
    data.get(..4) == Some(b"wOFF")
}

pub fn is_woff2(data: &[u8]) -> bool {
    data.get(..4) == Some(b"wOF2")
}

/// Unpack a WOFF 1.0 file into a plain sfnt.
pub fn woff_to_sfnt(data: &[u8], limits: &Limits) -> Result<Vec<u8>> {
    let r = R::new(data);
    if !is_woff(data) {
        return Err(Error::corrupt("not a WOFF file"));
    }
    let flavor = r.tag(4).ok_or_else(|| Error::truncated("WOFF header"))?;
    let n = r.u16(12).ok_or_else(|| Error::truncated("WOFF header"))? as usize;
    if n > 4096 {
        return Err(Error::corrupt("too many WOFF tables"));
    }
    let mut tables = Vec::with_capacity(n);
    let mut total = 0u64;
    for i in 0..n {
        let e = 44 + i * 20;
        let (Some(tag), Some(off), Some(clen), Some(olen)) =
            (r.tag(e), r.u32(e + 4), r.u32(e + 8), r.u32(e + 12))
        else {
            return Err(Error::truncated("WOFF table directory"));
        };
        total += u64::from(olen);
        limits.check_alloc(total)?;
        let raw = r
            .slice(off as usize, clen as usize)
            .ok_or_else(|| Error::truncated("WOFF table data"))?;
        let body = if clen < olen {
            let v = oc_compress::zlib::decompress(raw, limits, &NoProgress)?;
            if v.len() != olen as usize {
                return Err(Error::corrupt("WOFF table length mismatch"));
            }
            v
        } else if clen == olen {
            raw.to_vec()
        } else {
            return Err(Error::corrupt("WOFF table is longer than its original"));
        };
        tables.push((tag, body));
    }
    Ok(build_sfnt(flavor, &mut tables))
}

/// Wrap an sfnt as WOFF 1.0 (each table deflated when that makes it smaller).
pub fn sfnt_to_woff(data: &[u8], level: u32) -> Result<Vec<u8>> {
    let (flavor, mut tables) = tables_of(data)?;
    tables.sort_by_key(|t| t.0);
    let n = tables.len();
    let mut comp: Vec<Vec<u8>> = Vec::with_capacity(n);
    for (_, d) in &tables {
        let z = oc_compress::zlib::compress(d, level);
        comp.push(if z.len() < d.len() { z } else { d.clone() });
    }
    let dir_end = 44 + 20 * n;
    let mut offset = dir_end;
    let mut dir = Vec::new();
    let mut body = Vec::new();
    for ((tag, d), c) in tables.iter().zip(&comp) {
        dir.extend_from_slice(tag);
        put_u32(&mut dir, offset as u32);
        put_u32(&mut dir, c.len() as u32);
        put_u32(&mut dir, d.len() as u32);
        put_u32(&mut dir, checksum(d));
        body.extend_from_slice(c);
        offset += c.len();
        while body.len() % 4 != 0 {
            body.push(0);
            offset += 1;
        }
    }
    let sfnt_size = 12 + 16 * n + tables.iter().map(|(_, d)| d.len().div_ceil(4) * 4).sum::<usize>();
    let mut out = Vec::new();
    out.extend_from_slice(b"wOFF");
    out.extend_from_slice(&flavor);
    put_u32(&mut out, (dir_end + body.len()) as u32);
    put_u16(&mut out, n as u16);
    put_u16(&mut out, 0);
    put_u32(&mut out, sfnt_size as u32);
    put_u16(&mut out, 1);
    put_u16(&mut out, 0);
    for _ in 0..5 {
        put_u32(&mut out, 0);
    }
    out.extend_from_slice(&dir);
    out.extend_from_slice(&body);
    Ok(out)
}
