//! JPEG 2000 codestream decoding (T.800 Annexes A, B, E, F, G).
use crate::dwt;
use crate::t1::{Seg, STYLE_BYPASS, STYLE_TERMALL, T1};
use crate::t2::{HdrBits, TagTree};
use oc_core::{Error, ErrorCode, Limits, Result};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct CompInfo {
    pub prec: u32,
    pub signed: bool,
    pub dx: u32,
    pub dy: u32,
}

pub struct Component {
    pub info: CompInfo,
    pub w: usize,
    pub h: usize,
    pub data: Vec<i32>,
}

pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub comps: Vec<Component>,
}

struct Siz {
    x: u32,
    y: u32,
    xo: u32,
    yo: u32,
    xt: u32,
    yt: u32,
    xto: u32,
    yto: u32,
    comps: Vec<CompInfo>,
}

#[derive(Clone, Default)]
struct CompCod {
    levels: u8,
    xcb: u8,
    ycb: u8,
    style: u8,
    reversible: bool,
    precincts: Vec<(u8, u8)>,
}

#[derive(Clone, Default)]
struct Qcd {
    style: u8,
    guard: u8,
    steps: Vec<(u8, u16)>,
}

#[derive(Clone, Copy)]
struct Global {
    prog: u8,
    layers: u16,
    mct: u8,
    sop: bool,
    eph: bool,
}

#[derive(Clone, Copy)]
struct Poc {
    rs: u8,
    cs: u16,
    lye: u16,
    re: u8,
    ce: u16,
    prog: u8,
}

#[derive(Clone, Default)]
struct Params {
    global: Option<Global>,
    cod: Option<CompCod>,
    coc: Vec<Option<CompCod>>,
    qcd: Option<Qcd>,
    qcc: Vec<Option<Qcd>>,
    roi: Vec<u8>,
    poc: Vec<Poc>,
}

fn u16_at(b: &[u8], o: usize) -> Result<u16> {
    match b.get(o..o + 2) {
        Some(s) => Ok(u16::from_be_bytes([s[0], s[1]])),
        None => Err(Error::truncated("JPEG 2000: marker segment")),
    }
}
fn u32_at(b: &[u8], o: usize) -> Result<u32> {
    match b.get(o..o + 4) {
        Some(s) => Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]])),
        None => Err(Error::truncated("JPEG 2000: marker segment")),
    }
}
fn u8_at(b: &[u8], o: usize) -> Result<u8> {
    b.get(o)
        .copied()
        .ok_or_else(|| Error::truncated("JPEG 2000: marker segment"))
}

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorCode::UnsupportedFormat,
        format!("JPEG 2000: {what} is not supported"),
    )
}

fn ceil_div(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}

fn parse_siz(b: &[u8], limits: &Limits) -> Result<Siz> {
    let n = usize::from(u16_at(b, 34)?);
    if n == 0 || n > 16384 {
        return Err(Error::corrupt("JPEG 2000: bad component count"));
    }
    let mut comps = Vec::new();
    for i in 0..n {
        let s = u8_at(b, 36 + i * 3)?;
        let dx = u32::from(u8_at(b, 37 + i * 3)?);
        let dy = u32::from(u8_at(b, 38 + i * 3)?);
        if dx == 0 || dy == 0 {
            return Err(Error::corrupt("JPEG 2000: zero subsampling"));
        }
        let prec = u32::from(s & 0x7f) + 1;
        if prec > 31 {
            return Err(unsupported("sample precision above 31 bits"));
        }
        comps.push(CompInfo {
            prec,
            signed: s & 0x80 != 0,
            dx,
            dy,
        });
    }
    let s = Siz {
        x: u32_at(b, 2)?,
        y: u32_at(b, 6)?,
        xo: u32_at(b, 10)?,
        yo: u32_at(b, 14)?,
        xt: u32_at(b, 18)?,
        yt: u32_at(b, 22)?,
        xto: u32_at(b, 26)?,
        yto: u32_at(b, 30)?,
        comps,
    };
    if s.x <= s.xo || s.y <= s.yo || s.xt == 0 || s.yt == 0 || s.xto > s.xo || s.yto > s.yo {
        return Err(Error::corrupt("JPEG 2000: bad image geometry"));
    }
    if u64::from(s.xto) + u64::from(s.xt) <= u64::from(s.xo)
        || u64::from(s.yto) + u64::from(s.yt) <= u64::from(s.yo)
    {
        return Err(Error::corrupt("JPEG 2000: bad tile geometry"));
    }
    limits.check_pixels(s.x - s.xo, s.y - s.yo)?;
    limits.check_alloc(u64::from(s.x - s.xo) * u64::from(s.y - s.yo) * 4 * s.comps.len() as u64)?;
    let tiles = ceil_div(u64::from(s.x - s.xto), u64::from(s.xt))
        * ceil_div(u64::from(s.y - s.yto), u64::from(s.yt));
    if tiles > 65535 {
        return Err(Error::corrupt("JPEG 2000: too many tiles"));
    }
    Ok(s)
}

