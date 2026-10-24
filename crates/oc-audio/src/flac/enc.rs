use super::{crc16, crc8};
use crate::tags::{write_flac_picture, write_vorbis_comment};
use crate::{Audio, Samples};
use oc_core::{Error, ErrorCode, Notes, Result};

/// Encoder settings.
#[derive(Debug, Clone)]
pub struct Options {
    /// 0 (fastest) to 8 (smallest), the same ladder as the reference encoder.
    pub level: u8,
    /// Samples per frame; `None` takes the level's default (1152 up to level 2, else 4096).
    pub block_size: Option<usize>,
    /// Seconds between seek points; 0 writes no seek table.
    pub seek_interval: f64,
    pub vendor: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            level: 5,
            block_size: None,
            seek_interval: 10.0,
            vendor: "OpenConverter".into(),
        }
    }
}

struct Params {
    block: usize,
    max_lpc: usize,
    max_po: u32,
    stereo: u8, // 0 independent, 1 estimate, 2 exhaustive
    exhaustive: bool,
    windows: &'static [f64], // Tukey p values
}

fn params(level: u8) -> Params {
    const LPC: [usize; 9] = [0, 0, 0, 6, 8, 8, 8, 12, 12];
    const PO: [u32; 9] = [3, 3, 3, 4, 4, 5, 6, 6, 6];
    const ST: [u8; 9] = [0, 1, 2, 0, 1, 2, 2, 2, 2];
    let l = usize::from(level.min(8));
    Params {
        block: if l <= 2 { 1152 } else { 4096 },
        max_lpc: LPC[l],
        max_po: PO[l],
        stereo: ST[l],
        exhaustive: l == 8,
        windows: match l {
            0..=5 => &[0.5],
            6 | 7 => &[0.5, 0.1],
            _ => &[0.5, 0.2, 0.05],
        },
    }
}

