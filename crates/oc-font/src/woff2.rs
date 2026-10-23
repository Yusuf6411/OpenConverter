//! Reading WOFF 2.0 (W3C): Brotli-decompress the table data and undo the glyf, loca and hmtx
//! transforms, giving a plain sfnt.
#![allow(clippy::indexing_slicing)] // every index is guarded by the stream-length checks in `Cur`
use crate::build::{build_sfnt, WOFF2_KNOWN};
use crate::sfnt::Tag;
use oc_core::limits::Limits;
use oc_core::progress::NoProgress;
use oc_core::{Error, Result};

fn bad(m: &'static str) -> Error {
    Error::corrupt(m)
}

/// A forward cursor over a byte slice; every read is bounds-checked.
struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, p: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self
            .d
            .get(
                self.p
                    ..self
                        .p
                        .checked_add(n)
                        .ok_or_else(|| bad("woff2: overflow"))?,
            )
            .ok_or_else(|| Error::truncated("woff2: stream ends early"))?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn base128(&mut self) -> Result<u32> {
        let mut acc = 0u32;
        for i in 0..5 {
            let b = self.u8()?;
            if i == 0 && b == 0x80 {
                return Err(bad("woff2: UIntBase128 has a leading zero"));
            }
            if acc & 0xFE00_0000 != 0 {
                return Err(bad("woff2: UIntBase128 overflows"));
            }
            acc = (acc << 7) | u32::from(b & 0x7F);
            if b & 0x80 == 0 {
                return Ok(acc);
            }
        }
        Err(bad("woff2: UIntBase128 is longer than 5 bytes"))
    }
    fn u255(&mut self) -> Result<u16> {
        match self.u8()? {
            253 => self.u16(),
            255 => Ok(u16::from(self.u8()?) + 253),
            254 => Ok(u16::from(self.u8()?) + 506),
            c => Ok(u16::from(c)),
        }
    }
}

struct Entry {
    tag: Tag,
    xform: u8,
    orig: u32,
    transformed: Option<u32>,
}

fn with_sign(flag: u8, v: i32) -> i32 {
    if flag & 1 != 0 {
        v
    } else {
        -v
    }
}

/// One (dx, dy) coordinate pair for a flag byte (without the on-curve bit), WOFF2 section 5.2.
fn triplet(flag: u8, g: &mut Cur) -> Result<(i32, i32)> {
    let f = i32::from(flag);
    Ok(if flag < 10 {
        let b0 = i32::from(g.u8()?);
        (0, with_sign(flag, ((f >> 1) << 8) + b0))
    } else if flag < 20 {
        let b0 = i32::from(g.u8()?);
        (with_sign(flag, (((f - 10) >> 1) << 8) + b0), 0)
    } else if flag < 84 {
        let k = f - 20;
        let b = i32::from(g.u8()?);
        let dx = 1 + (k / 16) * 16 + (b >> 4);
        let dy = 1 + ((k % 16) / 4) * 16 + (b & 15);
        (with_sign(flag, dx), with_sign(flag >> 1, dy))
    } else if flag < 120 {
        let k = f - 84;
        let (b0, b1) = (i32::from(g.u8()?), i32::from(g.u8()?));
        let dx = 1 + (k / 12) * 256 + b0;
        let dy = 1 + ((k % 12) / 4) * 256 + b1;
        (with_sign(flag, dx), with_sign(flag >> 1, dy))
    } else if flag < 124 {
        let (b0, b1, b2) = (i32::from(g.u8()?), i32::from(g.u8()?), i32::from(g.u8()?));
        (
            with_sign(flag, (b0 << 4) | (b1 >> 4)),
            with_sign(flag >> 1, ((b1 & 15) << 8) | b2),
        )
    } else {
        let (b0, b1, b2, b3) = (
            i32::from(g.u8()?),
            i32::from(g.u8()?),
            i32::from(g.u8()?),
            i32::from(g.u8()?),
        );
        (
            with_sign(flag, (b0 << 8) | b1),
            with_sign(flag >> 1, (b2 << 8) | b3),
        )
    })
}

