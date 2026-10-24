//! Text regions (6.4) and symbol dictionaries (6.5).
use crate::arith::{Cx, Ia, Iaid, Mq};
use crate::bitmap::{decode_generic, decode_refinement, generic_contexts, refine_contexts, Bm};
use crate::huffman::{corrupt, standard, Bits, Table};
use crate::Env;
use oc_core::Result;
use std::rc::Rc;

pub fn ceil_log2(n: usize) -> u32 {
    let mut b = 0;
    while (1usize << b) < n {
        b += 1;
    }
    b
}

/// Arithmetic coding state of text region decoding: integer decoders and refinement contexts.
pub struct TextCtx {
    iadt: Ia,
    iafs: Ia,
    iads: Ia,
    iait: Ia,
    iari: Ia,
    iardw: Ia,
    iardh: Ia,
    iardx: Ia,
    iardy: Ia,
    iaid: Iaid,
    pub gr: Vec<Cx>,
}

impl TextCtx {
    pub fn new(code_len: u32, rtemplate: u8) -> TextCtx {
        TextCtx {
            iadt: Ia::default(),
            iafs: Ia::default(),
            iads: Ia::default(),
            iait: Ia::default(),
            iari: Ia::default(),
            iardw: Ia::default(),
            iardh: Ia::default(),
            iardx: Ia::default(),
            iardy: Ia::default(),
            iaid: Iaid::new(code_len),
            gr: vec![Cx::default(); refine_contexts(rtemplate)],
        }
    }
}

pub struct TextTables {
    pub fs: Table,
    pub ds: Table,
    pub dt: Table,
    pub rdw: Table,
    pub rdh: Table,
    pub rdx: Table,
    pub rdy: Table,
    pub rsize: Table,
    pub ids: Table,
}

pub struct TextParams {
    pub refine: bool,
    pub w: usize,
    pub h: usize,
    pub ninst: u32,
    pub log_strips: u32,
    pub def_pixel: u8,
    pub comb_op: u8,
    pub transposed: bool,
    /// 0 bottom-left, 1 top-left, 2 bottom-right, 3 top-right
    pub corner: u8,
    pub ds_off: i32,
    pub rtemplate: u8,
    pub rat: [(i8, i8); 2],
}