fn parse_spcod(b: &[u8], o: usize, defined: bool) -> Result<CompCod> {
    let levels = u8_at(b, o)?;
    let xcb = (u8_at(b, o + 1)? & 15) + 2;
    let ycb = (u8_at(b, o + 2)? & 15) + 2;
    if levels > 32 || xcb > 10 || ycb > 10 || xcb + ycb > 12 {
        return Err(Error::corrupt("JPEG 2000: bad coding style"));
    }
    let style = u8_at(b, o + 3)?;
    let reversible = u8_at(b, o + 4)? == 1;
    let mut precincts = Vec::new();
    for i in 0..=usize::from(levels) {
        if defined {
            let v = u8_at(b, o + 5 + i)?;
            precincts.push((v & 15, v >> 4));
        } else {
            precincts.push((15, 15));
        }
    }
    Ok(CompCod {
        levels,
        xcb,
        ycb,
        style,
        reversible,
        precincts,
    })
}

fn parse_qcd(b: &[u8], o: usize) -> Result<Qcd> {
    let s = u8_at(b, o)?;
    let (style, guard) = (s & 31, s >> 5);
    let mut steps = Vec::new();
    let mut p = o + 1;
    match style {
        0 => {
            while p < b.len() {
                steps.push((b[p] >> 3, 0));
                p += 1;
            }
        }
        1 | 2 => {
            while p + 1 < b.len() {
                let v = u16_at(b, p)?;
                steps.push(((v >> 11) as u8, v & 0x7ff));
                p += 2;
            }
        }
        _ => return Err(Error::corrupt("JPEG 2000: bad quantisation style")),
    }
    if steps.is_empty() {
        return Err(Error::corrupt("JPEG 2000: empty quantisation"));
    }
    Ok(Qcd {
        style,
        guard,
        steps,
    })
}

fn parse_marker(code: u16, b: &[u8], p: &mut Params, ncomp: usize) -> Result<()> {
    let wide = ncomp > 256;
    let comp_at = |o: usize| -> Result<(usize, usize)> {
        if wide {
            Ok((usize::from(u16_at(b, o)?), o + 2))
        } else {
            Ok((usize::from(u8_at(b, o)?), o + 1))
        }
    };
    match code {
        0xFF52 => {
            let scod = u8_at(b, 0)?;
            let g = Global {
                prog: u8_at(b, 1)?,
                layers: u16_at(b, 2)?,
                mct: u8_at(b, 4)?,
                sop: scod & 2 != 0,
                eph: scod & 4 != 0,
            };
            if g.layers == 0 || g.prog > 4 {
                return Err(Error::corrupt("JPEG 2000: bad progression"));
            }
            p.global = Some(g);
            p.cod = Some(parse_spcod(b, 5, scod & 1 != 0)?);
        }
        0xFF53 => {
            let (c, o) = comp_at(0)?;
            if c >= ncomp {
                return Err(Error::corrupt("JPEG 2000: COC component out of range"));
            }
            let scoc = u8_at(b, o)?;
            p.coc[c] = Some(parse_spcod(b, o + 1, scoc & 1 != 0)?);
        }
        0xFF5C => p.qcd = Some(parse_qcd(b, 0)?),
        0xFF5D => {
            let (c, o) = comp_at(0)?;
            if c >= ncomp {
                return Err(Error::corrupt("JPEG 2000: QCC component out of range"));
            }
            p.qcc[c] = Some(parse_qcd(b, o)?);
        }
        0xFF5E => {
            let (c, o) = comp_at(0)?;
            if c >= ncomp {
                return Err(Error::corrupt("JPEG 2000: RGN component out of range"));
            }
            if u8_at(b, o)? == 0 {
                p.roi[c] = u8_at(b, o + 1)?;
            }
        }
        0xFF5F => {
            let sz = if wide { 9 } else { 7 };
            let mut o = 0;
            while o + sz <= b.len() {
                let rs = b[o];
                let (cs, o2) = comp_at(o + 1)?;
                let lye = u16_at(b, o2)?;
                let re = b[o2 + 2];
                let (ce, o3) = comp_at(o2 + 3)?;
                let ce = if ce == 0 { 256 } else { ce };
                p.poc.push(Poc {
                    rs,
                    cs: cs as u16,
                    lye,
                    re,
                    ce: ce as u16,
                    prog: b[o3],
                });
                o = o3 + 1;
            }
        }
        0xFF60 | 0xFF61 => return Err(unsupported("packed packet headers (PPM/PPT)")),
        _ => {}
    }
    Ok(())
}

#[derive(Default)]
struct TileData {
    params: Params,
    body: Vec<u8>,
    seen: bool,
}