struct Rebuilt {
    glyf: Vec<u8>,
    loca: Vec<u8>,
    x_min: Vec<i16>,
}

fn push_i16(o: &mut Vec<u8>, v: i16) {
    o.extend_from_slice(&v.to_be_bytes());
}

/// Undoes the glyf transform (section 5.1), producing glyf, loca and each glyph's xMin.
fn rebuild_glyf(t: &[u8], limits: &Limits) -> Result<Rebuilt> {
    let mut h = Cur::new(t);
    if h.u16()? != 0 {
        return Err(bad("woff2: glyf transform reserved field is not zero"));
    }
    let option_flags = h.u16()?;
    let n = usize::from(h.u16()?);
    let index_format = h.u16()?;
    if index_format > 1 {
        return Err(bad("woff2: bad loca index format"));
    }
    let mut sizes = [0usize; 7];
    for s in &mut sizes {
        *s = h.u32()? as usize;
    }
    let [n_contour_sz, n_points_sz, flag_sz, glyph_sz, comp_sz, bbox_sz, instr_sz] = sizes;
    let mut n_contour = Cur::new(h.take(n_contour_sz)?);
    let mut n_points = Cur::new(h.take(n_points_sz)?);
    let mut flags = Cur::new(h.take(flag_sz)?);
    let mut glyphs = Cur::new(h.take(glyph_sz)?);
    let mut comp = Cur::new(h.take(comp_sz)?);
    let bbox_all = h.take(bbox_sz)?;
    let bitmap_len = 4 * n.div_ceil(32);
    if bbox_all.len() < bitmap_len {
        return Err(bad("woff2: bbox stream is shorter than its bitmap"));
    }
    let (bitmap, bbox_vals) = bbox_all.split_at(bitmap_len);
    let mut bbox = Cur::new(bbox_vals);
    let mut instr = Cur::new(h.take(instr_sz)?);
    let overlap = if option_flags & 1 != 0 {
        h.take(n.div_ceil(8))?
    } else {
        &[]
    };
    let mut glyf: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut x_min = Vec::with_capacity(n);
    for gi in 0..n {
        limits.check_alloc(glyf.len() as u64)?;
        offsets.push(glyf.len() as u32);
        let nc = n_contour.i16()?;
        let has_bbox = bitmap[gi / 8] & (0x80 >> (gi % 8)) != 0;
        if nc == 0 {
            if has_bbox {
                return Err(bad("woff2: empty glyph has a bounding box"));
            }
            x_min.push(0);
            continue;
        }
        if nc > 0 {
            let nc = nc as usize;
            let mut ends = Vec::with_capacity(nc);
            let mut total = 0usize;
            for _ in 0..nc {
                total += usize::from(n_points.u255()?);
                ends.push(
                    total
                        .checked_sub(1)
                        .ok_or_else(|| bad("woff2: contour has no points"))?,
                );
            }
            if total > 1 << 20 {
                return Err(bad("woff2: glyph has too many points"));
            }
            let fl = flags.take(total)?;
            let mut pts: Vec<(i32, i32, bool)> = Vec::with_capacity(total);
            let (mut x, mut y) = (0i32, 0i32);
            for &f in fl {
                let (dx, dy) = triplet(f & 0x7F, &mut glyphs)?;
                x += dx;
                y += dy;
                pts.push((x, y, f & 0x80 == 0));
            }
            let ilen = usize::from(glyphs.u255()?);
            let ins = instr.take(ilen)?;
            let bb = if has_bbox {
                [bbox.i16()?, bbox.i16()?, bbox.i16()?, bbox.i16()?]
            } else {
                let xs = pts.iter().map(|p| p.0);
                let ys = pts.iter().map(|p| p.1);
                let c = |v: i32| v.clamp(-32768, 32767) as i16;
                [
                    c(xs.clone().min().unwrap_or(0)),
                    c(ys.clone().min().unwrap_or(0)),
                    c(xs.max().unwrap_or(0)),
                    c(ys.max().unwrap_or(0)),
                ]
            };
            push_i16(&mut glyf, nc as i16);
            for v in bb {
                push_i16(&mut glyf, v);
            }
            for &e in &ends {
                glyf.extend_from_slice(&(e as u16).to_be_bytes());
            }
            glyf.extend_from_slice(&(ilen as u16).to_be_bytes());
            glyf.extend_from_slice(ins);
            // Flags with repeat runs, then x and y deltas in the shortest TrueType form.
            let ov = overlap
                .get(gi / 8)
                .is_some_and(|b| b & (0x80 >> (gi % 8)) != 0);
            let mut fv: Vec<u8> = Vec::with_capacity(pts.len());
            let (mut xo, mut yo) = (Vec::new(), Vec::new());
            let (mut px, mut py) = (0i32, 0i32);
            for (i, &(x, y, on)) in pts.iter().enumerate() {
                let mut f = u8::from(on);
                if i == 0 && ov {
                    f |= 0x40;
                }
                let (dx, dy) = (x - px, y - py);
                (px, py) = (x, y);
                match dx {
                    0 => f |= 0x10,
                    -255..=255 => {
                        f |= 0x02 | if dx > 0 { 0x10 } else { 0 };
                        xo.push(dx.unsigned_abs() as u8);
                    }
                    _ => xo.extend_from_slice(&(dx as i16).to_be_bytes()),
                }
                match dy {
                    0 => f |= 0x20,
                    -255..=255 => {
                        f |= 0x04 | if dy > 0 { 0x20 } else { 0 };
                        yo.push(dy.unsigned_abs() as u8);
                    }
                    _ => yo.extend_from_slice(&(dy as i16).to_be_bytes()),
                }
                fv.push(f);
            }
            let mut fl_out: Vec<u8> = Vec::new();
            let mut i = 0;
            while i < fv.len() {
                let mut run = 1;
                while i + run < fv.len() && fv[i + run] == fv[i] && run < 256 {
                    run += 1;
                }
                if run >= 2 {
                    fl_out.push(fv[i] | 0x08);
                    fl_out.push((run - 1) as u8);
                } else {
                    fl_out.push(fv[i]);
                }
                i += run;
            }
            glyf.extend_from_slice(&fl_out);
            glyf.extend_from_slice(&xo);
            glyf.extend_from_slice(&yo);
            x_min.push(bb[0]);
        } else if nc == -1 {
            if !has_bbox {
                return Err(bad("woff2: composite glyph has no bounding box"));
            }
            let bb = [bbox.i16()?, bbox.i16()?, bbox.i16()?, bbox.i16()?];
            push_i16(&mut glyf, -1);
            for v in bb {
                push_i16(&mut glyf, v);
            }
            let mut have_instr = false;
            loop {
                let fl = comp.u16()?;
                glyf.extend_from_slice(&fl.to_be_bytes());
                let mut len = 2 + if fl & 1 != 0 { 4 } else { 2 };
                if fl & 0x08 != 0 {
                    len += 2;
                } else if fl & 0x40 != 0 {
                    len += 4;
                } else if fl & 0x80 != 0 {
                    len += 8;
                }
                glyf.extend_from_slice(comp.take(len)?);
                have_instr |= fl & 0x100 != 0;
                if fl & 0x20 == 0 {
                    break;
                }
            }
            if have_instr {
                let ilen = glyphs.u255()?;
                glyf.extend_from_slice(&ilen.to_be_bytes());
                glyf.extend_from_slice(instr.take(usize::from(ilen))?);
            }
            x_min.push(bb[0]);
        } else {
            return Err(bad("woff2: bad contour count"));
        }
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    offsets.push(glyf.len() as u32);
    let mut loca = Vec::new();
    for &o in &offsets {
        if index_format == 0 {
            if o / 2 > 0xFFFF {
                return Err(bad("woff2: glyf is too large for short loca offsets"));
            }
            loca.extend_from_slice(&((o / 2) as u16).to_be_bytes());
        } else {
            loca.extend_from_slice(&o.to_be_bytes());
        }
    }
    Ok(Rebuilt { glyf, loca, x_min })
}

/// Undoes the hmtx transform (section 5.4).
fn rebuild_hmtx(t: &[u8], n_hm: usize, n_glyphs: usize, x_min: &[i16]) -> Result<Vec<u8>> {
    let mut c = Cur::new(t);
    let flags = c.u8()?;
    if flags & 3 == 0 || flags & !3 != 0 {
        return Err(bad("woff2: invalid hmtx transform flags"));
    }
    if n_hm == 0 || n_hm > n_glyphs {
        return Err(bad("woff2: hhea and maxp disagree about the glyph count"));
    }
    let mut adv = Vec::with_capacity(n_hm);
    for _ in 0..n_hm {
        adv.push(c.u16()?);
    }
    let mut lsb = Vec::with_capacity(n_glyphs);
    for i in 0..n_hm {
        lsb.push(if flags & 1 != 0 {
            x_min.get(i).copied().unwrap_or(0)
        } else {
            c.i16()?
        });
    }
    for i in n_hm..n_glyphs {
        lsb.push(if flags & 2 != 0 {
            x_min.get(i).copied().unwrap_or(0)
        } else {
            c.i16()?
        });
    }
    let mut out = Vec::with_capacity(n_hm * 4 + (n_glyphs - n_hm) * 2);
    for i in 0..n_glyphs {
        if i < n_hm {
            out.extend_from_slice(&adv[i].to_be_bytes());
        }
        push_i16(&mut out, lsb[i]);
    }
    Ok(out)
}

/// Unpack a WOFF 2.0 file into a plain sfnt. For a collection, `index` picks the font.
pub fn woff2_to_sfnt(data: &[u8], index: usize, limits: &Limits) -> Result<Vec<u8>> {
    let mut c = Cur::new(data);
    if c.take(4)? != b"wOF2" {
        return Err(bad("not a WOFF2 file"));
    }
    let flavor: [u8; 4] = c.take(4)?.try_into().map_err(|_| bad("woff2: flavor"))?;
    let _length = c.u32()?;
    let n_tables = usize::from(c.u16()?);
    c.take(2 + 4)?; // reserved, totalSfntSize
    let total_compressed = c.u32()? as usize;
    c.take(2 + 2 + 4 * 5)?; // versions, meta and private block fields
    let mut entries = Vec::with_capacity(n_tables);
    for _ in 0..n_tables {
        let fl = c.u8()?;
        let tag: Tag = if fl & 63 == 63 {
            c.take(4)?.try_into().map_err(|_| bad("woff2: tag"))?
        } else {
            *WOFF2_KNOWN[usize::from(fl & 63)]
        };
        let xform = fl >> 6;
        let orig = c.base128()?;
        let is_gl = &tag == b"glyf" || &tag == b"loca";
        let transformed = if (is_gl && xform == 0) || (!is_gl && xform != 0) {
            if !is_gl && !(&tag == b"hmtx" && xform == 1) {
                return Err(Error::new(
                    oc_core::ErrorCode::UnsupportedVariant,
                    "woff2: unknown table transform",
                ));
            }
            Some(c.base128()?)
        } else {
            None
        };
        if is_gl && xform == 1 || is_gl && xform == 2 {
            return Err(Error::new(
                oc_core::ErrorCode::UnsupportedVariant,
                "woff2: unknown glyf transform",
            ));
        }
        entries.push(Entry {
            tag,
            xform,
            orig,
            transformed,
        });
    }
    // Collection directory.
    let mut members: Vec<Vec<usize>> = Vec::new();
    let mut flavors: Vec<[u8; 4]> = Vec::new();
    if &flavor == b"ttcf" {
        let _version = c.u32()?;
        let n_fonts = usize::from(c.u255()?);
        for _ in 0..n_fonts {
            let nt = usize::from(c.u255()?);
            let fv: [u8; 4] = c.take(4)?.try_into().map_err(|_| bad("woff2: flavor"))?;
            let mut idx = Vec::with_capacity(nt);
            for _ in 0..nt {
                let i = usize::from(c.u255()?);
                if i >= entries.len() {
                    return Err(bad("woff2: collection names a missing table"));
                }
                idx.push(i);
            }
            members.push(idx);
            flavors.push(fv);
        }
        if index >= members.len() {
            return Err(bad("woff2: font number is outside the collection"));
        }
    } else {
        members.push((0..entries.len()).collect());
        flavors.push(flavor);
    }
    let comp = c.take(total_compressed)?;
    let blob = oc_compress::brotli::decompress(comp, limits, &NoProgress)?;
    let mut pos = 0usize;
    let mut raw: Vec<&[u8]> = Vec::with_capacity(entries.len());
    for e in &entries {
        let len = e.transformed.unwrap_or(e.orig) as usize;
        let s = blob
            .get(pos..pos.checked_add(len).ok_or_else(|| bad("woff2: overflow"))?)
            .ok_or_else(|| Error::truncated("woff2: table data ends early"))?;
        raw.push(s);
        pos += len;
    }
    if pos != blob.len() {
        return Err(bad("woff2: extra data after the last table"));
    }
    // Tables of the chosen font, with glyf/loca/hmtx rebuilt.
    let chosen = members.get(index).ok_or_else(|| bad("woff2: no font"))?;
    let by_tag = |t: &[u8; 4]| chosen.iter().copied().find(|&i| &entries[i].tag == t);
    let mut rebuilt: Option<Rebuilt> = None;
    if let Some(g) = by_tag(b"glyf") {
        if entries[g].xform == 0 {
            rebuilt = Some(rebuild_glyf(raw[g], limits)?);
        }
    }
    let mut tables: Vec<(Tag, Vec<u8>)> = Vec::new();
    for &i in chosen {
        let e = &entries[i];
        let body = match (&e.tag, e.xform) {
            (b"glyf", 0) => rebuilt.as_ref().map(|r| r.glyf.clone()).unwrap_or_default(),
            (b"loca", 0) => rebuilt.as_ref().map(|r| r.loca.clone()).unwrap_or_default(),
            (b"hmtx", 1) => {
                let hhea = by_tag(b"hhea")
                    .map(|j| raw[j])
                    .ok_or_else(|| bad("woff2: hmtx without hhea"))?;
                let maxp = by_tag(b"maxp")
                    .map(|j| raw[j])
                    .ok_or_else(|| bad("woff2: hmtx without maxp"))?;
                let n_hm = hhea
                    .get(34..36)
                    .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
                    .ok_or_else(|| bad("woff2: short hhea"))?;
                let n_gl = maxp
                    .get(4..6)
                    .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
                    .ok_or_else(|| bad("woff2: short maxp"))?;
                let xs = match &rebuilt {
                    Some(r) => r.x_min.clone(),
                    None => plain_x_min(
                        &tables,
                        by_tag(b"glyf").map(|j| raw[j]),
                        by_tag(b"loca").map(|j| raw[j]),
                        n_gl,
                    ),
                };
                rebuild_hmtx(raw[i], n_hm, n_gl, &xs)?
            }
            _ => raw[i].to_vec(),
        };
        tables.push((e.tag, body));
    }
    let fv = flavors.get(index).copied().unwrap_or(flavor);
    Ok(build_sfnt(fv, &mut tables))
}

/// xMin of each glyph from an untransformed glyf/loca pair (used when only hmtx is transformed).
fn plain_x_min(
    _t: &[(Tag, Vec<u8>)],
    glyf: Option<&[u8]>,
    loca: Option<&[u8]>,
    n: usize,
) -> Vec<i16> {
    let (Some(g), Some(l)) = (glyf, loca) else {
        return vec![0; n];
    };
    let long = l.len() >= (n + 1) * 4;
    let off = |i: usize| -> usize {
        if long {
            l.get(i * 4..i * 4 + 4)
                .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        } else {
            l.get(i * 2..i * 2 + 2)
                .map_or(0, |b| usize::from(u16::from_be_bytes([b[0], b[1]])) * 2)
        }
    };
    (0..n)
        .map(|i| {
            if off(i + 1) <= off(i) {
                0
            } else {
                g.get(off(i) + 2..off(i) + 4)
                    .map_or(0, |b| i16::from_be_bytes([b[0], b[1]]))
            }
        })
        .collect()
}
