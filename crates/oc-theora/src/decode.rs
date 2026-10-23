//! Frame decoding (spec chapter 7): flags, modes, motion vectors, coefficients, DC prediction,
//! reconstruction and the loop filter. Planes are kept as the spec has them, with row 0 at the
//! bottom, and flipped only when a frame is handed out.
use crate::bits::Bits;
use crate::header::{Info, Setup};
use crate::idct;
use crate::layout::Layout;
use oc_core::{Error, Limits, Result};
use std::sync::Arc;

fn bad(msg: &str) -> Error {
    Error::corrupt(format!("Theora: {msg}"))
}

/// Zig-zag index of each coefficient in natural (row-major) order (figure 2.8).
const ZIGZAG: [u8; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, //
    2, 4, 7, 13, 16, 26, 29, 42, //
    3, 8, 12, 17, 25, 30, 41, 43, //
    9, 11, 18, 24, 31, 40, 44, 53, //
    10, 19, 23, 32, 39, 45, 52, 54, //
    20, 22, 33, 38, 46, 51, 55, 60, //
    21, 34, 37, 47, 50, 56, 59, 61, //
    35, 36, 48, 49, 57, 58, 62, 63,
];

/// Natural index of each zig-zag position.
const UNZIGZAG: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[ZIGZAG[i] as usize] = i as u8;
        i += 1;
    }
    t
};

/// Reference frame used by each coding mode (table 7.46): 0 none, 1 previous, 2 golden.
const REF_OF_MODE: [u8; 8] = [1, 0, 1, 1, 1, 2, 2, 1];

const MODE_INTER_NOMV: u8 = 0;
const MODE_INTRA: u8 = 1;
const MODE_INTER_MV: u8 = 2;
const MODE_INTER_MV_LAST: u8 = 3;
const MODE_INTER_MV_LAST2: u8 = 4;
const MODE_GOLDEN_MV: u8 = 6;
const MODE_INTER_MV_FOUR: u8 = 7;

/// Mode alphabets for schemes 1 to 6 (table 7.19).
const SCHEMES: [[u8; 8]; 6] = [
    [3, 4, 2, 0, 1, 5, 6, 7],
    [3, 4, 0, 2, 1, 5, 6, 7],
    [3, 2, 4, 0, 1, 5, 6, 7],
    [3, 2, 0, 4, 1, 5, 6, 7],
    [0, 3, 4, 2, 1, 5, 6, 7],
    [0, 5, 3, 4, 2, 1, 6, 7],
];

/// DC predictor weights and divisors by the set of usable neighbours (table 7.47); the index
/// has one bit each for left, lower-left, lower and lower-right.
const DC_WEIGHTS: [([i32; 4], i32); 16] = [
    ([0, 0, 0, 0], 1),
    ([1, 0, 0, 0], 1),
    ([0, 1, 0, 0], 1),
    ([1, 0, 0, 0], 1),
    ([0, 0, 1, 0], 1),
    ([1, 0, 1, 0], 2),
    ([0, 0, 1, 0], 1),
    ([29, -26, 29, 0], 32),
    ([0, 0, 0, 1], 1),
    ([75, 0, 0, 53], 128),
    ([0, 1, 0, 1], 2),
    ([75, 0, 0, 53], 128),
    ([0, 0, 1, 0], 1),
    ([75, 0, 0, 53], 128),
    ([0, 3, 10, 3], 16),
    ([29, -26, 29, 0], 32),
];

/// The three planes of a frame, bottom row first.
#[derive(Debug)]
struct Planes {
    p: [Vec<u8>; 3],
}

/// A decoded picture, cropped to the picture region, rows from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// 0 is 4:2:0, 2 is 4:2:2, 3 is 4:4:4.
    pub pixel_format: u8,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    pub chroma_width: usize,
    pub chroma_height: usize,
    pub key: bool,
}

pub struct Decoder {
    info: Info,
    setup: Setup,
    lay: Layout,
    prev: Option<Arc<Planes>>,
    golden: Option<Arc<Planes>>,
}