pub enum Src<'a, 'b> {
    Arith(&'a mut Mq<'b>),
    Huff(&'a mut Bits<'b>, &'a TextTables),
}

#[derive(Clone, Copy)]
enum Field {
    Dt,
    Fs,
    Ds,
    It,
    Ri,
    Rdw,
    Rdh,
    Rdx,
    Rdy,
}

fn field(src: &mut Src, ctx: &mut TextCtx, f: Field, log_strips: u32) -> Result<Option<i32>> {
    match src {
        Src::Arith(mq) => Ok(match f {
            Field::Dt => ctx.iadt.decode(mq),
            Field::Fs => ctx.iafs.decode(mq),
            Field::Ds => ctx.iads.decode(mq),
            Field::It => ctx.iait.decode(mq),
            Field::Ri => ctx.iari.decode(mq),
            Field::Rdw => ctx.iardw.decode(mq),
            Field::Rdh => ctx.iardh.decode(mq),
            Field::Rdx => ctx.iardx.decode(mq),
            Field::Rdy => ctx.iardy.decode(mq),
        }),
        Src::Huff(r, t) => match f {
            Field::Dt => t.dt.decode(r),
            Field::Fs => t.fs.decode(r),
            Field::Ds => t.ds.decode(r),
            Field::It => Ok(Some(r.bits(log_strips)? as i32)),
            Field::Ri => Ok(Some(r.bit()? as i32)),
            Field::Rdw => t.rdw.decode(r),
            Field::Rdh => t.rdh.decode(r),
            Field::Rdx => t.rdx.decode(r),
            Field::Rdy => t.rdy.decode(r),
        },
    }
}

fn need(v: Option<i32>) -> Result<i64> {
    v.map(i64::from)
        .ok_or_else(|| corrupt("unexpected out-of-band value"))
}

pub fn decode_text(
    p: &TextParams,
    syms: &[Rc<Bm>],
    src: &mut Src,
    ctx: &mut TextCtx,
    env: &Env,
) -> Result<Bm> {
    env.check_bitmap(p.w, p.h)?;
    let strips = 1i64 << p.log_strips;
    let mut bm = Bm::new(p.w, p.h, p.def_pixel);
    let mut stript = -need(field(src, ctx, Field::Dt, p.log_strips)?)? * strips;
    let mut firsts = 0i64;
    let mut n = 0u32;
    while n < p.ninst {
        stript += need(field(src, ctx, Field::Dt, p.log_strips)?)? * strips;
        let mut first = true;
        let mut curs = 0i64;
        loop {
            if first {
                firsts += need(field(src, ctx, Field::Fs, p.log_strips)?)?;
                curs = firsts;
                first = false;
            } else {
                match field(src, ctx, Field::Ds, p.log_strips)? {
                    None => break,
                    Some(ids) => curs += i64::from(ids) + i64::from(p.ds_off),
                }
                if n >= p.ninst {
                    return Err(corrupt("too many symbol instances"));
                }
            }
            let curt = if strips == 1 {
                0
            } else {
                need(field(src, ctx, Field::It, p.log_strips)?)?
            };
            let ti = stript + curt;
            let id = match src {
                Src::Arith(mq) => ctx.iaid.decode(mq),
                Src::Huff(r, t) => t.ids.decode(r)?.unwrap_or(-1).max(0) as usize,
            };
            let base = syms
                .get(id)
                .ok_or_else(|| corrupt("symbol id out of range"))?;
            let ri = if p.refine {
                need(field(src, ctx, Field::Ri, p.log_strips)?)?
            } else {
                0
            };
            let ib: Rc<Bm> = if ri == 0 {
                base.clone()
            } else {
                let rdw = need(field(src, ctx, Field::Rdw, p.log_strips)?)?;
                let rdh = need(field(src, ctx, Field::Rdh, p.log_strips)?)?;
                let rdx = need(field(src, ctx, Field::Rdx, p.log_strips)?)?;
                let rdy = need(field(src, ctx, Field::Rdy, p.log_strips)?)?;
                let rsize = if let Src::Huff(r, t) = src {
                    let v = need(t.rsize.decode(r)?)?;
                    r.align();
                    v.max(0) as usize
                } else {
                    0
                };
                let gw = base.w as i64 + rdw;
                let gh = base.h as i64 + rdh;
                if !(0..=1 << 16).contains(&gw) || !(0..=1 << 16).contains(&gh) {
                    return Err(corrupt("refined symbol size out of range"));
                }
                let (gw, gh) = (gw as usize, gh as usize);
                env.check_bitmap(gw, gh)?;
                let dx = rdw.div_euclid(2) + rdx;
                let dy = rdh.div_euclid(2) + rdy;
                let (dx, dy) = (
                    dx.clamp(-1 << 20, 1 << 20) as i32,
                    dy.clamp(-1 << 20, 1 << 20) as i32,
                );
                let out = match src {
                    Src::Arith(mq) => decode_refinement(
                        mq,
                        &mut ctx.gr,
                        gw,
                        gh,
                        base,
                        dx,
                        dy,
                        p.rtemplate,
                        false,
                        &p.rat,
                    ),
                    Src::Huff(r, _) => {
                        let rest = r.rest();
                        let mut mq = Mq::new(rest.get(..rsize.min(rest.len())).unwrap_or_default());
                        let o = decode_refinement(
                            &mut mq,
                            &mut ctx.gr,
                            gw,
                            gh,
                            base,
                            dx,
                            dy,
                            p.rtemplate,
                            false,
                            &p.rat,
                        );
                        r.skip_bytes(rsize);
                        o
                    }
                };
                Rc::new(out)
            };
            let (wi, hi) = (ib.w as i64, ib.h as i64);
            let right = p.corner >= 2;
            let bottom = p.corner == 0 || p.corner == 2;
            if !p.transposed && right {
                curs += wi - 1;
            } else if p.transposed && bottom {
                curs += hi - 1;
            }
            let si = curs;
            let (mut x, mut y) = if p.transposed { (ti, si) } else { (si, ti) };
            if right {
                x -= wi - 1;
            }
            if bottom {
                y -= hi - 1;
            }
            let clamp = |v: i64| v.clamp(-(1 << 30), 1 << 30) as i32;
            bm.combine(&ib, clamp(x), clamp(y), p.comb_op);
            if !p.transposed && !right {
                curs += wi - 1;
            } else if p.transposed && !bottom {
                curs += hi - 1;
            }
            n += 1;
        }
    }
    Ok(bm)
}

pub struct SdParams {
    pub huff: bool,
    pub refagg: bool,
    pub template: u8,
    pub rtemplate: u8,
    pub at: [(i8, i8); 4],
    pub rat: [(i8, i8); 2],
    pub num_ex: u32,
    pub num_new: u32,
    pub dh: Table,
    pub dw: Table,
    pub bmsize: Table,
    pub agginst: Table,
}

pub struct SdOut {
    pub exported: Vec<Rc<Bm>>,
    pub gb: Vec<Cx>,
    pub gr: Vec<Cx>,
}

pub fn decode_symbol_dict(
    data: &[u8],
    p: &SdParams,
    insyms: &[Rc<Bm>],
    retained: Option<(&[Cx], &[Cx])>,
    env: &Env,
) -> Result<SdOut> {
    let total = insyms.len() + p.num_new as usize;
    if p.num_new as usize > data.len().saturating_mul(64) + 1024 && !p.refagg {
        return Err(corrupt("symbol count exceeds what the data can hold"));
    }
    env.lim.check_entries(total as u64)?;
    let mut code_len = ceil_log2(total);
    if p.huff {
        code_len = code_len.max(1);
    }
    if p.huff && p.refagg {
        return Err(oc_core::Error::new(
            oc_core::ErrorCode::UnsupportedVariant,
            "JBIG2: Huffman-coded refinement/aggregate symbol dictionaries are not supported",
        ));
    }
    let mut mq = Mq::new(data);
    let mut bits = Bits::new(data);
    let (mut gb, gr) = match retained {
        Some((a, b)) => (a.to_vec(), b.to_vec()),
        None => (
            vec![Cx::default(); generic_contexts(p.template)],
            vec![Cx::default(); refine_contexts(p.rtemplate)],
        ),
    };
    let mut tctx = TextCtx::new(code_len, p.rtemplate);
    tctx.gr = gr;
    let (mut iadh, mut iadw, mut iaex, mut iaai) =
        (Ia::default(), Ia::default(), Ia::default(), Ia::default());
    let mut all: Vec<Rc<Bm>> = insyms.to_vec();
    let mut widths: Vec<usize> = Vec::new();
    let mut hcheight = 0i64;
    let mut decoded = 0usize;
    let new = p.num_new as usize;
    while decoded < new {
        let dh = if p.huff {
            need(p.dh.decode(&mut bits)?)?
        } else {
            need(iadh.decode(&mut mq))?
        };
        hcheight += dh;
        if !(0..=1 << 20).contains(&hcheight) {
            return Err(corrupt("symbol height out of range"));
        }
        let h = hcheight as usize;
        let mut symwidth = 0i64;
        let mut totwidth = 0usize;
        let first = decoded;
        loop {
            let dw = if p.huff {
                p.dw.decode(&mut bits)?
            } else {
                iadw.decode(&mut mq)
            };
            let Some(dw) = dw else { break };
            if decoded >= new {
                return Err(corrupt("too many symbols in height class"));
            }
            symwidth += i64::from(dw);
            if !(0..=1 << 20).contains(&symwidth) {
                return Err(corrupt("symbol width out of range"));
            }
            let w = symwidth as usize;
            totwidth += w;
            if !p.huff || p.refagg {
                env.check_bitmap(w, h)?;
                let bm = if !p.refagg {
                    decode_generic(&mut mq, &mut gb, w, h, p.template, false, &p.at, None)
                } else {
                    let ninst = if p.huff {
                        need(p.agginst.decode(&mut bits)?)?
                    } else {
                        need(iaai.decode(&mut mq))?
                    };
                    if ninst > 1 {
                        let tp = TextParams {
                            refine: true,
                            w,
                            h,
                            ninst: ninst as u32,
                            log_strips: 0,
                            def_pixel: 0,
                            comb_op: 0,
                            transposed: false,
                            corner: 1,
                            ds_off: 0,
                            rtemplate: p.rtemplate,
                            rat: p.rat,
                        };
                        decode_text(&tp, &all, &mut Src::Arith(&mut mq), &mut tctx, env)?
                    } else {
                        let id = tctx.iaid.decode(&mut mq);
                        let rdx = need(tctx.iardx.decode(&mut mq))?;
                        let rdy = need(tctx.iardy.decode(&mut mq))?;
                        let base = all
                            .get(id)
                            .ok_or_else(|| corrupt("symbol id out of range"))?;
                        decode_refinement(
                            &mut mq,
                            &mut tctx.gr,
                            w,
                            h,
                            base,
                            rdx.clamp(-1 << 20, 1 << 20) as i32,
                            rdy.clamp(-1 << 20, 1 << 20) as i32,
                            p.rtemplate,
                            false,
                            &p.rat,
                        )
                    }
                };
                all.push(Rc::new(bm));
            } else {
                widths.push(w);
            }
            decoded += 1;
        }
        if p.huff && !p.refagg {
            let bmsize = need(p.bmsize.decode(&mut bits)?)?.max(0) as usize;
            bits.align();
            env.check_bitmap(totwidth, h)?;
            let coll = if bmsize == 0 {
                let rb = totwidth.div_ceil(8);
                let raw = bits.rest();
                let b = Bm::from_packed(raw, totwidth, h);
                bits.skip_bytes(rb * h);
                b
            } else {
                let Some(mmr) = env.mmr else {
                    return Err(oc_core::Error::new(
                        oc_core::ErrorCode::CodecNotInBuild,
                        "JBIG2: MMR decoding is not available here",
                    ));
                };
                let rest = bits.rest();
                let (packed, _) = mmr(
                    rest.get(..bmsize.min(rest.len())).unwrap_or_default(),
                    totwidth,
                    h,
                );
                bits.skip_bytes(bmsize);
                Bm::from_packed(&packed, totwidth, h)
            };
            let mut x = 0usize;
            let sub = widths.split_off(0);
            for (k, &w) in sub.iter().enumerate() {
                let _ = k;
                all.push(Rc::new(coll.crop(x as i32, 0, w, h)));
                x += w;
            }
            let _ = first;
        }
    }
    // export flags
    let mut flags = vec![false; all.len()];
    let mut idx = 0usize;
    let mut cur = false;
    let mut guard = 0;
    while idx < all.len() {
        let run = if p.huff {
            need(standard(1).decode(&mut bits)?)?
        } else {
            need(iaex.decode(&mut mq))?
        };
        if run < 0 || idx + run as usize > all.len() {
            return Err(corrupt("bad export run"));
        }
        for f in flags.iter_mut().skip(idx).take(run as usize) {
            *f = cur;
        }
        idx += run as usize;
        cur = !cur;
        guard += 1;
        if guard > all.len() * 2 + 4 {
            return Err(corrupt("export runs do not end"));
        }
    }
    let exported: Vec<Rc<Bm>> = all
        .iter()
        .zip(&flags)
        .filter(|(_, f)| **f)
        .map(|(b, _)| b.clone())
        .collect();
    Ok(SdOut {
        exported,
        gb,
        gr: tctx.gr,
    })
}
