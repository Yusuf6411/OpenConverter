//! JBIG2 decoder (ITU-T T.88 / ISO/IEC 14492), written for OpenConverter.
//!
//! Decodes embedded streams (as carried by PDF's `JBIG2Decode` filter, with optional global
//! segments) and standalone `.jb2` files, sequential or random access: generic regions,
//! generic refinement regions, symbol dictionaries, text regions, pattern dictionaries and
//! halftone regions, arithmetic or Huffman coded. MMR (T.6) data goes through a callback so
//! this crate needs no fax tables of its own.
#![allow(
    dead_code,
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

mod arith;
mod bitmap;
mod huffman;
mod symbol;

use arith::{Cx, Mq};
use bitmap::{decode_generic, decode_refinement, generic_contexts, refine_contexts, Bm};
use huffman::{corrupt, standard, Bits, Table};
use oc_core::{Error, ErrorCode, Limits, Result};
use std::collections::HashMap;
use std::rc::Rc;
use symbol::{
    ceil_log2, decode_symbol_dict, decode_text, SdParams, Src, TextCtx, TextParams, TextTables,
};

/// Decodes MMR (T.6) data to packed rows (1 = black); returns the rows and the bytes used.
pub type MmrFn = fn(&[u8], usize, usize) -> (Vec<u8>, usize);

pub struct Env {
    pub mmr: Option<MmrFn>,
    pub lim: Limits,
    /// Pixels allocated so far, over all bitmaps of one decode.
    spent: std::cell::Cell<u64>,
}

impl Env {
    pub fn new(mmr: Option<MmrFn>, lim: Limits) -> Env {
        Env {
            mmr,
            lim,
            spent: std::cell::Cell::new(0),
        }
    }

    /// Refuses a bitmap over the pixel limit, and a decode whose bitmaps add up to more than
    /// four times that limit.
    pub fn check_bitmap(&self, w: usize, h: usize) -> Result<()> {
        let w32 = u32::try_from(w).map_err(|_| corrupt("bitmap too wide"))?;
        let h32 = u32::try_from(h).map_err(|_| corrupt("bitmap too tall"))?;
        let px = self.lim.check_pixels(w32, h32)?;
        let total = self.spent.get().saturating_add(px);
        self.spent.set(total);
        if total > self.lim.max_pixels.saturating_mul(4) {
            return Err(Error::limit(
                "max_pixels",
                "JBIG2: bitmaps add up to too much",
            ));
        }
        Ok(())
    }

    fn mmr_fn(&self) -> Result<MmrFn> {
        self.mmr.ok_or_else(|| {
            Error::new(
                ErrorCode::CodecNotInBuild,
                "JBIG2: MMR decoding is not available here",
            )
        })
    }
}

/// A decoded page: packed rows, most significant bit first, 1 = black.
pub struct Page {
    pub width: usize,
    pub height: usize,
    pub bits: Vec<u8>,
}

/// True for the file header of a standalone JBIG2 file.
pub fn sniff(d: &[u8]) -> bool {
    d.starts_with(&[0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A])
}

struct Seg {
    num: u32,
    typ: u8,
    refs: Vec<u32>,
    page: u32,
    len: Option<usize>,
}

