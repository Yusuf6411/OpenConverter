//! The sfnt container (TrueType and OpenType), table access, metrics, character maps.
use crate::reader::R;
use oc_core::{Error, Result};

pub type Tag = [u8; 4];

#[derive(Debug, Clone)]
pub struct TableRec {
    pub tag: Tag,
    pub offset: usize,
    pub length: usize,
}

/// A parsed font file. Tables are located but not copied.
#[derive(Debug, Clone)]
pub struct Font {
    pub data: Vec<u8>,
    pub tables: Vec<TableRec>,
    pub units_per_em: u16,
    pub num_glyphs: u16,
    pub loca_long: bool,
    pub is_cff: bool,
    pub num_hmetrics: u16,
    pub ascent: i16,
    pub descent: i16,
    pub line_gap: i16,
}

pub fn tag_str(t: Tag) -> String {
    t.iter().map(|b| char::from(*b)).collect()
}

/// Offsets of the fonts in a file (one, unless it is a collection).
pub fn font_offsets(data: &[u8]) -> Vec<usize> {
    let r = R::new(data);
    if r.tag(0) == Some(*b"ttcf") {
        let n = r.u32(8).unwrap_or(0).min(1024) as usize;
        return (0..n)
            .filter_map(|i| r.u32(12 + i * 4).map(|v| v as usize))
            .collect();
    }
    vec![0]
}

pub fn is_sfnt(data: &[u8]) -> bool {
    matches!(
        data.get(..4),
        Some([0, 1, 0, 0]) | Some(b"OTTO") | Some(b"true") | Some(b"typ1") | Some(b"ttcf")
    )
}

impl Font {
    pub fn parse(data: &[u8]) -> Result<Font> {
        Self::parse_at(data, 0)
    }

    /// Parse the `index`th font of a file (0 for non-collections).
    pub fn parse_index(data: &[u8], index: usize) -> Result<Font> {
        let offs = font_offsets(data);
        let off = *offs
            .get(index)
            .ok_or_else(|| Error::corrupt("font index out of range"))?;
        Self::parse_at(data, off)
    }

    fn parse_at(data: &[u8], base: usize) -> Result<Font> {
        let r = R::new(data);
        let ver = r
            .tag(base)
            .ok_or_else(|| Error::truncated("font header"))?;
        let is_cff = &ver == b"OTTO";
        if !(ver == [0, 1, 0, 0] || is_cff || &ver == b"true" || &ver == b"typ1") {
            return Err(Error::corrupt("not an sfnt font"));
        }
        let n = r.u16(base + 4).ok_or_else(|| Error::truncated("font header"))? as usize;
        if n > 4096 {
            return Err(Error::corrupt("too many font tables"));
        }
        let mut tables = Vec::with_capacity(n);
        for i in 0..n {
            let e = base + 12 + i * 16;
            let tag = r.tag(e).ok_or_else(|| Error::truncated("table directory"))?;
            let offset = r.u32(e + 8).ok_or_else(|| Error::truncated("table directory"))? as usize;
            let length = r.u32(e + 12).ok_or_else(|| Error::truncated("table directory"))? as usize;
            // A table running past the end is clipped rather than refused; some fonts in the
            // wild have a wrong length on the last one.
            let length = length.min(data.len().saturating_sub(offset));
            if offset > data.len() {
                continue;
            }
            tables.push(TableRec {
                tag,
                offset,
                length,
            });
        }
        let mut f = Font {
            data: data.to_vec(),
            tables,
            units_per_em: 1000,
            num_glyphs: 0,
            loca_long: false,
            is_cff,
            num_hmetrics: 0,
            ascent: 0,
            descent: 0,
            line_gap: 0,
        };
        let head = f
            .table(b"head")
            .ok_or_else(|| Error::corrupt("font has no head table"))?
            .to_vec();
        let hr = R::new(&head);
        f.units_per_em = hr.u16(18).unwrap_or(1000).max(16);
        f.loca_long = hr.i16(50).unwrap_or(0) != 0;
        if let Some(m) = f.table(b"maxp").map(<[u8]>::to_vec) {
            f.num_glyphs = R::new(&m).u16(4).unwrap_or(0);
        }
        if let Some(h) = f.table(b"hhea").map(<[u8]>::to_vec) {
            let hr = R::new(&h);
            f.ascent = hr.i16(4).unwrap_or(0);
            f.descent = hr.i16(6).unwrap_or(0);
            f.line_gap = hr.i16(8).unwrap_or(0);
            f.num_hmetrics = hr.u16(34).unwrap_or(0);
        }
        if f.is_cff {
            // A CFF font's glyph count comes from its charstrings if maxp is damaged.
            if f.num_glyphs == 0 {
                if let Some(c) = crate::cff::Cff::parse(f.table(b"CFF ").unwrap_or_default()) {
                    f.num_glyphs = c.num_glyphs() as u16;
                }
            }
        } else if f.table(b"glyf").is_none() && f.table(b"CFF2").is_none() {
            return Err(Error::corrupt("font has no outlines"));
        }
        Ok(f)
    }