impl Decoder {
    pub fn new(info: Info, setup: Setup, limits: &Limits) -> Result<Self> {
        let (w, h) = (info.fmbw * 16, info.fmbh * 16);
        let px = limits.check_pixels(w, h)?;
        limits.check_alloc(px * 3 * 4)?;
        let lay = Layout::new(info.pixel_format, info.fmbw as usize, info.fmbh as usize);
        Ok(Self {
            info,
            setup,
            lay,
            prev: None,
            golden: None,
        })
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    /// Decode one data packet. An empty packet repeats the previous frame.
    pub fn decode(&mut self, pkt: &[u8]) -> Result<Frame> {
        if pkt.is_empty() {
            let prev = self
                .prev
                .clone()
                .ok_or_else(|| bad("the first packet is not a key frame"))?;
            return Ok(self.output(&prev, false));
        }
        let lay = &self.lay;
        let nbs = lay.nbs;
        let mut b = Bits::new(pkt);
        if b.bit() != 0 {
            return Err(bad("not a data packet"));
        }
        let ftype = b.bit();
        let mut qis = [0usize; 3];
        qis[0] = b.read(6) as usize;
        let mut nqis = 1usize;
        if b.bit() == 1 {
            qis[1] = b.read(6) as usize;
            nqis = 2;
            if b.bit() == 1 {
                qis[2] = b.read(6) as usize;
                nqis = 3;
            }
        }
        if ftype == 0 && b.read(3) != 0 {
            return Err(bad("reserved frame header bits are set"));
        }
        let intra = ftype == 0;
        if !intra && self.prev.is_none() {
            return Err(bad("the first packet is not a key frame"));
        }

        // 7.3 coded block flags.
        let mut bcoded = vec![true; nbs];
        if !intra {
            bcoded = coded_flags(&mut b, lay)?;
        }

        // 7.4 macro block modes.
        let mut modes = vec![MODE_INTRA; lay.nmbs];
        if !intra {
            let scheme = b.read(3) as usize;
            let mut alphabet = [0u8; 8];
            match scheme {
                0 => {
                    for mode in 0..8u8 {
                        alphabet[b.read(3) as usize] = mode;
                    }
                }
                1..=6 => alphabet = SCHEMES[scheme - 1],
                _ => {}
            }
            for mbi in 0..lay.nmbs {
                let luma = &lay.mb_luma[mbi];
                modes[mbi] = if luma.iter().any(|&bi| bcoded[bi as usize]) {
                    if scheme == 7 {
                        b.read(3) as u8
                    } else {
                        let mut k = 0;
                        while k < 7 && b.bit() == 1 {
                            k += 1;
                        }
                        alphabet[k]
                    }
                } else {
                    MODE_INTER_NOMV
                };
            }
        }

        // 7.5 motion vectors.
        let mut mvs = vec![(0i32, 0i32); nbs];
        if !intra {
            self.motion_vectors(&mut b, &bcoded, &modes, &mut mvs);
        }

        // 7.6 block-level qi.
        let mut qiis = vec![0u8; nbs];
        for qii in 0..nqis.saturating_sub(1) {
            let n = (0..nbs)
                .filter(|&bi| bcoded[bi] && qiis[bi] as usize == qii)
                .count();
            let bits = long_runs(&mut b, n)?;
            let mut it = bits.into_iter();
            for bi in 0..nbs {
                if bcoded[bi] && qiis[bi] as usize == qii && it.next() == Some(true) {
                    qiis[bi] += 1;
                }
            }
        }

        // 7.7 DCT coefficients.
        let mut coeffs = vec![0i16; nbs * 64];
        let mut ncoeffs = vec![0u8; nbs];
        self.coefficients(&mut b, &bcoded, &mut coeffs, &mut ncoeffs)?;

        // 7.8 undo DC prediction.
        self.undo_dc_prediction(&bcoded, &modes, &mut coeffs);

        // 7.9 reconstruction and 7.10 loop filter.
        let mut rec = Planes {
            p: [
                vec![0; lay.pw[0] * lay.ph[0]],
                vec![0; lay.pw[1] * lay.ph[1]],
                vec![0; lay.pw[2] * lay.ph[2]],
            ],
        };
        self.reconstruct(
            &mut rec, &bcoded, &modes, &mvs, &coeffs, &ncoeffs, &qis, &qiis,
        );
        self.loop_filter(&mut rec, &bcoded, qis[0]);

        let rec = Arc::new(rec);
        if intra {
            self.golden = Some(rec.clone());
        }
        self.prev = Some(rec.clone());
        Ok(self.output(&rec, intra))
    }

    fn motion_vectors(&self, b: &mut Bits, bcoded: &[bool], modes: &[u8], mvs: &mut [(i32, i32)]) {
        let lay = &self.lay;
        let mvmode = b.bit();
        let mut last1 = (0i32, 0i32);
        let mut last2 = (0i32, 0i32);
        let component = |b: &mut Bits| -> i32 {
            if mvmode == 0 {
                match b.read(3) {
                    0 => 0,
                    1 => 1,
                    2 => -1,
                    3 => {
                        let s = b.bit();
                        if s == 0 {
                            2
                        } else {
                            -2
                        }
                    }
                    4 => {
                        let s = b.bit();
                        if s == 0 {
                            3
                        } else {
                            -3
                        }
                    }
                    5 => {
                        let v = 4 + b.read(2) as i32;
                        if b.bit() == 0 {
                            v
                        } else {
                            -v
                        }
                    }
                    6 => {
                        let v = 8 + b.read(3) as i32;
                        if b.bit() == 0 {
                            v
                        } else {
                            -v
                        }
                    }
                    _ => {
                        let v = 16 + b.read(4) as i32;
                        if b.bit() == 0 {
                            v
                        } else {
                            -v
                        }
                    }
                }
            } else {
                let v = b.read(5) as i32;
                if b.bit() == 1 {
                    -v
                } else {
                    v
                }
            }
        };
        let chroma_n = lay.chroma_per_mb();
        for mbi in 0..lay.nmbs {
            let luma = lay.mb_luma[mbi];
            let chroma = lay.mb_chroma[mbi];
            let mode = modes[mbi];
            let mut mv = (0, 0);
            match mode {
                MODE_INTER_MV_FOUR => {
                    // Luma blocks A, B, C, D are lower-left, lower-right, upper-left, upper-right.
                    let mut four = [(0i32, 0i32); 4];
                    for k in 0..4 {
                        if bcoded[luma[k] as usize] {
                            let x = component(b);
                            let y = component(b);
                            four[k] = (x, y);
                            mv = (x, y);
                        }
                        mvs[luma[k] as usize] = four[k];
                    }
                    let avg = |s: i32, n: i32| -> i32 {
                        // round(s / n), ties away from zero.
                        if s >= 0 {
                            (2 * s + n) / (2 * n)
                        } else {
                            -((2 * -s + n) / (2 * n))
                        }
                    };
                    match lay.pf {
                        0 => {
                            let v = (
                                avg(four.iter().map(|m| m.0).sum(), 4),
                                avg(four.iter().map(|m| m.1).sum(), 4),
                            );
                            mvs[chroma[0] as usize] = v;
                            mvs[chroma[1] as usize] = v;
                        }
                        2 => {
                            let lo = (avg(four[0].0 + four[1].0, 2), avg(four[0].1 + four[1].1, 2));
                            let hi = (avg(four[2].0 + four[3].0, 2), avg(four[2].1 + four[3].1, 2));
                            mvs[chroma[0] as usize] = lo;
                            mvs[chroma[2] as usize] = lo;
                            mvs[chroma[1] as usize] = hi;
                            mvs[chroma[3] as usize] = hi;
                        }
                        _ => {
                            for k in 0..4 {
                                mvs[chroma[k] as usize] = four[k];
                                mvs[chroma[4 + k] as usize] = four[k];
                            }
                        }
                    }
                    last2 = last1;
                    last1 = mv;
                    continue;
                }
                MODE_GOLDEN_MV => {
                    let x = component(b);
                    let y = component(b);
                    mv = (x, y);
                }
                MODE_INTER_MV_LAST2 => {
                    mv = last2;
                    last2 = last1;
                    last1 = mv;
                }
                MODE_INTER_MV_LAST => mv = last1,
                MODE_INTER_MV => {
                    let x = component(b);
                    let y = component(b);
                    mv = (x, y);
                    last2 = last1;
                    last1 = mv;
                }
                _ => {}
            }
            for &bi in luma.iter().chain(&chroma[..chroma_n]) {
                mvs[bi as usize] = mv;
            }
        }
    }

    fn coefficients(
        &self,
        b: &mut Bits,
        bcoded: &[bool],
        coeffs: &mut [i16],
        ncoeffs: &mut [u8],
    ) -> Result<()> {
        let lay = &self.lay;
        let nbs = lay.nbs;
        let nlbs = lay.nmbs * 4;
        let mut tis = vec![0u8; nbs];
        let mut eobs = 0usize;
        let (mut htl, mut htc) = (0usize, 0usize);
        for ti in 0..64usize {
            if ti < 2 {
                htl = b.read(4) as usize;
                htc = b.read(4) as usize;
            }
            let hg = match ti {
                0 => 0,
                1..=5 => 1,
                6..=14 => 2,
                15..=27 => 3,
                _ => 4,
            };
            for bi in 0..nbs {
                if !bcoded[bi] || tis[bi] as usize != ti {
                    continue;
                }
                ncoeffs[bi] = ti as u8;
                if eobs > 0 {
                    tis[bi] = 64;
                    eobs -= 1;
                    continue;
                }
                let hti = 16 * hg + if bi < nlbs { htl } else { htc };
                let token = self.setup.trees[hti].decode(b);
                if token < 7 {
                    eobs = match token {
                        0 => 1,
                        1 => 2,
                        2 => 3,
                        3 => b.read(2) as usize + 4,
                        4 => b.read(3) as usize + 8,
                        5 => b.read(4) as usize + 16,
                        _ => {
                            let n = b.read(12) as usize;
                            if n == 0 {
                                (0..nbs).filter(|&j| bcoded[j] && tis[j] < 64).count()
                            } else {
                                n
                            }
                        }
                    };
                    ncoeffs[bi] = tis[bi];
                    tis[bi] = 64;
                    eobs -= 1;
                    continue;
                }
                // Coefficient tokens: zero run, then an optional value.
                let (run, value, counts): (usize, i32, bool) = match token {
                    7 => (b.read(3) as usize + 1, 0, false),
                    8 => (b.read(6) as usize + 1, 0, false),
                    9 => (0, 1, true),
                    10 => (0, -1, true),
                    11 => (0, 2, true),
                    12 => (0, -2, true),
                    13..=16 => {
                        let s = b.bit();
                        (0, signed(s, token as i32 - 10), true)
                    }
                    17 => {
                        let s = b.bit();
                        (0, signed(s, b.read(1) as i32 + 7), true)
                    }
                    18 => {
                        let s = b.bit();
                        (0, signed(s, b.read(2) as i32 + 9), true)
                    }
                    19 => {
                        let s = b.bit();
                        (0, signed(s, b.read(3) as i32 + 13), true)
                    }
                    20 => {
                        let s = b.bit();
                        (0, signed(s, b.read(4) as i32 + 21), true)
                    }
                    21 => {
                        let s = b.bit();
                        (0, signed(s, b.read(5) as i32 + 37), true)
                    }
                    22 => {
                        let s = b.bit();
                        (0, signed(s, b.read(9) as i32 + 69), true)
                    }
                    23..=27 => {
                        let s = b.bit();
                        (token as usize - 22, signed(s, 1), true)
                    }
                    28 => {
                        let s = b.bit();
                        (b.read(2) as usize + 6, signed(s, 1), true)
                    }
                    29 => {
                        let s = b.bit();
                        (b.read(3) as usize + 10, signed(s, 1), true)
                    }
                    30 => {
                        let s = b.bit();
                        (1, signed(s, b.read(1) as i32 + 2), true)
                    }
                    _ => {
                        let s = b.bit();
                        let mag = b.read(1) as i32 + 2;
                        (b.read(1) as usize + 2, signed(s, mag), true)
                    }
                };
                let used = run + usize::from(counts);
                if ti + used > 64 {
                    return Err(bad("a token runs past the end of its block"));
                }
                if counts {
                    coeffs[bi * 64 + ti + run] = value as i16;
                }
                tis[bi] += used as u8;
                if counts {
                    ncoeffs[bi] = tis[bi];
                }
            }
        }
        Ok(())
    }

    fn undo_dc_prediction(&self, bcoded: &[bool], modes: &[u8], coeffs: &mut [i16]) {
        let lay = &self.lay;
        for pli in 0..3 {
            let (bw, bh) = (lay.bw[pli], lay.bh[pli]);
            let mut last = [0i32; 3];
            for by in 0..bh {
                for bx in 0..bw {
                    let bi = lay.index[pli][by * bw + bx] as usize;
                    if !bcoded[bi] {
                        continue;
                    }
                    let rfi = REF_OF_MODE[modes[lay.block_mb[bi] as usize] as usize];
                    let usable = |x: usize, y: usize| -> Option<usize> {
                        let bj = lay.index[pli][y * bw + x] as usize;
                        (bcoded[bj]
                            && REF_OF_MODE[modes[lay.block_mb[bj] as usize] as usize] == rfi)
                            .then_some(bj)
                    };
                    let mut pbi = [None; 4];
                    if bx > 0 {
                        pbi[0] = usable(bx - 1, by);
                    }
                    if bx > 0 && by > 0 {
                        pbi[1] = usable(bx - 1, by - 1);
                    }
                    if by > 0 {
                        pbi[2] = usable(bx, by - 1);
                    }
                    if bx + 1 < bw && by > 0 {
                        pbi[3] = usable(bx + 1, by - 1);
                    }
                    let mask = pbi
                        .iter()
                        .enumerate()
                        .fold(0usize, |m, (i, p)| m | (usize::from(p.is_some()) << i));
                    let dc = |i: usize| -> i32 { pbi[i].map_or(0, |j| i32::from(coeffs[j * 64])) };
                    let pred = if mask == 0 {
                        last[rfi as usize]
                    } else {
                        let (w, div) = DC_WEIGHTS[mask];
                        let mut p = 0;
                        for i in 0..4 {
                            p += w[i] * dc(i);
                        }
                        p /= div;
                        if mask & 7 == 7 {
                            let (l, dl, d) = (dc(0), dc(1), dc(2));
                            if (p - d).abs() > 128 {
                                p = d;
                            } else if (p - l).abs() > 128 {
                                p = l;
                            } else if (p - dl).abs() > 128 {
                                p = dl;
                            }
                        }
                        p
                    };
                    let v = (i32::from(coeffs[bi * 64]) + pred) as i16;
                    coeffs[bi * 64] = v;
                    last[rfi as usize] = i32::from(v);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn reconstruct(
        &self,
        rec: &mut Planes,
        bcoded: &[bool],
        modes: &[u8],
        mvs: &[(i32, i32)],
        coeffs: &[i16],
        ncoeffs: &[u8],
        qis: &[usize; 3],
        qiis: &[u8],
    ) {
        let lay = &self.lay;
        let pf = lay.pf;
        for bi in 0..lay.nbs {
            let (pli, cx, cy) = lay.order[bi];
            let (pli, bx, by) = (usize::from(pli), cx as usize * 8, cy as usize * 8);
            let pw = lay.pw[pli];
            let mut pred = [0i32; 64];
            let mut res = [0i32; 64];
            if bcoded[bi] {
                let mode = modes[lay.block_mb[bi] as usize];
                let qti = usize::from(mode != MODE_INTRA);
                let rfi = REF_OF_MODE[mode as usize];
                if rfi == 0 {
                    pred = [128; 64];
                } else {
                    let planes = if rfi == 1 { &self.prev } else { &self.golden };
                    let Some(refp) = planes else { continue };
                    let (sx, sy) = (pli > 0 && pf != 3, pli > 0 && pf == 0);
                    let (mx, my) = mvs[bi];
                    let (x1, x2) = split_mv(mx, if sx { 4 } else { 2 });
                    let (y1, y2) = split_mv(my, if sy { 4 } else { 2 });
                    predict(
                        &refp.p[pli],
                        pw,
                        lay.ph[pli],
                        bx as i32,
                        by as i32,
                        (x1, y1),
                        (x2, y2),
                        &mut pred,
                    );
                }
                let qi0 = qis[0];
                if ncoeffs[bi] < 2 {
                    let q = self.setup.qmat(qti, pli, qi0);
                    let dc = ((i32::from(coeffs[bi * 64]) * i32::from(q[0]) + 15) >> 5) as i16;
                    res = [i32::from(dc); 64];
                } else {
                    let qi = qis[usize::from(qiis[bi])];
                    let q0 = self.setup.qmat(qti, pli, qi0);
                    let qa = self.setup.qmat(qti, pli, qi);
                    let mut dqc = [0i32; 64];
                    dqc[0] = i32::from((i32::from(coeffs[bi * 64]) * i32::from(q0[0])) as i16);
                    for zzi in 1..64 {
                        let ci = usize::from(UNZIGZAG[zzi]);
                        dqc[ci] = i32::from(
                            (i32::from(coeffs[bi * 64 + zzi]) * i32::from(qa[ci])) as i16,
                        );
                    }
                    idct::idct2d(&dqc, &mut res);
                }
            } else if let Some(prev) = &self.prev {
                predict(
                    &prev.p[pli],
                    pw,
                    lay.ph[pli],
                    bx as i32,
                    by as i32,
                    (0, 0),
                    (0, 0),
                    &mut pred,
                );
            }
            let plane = &mut rec.p[pli];
            for y in 0..8 {
                let row = (by + y) * pw + bx;
                for x in 0..8 {
                    plane[row + x] = (pred[y * 8 + x] + res[y * 8 + x]).clamp(0, 255) as u8;
                }
            }
        }
    }

    fn loop_filter(&self, rec: &mut Planes, bcoded: &[bool], qi0: usize) {
        let lim = i32::from(self.setup.lflims[qi0]);
        if lim == 0 {
            return;
        }
        let lay = &self.lay;
        for pli in 0..3 {
            let (bw, bh, pw, ph) = (lay.bw[pli], lay.bh[pli], lay.pw[pli], lay.ph[pli]);
            let plane = &mut rec.p[pli];
            for cy in 0..bh {
                for cx in 0..bw {
                    if !bcoded[lay.index[pli][cy * bw + cx] as usize] {
                        continue;
                    }
                    let (x, y) = (cx * 8, cy * 8);
                    if x > 0 {
                        filter_h(plane, pw, x - 2, y, lim);
                    }
                    if y > 0 {
                        filter_v(plane, pw, x, y - 2, lim);
                    }
                    if x + 8 < pw && !bcoded[lay.index[pli][cy * bw + cx + 1] as usize] {
                        filter_h(plane, pw, x + 6, y, lim);
                    }
                    if y + 8 < ph && !bcoded[lay.index[pli][(cy + 1) * bw + cx] as usize] {
                        filter_v(plane, pw, x, y + 6, lim);
                    }
                }
            }
        }
    }

    /// Crop to the picture region and flip to top-down rows.
    fn output(&self, rec: &Planes, key: bool) -> Frame {
        let i = &self.info;
        let (w, h) = (i.pic_w as usize, i.pic_h as usize);
        let (x0, y0) = (i.pic_x as usize, i.pic_y as usize);
        let pf = i.pixel_format;
        let y = {
            let pw = self.lay.pw[0];
            let mut out = Vec::with_capacity(w * h);
            for r in 0..h {
                let from_bottom = y0 + h - 1 - r;
                let row = from_bottom * pw + x0;
                out.extend_from_slice(&rec.p[0][row..row + w]);
            }
            out
        };
        let (sx, sy) = (pf != 3, pf == 0);
        let (cx0, cx1) = if sx {
            (x0 / 2, (x0 + w).div_ceil(2))
        } else {
            (x0, x0 + w)
        };
        let (cy0, cy1) = if sy {
            (y0 / 2, (y0 + h).div_ceil(2))
        } else {
            (y0, y0 + h)
        };
        let (cw, ch) = (cx1 - cx0, cy1 - cy0);
        let chroma = |pli: usize| -> Vec<u8> {
            let pw = self.lay.pw[pli];
            let mut out = Vec::with_capacity(cw * ch);
            for r in 0..ch {
                let from_bottom = cy0 + ch - 1 - r;
                let row = from_bottom * pw + cx0;
                out.extend_from_slice(&rec.p[pli][row..row + cw]);
            }
            out
        };
        Frame {
            width: w,
            height: h,
            pixel_format: pf,
            y,
            cb: chroma(1),
            cr: chroma(2),
            chroma_width: cw,
            chroma_height: ch,
            key,
        }
    }
}

fn signed(sign: u32, mag: i32) -> i32 {
    if sign == 0 {
        mag
    } else {
        -mag
    }
}

/// The two whole-pixel offsets around a motion vector component given in `div` units per pixel:
/// truncated towards zero and rounded away from it.
fn split_mv(mv: i32, div: i32) -> (i32, i32) {
    let q = mv / div;
    if mv % div != 0 {
        (q, q + mv.signum())
    } else {
        (q, q)
    }
}

#[allow(clippy::too_many_arguments)]
fn predict(
    refp: &[u8],
    rw: usize,
    rh: usize,
    bx: i32,
    by: i32,
    v1: (i32, i32),
    v2: (i32, i32),
    out: &mut [i32; 64],
) {
    let (maxx, maxy) = (rw as i32 - 1, rh as i32 - 1);
    for y in 0..8i32 {
        let ry1 = (by + v1.1 + y).clamp(0, maxy) as usize;
        let ry2 = (by + v2.1 + y).clamp(0, maxy) as usize;
        for x in 0..8i32 {
            let rx1 = (bx + v1.0 + x).clamp(0, maxx) as usize;
            let rx2 = (bx + v2.0 + x).clamp(0, maxx) as usize;
            out[(y * 8 + x) as usize] =
                (i32::from(refp[ry1 * rw + rx1]) + i32::from(refp[ry2 * rw + rx2])) >> 1;
        }
    }
}

/// The loop filter response (spec 7.10).
fn lflim(r: i32, l: i32) -> i32 {
    if r <= -2 * l {
        0
    } else if r <= -l {
        -r - 2 * l
    } else if r < l {
        r
    } else if r < 2 * l {
        -r + 2 * l
    } else {
        0
    }
}

/// Filters across a vertical edge: the four pixels at `x..x+4` of each of 8 rows.
fn filter_h(p: &mut [u8], stride: usize, x: usize, y: usize, l: i32) {
    for r in 0..8 {
        let o = (y + r) * stride + x;
        let v: [i32; 4] = std::array::from_fn(|k| i32::from(p[o + k]));
        let f = lflim((v[0] - 3 * v[1] + 3 * v[2] - v[3] + 4) >> 3, l);
        p[o + 1] = (v[1] + f).clamp(0, 255) as u8;
        p[o + 2] = (v[2] - f).clamp(0, 255) as u8;
    }
}

/// Filters across a horizontal edge: the four pixels at `y..y+4` of each of 8 columns.
fn filter_v(p: &mut [u8], stride: usize, x: usize, y: usize, l: i32) {
    for c in 0..8 {
        let o = y * stride + x + c;
        let v: [i32; 4] = std::array::from_fn(|k| i32::from(p[o + k * stride]));
        let f = lflim((v[0] - 3 * v[1] + 3 * v[2] - v[3] + 4) >> 3, l);
        p[o + stride] = (v[1] + f).clamp(0, 255) as u8;
        p[o + 2 * stride] = (v[2] - f).clamp(0, 255) as u8;
    }
}

/// Long-run bit string (spec 7.2.1).
fn long_runs(b: &mut Bits, n: usize) -> Result<Vec<bool>> {
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return Ok(out);
    }
    let mut bit = b.bit();
    loop {
        let (start, extra) = if b.bit() == 0 {
            (1, 0)
        } else if b.bit() == 0 {
            (2, 1)
        } else if b.bit() == 0 {
            (4, 1)
        } else if b.bit() == 0 {
            (6, 2)
        } else if b.bit() == 0 {
            (10, 3)
        } else if b.bit() == 0 {
            (18, 4)
        } else {
            (34, 12)
        };
        let rlen = start + b.read(extra) as usize;
        if out.len() + rlen > n {
            return Err(bad("a run of block flags is longer than the frame"));
        }
        out.resize(out.len() + rlen, bit == 1);
        if out.len() == n {
            return Ok(out);
        }
        bit = if rlen == 4129 { b.bit() } else { bit ^ 1 };
        if b.overrun() {
            return Err(Error::truncated("Theora packet ended early"));
        }
    }
}

/// Short-run bit string (spec 7.2.2).
fn short_runs(b: &mut Bits, n: usize) -> Result<Vec<bool>> {
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return Ok(out);
    }
    let mut bit = b.bit();
    loop {
        let (start, extra) = if b.bit() == 0 {
            (1, 1)
        } else if b.bit() == 0 {
            (3, 1)
        } else if b.bit() == 0 {
            (5, 1)
        } else if b.bit() == 0 {
            (7, 2)
        } else if b.bit() == 0 {
            (11, 2)
        } else {
            (15, 4)
        };
        let rlen = start + b.read(extra) as usize;
        if out.len() + rlen > n {
            return Err(bad("a run of block flags is longer than the frame"));
        }
        out.resize(out.len() + rlen, bit == 1);
        if out.len() == n {
            return Ok(out);
        }
        bit ^= 1;
        if b.overrun() {
            return Err(Error::truncated("Theora packet ended early"));
        }
    }
}

/// Which blocks of an inter frame are coded (spec 7.3).
fn coded_flags(b: &mut Bits, lay: &Layout) -> Result<Vec<bool>> {
    let nsbs = lay.sbs.len();
    let partial = long_runs(b, nsbs)?;
    let n_whole = partial.iter().filter(|p| !**p).count();
    let full = long_runs(b, n_whole)?;
    let n_blocks: usize = lay
        .sbs
        .iter()
        .zip(&partial)
        .filter(|(_, p)| **p)
        .map(|(s, _)| s.1 as usize)
        .sum();
    let flags = short_runs(b, n_blocks)?;
    let mut bcoded = vec![false; lay.nbs];
    let (mut fi, mut bit_i) = (0, 0);
    for (sbi, &(first, count)) in lay.sbs.iter().enumerate() {
        if partial[sbi] {
            for k in 0..count as usize {
                bcoded[first as usize + k] = flags[bit_i];
                bit_i += 1;
            }
        } else {
            let all = full[fi];
            fi += 1;
            for k in 0..count as usize {
                bcoded[first as usize + k] = all;
            }
        }
    }
    Ok(bcoded)
}