fn be32(d: &[u8], o: usize) -> Result<u32> {
    let b = d
        .get(o..o + 4)
        .ok_or_else(|| Error::new(ErrorCode::TruncatedFile, "JBIG2: data ends early"))?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be16(d: &[u8], o: usize) -> Result<u32> {
    let b = d
        .get(o..o + 2)
        .ok_or_else(|| Error::new(ErrorCode::TruncatedFile, "JBIG2: data ends early"))?;
    Ok(u32::from(u16::from_be_bytes([b[0], b[1]])))
}

fn byte(d: &[u8], o: usize) -> Result<u8> {
    d.get(o)
        .copied()
        .ok_or_else(|| Error::new(ErrorCode::TruncatedFile, "JBIG2: data ends early"))
}

fn parse_header(d: &[u8], pos: &mut usize) -> Result<Seg> {
    let num = be32(d, *pos)?;
    let flags = byte(d, *pos + 4)?;
    *pos += 5;
    let rt = byte(d, *pos)?;
    let mut count = u32::from(rt >> 5);
    if count == 7 {
        count = be32(d, *pos)? & 0x1FFF_FFFF;
        if count as usize > d.len() {
            return Err(corrupt("too many referred-to segments"));
        }
        *pos += 4 + (count as usize + 8) / 8;
    } else {
        *pos += 1;
    }
    let rs = if num <= 256 {
        1
    } else if num <= 65536 {
        2
    } else {
        4
    };
    let mut refs = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let v = match rs {
            1 => u32::from(byte(d, *pos)?),
            2 => be16(d, *pos)?,
            _ => be32(d, *pos)?,
        };
        refs.push(v);
        *pos += rs;
    }
    let page = if flags & 0x40 != 0 {
        let v = be32(d, *pos)?;
        *pos += 4;
        v
    } else {
        let v = u32::from(byte(d, *pos)?);
        *pos += 1;
        v
    };
    let len = be32(d, *pos)?;
    *pos += 4;
    Ok(Seg {
        num,
        typ: flags & 0x3F,
        refs,
        page,
        len: (len != 0xFFFF_FFFF).then_some(len as usize),
    })
}

#[derive(Clone, Copy)]
struct Region {
    w: usize,
    h: usize,
    x: i32,
    y: i32,
    op: u8,
}

fn parse_region(d: &[u8]) -> Result<Region> {
    let w = be32(d, 0)? as usize;
    let h = be32(d, 4)? as usize;
    let x = be32(d, 8)? as i32;
    let y = be32(d, 12)? as i32;
    let op = byte(d, 16)? & 7;
    Ok(Region { w, h, x, y, op })
}

fn at_pairs<const N: usize>(d: &[u8], pos: &mut usize) -> Result<[(i8, i8); N]> {
    let mut out = [(0i8, 0i8); N];
    for p in out.iter_mut() {
        *p = (byte(d, *pos)? as i8, byte(d, *pos + 1)? as i8);
        *pos += 2;
    }
    Ok(out)
}

struct SdStore {
    syms: Vec<Rc<Bm>>,
    gb: Option<Vec<Cx>>,
    gr: Option<Vec<Cx>>,
}

enum Stored {
    Syms(Rc<SdStore>),
    Pats(Rc<Vec<Bm>>),
    Table(Rc<Table>),
    Region(Rc<Bm>),
}

struct PageState {
    bm: Bm,
    unknown_h: bool,
    default: u8,
}

struct Dec<'e> {
    env: &'e Env,
    store: HashMap<u32, Stored>,
    page: Option<PageState>,
    page_no: Option<u32>,
    done: bool,
}