// ---- tile structures ----

struct Cb {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    included: bool,
    zbp: u32,
    lblock: u32,
    segs: Vec<Seg>,
}

struct BandPrec {
    cbs: Vec<Cb>,
    ncw: usize,
    incl: TagTree,
    zbp: TagTree,
}

struct Band {
    orient: u8,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    /// position of the band inside the tile-component array
    ox: usize,
    oy: usize,
    mb: u32,
    step: f32,
    precs: Vec<BandPrec>,
}

struct Res {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    pw: usize,
    ph: usize,
    ppx: u8,
    ppy: u8,
    bands: Vec<Band>,
}

struct TileComp {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    cod: CompCod,
    res: Vec<Res>,
}

fn gain(orient: u8) -> u32 {
    match orient {
        0 => 0,
        3 => 2,
        _ => 1,
    }
}

fn build_tile_comp(
    info: &CompInfo,
    cod: &CompCod,
    q: &Qcd,
    roi: u8,
    tx0: u64,
    ty0: u64,
    tx1: u64,
    ty1: u64,
) -> Result<TileComp> {
    let (dx, dy) = (u64::from(info.dx), u64::from(info.dy));
    let (x0, y0, x1, y1) = (
        ceil_div(tx0, dx),
        ceil_div(ty0, dy),
        ceil_div(tx1, dx),
        ceil_div(ty1, dy),
    );
    let nl = usize::from(cod.levels);
    let mut res = Vec::new();
    for r in 0..=nl {
        let sh = (nl - r) as u32;
        let (rx0, ry0, rx1, ry1) = (
            ceil_div(x0, 1 << sh),
            ceil_div(y0, 1 << sh),
            ceil_div(x1, 1 << sh),
            ceil_div(y1, 1 << sh),
        );
        let (ppx, ppy) = *cod
            .precincts
            .get(r)
            .ok_or_else(|| Error::corrupt("JPEG 2000: precinct sizes"))?;
        if r > 0 && (ppx == 0 || ppy == 0) {
            return Err(Error::corrupt("JPEG 2000: zero precinct size"));
        }
        let pw = if rx1 > rx0 {
            (ceil_div(rx1, 1 << ppx) - (rx0 >> ppx)) as usize
        } else {
            0
        };
        let ph = if ry1 > ry0 {
            (ceil_div(ry1, 1 << ppy) - (ry0 >> ppy)) as usize
        } else {
            0
        };
        if pw.saturating_mul(ph) > 1 << 22 {
            return Err(Error::corrupt("JPEG 2000: too many precincts"));
        }
        let orients: &[u8] = if r == 0 { &[0] } else { &[1, 2, 3] };
        let mut bands = Vec::new();
        for &o in orients {
            let nb = if r == 0 { nl } else { nl - r + 1 } as u32;
            let (xob, yob) = (u64::from(o == 1 || o == 3), u64::from(o == 2 || o == 3));
            let bound = |t: u64, ob: u64| -> u64 {
                if r == 0 {
                    ceil_div(t, 1 << nb)
                } else {
                    let off = ob << (nb - 1);
                    ceil_div(t.saturating_sub(off), 1 << nb)
                }
            };
            let (bx0, by0, bx1, by1) = (
                bound(x0, xob),
                bound(y0, yob),
                bound(x1, xob),
                bound(y1, yob),
            );
            // quantisation
            let bidx = if r == 0 {
                0
            } else {
                3 * (r - 1) + usize::from(o)
            };
            let (eps, mu) = if q.style == 1 {
                let (e0, m0) = q.steps[0];
                let e = if r == 0 {
                    i32::from(e0)
                } else {
                    i32::from(e0) - r as i32 + 1
                };
                (e.max(0) as u32, m0)
            } else {
                let (e, m) = *q
                    .steps
                    .get(bidx)
                    .ok_or_else(|| Error::corrupt("JPEG 2000: missing quantisation step"))?;
                (u32::from(e), m)
            };
            let mb = (u32::from(q.guard) + eps).saturating_sub(1) + u32::from(roi);
            if mb > 30 {
                return Err(unsupported("more than 30 magnitude bit-planes"));
            }
            let rb = info.prec + gain(o);
            let step = if cod.reversible {
                1.0
            } else {
                (2.0f32).powi(rb as i32 - eps as i32) * (1.0 + f32::from(mu) / 2048.0)
            };
            // code-block partition
            let cbw = if r == 0 {
                cod.xcb.min(ppx)
            } else {
                cod.xcb.min(ppx - 1)
            };
            let cbh = if r == 0 {
                cod.ycb.min(ppy)
            } else {
                cod.ycb.min(ppy - 1)
            };
            let mut precs = Vec::new();
            for py in 0..ph {
                for px in 0..pw {
                    let (sx, sy) = if r == 0 {
                        (u32::from(ppx), u32::from(ppy))
                    } else {
                        (u32::from(ppx) - 1, u32::from(ppy) - 1)
                    };
                    let ax0 = (((rx0 >> ppx) + px as u64) << sx).max(bx0);
                    let ay0 = (((ry0 >> ppy) + py as u64) << sy).max(by0);
                    let ax1 = (((rx0 >> ppx) + px as u64 + 1) << sx).min(bx1);
                    let ay1 = (((ry0 >> ppy) + py as u64 + 1) << sy).min(by1);
                    let mut cbs = Vec::new();
                    let (mut ncw, mut nch) = (0usize, 0usize);
                    if ax1 > ax0 && ay1 > ay0 {
                        let gx0 = ax0 >> cbw;
                        let gx1 = ceil_div(ax1, 1 << cbw);
                        let gy0 = ay0 >> cbh;
                        let gy1 = ceil_div(ay1, 1 << cbh);
                        ncw = (gx1 - gx0) as usize;
                        nch = (gy1 - gy0) as usize;
                        if ncw.saturating_mul(nch) > 1 << 22 {
                            return Err(Error::corrupt("JPEG 2000: too many code-blocks"));
                        }
                        for gy in gy0..gy1 {
                            for gx in gx0..gx1 {
                                cbs.push(Cb {
                                    x0: ((gx << cbw).max(ax0) - bx0) as usize,
                                    y0: ((gy << cbh).max(ay0) - by0) as usize,
                                    x1: (((gx + 1) << cbw).min(ax1) - bx0) as usize,
                                    y1: (((gy + 1) << cbh).min(ay1) - by0) as usize,
                                    included: false,
                                    zbp: 0,
                                    lblock: 3,
                                    segs: Vec::new(),
                                });
                            }
                        }
                    }
                    precs.push(BandPrec {
                        cbs,
                        ncw,
                        incl: TagTree::new(ncw, nch),
                        zbp: TagTree::new(ncw, nch),
                    });
                }
            }
            bands.push(Band {
                orient: o,
                x0: bx0 as usize,
                y0: by0 as usize,
                x1: bx1 as usize,
                y1: by1 as usize,
                ox: 0,
                oy: 0,
                mb,
                step,
                precs,
            });
        }
        res.push(Res {
            x0: rx0 as usize,
            y0: ry0 as usize,
            x1: rx1 as usize,
            y1: ry1 as usize,
            pw,
            ph,
            ppx,
            ppy,
            bands,
        });
    }
    // Mallat offsets
    for r in 1..res.len() {
        let (lw, lh) = (res[r - 1].x1 - res[r - 1].x0, res[r - 1].y1 - res[r - 1].y0);
        for b in res[r].bands.iter_mut() {
            b.ox = if b.orient == 1 || b.orient == 3 {
                lw
            } else {
                0
            };
            b.oy = if b.orient == 2 || b.orient == 3 {
                lh
            } else {
                0
            };
        }
    }
    Ok(TileComp {
        x0: x0 as usize,
        y0: y0 as usize,
        x1: x1 as usize,
        y1: y1 as usize,
        cod: cod.clone(),
        res,
    })
}

