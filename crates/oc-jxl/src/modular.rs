//! Modular mode: the meta-adaptive context tree, the predictors and the (inverse) transforms.
//!
//! The structure follows ISO/IEC 18181-1 annex H; `jxl-oxide` (MIT/Apache-2.0) served as the
//! reference for the details the specification text leaves open.
use crate::bits::{bad, unpack_signed, Br};
use crate::entropy::Decoder;
use oc_core::{Error, ErrorCode, Result};

/// One channel of a Modular image. `hs`/`vs` are the horizontal and vertical shifts (-1 for
/// channels that are not spatially related to the image, such as palettes).
#[derive(Clone, Debug)]
pub struct Chan {
    pub w: usize,
    pub h: usize,
    pub hs: i32,
    pub vs: i32,
    pub d: Vec<i32>,
}

impl Chan {
    pub fn info(w: usize, h: usize, hs: i32, vs: i32) -> Self {
        Self { w, h, hs, vs, d: Vec::new() }
    }

    pub fn alloc(&mut self) {
        self.d = vec![0; self.w * self.h];
    }
}

fn unsupported(msg: &str) -> Error {
    Error::new(ErrorCode::UnsupportedVariant, format!("JPEG XL: {msg}"))
}

// ---------------------------------------------------------------------------------------------
// Header

#[derive(Clone, Debug)]
pub struct WpHeader {
    p1: i64,
    p2: i64,
    p3: [i64; 5],
    w: [u32; 4],
}

impl WpHeader {
    fn parse(br: &mut Br) -> Result<Self> {
        if br.bool()? {
            return Ok(Self { p1: 16, p2: 10, p3: [7, 7, 7, 0, 0], w: [13, 12, 12, 12] });
        }
        let p1 = i64::from(br.read(5)?);
        let p2 = i64::from(br.read(5)?);
        let mut p3 = [0i64; 5];
        for v in &mut p3 {
            *v = i64::from(br.read(5)?);
        }
        let mut w = [0u32; 4];
        for v in &mut w {
            *v = br.read(4)?;
        }
        Ok(Self { p1, p2, p3, w })
    }
}

#[derive(Clone, Copy, Debug)]
struct SqueezeParam {
    horizontal: bool,
    in_place: bool,
    begin: usize,
    num: usize,
}

#[derive(Clone, Debug)]
enum Transform {
    Rct { begin: usize, ty: u32 },
    Palette { begin: usize, num_c: usize, nb_colours: usize, nb_deltas: i32, d_pred: u32 },
    Squeeze(Vec<SqueezeParam>),
}

const CHAN_BEGIN: [(u32, u32); 4] = [(0, 3), (8, 6), (72, 10), (1096, 13)];

impl Transform {
    fn parse(br: &mut Br) -> Result<Self> {
        match br.read(2)? {
            0 => {
                let begin = br.u32(CHAN_BEGIN)? as usize;
                let ty = br.u32([(6, 0), (0, 2), (2, 4), (10, 6)])?;
                if ty >= 42 {
                    return Err(bad("invalid RCT type"));
                }
                Ok(Self::Rct { begin, ty })
            }
            1 => {
                let begin = br.u32(CHAN_BEGIN)? as usize;
                let num_c = br.u32([(1, 0), (3, 0), (4, 0), (1, 13)])? as usize;
                let nb_colours = br.u32([(0, 8), (256, 10), (1280, 12), (5376, 16)])? as usize;
                let nb_deltas = br.u32([(0, 0), (1, 8), (257, 10), (1281, 16)])? as i32;
                let d_pred = br.read(4)?;
                if d_pred > 13 {
                    return Err(bad("invalid palette predictor"));
                }
                Ok(Self::Palette { begin, num_c, nb_colours, nb_deltas, d_pred })
            }
            2 => {
                let n = br.u32([(0, 0), (1, 4), (9, 6), (41, 8)])? as usize;
                let mut sp = Vec::with_capacity(n);
                for _ in 0..n {
                    let horizontal = br.bool()?;
                    let in_place = br.bool()?;
                    let begin = br.u32(CHAN_BEGIN)? as usize;
                    let num = br.u32([(1, 0), (2, 0), (3, 0), (4, 4)])? as usize;
                    sp.push(SqueezeParam { horizontal, in_place, begin, num });
                }
                Ok(Self::Squeeze(sp))
            }
            _ => Err(bad("invalid transform id")),
        }
    }