impl<'e> Dec<'e> {
    fn new(env: &'e Env) -> Dec<'e> {
        Dec {
            env,
            store: HashMap::new(),
            page: None,
            page_no: None,
            done: false,
        }
    }

    fn compose(&mut self, bm: &Bm, r: &Region) -> Result<()> {
        if self.page.is_none() {
            // regions without page information: size the page to fit
            let (w, h) = (r.x.max(0) as usize + bm.w, r.y.max(0) as usize + bm.h);
            self.env.check_bitmap(w, h)?;
            self.page = Some(PageState {
                bm: Bm::new(w, h, 0),
                unknown_h: false,
                default: 0,
            });
        }
        let Some(p) = self.page.as_mut() else {
            return Ok(());
        };
        if p.unknown_h {
            let need = (r.y.max(0) as usize).saturating_add(bm.h);
            if need > p.bm.h {
                self.env.check_bitmap(p.bm.w, need)?;
                p.bm.d.resize(p.bm.w * need, p.default);
                p.bm.h = need;
            }
        }
        p.bm.combine(bm, r.x, r.y, r.op);
        Ok(())
    }

    fn finish_region(&mut self, seg: &Seg, bm: Bm, r: &Region, intermediate: bool) -> Result<()> {
        if intermediate {
            self.store.insert(seg.num, Stored::Region(Rc::new(bm)));
            Ok(())
        } else {
            self.compose(&bm, r)
        }
    }

    fn symbols_of(&self, seg: &Seg) -> Vec<Rc<Bm>> {
        let mut out = Vec::new();
        for r in &seg.refs {
            if let Some(Stored::Syms(s)) = self.store.get(r) {
                out.extend(s.syms.iter().cloned());
            }
        }
        out
    }

    fn tables_of(&self, seg: &Seg) -> Vec<Rc<Table>> {
        seg.refs
            .iter()
            .filter_map(|r| match self.store.get(r) {
                Some(Stored::Table(t)) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    fn process(&mut self, seg: &Seg, d: &[u8]) -> Result<()> {
        match seg.typ {
            0 => self.symbol_dict(seg, d),
            4 | 6 | 7 => self.text_region(seg, d),
            16 => self.pattern_dict(seg, d),
            20 | 22 | 23 => self.halftone(seg, d),
            36 | 38 | 39 => self.generic_region(seg, d),
            40 | 42 | 43 => self.refinement_region(seg, d),
            48 => {
                let w = be32(d, 0)? as usize;
                let h32 = be32(d, 4)?;
                let flags = byte(d, 16)?;
                let unknown = h32 == 0xFFFF_FFFF;
                let h = if unknown { 0 } else { h32 as usize };
                self.env.check_bitmap(w, h)?;
                let default = (flags >> 2) & 1;
                self.page = Some(PageState {
                    bm: Bm::new(w, h, default),
                    unknown_h: unknown,
                    default,
                });
                self.page_no.get_or_insert(seg.page);
                Ok(())
            }
            49 | 51 => {
                self.done = true;
                Ok(())
            }
            50 => {
                let y = be32(d, 0)? as usize;
                if let Some(p) = self.page.as_mut() {
                    if p.unknown_h && y + 1 > p.bm.h {
                        self.env.check_bitmap(p.bm.w, y + 1)?;
                        p.bm.d.resize(p.bm.w * (y + 1), p.default);
                        p.bm.h = y + 1;
                    }
                }
                Ok(())
            }
            53 => {
                self.store
                    .insert(seg.num, Stored::Table(Rc::new(Table::custom(d)?)));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn symbol_dict(&mut self, seg: &Seg, d: &[u8]) -> Result<()> {
        let f = be16(d, 0)?;
        let huff = f & 1 != 0;
        let refagg = f & 2 != 0;
        let (dh_sel, dw_sel) = ((f >> 2) & 3, (f >> 4) & 3);
        let (bm_sel, agg_sel) = ((f >> 6) & 1, (f >> 7) & 1);
        let ctx_used = f & 0x100 != 0;
        let ctx_kept = f & 0x200 != 0;
        let template = ((f >> 10) & 3) as u8;
        let rtemplate = ((f >> 12) & 1) as u8;
        let mut pos = 2;
        let mut at = [(3i8, -1i8), (-3, -1), (2, -2), (-2, -2)];
        if !huff {
            if template == 0 {
                at = at_pairs::<4>(d, &mut pos)?;
            } else {
                at[0] = at_pairs::<1>(d, &mut pos)?[0];
            }
        }
        let mut rat = [(-1i8, -1i8), (-1, -1)];
        if refagg && rtemplate == 0 {
            rat = at_pairs::<2>(d, &mut pos)?;
        }
        let num_ex = be32(d, pos)?;
        let num_new = be32(d, pos + 4)?;
        pos += 8;
        let insyms = self.symbols_of(seg);
        let mut user = self.tables_of(seg).into_iter();
        let mut pick = |sel: u32, std: &[usize]| -> Result<Table> {
            match std.get(sel as usize) {
                Some(&n) if sel != 3 => Ok(standard(n)),
                _ => user
                    .next()
                    .map(|t| (*t).clone())
                    .ok_or_else(|| corrupt("missing custom Huffman table")),
            }
        };
        let (dh, dw, bmsize, agginst) = if huff {
            (
                pick(dh_sel, &[4, 5])?,
                pick(dw_sel, &[2, 3])?,
                pick(bm_sel * 3, &[1])?,
                pick(agg_sel * 3, &[1])?,
            )
        } else {
            (standard(4), standard(2), standard(1), standard(1))
        };
        let retained = if ctx_used {
            seg.refs.iter().rev().find_map(|r| match self.store.get(r) {
                Some(Stored::Syms(s)) => Some(s.clone()),
                _ => None,
            })
        } else {
            None
        };
        let ret = retained
            .as_ref()
            .and_then(|s| Some((s.gb.as_deref()?, s.gr.as_deref()?)));
        let p = SdParams {
            huff,
            refagg,
            template,
            rtemplate,
            at,
            rat,
            num_ex,
            num_new,
            dh,
            dw,
            bmsize,
            agginst,
        };
        let out = decode_symbol_dict(d.get(pos..).unwrap_or_default(), &p, &insyms, ret, self.env)?;
        if out.exported.len() != num_ex as usize {
            // tolerate a wrong declared count; the flags are authoritative
        }
        self.store.insert(
            seg.num,
            Stored::Syms(Rc::new(SdStore {
                syms: out.exported,
                gb: ctx_kept.then_some(out.gb),
                gr: ctx_kept.then_some(out.gr),
            })),
        );
        Ok(())
    }

    fn text_region(&mut self, seg: &Seg, d: &[u8]) -> Result<()> {
        let r = parse_region(d)?;
        let f = be16(d, 17)?;
        let huff = f & 1 != 0;
        let refine = f & 2 != 0;
        let log_strips = (f >> 2) & 3;
        let corner = ((f >> 4) & 3) as u8;
        let transposed = f & 0x40 != 0;
        let comb_op = ((f >> 7) & 3) as u8;
        let def_pixel = ((f >> 9) & 1) as u8;
        let mut ds_off = ((f >> 10) & 0x1F) as i32;
        if ds_off > 15 {
            ds_off -= 32;
        }
        let rtemplate = ((f >> 15) & 1) as u8;
        let mut pos = 19;
        let hf = if huff {
            let v = be16(d, pos)?;
            pos += 2;
            v
        } else {
            0
        };
        let mut rat = [(-1i8, -1i8), (-1, -1)];
        if refine && rtemplate == 0 {
            rat = at_pairs::<2>(d, &mut pos)?;
        }
        let ninst = be32(d, pos)?;
        pos += 4;
        let syms = self.symbols_of(seg);
        let code_len = ceil_log2(syms.len());
        let p = TextParams {
            refine,
            w: r.w,
            h: r.h,
            ninst,
            log_strips,
            def_pixel,
            comb_op,
            transposed,
            corner,
            ds_off,
            rtemplate,
            rat,
        };
        self.env.check_bitmap(r.w, r.h)?;
        let mut ctx = TextCtx::new(code_len, rtemplate);
        let rest = d.get(pos..).unwrap_or_default();
        let bm = if huff {
            let mut user = self.tables_of(seg).into_iter();
            let mut pick = |sel: u32, std: &[usize]| -> Result<Table> {
                match std.get(sel as usize) {
                    Some(&n) if sel != 3 => Ok(standard(n)),
                    _ => user
                        .next()
                        .map(|t| (*t).clone())
                        .ok_or_else(|| corrupt("missing custom Huffman table")),
                }
            };
            let fs = pick(hf & 3, &[6, 7])?;
            let ds = pick((hf >> 2) & 3, &[8, 9, 10])?;
            let dt = pick((hf >> 4) & 3, &[11, 12, 13])?;
            let rdw = pick((hf >> 6) & 3, &[14, 15])?;
            let rdh = pick((hf >> 8) & 3, &[14, 15])?;
            let rdx = pick((hf >> 10) & 3, &[14, 15])?;
            let rdy = pick((hf >> 12) & 3, &[14, 15])?;
            let rsize = pick(((hf >> 14) & 1) * 3, &[1])?;
            let mut bits = Bits::new(rest);
            let mut run_lens = Vec::with_capacity(35);
            for _ in 0..35 {
                run_lens.push(bits.bits(4)?);
            }
            let run_table = Table::symbols(&run_lens)?;
            let mut lens: Vec<u32> = Vec::with_capacity(syms.len());
            while lens.len() < syms.len() {
                let c = run_table
                    .decode(&mut bits)?
                    .ok_or_else(|| corrupt("bad symbol code table"))?;
                match c {
                    0..=31 => lens.push(c as u32),
                    32 => {
                        let n = 3 + bits.bits(2)?;
                        let prev = *lens
                            .last()
                            .ok_or_else(|| corrupt("bad symbol code table"))?;
                        lens.extend(std::iter::repeat_n(prev, n as usize));
                    }
                    33 => lens.extend(std::iter::repeat_n(0, 3 + bits.bits(3)? as usize)),
                    _ => lens.extend(std::iter::repeat_n(0, 11 + bits.bits(7)? as usize)),
                }
            }
            lens.truncate(syms.len());
            bits.align();
            let ids = Table::symbols(&lens)?;
            let tables = TextTables {
                fs,
                ds,
                dt,
                rdw,
                rdh,
                rdx,
                rdy,
                rsize,
                ids,
            };
            decode_text(
                &p,
                &syms,
                &mut Src::Huff(&mut bits, &tables),
                &mut ctx,
                self.env,
            )?
        } else {
            let mut mq = Mq::new(rest);
            decode_text(&p, &syms, &mut Src::Arith(&mut mq), &mut ctx, self.env)?
        };
        self.finish_region(seg, bm, &r, seg.typ == 4)
    }

    fn pattern_dict(&mut self, seg: &Seg, d: &[u8]) -> Result<()> {
        let f = byte(d, 0)?;
        let mmr = f & 1 != 0;
        let template = (f >> 1) & 3;
        let pw = usize::from(byte(d, 1)?);
        let ph = usize::from(byte(d, 2)?);
        let graymax = be32(d, 3)? as usize;
        if pw == 0 || ph == 0 || graymax > 1 << 20 {
            return Err(corrupt("bad pattern dictionary"));
        }
        let w = (graymax + 1) * pw;
        self.env.check_bitmap(w, ph)?;
        let data = d.get(7..).unwrap_or_default();
        let coll = if mmr {
            let (packed, _) = self.env.mmr_fn()?(data, w, ph);
            Bm::from_packed(&packed, w, ph)
        } else {
            let at = [(-(pw as i32) as i8, 0i8), (-3, -1), (2, -2), (-2, -2)];
            let mut mq = Mq::new(data);
            let mut cx = vec![Cx::default(); generic_contexts(template)];
            decode_generic(&mut mq, &mut cx, w, ph, template, false, &at, None)
        };
        let pats: Vec<Bm> = (0..=graymax)
            .map(|i| coll.crop((i * pw) as i32, 0, pw, ph))
            .collect();
        self.store.insert(seg.num, Stored::Pats(Rc::new(pats)));
        Ok(())
    }

    fn halftone(&mut self, seg: &Seg, d: &[u8]) -> Result<()> {
        let r = parse_region(d)?;
        let f = byte(d, 17)?;
        let mmr = f & 1 != 0;
        let template = (f >> 1) & 3;
        let skip_on = f & 8 != 0;
        let comb = (f >> 4) & 7;
        let defpix = f >> 7;
        let gw = be32(d, 18)? as usize;
        let gh = be32(d, 22)? as usize;
        let gx = i64::from(be32(d, 26)? as i32);
        let gy = i64::from(be32(d, 30)? as i32);
        let rx = i64::from(be16(d, 34)?);
        let ry = i64::from(be16(d, 36)?);
        let data = d.get(38..).unwrap_or_default();
        let pats = seg
            .refs
            .iter()
            .find_map(|r| match self.store.get(r) {
                Some(Stored::Pats(p)) => Some(p.clone()),
                _ => None,
            })
            .ok_or_else(|| corrupt("halftone region without pattern dictionary"))?;
        let (pw, ph) = (pats[0].w as i64, pats[0].h as i64);
        self.env.check_bitmap(r.w, r.h)?;
        self.env.check_bitmap(gw, gh)?;
        let mut out = Bm::new(r.w, r.h, defpix);
        let pos_of = |m: usize, n: usize| -> (i64, i64) {
            let (m, n) = (m as i64, n as i64);
            ((gx + m * ry + n * rx) >> 8, (gy + m * rx - n * ry) >> 8)
        };
        let skip = if skip_on {
            let mut s = Bm::new(gw, gh, 0);
            for m in 0..gh {
                for n in 0..gw {
                    let (x, y) = pos_of(m, n);
                    if x + pw <= 0 || x >= r.w as i64 || y + ph <= 0 || y >= r.h as i64 {
                        s.d[m * gw + n] = 1;
                    }
                }
            }
            Some(s)
        } else {
            None
        };
        let bpp = ceil_log2(pats.len()) as usize;
        let mut gray = vec![0u32; gw * gh];
        if bpp > 0 {
            let at = [
                (if template <= 1 { 3i8 } else { 2 }, -1i8),
                (-3, -1),
                (2, -2),
                (-2, -2),
            ];
            let mut mq = Mq::new(data);
            let mut cx = vec![Cx::default(); generic_contexts(template)];
            let mut off = 0usize;
            let mut prev: Option<Bm> = None;
            for j in (0..bpp).rev() {
                let mut plane = if mmr {
                    let rest = data.get(off..).unwrap_or_default();
                    let (packed, used) = self.env.mmr_fn()?(rest, gw, gh);
                    off += used;
                    Bm::from_packed(&packed, gw, gh)
                } else {
                    decode_generic(
                        &mut mq,
                        &mut cx,
                        gw,
                        gh,
                        template,
                        false,
                        &at,
                        skip.as_ref(),
                    )
                };
                if let Some(p) = &prev {
                    for (a, b) in plane.d.iter_mut().zip(&p.d) {
                        *a ^= *b;
                    }
                }
                for (g, b) in gray.iter_mut().zip(&plane.d) {
                    *g |= u32::from(*b) << j;
                }
                prev = Some(plane);
            }
        }
        for m in 0..gh {
            for n in 0..gw {
                let (x, y) = pos_of(m, n);
                let g = (gray[m * gw + n] as usize).min(pats.len() - 1);
                let clamp = |v: i64| v.clamp(-(1 << 30), 1 << 30) as i32;
                out.combine(&pats[g], clamp(x), clamp(y), comb);
            }
        }
        self.finish_region(seg, out, &r, seg.typ == 20)
    }

    fn generic_region(&mut self, seg: &Seg, d: &[u8]) -> Result<()> {
        let mut r = parse_region(d)?;
        let f = byte(d, 17)?;
        let mmr = f & 1 != 0;
        let template = (f >> 1) & 3;
        let tpgdon = f & 8 != 0;
        if f & 0x10 != 0 {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "JBIG2: extended generic templates are not supported",
            ));
        }
        let mut pos = 18;
        let mut at = [(3i8, -1i8), (-3, -1), (2, -2), (-2, -2)];
        if !mmr {
            if template == 0 {
                at = at_pairs::<4>(d, &mut pos)?;
            } else {
                at[0] = at_pairs::<1>(d, &mut pos)?[0];
            }
        }
        let mut data = d.get(pos..).unwrap_or_default();
        if seg.len.is_none() {
            // unknown length: the last four bytes hold the real row count
            if data.len() >= 4 {
                let rows = be32(data, data.len() - 4)? as usize;
                data = &data[..data.len() - 4];
                r.h = r.h.min(rows);
            }
        }
        self.env.check_bitmap(r.w, r.h)?;
        let bm = if mmr {
            let (packed, _) = self.env.mmr_fn()?(data, r.w, r.h);
            Bm::from_packed(&packed, r.w, r.h)
        } else {
            let mut mq = Mq::new(data);
            let mut cx = vec![Cx::default(); generic_contexts(template)];
            decode_generic(&mut mq, &mut cx, r.w, r.h, template, tpgdon, &at, None)
        };
        self.finish_region(seg, bm, &r, seg.typ == 36)
    }

    fn refinement_region(&mut self, seg: &Seg, d: &[u8]) -> Result<()> {
        let r = parse_region(d)?;
        self.env.check_bitmap(r.w, r.h)?;
        let f = byte(d, 17)?;
        let template = f & 1;
        let tpgron = f & 2 != 0;
        let mut pos = 18;
        let mut rat = [(-1i8, -1i8), (-1, -1)];
        if template == 0 {
            rat = at_pairs::<2>(d, &mut pos)?;
        }
        let data = d.get(pos..).unwrap_or_default();
        let reference = seg
            .refs
            .iter()
            .find_map(|r| match self.store.get(r) {
                Some(Stored::Region(b)) => Some((**b).clone()),
                _ => None,
            })
            .or_else(|| self.page.as_ref().map(|p| p.bm.crop(r.x, r.y, r.w, r.h)))
            .ok_or_else(|| corrupt("refinement region without reference"))?;
        let mut mq = Mq::new(data);
        let mut cx = vec![Cx::default(); refine_contexts(template)];
        let bm = decode_refinement(
            &mut mq, &mut cx, r.w, r.h, &reference, 0, 0, template, tpgron, &rat,
        );
        self.finish_region(seg, bm, &r, seg.typ == 40)
    }

    fn run_sequential(&mut self, d: &[u8], embedded_len_guess: bool) -> Result<()> {
        let _ = embedded_len_guess;
        let mut pos = 0usize;
        while pos < d.len() && !self.done {
            let seg = match parse_header(d, &mut pos) {
                Ok(s) => s,
                Err(e) if self.page.is_some() => {
                    let _ = e;
                    break;
                }
                Err(e) => return Err(e),
            };
            let len = match seg.len {
                Some(l) => l,
                None => unknown_length(d, pos, &seg)?,
            };
            let end = pos.saturating_add(len).min(d.len());
            let body = &d[pos..end];
            pos = end;
            self.run_segment(&seg, body)?;
        }
        Ok(())
    }

    fn run_segment(&mut self, seg: &Seg, body: &[u8]) -> Result<()> {
        if seg.page != 0 {
            if let Some(p) = self.page_no {
                if seg.page != p {
                    return Ok(());
                }
            }
        }
        self.process(seg, body)
    }

    fn into_page(self) -> Result<Page> {
        let p = self.page.ok_or_else(|| corrupt("no page was decoded"))?;
        Ok(Page {
            width: p.bm.w,
            height: p.bm.h,
            bits: p.bm.pack(),
        })
    }
}

/// Length of a segment of unknown size (only immediate generic regions may be): up to and
/// including the end marker and the row count that follows it.
fn unknown_length(d: &[u8], pos: usize, seg: &Seg) -> Result<usize> {
    if seg.typ != 38 && seg.typ != 39 {
        return Err(corrupt("segment of unknown length"));
    }
    let body = d.get(pos..).unwrap_or_default();
    let mmr = body.get(17).is_some_and(|f| f & 1 != 0);
    let marker: [u8; 2] = if mmr { [0x00, 0x00] } else { [0xFF, 0xAC] };
    let from = 18.min(body.len());
    let at = body[from..]
        .windows(2)
        .position(|w| w == marker)
        .ok_or_else(|| corrupt("unterminated generic region"))?;
    Ok(from + at + 2 + 4)
}

/// Decodes an embedded stream, with optional global segments.
pub fn decode_embedded(globals: Option<&[u8]>, data: &[u8], env: &Env) -> Result<Page> {
    env.spent.set(0);
    let mut dec = Dec::new(env);
    if let Some(g) = globals {
        dec.run_sequential(g, true)?;
        dec.done = false;
    }
    dec.run_sequential(data, true)?;
    dec.into_page()
}

/// Decodes the first page of a standalone JBIG2 file.
pub fn decode_file(data: &[u8], env: &Env) -> Result<Page> {
    env.spent.set(0);
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a JBIG2 file"));
    }
    let flags = byte(data, 8)?;
    let sequential = flags & 1 != 0;
    let mut pos = 9;
    if flags & 2 == 0 {
        pos += 4;
    }
    let body = data.get(pos..).unwrap_or_default();
    let mut dec = Dec::new(env);
    if sequential {
        dec.run_sequential(body, false)?;
    } else {
        let mut p = 0usize;
        let mut segs = Vec::new();
        while p < body.len() {
            let s = parse_header(body, &mut p)?;
            let end = s.typ == 51;
            segs.push(s);
            if end {
                break;
            }
        }
        for s in &segs {
            if dec.done {
                break;
            }
            let len = s.len.unwrap_or(0);
            let e = p.saturating_add(len).min(body.len());
            let seg_body = &body[p..e];
            p = e;
            dec.run_segment(s, seg_body)?;
        }
    }
    dec.into_page()
}