fn seg_max(style: u8, index: usize) -> u32 {
    if style & STYLE_TERMALL != 0 {
        1
    } else if style & STYLE_BYPASS != 0 {
        if index == 0 {
            10
        } else if index % 2 == 1 {
            2
        } else {
            1
        }
    } else {
        u32::MAX
    }
}

fn floor_log2(v: u32) -> u32 {
    31 - v.leading_zeros()
}

/// Reads one packet. Returns an error if the data ends inside the header.
fn read_packet(
    tc: &mut TileComp,
    r: usize,
    k: usize,
    layer: u32,
    data: &[u8],
    pos: &mut usize,
    sop: bool,
    eph: bool,
) -> Result<()> {
    if sop && data.get(*pos..*pos + 2) == Some(&[0xFF, 0x91]) {
        *pos += 6;
    }
    let style = tc.cod.style;
    let mut hb = HdrBits::new(data, *pos);
    let mut contrib: Vec<(usize, usize, usize, usize)> = Vec::new();
    if hb.bit()? == 1 {
        for (bi, band) in tc.res[r].bands.iter_mut().enumerate() {
            let prec = &mut band.precs[k];
            let ncw = prec.ncw;
            for cbi in 0..prec.cbs.len() {
                let (cx, cy) = (cbi % ncw, cbi / ncw);
                let included = if prec.cbs[cbi].included {
                    hb.bit()? == 1
                } else {
                    prec.incl.decode(&mut hb, cx, cy, layer + 1)?
                };
                if !included {
                    continue;
                }
                if !prec.cbs[cbi].included {
                    let mut t = 1;
                    while !prec.zbp.decode(&mut hb, cx, cy, t)? {
                        t += 1;
                        if t > 64 {
                            return Err(Error::corrupt("JPEG 2000: zero bit-planes"));
                        }
                    }
                    prec.cbs[cbi].zbp = prec.zbp.value(cx, cy);
                    prec.cbs[cbi].included = true;
                }
                let n = if hb.bit()? == 0 {
                    1
                } else if hb.bit()? == 0 {
                    2
                } else {
                    let v = hb.bits(2)?;
                    if v != 3 {
                        3 + v
                    } else {
                        let v = hb.bits(5)?;
                        if v != 31 {
                            6 + v
                        } else {
                            37 + hb.bits(7)?
                        }
                    }
                };
                while hb.bit()? == 1 {
                    prec.cbs[cbi].lblock += 1;
                    if prec.cbs[cbi].lblock > 32 {
                        return Err(Error::corrupt("JPEG 2000: Lblock"));
                    }
                }
                let mut remaining = n;
                while remaining > 0 {
                    let cb = &mut prec.cbs[cbi];
                    let need_new = cb.segs.last().is_none_or(|s| s.passes >= s.max_passes);
                    if need_new {
                        let idx = cb.segs.len();
                        cb.segs.push(Seg {
                            passes: 0,
                            max_passes: seg_max(style, idx),
                            data: Vec::new(),
                        });
                    }
                    let si = cb.segs.len() - 1;
                    let take = remaining.min(cb.segs[si].max_passes - cb.segs[si].passes);
                    let bits = cb.lblock + floor_log2(take);
                    if bits > 32 {
                        return Err(Error::corrupt("JPEG 2000: segment length"));
                    }
                    let len = hb.bits(bits)? as usize;
                    cb.segs[si].passes += take;
                    remaining -= take;
                    contrib.push((bi, cbi, si, len));
                }
            }
        }
    }
    hb.align();
    *pos = hb.pos;
    if eph && data.get(*pos..*pos + 2) == Some(&[0xFF, 0x92]) {
        *pos += 2;
    }
    for (bi, cbi, si, len) in contrib {
        let avail = data.len().saturating_sub(*pos).min(len);
        tc.res[r].bands[bi].precs[k].cbs[cbi].segs[si]
            .data
            .extend_from_slice(&data[*pos..*pos + avail]);
        *pos += len;
        if avail < len {
            return Err(Error::truncated("JPEG 2000: packet body"));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Pkt {
    layer: u32,
    c: usize,
    r: usize,
    k: usize,
}

fn packet_order(
    tcs: &[TileComp],
    infos: &[CompInfo],
    g: &Global,
    pocs: &[Poc],
    tile: (u64, u64),
) -> Vec<Pkt> {
    let ncomp = tcs.len();
    let maxres = tcs.iter().map(|t| t.res.len()).max().unwrap_or(0);
    let mut seen: HashSet<(u32, usize, usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    let vols: Vec<Poc> = if pocs.is_empty() {
        vec![Poc {
            rs: 0,
            cs: 0,
            lye: g.layers,
            re: maxres as u8,
            ce: ncomp as u16,
            prog: g.prog,
        }]
    } else {
        pocs.to_vec()
    };
    let key = |c: usize, r: usize, k: usize| -> (u64, u64) {
        let tc = &tcs[c];
        let res = &tc.res[r];
        let (px, py) = (k % res.pw.max(1), k / res.pw.max(1));
        let nl = usize::from(tc.cod.levels);
        let shx = u32::from(res.ppx) + (nl - r) as u32;
        let shy = u32::from(res.ppy) + (nl - r) as u32;
        let ox = (((res.x0 as u64) >> res.ppx) + px as u64) << shx;
        let oy = (((res.y0 as u64) >> res.ppy) + py as u64) << shy;
        (
            (oy * u64::from(infos[c].dy)).max(tile.1),
            (ox * u64::from(infos[c].dx)).max(tile.0),
        )
    };
    let mut push = |out: &mut Vec<Pkt>, l: u32, c: usize, r: usize, k: usize| {
        if seen.insert((l, c, r, k)) {
            out.push(Pkt { layer: l, c, r, k });
        }
    };
    for v in vols {
        let lye = u32::from(v.lye).min(g.layers.into());
        let re = usize::from(v.re).min(maxres);
        let ce = usize::from(v.ce).min(ncomp);
        let (rs, cs) = (usize::from(v.rs), usize::from(v.cs));
        let nprec = |c: usize, r: usize| tcs[c].res.get(r).map_or(0, |x| x.pw * x.ph);
        match v.prog {
            0 => {
                for l in 0..lye {
                    for r in rs..re {
                        for c in cs..ce {
                            for k in 0..nprec(c, r) {
                                push(&mut out, l, c, r, k);
                            }
                        }
                    }
                }
            }
            1 => {
                for r in rs..re {
                    for l in 0..lye {
                        for c in cs..ce {
                            for k in 0..nprec(c, r) {
                                push(&mut out, l, c, r, k);
                            }
                        }
                    }
                }
            }
            2 => {
                for r in rs..re {
                    let mut list: Vec<(u64, u64, usize, usize)> = Vec::new();
                    for c in cs..ce {
                        for k in 0..nprec(c, r) {
                            let (y, x) = key(c, r, k);
                            list.push((y, x, c, k));
                        }
                    }
                    list.sort_unstable();
                    for (_, _, c, k) in list {
                        for l in 0..lye {
                            push(&mut out, l, c, r, k);
                        }
                    }
                }
            }
            3 => {
                let mut list: Vec<(u64, u64, usize, usize, usize)> = Vec::new();
                for c in cs..ce {
                    for r in rs..re {
                        for k in 0..nprec(c, r) {
                            let (y, x) = key(c, r, k);
                            list.push((y, x, c, r, k));
                        }
                    }
                }
                list.sort_unstable();
                for (_, _, c, r, k) in list {
                    for l in 0..lye {
                        push(&mut out, l, c, r, k);
                    }
                }
            }
            _ => {
                for c in cs..ce {
                    let mut list: Vec<(u64, u64, usize, usize)> = Vec::new();
                    for r in rs..re {
                        for k in 0..nprec(c, r) {
                            let (y, x) = key(c, r, k);
                            list.push((y, x, r, k));
                        }
                    }
                    list.sort_unstable();
                    for (_, _, r, k) in list {
                        for l in 0..lye {
                            push(&mut out, l, c, r, k);
                        }
                    }
                }
            }
        }
    }
    out
}

enum Buf {
    I(Vec<i32>),
    F(Vec<f32>),
}

fn decode_blocks(tc: &TileComp, info: &CompInfo, roi: u8) -> Result<(Buf, usize, usize)> {
    let (w, h) = (tc.x1 - tc.x0, tc.y1 - tc.y0);
    let rev = tc.cod.reversible;
    let mut buf = if rev {
        Buf::I(vec![0; w * h])
    } else {
        Buf::F(vec![0.0; w * h])
    };
    let mut t1 = T1::new();
    let _ = info;
    for res in &tc.res {
        for band in &res.bands {
            let bw = band.x1 - band.x0;
            let _ = bw;
            for prec in &band.precs {
                for cb in &prec.cbs {
                    let passes: u32 = cb.segs.iter().map(|s| s.passes).sum();
                    if passes == 0 || !cb.included {
                        continue;
                    }
                    let planes = band.mb as i64 - cb.zbp as i64;
                    if planes <= 0 {
                        continue;
                    }
                    let (cw, ch) = (cb.x1 - cb.x0, cb.y1 - cb.y0);
                    t1.decode(
                        cw,
                        ch,
                        band.orient,
                        planes as u32 - 1,
                        tc.cod.style,
                        &cb.segs,
                    );
                    for y in 0..ch {
                        for x in 0..cw {
                            let mut m = t1.mag[y * cw + x];
                            if m == 0 {
                                continue;
                            }
                            if roi > 0 && m >= 1u32 << (roi + 1).min(31) {
                                m >>= roi;
                            }
                            let neg = t1.negative(x, y);
                            let pos = (band.oy + cb.y0 + y) * w + band.ox + cb.x0 + x;
                            match &mut buf {
                                Buf::I(v) => {
                                    let a = (m >> 1) as i32;
                                    v[pos] = if neg { -a } else { a };
                                }
                                Buf::F(v) => {
                                    let a = m as f32 * 0.5 * band.step;
                                    v[pos] = if neg { -a } else { a };
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // inverse DWT
    for r in 1..tc.res.len() {
        let cur = &tc.res[r];
        let prev = &tc.res[r - 1];
        let (rw, rh) = (cur.x1 - cur.x0, cur.y1 - cur.y0);
        let (lw, lh) = (prev.x1 - prev.x0, prev.y1 - prev.y0);
        if rw == 0 || rh == 0 {
            continue;
        }
        match &mut buf {
            Buf::I(v) => dwt::inverse_level(v, w, rw, rh, lw, lh, cur.x0, cur.y0, true),
            Buf::F(v) => dwt::inverse_level(v, w, rw, rh, lw, lh, cur.x0, cur.y0, false),
        }
    }
    Ok((buf, w, h))
}

fn resolve<'a>(main: &'a Params, tile: &'a Params, c: usize) -> Result<(&'a CompCod, &'a Qcd)> {
    let cod = tile.coc[c]
        .as_ref()
        .or(tile.cod.as_ref())
        .or(main.coc[c].as_ref())
        .or(main.cod.as_ref())
        .ok_or_else(|| Error::corrupt("JPEG 2000: no COD marker"))?;
    let q = tile.qcc[c]
        .as_ref()
        .or(tile.qcd.as_ref())
        .or(main.qcc[c].as_ref())
        .or(main.qcd.as_ref())
        .ok_or_else(|| Error::corrupt("JPEG 2000: no QCD marker"))?;
    Ok((cod, q))
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<Decoded> {
    if data.get(0..2) != Some(&[0xFF, 0x4F]) {
        return Err(Error::corrupt("JPEG 2000: not a codestream"));
    }
    let mut pos = 2;
    let mut siz: Option<Siz> = None;
    let mut main = Params::default();
    let mut tiles: Vec<TileData> = Vec::new();
    let mut ntx = 0usize;
    loop {
        if pos + 2 > data.len() {
            break;
        }
        let code = u16_at(data, pos)?;
        if code == 0xFFD9 {
            break;
        }
        if code == 0xFF90 {
            // SOT
            let s = siz
                .as_ref()
                .ok_or_else(|| Error::corrupt("JPEG 2000: SOT before SIZ"))?;
            let sot_start = pos;
            let isot = usize::from(u16_at(data, pos + 4)?);
            let psot = u32_at(data, pos + 6)? as usize;
            if isot >= tiles.len() {
                return Err(Error::corrupt("JPEG 2000: tile index out of range"));
            }
            let end = if psot == 0 {
                data.len()
            } else {
                (sot_start + psot).min(data.len())
            };
            pos += 12;
            let ncomp = s.comps.len();
            let td = &mut tiles[isot];
            if !td.seen {
                td.params = Params {
                    coc: vec![None; ncomp],
                    qcc: vec![None; ncomp],
                    roi: vec![0; ncomp],
                    ..Default::default()
                };
                td.seen = true;
            }
            loop {
                if pos + 2 > end {
                    return Err(Error::truncated("JPEG 2000: tile-part header"));
                }
                let m = u16_at(data, pos)?;
                if m == 0xFF93 {
                    pos += 2;
                    break;
                }
                let len = usize::from(u16_at(data, pos + 2)?);
                if len < 2 || pos + 2 + len > end {
                    return Err(Error::truncated("JPEG 2000: tile-part marker"));
                }
                parse_marker(m, &data[pos + 4..pos + 2 + len], &mut td.params, ncomp)?;
                pos += 2 + len;
            }
            let mut body_end = end;
            if psot == 0 && data[..end].ends_with(&[0xFF, 0xD9]) {
                body_end -= 2;
            }
            if pos <= body_end {
                td.body.extend_from_slice(&data[pos..body_end]);
            }
            pos = end;
            continue;
        }
        let len = usize::from(u16_at(data, pos + 2)?);
        if len < 2 || pos + 2 + len > data.len() {
            return Err(Error::truncated("JPEG 2000: main header marker"));
        }
        let seg = &data[pos + 4..pos + 2 + len];
        if code == 0xFF51 {
            let s = parse_siz(seg, limits)?;
            ntx = ceil_div(u64::from(s.x - s.xto), u64::from(s.xt)) as usize;
            let nty = ceil_div(u64::from(s.y - s.yto), u64::from(s.yt)) as usize;
            tiles = (0..ntx * nty).map(|_| TileData::default()).collect();
            let n = s.comps.len();
            main = Params {
                coc: vec![None; n],
                qcc: vec![None; n],
                roi: vec![0; n],
                ..Default::default()
            };
            siz = Some(s);
        } else {
            let n = siz.as_ref().map_or(0, |s| s.comps.len());
            if siz.is_none() && code != 0xFF64 {
                return Err(Error::corrupt("JPEG 2000: marker before SIZ"));
            }
            parse_marker(code, seg, &mut main, n)?;
        }
        pos += 2 + len;
    }
    let s = siz.ok_or_else(|| Error::corrupt("JPEG 2000: no SIZ marker"))?;
    let (w, h) = ((s.x - s.xo) as usize, (s.y - s.yo) as usize);
    let mut comps: Vec<Component> = s
        .comps
        .iter()
        .map(|ci| {
            let cw = (ceil_div(u64::from(s.x), u64::from(ci.dx))
                - ceil_div(u64::from(s.xo), u64::from(ci.dx))) as usize;
            let ch = (ceil_div(u64::from(s.y), u64::from(ci.dy))
                - ceil_div(u64::from(s.yo), u64::from(ci.dy))) as usize;
            Component {
                info: ci.clone(),
                w: cw,
                h: ch,
                data: vec![0; cw * ch],
            }
        })
        .collect();
    let mut any = false;
    for (ti, td) in tiles.iter().enumerate() {
        if !td.seen {
            continue;
        }
        if decode_tile(&s, &main, td, ti, ntx, &mut comps).is_ok() {
            any = true;
        }
    }
    if !any {
        return Err(Error::corrupt("JPEG 2000: no tile could be decoded"));
    }
    Ok(Decoded {
        width: w as u32,
        height: h as u32,
        comps,
    })
}

fn decode_tile(
    s: &Siz,
    main: &Params,
    td: &TileData,
    ti: usize,
    ntx: usize,
    out: &mut [Component],
) -> Result<()> {
    let (p, q) = (ti % ntx, ti / ntx);
    let tx0 = (u64::from(s.xto) + p as u64 * u64::from(s.xt)).max(u64::from(s.xo));
    let ty0 = (u64::from(s.yto) + q as u64 * u64::from(s.yt)).max(u64::from(s.yo));
    let tx1 = (u64::from(s.xto) + (p as u64 + 1) * u64::from(s.xt)).min(u64::from(s.x));
    let ty1 = (u64::from(s.yto) + (q as u64 + 1) * u64::from(s.yt)).min(u64::from(s.y));
    let g = td
        .params
        .global
        .or(main.global)
        .ok_or_else(|| Error::corrupt("JPEG 2000: no COD marker"))?;
    let mut tcs = Vec::new();
    for (c, ci) in s.comps.iter().enumerate() {
        let (cod, qc) = resolve(main, &td.params, c)?;
        let roi = if td.params.roi[c] != 0 {
            td.params.roi[c]
        } else {
            main.roi[c]
        };
        tcs.push(build_tile_comp(ci, cod, qc, roi, tx0, ty0, tx1, ty1)?);
    }
    let pocs = if td.params.poc.is_empty() {
        &main.poc
    } else {
        &td.params.poc
    };
    let order = packet_order(&tcs, &s.comps, &g, pocs, (tx0, ty0));
    let mut pos = 0usize;
    for pk in order {
        if pos >= td.body.len() {
            break;
        }
        if read_packet(
            &mut tcs[pk.c],
            pk.r,
            pk.k,
            pk.layer,
            &td.body,
            &mut pos,
            g.sop,
            g.eph,
        )
        .is_err()
        {
            break;
        }
    }
    // coefficients and inverse DWT
    let mut bufs = Vec::new();
    for (c, tc) in tcs.iter().enumerate() {
        let roi = if td.params.roi[c] != 0 {
            td.params.roi[c]
        } else {
            main.roi[c]
        };
        bufs.push(decode_blocks(tc, &s.comps[c], roi)?);
    }
    // multiple component transform
    if g.mct != 0 && bufs.len() >= 3 {
        let same = (0..3).all(|i| bufs[i].1 == bufs[0].1 && bufs[i].2 == bufs[0].2);
        if same {
            let n = bufs[0].1 * bufs[0].2;
            let (a, rest) = bufs.split_at_mut(1);
            let (b, c) = rest.split_at_mut(1);
            match (&mut a[0].0, &mut b[0].0, &mut c[0].0) {
                (Buf::I(y0), Buf::I(y1), Buf::I(y2)) => {
                    for i in 0..n {
                        let gg = y0[i] - ((y2[i] + y1[i]) >> 2);
                        let r = y2[i] + gg;
                        let bb = y1[i] + gg;
                        y0[i] = r;
                        y1[i] = gg;
                        y2[i] = bb;
                    }
                }
                (Buf::F(y0), Buf::F(y1), Buf::F(y2)) => {
                    for i in 0..n {
                        let (a0, a1, a2) = (y0[i], y1[i], y2[i]);
                        y0[i] = a0 + 1.402 * a2;
                        y1[i] = a0 - 0.344_136 * a1 - 0.714_136 * a2;
                        y2[i] = a0 + 1.772 * a1;
                    }
                }
                _ => {
                    return Err(unsupported(
                        "a multiple component transform over mixed wavelets",
                    ))
                }
            }
        }
    }
    // level shift, clamp and store
    for (c, (buf, w, h)) in bufs.iter().enumerate() {
        let ci = &s.comps[c];
        let tc = &tcs[c];
        let shift = if ci.signed { 0 } else { 1i32 << (ci.prec - 1) };
        let (lo, hi) = if ci.signed {
            (-(1i64 << (ci.prec - 1)), (1i64 << (ci.prec - 1)) - 1)
        } else {
            (0, (1i64 << ci.prec) - 1)
        };
        let comp = &mut out[c];
        let cx0 = ceil_div(u64::from(s.xo), u64::from(ci.dx)) as usize;
        let cy0 = ceil_div(u64::from(s.yo), u64::from(ci.dy)) as usize;
        for y in 0..*h {
            for x in 0..*w {
                let v = match buf {
                    Buf::I(v) => i64::from(v[y * w + x]) + i64::from(shift),
                    Buf::F(v) => (v[y * w + x].round() as i64) + i64::from(shift),
                };
                let v = v.clamp(lo, hi) as i32;
                let ox = tc.x0 + x - cx0;
                let oy = tc.y0 + y - cy0;
                if ox < comp.w && oy < comp.h {
                    comp.data[oy * comp.w + ox] = v;
                }
            }
        }
    }
    Ok(())
}