    /// Update the channel list as the forward transform would.
    fn apply_info(&mut self, ch: &mut Vec<Chan>, nb_meta: &mut usize) -> Result<()> {
        match self {
            Self::Rct { begin, .. } => {
                let end = *begin + 3;
                if end > ch.len() {
                    return Err(bad("RCT channels out of range"));
                }
                if ch[*begin + 1..end].iter().any(|c| c.w != ch[*begin].w || c.h != ch[*begin].h) {
                    return Err(bad("RCT channels differ in size"));
                }
            }
            Self::Palette { begin, num_c, nb_colours, .. } => {
                let end = *begin + *num_c;
                if end > ch.len() {
                    return Err(bad("palette channels out of range"));
                }
                if *begin < *nb_meta {
                    if end > *nb_meta {
                        return Err(bad("palette crosses the meta channels"));
                    }
                    *nb_meta = *nb_meta + 2 - *num_c;
                } else {
                    *nb_meta += 1;
                }
                if ch[*begin + 1..end].iter().any(|c| c.w != ch[*begin].w || c.h != ch[*begin].h) {
                    return Err(bad("palette channels differ in size"));
                }
                ch.drain(*begin + 1..end);
                ch.insert(0, Chan::info(*nb_colours, *num_c, -1, -1));
            }
            Self::Squeeze(sp) => {
                if sp.is_empty() {
                    *sp = default_squeeze(ch, *nb_meta);
                }
                for p in sp.iter() {
                    let end = p.begin + p.num;
                    if end > ch.len() {
                        return Err(bad("squeeze channels out of range"));
                    }
                    if p.begin < *nb_meta {
                        if !p.in_place || end > *nb_meta {
                            return Err(bad("squeeze crosses the meta channels"));
                        }
                        *nb_meta += p.num;
                    }
                    let mut residual = Vec::with_capacity(p.num);
                    for c in &mut ch[p.begin..end] {
                        if c.w == 0 || c.h == 0 || c.hs > 30 || c.vs > 30 {
                            return Err(bad("cannot squeeze this channel"));
                        }
                        let mut r = Chan::info(c.w, c.h, c.hs, c.vs);
                        if p.horizontal {
                            r.w = c.w / 2;
                            c.w = c.w.div_ceil(2);
                            if c.hs >= 0 {
                                c.hs += 1;
                                r.hs += 1;
                            }
                        } else {
                            r.h = c.h / 2;
                            c.h = c.h.div_ceil(2);
                            if c.vs >= 0 {
                                c.vs += 1;
                                r.vs += 1;
                            }
                        }
                        residual.push(r);
                    }
                    if p.in_place {
                        residual.extend(ch.drain(end..));
                    }
                    ch.extend(residual);
                }
            }
        }
        Ok(())
    }

    fn inverse(&self, ch: &mut Vec<Chan>, bit_depth: u32, wp: &WpHeader) -> Result<()> {
        match self {
            Self::Rct { begin, ty } => inverse_rct(ch, *begin, *ty),
            Self::Palette { begin, num_c, nb_colours, nb_deltas, d_pred } => {
                inverse_palette(ch, *begin, *num_c, *nb_colours, *nb_deltas, *d_pred, bit_depth, wp)
            }
            Self::Squeeze(sp) => {
                for p in sp.iter().rev() {
                    let end = p.begin + p.num;
                    let residuals: Vec<Chan> = if p.in_place {
                        ch.drain(end..end + p.num).collect()
                    } else {
                        let at = ch.len() - p.num;
                        ch.drain(at..).collect()
                    };
                    for (c, r) in ch[p.begin..end].iter_mut().zip(residuals) {
                        inverse_squeeze(c, &r, p.horizontal);
                    }
                }
                Ok(())
            }
        }
    }
}

fn default_squeeze(ch: &[Chan], nb_meta: usize) -> Vec<SqueezeParam> {
    let mut sp = Vec::new();
    if nb_meta >= ch.len() {
        return sp;
    }
    let first = nb_meta;
    let (mut w, mut h) = (ch[first].w, ch[first].h);
    if ch.len() - first >= 3 && ch[first + 1].w == w && ch[first + 1].h == h {
        let base = SqueezeParam { horizontal: false, in_place: false, begin: first + 1, num: 2 };
        sp.push(SqueezeParam { horizontal: true, ..base });
        sp.push(base);
    }
    let base = SqueezeParam { horizontal: false, in_place: true, begin: first, num: ch.len() - first };
    if h >= w && h > 8 {
        sp.push(base);
        h = h.div_ceil(2);
    }
    while w > 8 || h > 8 {
        if w > 8 {
            sp.push(SqueezeParam { horizontal: true, ..base });
            w = w.div_ceil(2);
        }
        if h > 8 {
            sp.push(base);
            h = h.div_ceil(2);
        }
    }
    sp
}