/// Bit sink; `put` takes up to 32 bits.
#[derive(Clone, Default)]
struct Bw {
    buf: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bw {
    fn put(&mut self, v: u64, bits: u32) {
        debug_assert!(bits <= 32);
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | (v & ((1u64 << bits) - 1));
        self.n += bits;
        if self.n >= 8 {
            let nb = (self.n / 8) as usize;
            self.n %= 8;
            let bytes = (self.acc >> self.n).to_be_bytes();
            self.buf.extend_from_slice(&bytes[8 - nb..]);
        }
        self.acc &= (1u64 << self.n) - 1;
    }
    fn put_wide(&mut self, v: u64, bits: u32) {
        if bits > 32 {
            self.put(v >> 32, bits - 32);
            self.put(v & 0xFFFF_FFFF, 32);
        } else {
            self.put(v, bits);
        }
    }
    fn unary_zeros(&mut self, q: u64) {
        let mut q = q;
        while q >= 32 {
            self.put(0, 32);
            q -= 32;
        }
        self.put(1, q as u32 + 1);
    }
    fn bits(&self) -> usize {
        self.buf.len() * 8 + self.n as usize
    }
    fn append(&mut self, o: &Bw) {
        for &b in &o.buf {
            self.put(u64::from(b), 8);
        }
        self.put(o.acc, o.n);
    }
    fn align(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// Cheapest Rice coding of `res[order..]` for a block of `block` samples.
/// Returns (bits including the partition header, partition order, parameters, 5-bit method).
fn best_rice(res: &[i64], order: usize, max_po: u32) -> (usize, u32, Vec<u32>, bool) {
    let block = res.len();
    let mut po_limit = 0;
    while po_limit < max_po.min(15)
        && block.is_multiple_of(2usize << po_limit)
        && (block >> (po_limit + 1)) > order
    {
        po_limit += 1;
    }
    // Partition sums at the finest order; coarser orders add neighbours. The cost of a
    // partition with parameter k is estimated as (sum >> k) + n * (k + 1), which is within n
    // bits of the exact size; the estimate only chooses, the real parameters are written.
    let parts_f = 1usize << po_limit;
    let per_f = block >> po_limit;
    let mut sums: Vec<u64> = Vec::with_capacity(parts_f);
    let mut counts: Vec<usize> = Vec::with_capacity(parts_f);
    for p in 0..parts_f {
        let lo = p * per_f + if p == 0 { order } else { 0 };
        let hi = (p + 1) * per_f;
        let mut sum = 0u64;
        for &v in res.get(lo..hi).unwrap_or(&[]) {
            sum += zigzag(v);
        }
        sums.push(sum);
        counts.push(hi.saturating_sub(lo));
    }
    let mut best: Option<(usize, u32, Vec<u32>, bool)> = None;
    for po in (0..=po_limit).rev() {
        let mut params = Vec::with_capacity(sums.len());
        let mut total = 6; // method (2) + order (4)
        let mut wide = false;
        for (&sum, &n) in sums.iter().zip(&counts) {
            let mean = sum / (n.max(1) as u64);
            let k0 = if mean == 0 {
                0
            } else {
                63 - mean.leading_zeros()
            };
            let mut bestk = 0;
            let mut bestc = u64::MAX;
            for k in k0.saturating_sub(1)..=(k0 + 1).min(30) {
                let c = (sum >> k) + n as u64 * (u64::from(k) + 1);
                if c < bestc {
                    bestc = c;
                    bestk = k;
                }
            }
            if bestk > 14 {
                wide = true;
            }
            params.push(bestk);
            total += bestc as usize;
        }
        total += sums.len() * if wide { 5 } else { 4 };
        if best.as_ref().is_none_or(|b| total < b.0) {
            best = Some((total, po, params, wide));
        }
        if po > 0 {
            sums = sums.chunks(2).map(|c| c.iter().sum()).collect();
            counts = counts.chunks(2).map(|c| c.iter().sum()).collect();
        }
    }
    best.unwrap_or((usize::MAX, 0, vec![0], false))
}

fn write_residual(w: &mut Bw, res: &[i64], order: usize, po: u32, params: &[u32], wide: bool) {
    w.put(u64::from(wide), 2);
    w.put(u64::from(po), 4);
    let block = res.len();
    let per = block >> po;
    let pbits = if wide { 5 } else { 4 };
    for (p, &k) in params.iter().enumerate() {
        w.put(u64::from(k), pbits);
        let lo = p * per + if p == 0 { order } else { 0 };
        let hi = (p + 1) * per;
        for &v in &res[lo..hi] {
            let z = zigzag(v);
            let q = z >> k;
            if q + 1 + u64::from(k) <= 32 {
                // zeros, the stop bit and the k low bits in one write
                w.put((1u64 << k) | (z & ((1u64 << k) - 1)), q as u32 + 1 + k);
            } else {
                w.unary_zeros(q);
                w.put_wide(z & ((1u64 << k) - 1), k);
            }
        }
    }
}

fn header_bits(w: &mut Bw, kind: u64, wasted: u32) {
    w.put(0, 1);
    w.put(kind, 6);
    if wasted > 0 {
        w.put(1, 1);
        w.unary_zeros(u64::from(wasted - 1));
    } else {
        w.put(0, 1);
    }
}

struct Sub {
    w: Bw,
}

fn verbatim(x: &[i64], bps: u32, wasted: u32) -> Sub {
    let mut w = Bw::default();
    header_bits(&mut w, 1, wasted);
    for &v in x {
        w.put_wide((v as u64) & ((1u64 << bps) - 1), bps);
    }
    Sub { w }
}

fn fixed_residual(x: &[i64], order: usize) -> Vec<i64> {
    const C: [[i64; 4]; 5] = [
        [0; 4],
        [1, 0, 0, 0],
        [2, -1, 0, 0],
        [3, -3, 1, 0],
        [4, -6, 4, -1],
    ];
    let mut r = x.to_vec();
    for i in order..x.len() {
        let mut p = 0i64;
        for j in 0..order {
            p += C[order][j] * x[i - 1 - j];
        }
        r[i] = x[i] - p;
    }
    r
}

/// A subframe chosen by estimated size; only the winner is written out.
enum Plan {
    Fixed {
        order: usize,
        res: Vec<i64>,
        po: u32,
        params: Vec<u32>,
        wide: bool,
    },
    Lpc {
        q: Vec<i64>,
        shift: i32,
        precision: u32,
        res: Vec<i64>,
        po: u32,
        params: Vec<u32>,
        wide: bool,
    },
}

fn plan_fixed(x: &[i64], order: usize, bps: u32, p: &Params) -> (usize, Plan) {
    let res = fixed_residual(x, order);
    let (cost, po, params, wide) = best_rice(&res, order, p.max_po);
    (
        cost.saturating_add(order * bps as usize),
        Plan::Fixed {
            order,
            res,
            po,
            params,
            wide,
        },
    )
}

fn build(plan: Plan, x: &[i64], bps: u32, wasted: u32) -> Sub {
    let mut w = Bw::default();
    match plan {
        Plan::Fixed {
            order,
            res,
            po,
            params,
            wide,
        } => {
            header_bits(&mut w, 8 + order as u64, wasted);
            for &v in &x[..order] {
                w.put_wide((v as u64) & ((1u64 << bps) - 1), bps);
            }
            write_residual(&mut w, &res, order, po, &params, wide);
        }
        Plan::Lpc {
            q,
            shift,
            precision,
            res,
            po,
            params,
            wide,
        } => {
            let order = q.len();
            header_bits(&mut w, 31 + order as u64, wasted);
            for &v in &x[..order] {
                w.put_wide((v as u64) & ((1u64 << bps) - 1), bps);
            }
            w.put(u64::from(precision - 1), 4);
            w.put(shift as u64, 5);
            for &c in &q {
                w.put_wide((c as u64) & ((1u64 << precision) - 1), precision);
            }
            write_residual(&mut w, &res, order, po, &params, wide);
        }
    }
    Sub { w }
}

fn tukey(n: usize, p: f64) -> Vec<f64> {
    let mut w = vec![1.0; n];
    let edge = (p * n as f64 / 2.0) as usize;
    for i in 0..edge {
        let v = 0.5 * (1.0 - (std::f64::consts::PI * i as f64 / edge.max(1) as f64).cos());
        w[i] = v;
        w[n - 1 - i] = v;
    }
    w
}

fn levinson(ac: &[f64], max: usize) -> Vec<(Vec<f64>, f64)> {
    // Returns coefficients and prediction error for orders 1..=max.
    let mut out = Vec::new();
    let mut a = vec![0.0f64; max + 1];
    let mut err = ac[0];
    if err <= 0.0 {
        return out;
    }
    for m in 1..=max {
        let mut acc = ac[m];
        for j in 1..m {
            acc -= a[j] * ac[m - j];
        }
        let k = acc / err;
        let mut next = a.clone();
        next[m] = k;
        for j in 1..m {
            next[j] = a[j] - k * a[m - j];
        }
        a = next;
        err *= 1.0 - k * k;
        if err <= 0.0 || !err.is_finite() {
            break;
        }
        out.push((a[1..=m].to_vec(), err));
    }
    out
}

fn quantize(coefs: &[f64], precision: u32) -> Option<(Vec<i64>, i32)> {
    let cmax = coefs.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    if cmax <= 0.0 || !cmax.is_finite() {
        return None;
    }
    let log2 = cmax.log2().floor() as i32 + 1; // cmax < 2^log2
    let mut shift = precision as i32 - 1 - log2;
    shift = shift.min(15);
    if shift < 0 {
        return None;
    }
    let lim = (1i64 << (precision - 1)) - 1;
    let mut err = 0.0;
    let mut q = Vec::with_capacity(coefs.len());
    for &c in coefs {
        let v = c * f64::from(1u32 << shift) + err;
        let r = (v.round() as i64).clamp(-lim - 1, lim);
        err = v - r as f64;
        q.push(r);
    }
    Some((q, shift))
}

fn lpc_residual(x: &[i64], q: &[i64], shift: i32) -> Option<Vec<i64>> {
    let order = q.len();
    let mut r = x.to_vec();
    for i in order..x.len() {
        let mut p = 0i64;
        for (j, &c) in q.iter().enumerate() {
            p += c * x[i - 1 - j];
        }
        r[i] = x[i] - (p >> shift);
        if r[i].unsigned_abs() >= 1 << 33 {
            return None;
        }
    }
    Some(r)
}

fn plan_lpc(
    x: &[i64],
    q: Vec<i64>,
    shift: i32,
    precision: u32,
    bps: u32,
    p: &Params,
) -> Option<(usize, Plan)> {
    let order = q.len();
    let res = lpc_residual(x, &q, shift)?;
    let (cost, po, params, wide) = best_rice(&res, order, p.max_po);
    let cost = cost
        .saturating_add(order * bps as usize)
        .saturating_add(9 + order * precision as usize);
    Some((
        cost,
        Plan::Lpc {
            q,
            shift,
            precision,
            res,
            po,
            params,
            wide,
        },
    ))
}

/// Best subframe for one channel whose samples need `bps` bits.
fn encode_subframe(x: &[i64], bps: u32, p: &Params) -> Sub {
    let n = x.len();
    // Wasted low bits.
    let or = x.iter().fold(0i64, |a, &v| a | v);
    let wasted = if or == 0 {
        0
    } else {
        or.trailing_zeros().min(bps.saturating_sub(1))
    };
    let shifted: Vec<i64>;
    let (x, bps) = if wasted > 0 {
        shifted = x.iter().map(|&v| v >> wasted).collect();
        (&shifted[..], bps - wasted)
    } else {
        (x, bps)
    };
    if x.iter().all(|&v| v == x[0]) {
        let mut w = Bw::default();
        header_bits(&mut w, 0, wasted);
        w.put_wide((x[0] as u64) & ((1u64 << bps) - 1), bps);
        return Sub { w };
    }
    let mut best: Option<(usize, Plan)> = None;
    let verbatim_cost = n * bps as usize;
    let maxfix = 4.min(n.saturating_sub(1));
    for order in 0..=maxfix {
        let c = plan_fixed(x, order, bps, p);
        if best.as_ref().is_none_or(|b| c.0 < b.0) {
            best = Some(c);
        }
    }
    let max_lpc = p.max_lpc.min(n.saturating_sub(1)).min(32);
    if max_lpc > 0 && n > 32 {
        let precision: u32 = match n {
            0..=192 => 7,
            193..=384 => 8,
            385..=576 => 9,
            577..=1152 => 10,
            1153..=2304 => 11,
            2305..=4608 => 12,
            _ => 13,
        };
        let xf: Vec<f64> = x.iter().map(|&v| v as f64).collect();
        for &tp in p.windows {
            let win = tukey(n, tp);
            let wx: Vec<f64> = xf.iter().zip(&win).map(|(a, b)| a * b).collect();
            let mut ac = vec![0.0; max_lpc + 1];
            for (lag, a) in ac.iter_mut().enumerate() {
                *a = wx[lag..].iter().zip(&wx).map(|(a, b)| a * b).sum();
            }
            ac[0] *= 1.0 + 1e-9;
            let sets = levinson(&ac, max_lpc);
            // Candidate orders: all of them at the top level, else the two best by an estimate.
            let mut order_list: Vec<usize> = (1..=sets.len()).collect();
            if !p.exhaustive {
                let est = |i: usize| -> f64 {
                    let (_, err) = &sets[i - 1];
                    let per = (err / n as f64).max(1e-9);
                    0.5 * per.log2() * (n - i) as f64 + (i as f64) * f64::from(precision + bps)
                };
                order_list.sort_by(|&a, &b| {
                    est(a)
                        .partial_cmp(&est(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                order_list.truncate(2);
            }
            for o in order_list {
                let coefs = &sets[o - 1].0;
                let precisions: Vec<u32> = if p.exhaustive {
                    vec![precision - 1, precision, precision + 1]
                } else {
                    vec![precision]
                };
                for pr in precisions {
                    if let Some((q, shift)) = quantize(coefs, pr) {
                        if let Some(c) = plan_lpc(x, q, shift, pr, bps, p) {
                            if best.as_ref().is_none_or(|b| c.0 < b.0) {
                                best = Some(c);
                            }
                        }
                    }
                }
            }
        }
    }
    match best {
        Some((cost, plan)) if cost < verbatim_cost => build(plan, x, bps, wasted),
        _ => verbatim(x, bps, wasted),
    }
}

fn utf8_number(v: u64, out: &mut Vec<u8>) {
    if v < 0x80 {
        out.push(v as u8);
        return;
    }
    let mut n = 2;
    while v >= 1u64 << (5 * n + 1) {
        n += 1;
    }
    out.push((0xFF00u16 >> n) as u8 | (v >> (6 * (n - 1))) as u8);
    for i in (0..n - 1).rev() {
        out.push(0x80 | ((v >> (6 * i)) & 0x3F) as u8);
    }
}

fn frame_header(num: u64, block: usize, rate: u32, assignment: u8, bits: u8) -> Vec<u8> {
    let mut h = vec![0xFF, 0xF8];
    let (bs_code, bs_extra): (u8, Vec<u8>) = match block {
        192 => (1, vec![]),
        576 => (2, vec![]),
        1152 => (3, vec![]),
        2304 => (4, vec![]),
        4608 => (5, vec![]),
        256 => (8, vec![]),
        512 => (9, vec![]),
        1024 => (10, vec![]),
        2048 => (11, vec![]),
        4096 => (12, vec![]),
        8192 => (13, vec![]),
        16384 => (14, vec![]),
        32768 => (15, vec![]),
        b if b <= 256 => (6, vec![(b - 1) as u8]),
        b => (7, ((b - 1) as u16).to_be_bytes().to_vec()),
    };
    let (sr_code, sr_extra): (u8, Vec<u8>) = match rate {
        88200 => (1, vec![]),
        176_400 => (2, vec![]),
        192_000 => (3, vec![]),
        8000 => (4, vec![]),
        16_000 => (5, vec![]),
        22_050 => (6, vec![]),
        24_000 => (7, vec![]),
        32_000 => (8, vec![]),
        44_100 => (9, vec![]),
        48_000 => (10, vec![]),
        96_000 => (11, vec![]),
        r if r % 1000 == 0 && r / 1000 < 256 => (12, vec![(r / 1000) as u8]),
        r if r < 65536 => (13, (r as u16).to_be_bytes().to_vec()),
        r if r % 10 == 0 && r / 10 < 65536 => (14, ((r / 10) as u16).to_be_bytes().to_vec()),
        _ => (0, vec![]), // taken from STREAMINFO
    };
    let bps_code = match bits {
        8 => 1,
        12 => 2,
        16 => 4,
        20 => 5,
        24 => 6,
        32 => 7,
        _ => 0,
    };
    h.push(bs_code << 4 | sr_code);
    h.push(assignment << 4 | bps_code << 1);
    utf8_number(num, &mut h);
    h.extend_from_slice(&bs_extra);
    h.extend_from_slice(&sr_extra);
    let c = crc8(&h);
    h.push(c);
    h
}

struct Frame {
    bytes: Vec<u8>,
}

fn encode_frame(num: u64, chans: &[Vec<i64>], rate: u32, bits: u8, p: &Params) -> Frame {
    let block = chans[0].len();
    let bps = u32::from(bits);
    let mut w = Bw::default();
    let assignment: u8;
    if chans.len() == 2 {
        let l = &chans[0];
        let r = &chans[1];
        let mid: Vec<i64> = l.iter().zip(r).map(|(a, b)| (a + b) >> 1).collect();
        let side: Vec<i64> = l.iter().zip(r).map(|(a, b)| a - b).collect();
        let (mode, subs): (u8, Vec<Sub>) = if p.stereo == 0 {
            (
                1,
                vec![encode_subframe(l, bps, p), encode_subframe(r, bps, p)],
            )
        } else if p.stereo == 1 {
            // Estimate with the energy of each candidate pair and encode only the winner.
            let e = |x: &[i64]| -> u64 { x.windows(2).map(|w| (w[1] - w[0]).unsigned_abs()).sum() };
            let (el, er, em, es) = (e(l), e(r), e(&mid), e(&side));
            let costs = [(el + er, 1u8), (el + es, 8), (er + es, 9), (em + es, 10)];
            let best = costs.iter().min_by_key(|c| c.0).map_or(1, |c| c.1);
            match best {
                8 => (
                    8,
                    vec![
                        encode_subframe(l, bps, p),
                        encode_subframe(&side, bps + 1, p),
                    ],
                ),
                9 => (
                    9,
                    vec![
                        encode_subframe(&side, bps + 1, p),
                        encode_subframe(r, bps, p),
                    ],
                ),
                10 => (
                    10,
                    vec![
                        encode_subframe(&mid, bps, p),
                        encode_subframe(&side, bps + 1, p),
                    ],
                ),
                _ => (
                    1,
                    vec![encode_subframe(l, bps, p), encode_subframe(r, bps, p)],
                ),
            }
        } else {
            let sl = encode_subframe(l, bps, p);
            let sr = encode_subframe(r, bps, p);
            let sm = encode_subframe(&mid, bps, p);
            let ss = encode_subframe(&side, bps + 1, p);
            let cost = |a: &Sub, b: &Sub| a.w.bits() + b.w.bits();
            let c = [
                cost(&sl, &sr),
                cost(&sl, &ss),
                cost(&sr, &ss),
                cost(&sm, &ss),
            ];
            let best = (0..4).min_by_key(|&i| c[i]).unwrap_or(0);
            match best {
                0 => (1, vec![sl, sr]),
                1 => (8, vec![sl, ss]),
                2 => (9, vec![ss, sr]),
                _ => (10, vec![sm, ss]),
            }
        };
        assignment = if mode == 1 { 1 } else { mode };
        for s in &subs {
            w.append(&s.w);
        }
    } else {
        assignment = chans.len() as u8 - 1;
        for c in chans {
            w.append(&encode_subframe(c, bps, p).w);
        }
    }
    w.align();
    let mut bytes = frame_header(num, block, rate, assignment, bits);
    bytes.extend_from_slice(&w.buf);
    let crc = crc16(&bytes);
    bytes.extend_from_slice(&crc.to_be_bytes());
    Frame { bytes }
}

fn metadata_block(out: &mut Vec<u8>, kind: u8, last: bool, body: &[u8]) {
    out.push(kind | if last { 0x80 } else { 0 });
    out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(body);
}

pub fn write(audio: &Audio, opts: &Options, notes: &mut Notes) -> Result<Vec<u8>> {
    let Samples::Int { bits, data } = &audio.samples else {
        return Err(Error::new(
            ErrorCode::InternalError,
            "the FLAC writer needs integer samples",
        ));
    };
    let bits = *bits;
    let ch = usize::from(audio.channels);
    if !(1..=8).contains(&ch) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "FLAC holds 1 to 8 channels",
        ));
    }
    if !(4..=32).contains(&bits) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "FLAC holds 4 to 32 bit samples",
        ));
    }
    if audio.sample_rate > 655_350 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "FLAC cannot store sample rates above 655350 Hz",
        ));
    }
    let p = params(opts.level);
    let block = opts.block_size.unwrap_or(p.block).clamp(16, 65535);
    let frames = data.len() / ch;
    let width = usize::from(bits).div_ceil(8);

    // Header blocks. STREAMINFO is patched at the end.
    let mut out = b"fLaC".to_vec();
    let mut si = vec![0u8; 34];
    si[0..2].copy_from_slice(&(block as u16).to_be_bytes());
    si[2..4].copy_from_slice(&(block as u16).to_be_bytes());
    let packed: u64 = (u64::from(audio.sample_rate) << 44)
        | ((ch as u64 - 1) << 41)
        | ((u64::from(bits) - 1) << 36)
        | (frames as u64 & 0xF_FFFF_FFFF);
    si[10..18].copy_from_slice(&packed.to_be_bytes());
    let mut extra: Vec<(u8, Vec<u8>)> = Vec::new();
    let points = if opts.seek_interval > 0.0 && frames > 0 {
        (frames as f64 / (opts.seek_interval * f64::from(audio.sample_rate)))
            .ceil()
            .max(1.0) as usize
    } else {
        0
    };
    let seek_pos_in_extra = if points > 0 {
        extra.push((3, vec![0xFF; points * 18]));
        Some(extra.len() - 1)
    } else {
        None
    };
    let comment = write_vorbis_comment(&opts.vendor, &audio.tags, &audio.pictures, false);
    extra.push((4, comment));
    for pic in &audio.pictures {
        extra.push((6, write_flac_picture(pic)));
    }
    metadata_block(&mut out, 0, false, &si);
    let seek_offset = out.len() + 4;
    for (i, (k, b)) in extra.iter().enumerate() {
        metadata_block(&mut out, *k, i == extra.len() - 1, b);
        if Some(i) == seek_pos_in_extra {
            // seek table body starts right after its 4-byte block header
        }
    }
    let audio_start = out.len();

    // Frames.
    let mut md5 = oc_checksum::Md5::new();
    let mut md5buf = Vec::new();
    let mut min_frame = usize::MAX;
    let mut max_frame = 0usize;
    let mut seek: Vec<(u64, u64, u16)> = Vec::new();
    let mut next_seek = 0u64;
    let step = (opts.seek_interval * f64::from(audio.sample_rate)).max(1.0) as u64;
    let mut num = 0u64;
    let mut at = 0usize;
    while at < frames {
        let n = block.min(frames - at);
        let chans: Vec<Vec<i64>> = (0..ch)
            .map(|c| (0..n).map(|i| i64::from(data[(at + i) * ch + c])).collect())
            .collect();
        md5buf.clear();
        for &s in &data[at * ch..(at + n) * ch] {
            md5buf.extend_from_slice(&s.to_le_bytes()[..width]);
        }
        md5.update(&md5buf);
        let f = encode_frame(num, &chans, audio.sample_rate, bits, &p);
        if points > 0 && at as u64 >= next_seek && seek.len() < points {
            seek.push((at as u64, (out.len() - audio_start) as u64, n as u16));
            next_seek = at as u64 + step;
        }
        min_frame = min_frame.min(f.bytes.len());
        max_frame = max_frame.max(f.bytes.len());
        out.extend_from_slice(&f.bytes);
        num += 1;
        at += n;
    }
    if frames == 0 {
        min_frame = 0;
    }
    // Patch STREAMINFO (its body starts at byte 8).
    let min_frame = min_frame.min(0xFF_FFFF);
    let max_frame = max_frame.min(0xFF_FFFF);
    let short = if frames > 0 && frames < block {
        frames as u16
    } else {
        block as u16
    };
    out[8..10].copy_from_slice(&short.to_be_bytes());
    out[10..12].copy_from_slice(&short.to_be_bytes());
    out[12..15].copy_from_slice(&(min_frame as u32).to_be_bytes()[1..]);
    out[15..18].copy_from_slice(&(max_frame as u32).to_be_bytes()[1..]);
    out[26..42].copy_from_slice(&md5.finish());
    // Patch the seek table (unused slots stay as placeholder points).
    if points > 0 {
        for i in 0..points {
            let o = seek_offset + i * 18;
            let (s, off, n) = seek.get(i).copied().unwrap_or((u64::MAX, 0, 0));
            out[o..o + 8].copy_from_slice(&s.to_be_bytes());
            out[o + 8..o + 16].copy_from_slice(&off.to_be_bytes());
            out[o + 16..o + 18].copy_from_slice(&n.to_be_bytes());
        }
    }
    let _ = notes;
    Ok(out)
}