    pub fn table(&self, tag: &[u8; 4]) -> Option<&[u8]> {
        let t = self.tables.iter().find(|t| &t.tag == tag)?;
        self.data.get(t.offset..t.offset.checked_add(t.length)?)
    }

    pub fn has_table(&self, tag: &[u8; 4]) -> bool {
        self.tables.iter().any(|t| &t.tag == tag)
    }

    /// Horizontal advance of a glyph in font units.
    pub fn advance(&self, gid: u16) -> u16 {
        let Some(h) = self.table(b"hmtx") else {
            return self.units_per_em / 2;
        };
        let r = R::new(h);
        if self.num_hmetrics == 0 {
            return self.units_per_em / 2;
        }
        let i = gid.min(self.num_hmetrics - 1) as usize;
        r.u16(i * 4).unwrap_or(0)
    }

    pub fn left_side_bearing(&self, gid: u16) -> i16 {
        let Some(h) = self.table(b"hmtx") else {
            return 0;
        };
        let r = R::new(h);
        let nh = self.num_hmetrics as usize;
        let g = gid as usize;
        if g < nh {
            r.i16(g * 4 + 2).unwrap_or(0)
        } else {
            r.i16(nh * 4 + (g - nh) * 2).unwrap_or(0)
        }
    }

    /// Name table string (`id` 1 family, 2 subfamily, 4 full name, 6 PostScript name).
    pub fn name(&self, id: u16) -> Option<String> {
        let t = self.table(b"name")?;
        let r = R::new(t);
        let count = r.u16(2)? as usize;
        let storage = r.u16(4)? as usize;
        let mut best: Option<(u8, String)> = None;
        for i in 0..count.min(2000) {
            let e = 6 + i * 12;
            let (pid, eid, lang, nid, len, off) = (
                r.u16(e)?,
                r.u16(e + 2)?,
                r.u16(e + 4)?,
                r.u16(e + 6)?,
                r.u16(e + 8)? as usize,
                r.u16(e + 10)? as usize,
            );
            if nid != id {
                continue;
            }
            let raw = r.slice(storage + off, len)?;
            let (rank, s) = match (pid, eid) {
                (3, 1) | (3, 10) | (0, _) => {
                    let u: Vec<u16> = raw
                        .chunks_exact(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect();
                    (if pid == 3 && lang == 0x409 { 3 } else { 2 }, String::from_utf16_lossy(&u))
                }
                (1, 0) => (1, raw.iter().map(|b| char::from(*b)).collect()),
                _ => continue,
            };
            if best.as_ref().is_none_or(|(r0, _)| rank > *r0) {
                best = Some((rank, s));
            }
        }
        best.map(|(_, s)| s)
    }

    pub fn is_bold(&self) -> bool {
        self.table(b"OS/2")
            .and_then(|t| R::new(t).u16(4))
            .is_some_and(|w| w >= 600)
    }
    pub fn is_italic(&self) -> bool {
        self.table(b"OS/2")
            .and_then(|t| R::new(t).u16(62))
            .is_some_and(|s| s & 1 != 0)
    }
    pub fn is_monospace(&self) -> bool {
        self.table(b"post")
            .and_then(|t| R::new(t).u32(12))
            .is_some_and(|v| v != 0)
    }
    pub fn weight(&self) -> u16 {
        self.table(b"OS/2")
            .and_then(|t| R::new(t).u16(4))
            .unwrap_or(400)
    }

    /// Glyph id for a Unicode scalar through the best Unicode cmap subtable.
    pub fn glyph_for_char(&self, c: char) -> Option<u16> {
        let t = self.table(b"cmap")?;
        let r = R::new(t);
        let n = r.u16(2)? as usize;
        // Prefer full Unicode (3,10 / 0,4+), then BMP Unicode (3,1 / 0,*), then symbol (3,0).
        let mut best: Option<(u8, usize)> = None;
        for i in 0..n.min(64) {
            let (pid, eid, off) = (r.u16(4 + i * 8)?, r.u16(6 + i * 8)?, r.u32(8 + i * 8)? as usize);
            let rank = match (pid, eid) {
                (3, 10) | (0, 4) | (0, 6) => 4,
                (3, 1) | (0, 3) | (0, 2) | (0, 1) | (0, 0) => 3,
                (3, 0) => 2,
                _ => 0,
            };
            if rank > 0 && best.is_none_or(|(b, _)| rank > b) {
                best = Some((rank, off));
            }
        }
        let (rank, off) = best?;
        let sub = t.get(off..)?;
        let g = cmap_lookup(sub, c as u32).or_else(|| {
            // Symbol fonts keep their characters at 0xF000 + code.
            (rank == 2 && (c as u32) < 0x100)
                .then(|| cmap_lookup(sub, 0xF000 + c as u32))
                .flatten()
        })?;
        (g != 0).then_some(g)
    }

    /// Glyph id for a code in a specific (platform, encoding) cmap subtable.
    pub fn glyph_for_code(&self, platform: u16, encoding: u16, code: u32) -> Option<u16> {
        let t = self.table(b"cmap")?;
        let r = R::new(t);
        let n = r.u16(2)? as usize;
        for i in 0..n.min(64) {
            if r.u16(4 + i * 8)? == platform && r.u16(6 + i * 8)? == encoding {
                let off = r.u32(8 + i * 8)? as usize;
                return cmap_lookup(t.get(off..)?, code).filter(|g| *g != 0);
            }
        }
        None
    }

    pub fn has_cmap(&self, platform: u16, encoding: u16) -> bool {
        let Some(t) = self.table(b"cmap") else {
            return false;
        };
        let r = R::new(t);
        let n = r.u16(2).unwrap_or(0) as usize;
        (0..n.min(64)).any(|i| {
            r.u16(4 + i * 8) == Some(platform) && r.u16(6 + i * 8) == Some(encoding)
        })
    }

    /// Every (code, glyph) pair of the best Unicode map, for subsetting and listings.
    pub fn unicode_map(&self) -> Vec<(u32, u16)> {
        let Some(t) = self.table(b"cmap") else {
            return Vec::new();
        };
        let r = R::new(t);
        let n = r.u16(2).unwrap_or(0) as usize;
        let mut best: Option<(u8, usize)> = None;
        for i in 0..n.min(64) {
            let (Some(pid), Some(eid), Some(off)) =
                (r.u16(4 + i * 8), r.u16(6 + i * 8), r.u32(8 + i * 8))
            else {
                continue;
            };
            let rank = match (pid, eid) {
                (3, 10) | (0, 4) | (0, 6) => 4,
                (3, 1) | (0, 3) | (0, 2) | (0, 1) | (0, 0) => 3,
                _ => 0,
            };
            if rank > 0 && best.is_none_or(|(b, _)| rank > b) {
                best = Some((rank, off as usize));
            }
        }
        let mut out = Vec::new();
        if let Some((_, off)) = best {
            if let Some(sub) = t.get(off..) {
                cmap_enumerate(sub, &mut out);
            }
        }
        out
    }

    pub fn glyph_name(&self, gid: u16) -> Option<String> {
        if let Some(c) = self.cff_table() {
            return c.glyph_name(gid as usize);
        }
        crate::post::glyph_name(self.table(b"post")?, gid, self.num_glyphs)
    }

    pub fn cff_table(&self) -> Option<crate::cff::Cff<'_>> {
        crate::cff::Cff::parse(self.table(b"CFF ")?)
    }
}