// ---------------------------------------------------------------------------------------------
// Inverse transforms

fn inverse_rct(ch: &mut [Chan], begin: usize, rct_type: u32) -> Result<()> {
    let permutation = rct_type / 7;
    let ty = rct_type % 7;
    let n = ch[begin].d.len();
    if ch[begin + 1].d.len() != n || ch[begin + 2].d.len() != n {
        return Err(bad("RCT channels differ in size"));
    }
    for i in 0..n {
        let a = ch[begin].d[i];
        let b = ch[begin + 1].d[i];
        let c = ch[begin + 2].d[i];
        let (d, e, f);
        if ty == 6 {
            let tmp = a.wrapping_sub(c >> 1);
            e = c.wrapping_add(tmp);
            f = tmp.wrapping_sub(b >> 1);
            d = f.wrapping_add(b);
        } else {
            d = a;
            f = if ty & 1 != 0 { c.wrapping_add(a) } else { c };
            e = match ty >> 1 {
                1 => b.wrapping_add(a),
                2 => b.wrapping_add(a.wrapping_add(f) >> 1),
                _ => b,
            };
        }
        ch[begin].d[i] = d;
        ch[begin + 1].d[i] = e;
        ch[begin + 2].d[i] = f;
    }
    let (x, rest) = ch[begin..begin + 3].split_at_mut(1);
    let (y, z) = rest.split_at_mut(1);
    let (a, b, c) = (&mut x[0].d, &mut y[0].d, &mut z[0].d);
    match permutation {
        1 => {
            a.swap_with_slice(b);
            a.swap_with_slice(c);
        }
        2 => {
            a.swap_with_slice(b);
            b.swap_with_slice(c);
        }
        3 => b.swap_with_slice(c),
        4 => a.swap_with_slice(b),
        5 => a.swap_with_slice(c),
        _ => {}
    }
    Ok(())
}

fn tendency(a: i32, b: i32, c: i32) -> i32 {
    let (a, b, c) = (i64::from(a), i64::from(b), i64::from(c));
    let r = if a >= b && b >= c {
        let mut x = (4 * a - 3 * c - b + 6) / 12;
        if x - (x & 1) > 2 * (a - b) {
            x = 2 * (a - b) + 1;
        }
        if x + (x & 1) > 2 * (b - c) {
            x = 2 * (b - c);
        }
        x
    } else if a <= b && b <= c {
        let mut x = (4 * a - 3 * c - b - 6) / 12;
        if x + (x & 1) < 2 * (a - b) {
            x = 2 * (a - b) - 1;
        }
        if x - (x & 1) < 2 * (b - c) {
            x = 2 * (b - c);
        }
        x
    } else {
        0
    };
    r as i32
}

/// Undo one squeeze step: `c` holds the averages and `r` the residuals.
fn inverse_squeeze(c: &mut Chan, r: &Chan, horizontal: bool) {
    let (aw, ah) = (c.w, c.h);
    let (w, h) = if horizontal { (aw + r.w, ah) } else { (aw, ah + r.h) };
    let mut out = vec![0i32; w * h];
    if horizontal {
        for y in 0..h {
            let avg = &c.d[y * aw..(y + 1) * aw];
            let res = &r.d[y * r.w..(y + 1) * r.w];
            let o = &mut out[y * w..(y + 1) * w];
            let mut left = avg[0];
            for x in 0..r.w {
                let a = avg[x];
                let next = avg.get(x + 1).copied().unwrap_or(a);
                let diff = res[x].wrapping_add(tendency(left, a, next));
                let first = a.wrapping_add(diff / 2);
                let second = first.wrapping_sub(diff);
                o[2 * x] = first;
                o[2 * x + 1] = second;
                left = second;
            }
            if w % 2 == 1 {
                o[w - 1] = avg[aw - 1];
            }
        }
        c.hs = (c.hs - 1).max(if c.hs < 0 { -1 } else { 0 });
    } else {
        for x in 0..w {
            let mut top = c.d[x];
            for y in 0..r.h {
                let a = c.d[y * aw + x];
                let next = if y + 1 < ah { c.d[(y + 1) * aw + x] } else { a };
                let diff = r.d[y * r.w + x].wrapping_add(tendency(top, a, next));
                let first = a.wrapping_add(diff / 2);
                let second = first.wrapping_sub(diff);
                out[2 * y * w + x] = first;
                out[(2 * y + 1) * w + x] = second;
                top = second;
            }
            if h % 2 == 1 {
                out[(h - 1) * w + x] = c.d[(ah - 1) * aw + x];
            }
        }
        c.vs = (c.vs - 1).max(if c.vs < 0 { -1 } else { 0 });
    }
    c.w = w;
    c.h = h;
    c.d = out;
}

#[rustfmt::skip]
const DELTA_PALETTE: [[i16; 3]; 72] = [
    [0, 0, 0], [4, 4, 4], [11, 0, 0], [0, 0, -13], [0, -12, 0], [-10, -10, -10],
    [-18, -18, -18], [-27, -27, -27], [-18, -18, 0], [0, 0, -32], [-32, 0, 0], [-37, -37, -37],
    [0, -32, -32], [24, 24, 45], [50, 50, 50], [-45, -24, -24], [-24, -45, -45], [0, -24, -24],
    [-34, -34, 0], [-24, 0, -24], [-45, -45, -24], [64, 64, 64], [-32, 0, -32], [0, -32, 0],
    [-32, 0, 32], [-24, -45, -24], [45, 24, 45], [24, -24, -45], [-45, -24, 24], [80, 80, 80],
    [64, 0, 0], [0, 0, -64], [0, -64, -64], [-24, -24, 45], [96, 96, 96], [64, 64, 0],
    [45, -24, -24], [34, -34, 0], [112, 112, 112], [24, -45, -45], [45, 45, -24], [0, -32, 32],
    [24, -24, 45], [0, 96, 96], [45, -24, 24], [24, -45, -24], [-24, -45, 24], [0, -64, 0],
    [96, 0, 0], [128, 128, 128], [64, 0, 64], [144, 144, 144], [96, 96, 0], [-36, -36, 36],
    [45, -24, -45], [45, -45, -24], [0, 0, -96], [0, 128, 128], [0, 96, 0], [45, 24, -45],
    [-128, 0, 0], [24, -45, 24], [-45, 24, -45], [64, 0, -64], [64, -64, -64], [96, 0, 96],
    [45, -45, 24], [24, 45, -45], [64, 64, -64], [128, 128, 0], [0, 0, -128], [-24, 45, -45],
];