pub fn cmap_lookup(sub: &[u8], code: u32) -> Option<u16> {
    let r = R::new(sub);
    match r.u16(0)? {
        0 => (code < 256).then(|| r.u8(6 + code as usize).map(u16::from)).flatten(),
        4 => {
            if code > 0xFFFF {
                return None;
            }
            let segx2 = r.u16(6)? as usize;
            let segs = segx2 / 2;
            let (end, start, delta, range) = (14, 16 + segx2, 16 + segx2 * 2, 16 + segx2 * 3);
            // Binary search over the end codes.
            let (mut lo, mut hi) = (0usize, segs);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if (r.u16(end + mid * 2)? as u32) < code {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            if lo >= segs {
                return None;
            }
            let s = r.u16(start + lo * 2)? as u32;
            if code < s {
                return None;
            }
            let d = r.u16(delta + lo * 2)?;
            let ro = r.u16(range + lo * 2)? as usize;
            if ro == 0 {
                Some((code as u16).wrapping_add(d))
            } else {
                let p = range + lo * 2 + ro + (code - s) as usize * 2;
                let g = r.u16(p)?;
                (g != 0).then(|| g.wrapping_add(d))
            }
        }
        6 => {
            let first = r.u16(6)? as u32;
            let count = r.u16(8)? as u32;
            (code >= first && code < first + count)
                .then(|| r.u16(10 + (code - first) as usize * 2))
                .flatten()
        }
        12 | 13 => {
            let n = r.u32(12)? as usize;
            let (mut lo, mut hi) = (0usize, n.min(1 << 20));
            while lo < hi {
                let mid = (lo + hi) / 2;
                let e = 16 + mid * 12;
                if r.u32(e + 4)? < code {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            let e = 16 + lo * 12;
            let (s, en, g) = (r.u32(e)?, r.u32(e + 4)?, r.u32(e + 8)?);
            (code >= s && code <= en).then(|| {
                if r.u16(0) == Some(13) {
                    g as u16
                } else {
                    (g + (code - s)) as u16
                }
            })
        }
        _ => None,
    }
}

fn cmap_enumerate(sub: &[u8], out: &mut Vec<(u32, u16)>) {
    let r = R::new(sub);
    let Some(fmt) = r.u16(0) else { return };
    match fmt {
        0 => {
            for c in 0..256u32 {
                if let Some(g) = r.u8(6 + c as usize) {
                    if g != 0 {
                        out.push((c, u16::from(g)));
                    }
                }
            }
        }
        4 => {
            let Some(segx2) = r.u16(6) else { return };
            let segx2 = segx2 as usize;
            for s in 0..segx2 / 2 {
                let (Some(end), Some(start)) =
                    (r.u16(14 + s * 2), r.u16(16 + segx2 + s * 2))
                else {
                    break;
                };
                if start > end || end == 0xFFFF && start == 0xFFFF {
                    continue;
                }
                for c in u32::from(start)..=u32::from(end) {
                    if let Some(g) = cmap_lookup(sub, c) {
                        if g != 0 {
                            out.push((c, g));
                        }
                    }
                    if out.len() > 70_000 {
                        return;
                    }
                }
            }
        }
        6 | 12 | 13 => {
            if fmt == 6 {
                let (Some(first), Some(count)) = (r.u16(6), r.u16(8)) else {
                    return;
                };
                for i in 0..u32::from(count) {
                    if let Some(g) = r.u16(10 + i as usize * 2) {
                        if g != 0 {
                            out.push((u32::from(first) + i, g));
                        }
                    }
                }
            } else {
                let Some(n) = r.u32(12) else { return };
                for i in 0..(n as usize).min(1 << 16) {
                    let e = 16 + i * 12;
                    let (Some(s), Some(en), Some(g)) = (r.u32(e), r.u32(e + 4), r.u32(e + 8))
                    else {
                        break;
                    };
                    for c in s..=en.min(s.saturating_add(0x20000)) {
                        let gid = if fmt == 13 { g } else { g + (c - s) };
                        if gid != 0 {
                            out.push((c, gid as u16));
                        }
                        if out.len() > 200_000 {
                            return;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}