#[allow(clippy::too_many_arguments)]
fn inverse_palette(
    ch: &mut Vec<Chan>,
    begin: usize,
    num_c: usize,
    nb_colours: usize,
    nb_deltas: i32,
    d_pred: u32,
    bit_depth: u32,
    wp_hdr: &WpHeader,
) -> Result<()> {
    let palette = ch.remove(0);
    let (w, h) = (ch[begin].w, ch[begin].h);
    let (hs, vs) = (ch[begin].hs, ch[begin].vs);
    for c in 1..num_c {
        let mut extra = Chan::info(w, h, hs, vs);
        extra.d = vec![0; w * h];
        ch.insert(begin + c, extra);
    }
    let nb_colors = nb_colours as i32;
    let bd = bit_depth.clamp(1, 31);
    let mut need_delta: Vec<usize> = Vec::new();
    for i in 0..w * h {
        let index = ch[begin].d[i];
        if index < nb_deltas {
            need_delta.push(i);
        }
        for c in 0..num_c {
            let v = if (0..nb_colors).contains(&index) {
                palette.d[c * nb_colours + index as usize]
            } else if index >= nb_colors {
                let idx = index - nb_colors;
                if idx < 64 {
                    ((idx >> (2 * c)) % 4) * ((1i32 << bd) - 1) / 4 + (1i32 << bd.saturating_sub(3))
                } else {
                    let mut idx = idx - 64;
                    for _ in 0..c {
                        idx /= 5;
                    }
                    (idx % 5) * ((1i32 << bd) - 1) / 4
                }
            } else if c >= 3 {
                0
            } else {
                let idx = (-(i64::from(index) + 1) % 143) as usize;
                let mut t = i32::from(DELTA_PALETTE[(idx + 1) >> 1][c]);
                if idx & 1 == 0 {
                    t = -t;
                }
                if bd > 8 {
                    t <<= bd.min(24) - 8;
                }
                t
            };
            ch[begin + c].d[i] = v;
        }
    }
    if need_delta.is_empty() {
        return Ok(());
    }
    for c in 0..num_c {
        let cur = &mut ch[begin + c];
        let mut wp = (d_pred == 6).then(|| Wp::new(w, wp_hdr.clone()));
        let mut next = 0;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let nb = neighbours(&cur.d, w, x, y);
                let pr = wp.as_ref().map(|wp| wp.predict(&nb));
                let mut v = cur.d[i];
                if need_delta.get(next) == Some(&i) {
                    let sc = pr.as_ref().map_or(0, |p| p.prediction);
                    v = v.wrapping_add(predict(d_pred, &nb, sc));
                    cur.d[i] = v;
                    next += 1;
                }
                if let (Some(wp), Some(pr)) = (wp.as_mut(), pr) {
                    wp.record(pr, v);
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Prediction

#[derive(Clone, Copy)]
struct Nb {
    n: i32,
    w: i32,
    nw: i32,
    ne: i32,
    nn: i32,
    ww: i32,
    nee: i32,
}

#[inline]
fn neighbours(d: &[i32], width: usize, x: usize, y: usize) -> Nb {
    let i = y * width + x;
    let n = if y > 0 {
        d[i - width]
    } else if x > 0 {
        d[i - 1]
    } else {
        0
    };
    let w = if x > 0 {
        d[i - 1]
    } else if y > 0 {
        d[i - width]
    } else {
        0
    };
    let nw = if x > 0 && y > 0 { d[i - width - 1] } else { w };
    let ne = if y > 0 && x + 1 < width { d[i - width + 1] } else { n };
    let nn = if y > 1 { d[i - 2 * width] } else { n };
    let ww = if x > 1 { d[i - 2] } else { w };
    let nee = if y > 0 && x + 2 < width { d[i - width + 2] } else { ne };
    Nb { n, w, nw, ne, nn, ww, nee }
}

fn grad_clamped(n: i32, w: i32, nw: i32) -> i32 {
    let (n, w, nw) = (i64::from(n), i64::from(w), i64::from(nw));
    (n + w - nw).clamp(w.min(n), w.max(n)) as i32
}

/// `sc` is the self-correcting prediction (in 1/8 units) when the weighted predictor runs.
fn predict(p: u32, nb: &Nb, sc: i64) -> i32 {
    let (n, w, nw) = (i64::from(nb.n), i64::from(nb.w), i64::from(nb.nw));
    match p {
        0 => 0,
        1 => nb.w,
        2 => nb.n,
        3 => ((w + n) / 2) as i32,
        4 => {
            if nb.n.abs_diff(nb.nw) < nb.w.abs_diff(nb.nw) {
                nb.w
            } else {
                nb.n
            }
        }
        5 => grad_clamped(nb.n, nb.w, nb.nw),
        6 => ((sc + 3) >> 3) as i32,
        7 => nb.ne,
        8 => nb.nw,
        9 => nb.ww,
        10 => ((w + nw) / 2) as i32,
        11 => ((n + nw) / 2) as i32,
        12 => ((n + i64::from(nb.ne)) / 2) as i32,
        _ => {
            let (nn, ww, nee, ne) = (i64::from(nb.nn), i64::from(nb.ww), i64::from(nb.nee), i64::from(nb.ne));
            ((6 * n - 2 * nn + 7 * w + ww + nee + 3 * ne + 8) / 16) as i32
        }
    }
}

const DIV_LOOKUP: [u32; 65] = {
    let mut out = [0u32; 65];
    let mut i = 1usize;
    while i <= 64 {
        out[i] = ((1 << 24) / i) as u32;
        i += 1;
    }
    out
};

struct WpResult {
    prediction: i64,
    max_error: i32,
    subpred: [i64; 4],
}

/// The self-correcting ("weighted") predictor with its running error state.
struct Wp {
    width: usize,
    x: usize,
    y: usize,
    true_err_row: Vec<i32>,
    subpred_err_row: Vec<[u32; 4]>,
    hdr: WpHeader,
    true_err_w: i32,
    true_err_nw: i32,
    true_err_n: i32,
    true_err_ne: i32,
    sub_nw_ww: [u32; 4],
    sub_n_w: [u32; 4],
    sub_ne: [u32; 4],
}

impl Wp {
    fn new(width: usize, hdr: WpHeader) -> Self {
        Self {
            width,
            x: 0,
            y: 0,
            true_err_row: vec![0; width],
            subpred_err_row: vec![[0; 4]; width],
            hdr,
            true_err_w: 0,
            true_err_nw: 0,
            true_err_n: 0,
            true_err_ne: 0,
            sub_nw_ww: [0; 4],
            sub_n_w: [0; 4],
            sub_ne: [0; 4],
        }
    }

    fn predict(&self, nb: &Nb) -> WpResult {
        let wp = &self.hdr;
        let (tw, tnw, tn, tne) = (
            i64::from(self.true_err_w),
            i64::from(self.true_err_nw),
            i64::from(self.true_err_n),
            i64::from(self.true_err_ne),
        );
        let n3 = i64::from(nb.n) << 3;
        let nw3 = i64::from(nb.nw) << 3;
        let ne3 = i64::from(nb.ne) << 3;
        let w3 = i64::from(nb.w) << 3;
        let nn3 = i64::from(nb.nn) << 3;
        let subpred = [
            w3 + ne3 - n3,
            n3 - (((tw + tn + tne) * wp.p1) >> 5),
            w3 - (((tw + tn + tnw) * wp.p2) >> 5),
            n3 - ((tnw * wp.p3[0] + tn * wp.p3[1] + tne * wp.p3[2] + (nn3 - n3) * wp.p3[3] + (nw3 - w3) * wp.p3[4])
                >> 5),
        ];
        let mut weight = [0u32; 4];
        for i in 0..4 {
            let err_sum = self.sub_nw_ww[i].wrapping_add(self.sub_n_w[i]).wrapping_add(self.sub_ne[i]);
            let shift = ((u64::from(err_sum) + 1) >> 5).checked_ilog2().unwrap_or(0);
            weight[i] = 4 + ((wp.w[i] * DIV_LOOKUP[((err_sum >> shift) as usize + 1).min(64)]) >> shift);
        }
        let sum: u32 = weight.iter().sum();
        let log_weight = (u64::from(sum) >> 4).ilog2();
        for w in &mut weight {
            *w >>= log_weight;
        }
        let sum: u32 = weight.iter().sum();
        let mut s = (i64::from(sum) >> 1) - 1;
        for (p, w) in subpred.iter().zip(weight) {
            s += p * i64::from(w);
        }
        let mut prediction = (s * i64::from(DIV_LOOKUP[(sum as usize).min(64)])) >> 24;
        if (tn ^ tw) | (tn ^ tnw) <= 0 {
            prediction = prediction.clamp(n3.min(w3).min(ne3), n3.max(w3).max(ne3));
        }
        let mut max_error = tw;
        for e in [tn, tnw, tne] {
            if e.abs() > max_error.abs() {
                max_error = e;
            }
        }
        WpResult { prediction, max_error: max_error as i32, subpred }
    }

    fn record(&mut self, pred: WpResult, sample: i32) {
        let sample = i64::from(sample);
        let true_err = pred.prediction - (sample << 3);
        let mut sub_err = [0u32; 4];
        for (e, p) in sub_err.iter_mut().zip(pred.subpred) {
            *e = ((p.abs_diff(sample << 3) + 3) >> 3) as u32;
        }
        self.true_err_row[self.x] = true_err as i32;
        self.subpred_err_row[self.x] = sub_err;
        self.x += 1;
        if self.x >= self.width {
            self.y += 1;
            self.x = 0;
            self.true_err_w = 0;
            self.true_err_n = self.true_err_row[0];
            self.true_err_nw = self.true_err_n;
            self.sub_n_w = self.subpred_err_row[0];
            self.sub_nw_ww = self.sub_n_w;
            if self.width <= 1 {
                self.true_err_ne = self.true_err_n;
                self.sub_ne = self.sub_n_w;
            } else {
                self.true_err_ne = self.true_err_row[1];
                self.sub_ne = self.subpred_err_row[1];
            }
        } else {
            self.true_err_w = true_err as i32;
            self.true_err_nw = self.true_err_n;
            self.true_err_n = self.true_err_ne;
            self.sub_nw_ww = self.sub_n_w;
            self.sub_n_w = self.sub_ne;
            for (a, b) in self.sub_n_w.iter_mut().zip(sub_err) {
                *a = a.wrapping_add(b);
            }
            if self.x + 1 >= self.width {
                self.true_err_ne = self.true_err_n;
                self.sub_ne = self.sub_n_w;
            } else if self.y != 0 {
                let x = self.x;
                self.true_err_ne = self.true_err_row[x + 1];
                self.sub_ne = self.subpred_err_row[x + 1];
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The meta-adaptive tree

#[derive(Clone, Debug)]
enum Node {
    Decision { prop: u32, value: i32, first: u32, second: u32 },
    Leaf { cluster: u8, predictor: u32, offset: i32, multiplier: i32 },
}

#[derive(Clone, Debug)]
pub struct MaTree {
    nodes: Vec<Node>,
    decoder: Decoder,
    need_wp: bool,
}

impl MaTree {
    pub fn parse(br: &mut Br, node_limit: usize) -> Result<Self> {
        let mut tree = Decoder::parse(br, 6)?;
        tree.begin(br)?;
        let mut raw: Vec<Node> = Vec::new();
        let mut leaf_ctx: Vec<u32> = Vec::new();
        let mut planned = 1usize;
        let mut ctx = 0u32;
        while raw.len() < planned {
            if raw.len() > node_limit {
                return Err(bad("meta-adaptive tree is too large"));
            }
            let prop = tree.read(br, 1)?;
            if prop > 0 {
                let value = unpack_signed(tree.read(br, 0)?);
                raw.push(Node::Decision { prop: prop - 1, value, first: planned as u32, second: planned as u32 + 1 });
                planned += 2;
            } else {
                let predictor = tree.read(br, 2)?;
                if predictor > 13 {
                    return Err(bad("invalid tree predictor"));
                }
                let offset = unpack_signed(tree.read(br, 3)?);
                let mul_log = tree.read(br, 4)?;
                if mul_log > 30 {
                    return Err(bad("invalid tree multiplier"));
                }
                let mul_bits = tree.read(br, 5)?;
                if mul_bits > (1 << (31 - mul_log)) - 2 {
                    return Err(bad("invalid tree multiplier"));
                }
                raw.push(Node::Leaf {
                    cluster: 0,
                    predictor,
                    offset,
                    multiplier: ((mul_bits + 1) << mul_log) as i32,
                });
                leaf_ctx.push(ctx);
                ctx += 1;
            }
        }
        tree.finalize()?;
        let decoder = Decoder::parse(br, ctx)?;
        let mut need_wp = false;
        let mut li = 0;
        for n in &mut raw {
            match n {
                Node::Leaf { cluster, predictor, .. } => {
                    *cluster = decoder.clusters()[leaf_ctx[li] as usize];
                    li += 1;
                    need_wp |= *predictor == 6;
                }
                Node::Decision { prop, .. } => need_wp |= *prop == 15,
            }
        }
        Ok(Self { nodes: raw, decoder, need_wp })
    }
}

// ---------------------------------------------------------------------------------------------
// Streams

/// One Modular bitstream (the global image, a group, or an embedded image): its header, tree and
/// channels.
pub struct Stream {
    transforms: Vec<Transform>,
    wp: WpHeader,
    local_tree: Option<MaTree>,
    pub chans: Vec<Chan>,
    pub nb_meta: usize,
}

fn tree_of<'a>(local: &'a Option<MaTree>, global: Option<&'a MaTree>) -> Result<&'a MaTree> {
    local.as_ref().or(global).ok_or_else(|| bad("the global meta-adaptive tree is missing"))
}

impl Stream {
    /// Read the header (and the local tree, if there is one) for a stream whose channels, before
    /// transforms, are `input`.
    pub fn parse(br: &mut Br, global: Option<&MaTree>, input: Vec<Chan>, max_samples: u64) -> Result<Self> {
        let use_global = br.bool()?;
        let wp = WpHeader::parse(br)?;
        let n = br.u32([(0, 0), (1, 0), (2, 4), (18, 8)])? as usize;
        if n > 512 {
            return Err(bad("too many transforms"));
        }
        let mut transforms = Vec::with_capacity(n);
        for _ in 0..n {
            transforms.push(Transform::parse(br)?);
        }
        let mut chans = input;
        let mut nb_meta = 0usize;
        for t in &mut transforms {
            t.apply_info(&mut chans, &mut nb_meta)?;
            if chans.len() > 1 << 16 {
                return Err(bad("too many channels"));
            }
        }
        let samples: u64 = chans.iter().map(|c| c.w as u64 * c.h as u64).sum();
        if samples > max_samples {
            return Err(Error::limit("pixels", "JPEG XL: modular image is too large"));
        }
        let local_tree = if use_global {
            if global.is_none() {
                return Err(bad("the global meta-adaptive tree is missing"));
            }
            None
        } else {
            Some(MaTree::parse(br, (1024 + samples).min(1 << 20) as usize)?)
        };
        for c in &mut chans {
            c.alloc();
        }
        Ok(Self { transforms, wp, local_tree, chans, nb_meta })
    }

    /// How many leading channels belong to the global section: the meta channels and those that
    /// fit in a group.
    pub fn global_count(&self, group_dim: usize) -> usize {
        self.chans
            .iter()
            .enumerate()
            .take_while(|&(i, c)| i < self.nb_meta || (c.w <= group_dim && c.h <= group_dim))
            .count()
    }

    /// Decode the first `count` channels.
    pub fn decode(&mut self, br: &mut Br, global: Option<&MaTree>, count: usize, stream_idx: u32) -> Result<()> {
        let tree = tree_of(&self.local_tree, global)?;
        let mut dec = tree.decoder.clone();
        dec.begin(br)?;
        let dist_mult = self.chans[..count].iter().map(|c| c.w).max().unwrap_or(0) as u32;
        for ci in 0..count {
            let (before, rest) = self.chans.split_at_mut(ci);
            let cur = &mut rest[0];
            if cur.w == 0 || cur.h == 0 {
                continue;
            }
            // Earlier channels of identical geometry, most recent first.
            let prev: Vec<&Chan> = before
                .iter()
                .rev()
                .filter(|p| p.w == cur.w && p.h == cur.h && p.hs == cur.hs && p.vs == cur.vs)
                .collect();
            decode_channel(br, &mut dec, tree, &self.wp, &prev, cur, ci as u32, stream_idx, dist_mult)?;
        }
        dec.finalize()
    }

    /// Undo the transforms.
    pub fn finish(mut self, bit_depth: u32) -> Result<Vec<Chan>> {
        for t in self.transforms.iter().rev() {
            t.inverse(&mut self.chans, bit_depth, &self.wp)?;
        }
        Ok(self.chans)
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_channel(
    br: &mut Br,
    dec: &mut Decoder,
    tree: &MaTree,
    wp_hdr: &WpHeader,
    prev: &[&Chan],
    cur: &mut Chan,
    channel: u32,
    stream: u32,
    dist_mult: u32,
) -> Result<()> {
    let (width, height) = (cur.w, cur.h);
    let mut wp = tree.need_wp.then(|| Wp::new(width, wp_hdr.clone()));
    for y in 0..height {
        let mut prev_grad = 0i32;
        for x in 0..width {
            let nb = neighbours(&cur.d, width, x, y);
            let wp_res = wp.as_ref().map(|wp| wp.predict(&nb));
            let max_err = wp_res.as_ref().map_or(0, |r| r.max_error);
            let prop = |p: u32| -> i32 {
                match p {
                    0 => channel as i32,
                    1 => stream as i32,
                    2 => y as i32,
                    3 => x as i32,
                    4 => nb.n.unsigned_abs() as i32,
                    5 => nb.w.unsigned_abs() as i32,
                    6 => nb.n,
                    7 => nb.w,
                    8 => nb.w.wrapping_sub(prev_grad),
                    9 => nb.w.wrapping_sub(nb.nw).wrapping_add(nb.n),
                    10 => nb.w.wrapping_sub(nb.nw),
                    11 => nb.nw.wrapping_sub(nb.n),
                    12 => nb.n.wrapping_sub(nb.ne),
                    13 => nb.n.wrapping_sub(nb.nn),
                    14 => nb.w.wrapping_sub(nb.ww),
                    15 => max_err,
                    k => {
                        let k = (k - 16) as usize;
                        let Some(pc) = prev.get(k / 4) else { return 0 };
                        let c = pc.d[y * width + x];
                        match k % 4 {
                            0 => c.wrapping_abs(),
                            1 => c,
                            sel => {
                                let g = match (x, y) {
                                    (0, 0) => 0,
                                    (0, y) => pc.d[(y - 1) * width],
                                    (x, 0) => pc.d[x - 1],
                                    (x, y) => grad_clamped(
                                        pc.d[(y - 1) * width + x],
                                        pc.d[y * width + x - 1],
                                        pc.d[(y - 1) * width + x - 1],
                                    ),
                                };
                                if sel == 2 {
                                    c.abs_diff(g) as i32
                                } else {
                                    c.wrapping_sub(g)
                                }
                            }
                        }
                    }
                }
            };
            let mut at = 0usize;
            let (cluster, predictor, offset, multiplier) = loop {
                match &tree.nodes[at] {
                    Node::Decision { prop: p, value, first, second } => {
                        at = if prop(*p) > *value { *first as usize } else { *second as usize };
                    }
                    Node::Leaf { cluster, predictor, offset, multiplier } => {
                        break (*cluster, *predictor, *offset, *multiplier)
                    }
                }
            };
            let token = dec.read_clustered(br, cluster, dist_mult)?;
            let diff = unpack_signed(token).wrapping_mul(multiplier).wrapping_add(offset);
            let sc = wp_res.as_ref().map_or(0, |r| r.prediction);
            let value = diff.wrapping_add(predict(predictor, &nb, sc));
            cur.d[y * width + x] = value;
            prev_grad = nb.w.wrapping_sub(nb.nw).wrapping_add(nb.n);
            if let (Some(wp), Some(res)) = (wp.as_mut(), wp_res) {
                wp.record(res, value);
            }
        }
    }
    let _ = unsupported;
    Ok(())
}
