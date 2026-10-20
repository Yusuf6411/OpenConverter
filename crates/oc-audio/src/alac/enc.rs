//! ALAC encoder.
//!
//! Each element (a mono channel or a stereo pair) is coded several ways and the smallest wins:
//! plain or mid/side, with a sign-LMS predictor of order 4, 8 or 16 whose starting coefficients
//! come from a least-squares fit. The adaptive predictor and the Rice history are mirrored
//! exactly from the decoder, and every packet is decoded again before it is accepted; a packet
//! that does not round-trip is stored uncompressed instead.
use super::dec::{sign_extend, Decoder};
use super::{Cookie, CHANNEL_MAP};
use crate::{Audio, Samples};
use oc_core::{Error, ErrorCode, Result};

/// MSB-first bit sink.
#[derive(Default, Clone)]
struct W {
    buf: Vec<u8>,
    acc: u64,
    n: u32,
}

impl W {
    fn put(&mut self, v: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        let v = if bits >= 32 {
            u64::from(v)
        } else {
            u64::from(v) & ((1u64 << bits) - 1)
        };
        self.acc = (self.acc << bits) | v;
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.buf.push((self.acc >> self.n) as u8);
        }
        self.acc &= (1u64 << self.n) - 1;
    }
    fn bits(&self) -> usize {
        self.buf.len() * 8 + self.n as usize
    }
    fn append(&mut self, o: &W) {
        for &b in &o.buf {
            self.put(u32::from(b), 8);
        }
        if o.n > 0 {
            self.put(o.acc as u32, o.n);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
        self.buf
    }
}

fn log2(v: u32) -> u32 {
    if v == 0 {
        0
    } else {
        31 - v.leading_zeros()
    }
}

fn put_scalar(w: &mut W, x: u32, k: u32, bps: u32) {
    let m = (1u32 << k) - 1;
    let q = x / m;
    if q >= 9 {
        w.put(0x1FF, 9);
        w.put(x, bps);
        return;
    }
    // q ones then a zero
    w.put(((1u32 << q) - 1) << 1, q + 1);
    if k != 1 {
        let r = x % m;
        if r == 0 {
            w.put(0, k - 1);
        } else {
            w.put(r + 1, k);
        }
    }
}

fn rice_write(w: &mut W, res: &[i32], pb: u32, mb: u32, kb: u32, bps: u32) {
    let n = res.len();
    let mut history = mb;
    let mut sign_mod = 0u32;
    let mut i = 0usize;
    while i < n {
        let k = log2((history >> 9) + 3).min(kb);
        let r = res[i];
        let z = ((r << 1) ^ (r >> 31)) as u32;
        let x = z.wrapping_sub(sign_mod);
        put_scalar(w, x, k, bps);
        if x > 0xFFFF {
            history = 0xFFFF;
        } else {
            history = history
                .wrapping_add(z.wrapping_mul(pb))
                .wrapping_sub(history.wrapping_mul(pb) >> 9);
        }
        sign_mod = 0;
        if history < 128 && i + 1 < n {
            sign_mod = 1;
            let run = res[i + 1..]
                .iter()
                .take_while(|&&v| v == 0)
                .count()
                .min(65535);
            let k = (7 + ((history + 16) >> 6)).saturating_sub(log2(history));
            put_scalar(w, run as u32, k, 16);
            i += run;
            if run >= 65535 {
                sign_mod = 0;
            }
            history = 0;
        }
        i += 1;
    }
}

/// Least-squares coefficients for predicting `x[i] - d` from `x[i-order+j] - d`, where
/// `d = x[i-order-1]`, quantised to `den` fractional bits.
fn fit(x: &[i32], order: usize, den: u32) -> Vec<i16> {
    let n = x.len();
    if n < order * 2 + 8 {
        return vec![0; order];
    }
    let mut r = vec![0f64; order * order];
    let mut b = vec![0f64; order];
    let mut f = vec![0f64; order];
    for i in order + 1..n {
        let d = f64::from(x[i - order - 1]);
        for j in 0..order {
            f[j] = f64::from(x[i - order + j]) - d;
        }
        let t = f64::from(x[i]) - d;
        for a in 0..order {
            b[a] += f[a] * t;
            for c in a..order {
                r[a * order + c] += f[a] * f[c];
            }
        }
    }
    let trace: f64 = (0..order).map(|a| r[a * order + a]).sum::<f64>() / order as f64;
    for a in 0..order {
        for c in 0..a {
            r[a * order + c] = r[c * order + a];
        }
        r[a * order + a] += trace * 1e-9 + 1e-6;
    }
    // Gaussian elimination with partial pivoting
    let mut m = r;
    let mut v = b;
    for col in 0..order {
        let mut piv = col;
        for row in col + 1..order {
            if m[row * order + col].abs() > m[piv * order + col].abs() {
                piv = row;
            }
        }
        if m[piv * order + col].abs() < 1e-12 {
            return vec![0; order];
        }
        if piv != col {
            for c in 0..order {
                m.swap(col * order + c, piv * order + c);
            }
            v.swap(col, piv);
        }
        for row in col + 1..order {
            let fct = m[row * order + col] / m[col * order + col];
            for c in col..order {
                m[row * order + c] -= fct * m[col * order + c];
            }
            v[row] -= fct * v[col];
        }
    }
    let mut c = vec![0f64; order];
    for row in (0..order).rev() {
        let mut s = v[row];
        for k in row + 1..order {
            s -= m[row * order + k] * c[k];
        }
        c[row] = s / m[row * order + row];
    }
    let scale = f64::from(1u32 << den);
    c.iter()
        .map(|&v| (v * scale).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

/// Residuals, with the decoder's coefficient adaptation mirrored.
fn residuals(x: &[i32], coefs: &mut [i16], den: u32, bps: u32) -> Vec<i32> {
    let n = x.len();
    let order = coefs.len();
    let mut e = vec![0i32; n];
    if n == 0 {
        return e;
    }
    e[0] = x[0];
    if order == 0 {
        e[1..].copy_from_slice(&x[1..]);
        return e;
    }
    for i in 1..=order.min(n - 1) {
        e[i] = sign_extend(x[i].wrapping_sub(x[i - 1]), bps);
    }
    let half = if den > 0 { 1i32 << (den - 1) } else { 0 };
    for i in order + 1..n {
        let d = x[i - order - 1];
        let mut val = 0i32;
        for j in 0..order {
            val = val.wrapping_add(
                x[i - order + j]
                    .wrapping_sub(d)
                    .wrapping_mul(i32::from(coefs[j])),
            );
        }
        let pred = (val.wrapping_add(half) >> den).wrapping_add(d);
        let err = sign_extend(x[i].wrapping_sub(pred), bps);
        e[i] = err;
        let sgn = err.signum();
        if sgn != 0 {
            let mut left = err;
            let mut j = 0;
            while j < order && left.wrapping_mul(sgn) > 0 {
                let v = d.wrapping_sub(x[i - order + j]);
                let s = v.signum() * sgn;
                coefs[j] = coefs[j].wrapping_sub(s as i16);
                left = left.wrapping_sub((v.wrapping_mul(s) >> den).wrapping_mul(j as i32 + 1));
                j += 1;
            }
        }
    }
    e
}

struct Coded {
    order: usize,
    den: u32,
    coefs: Vec<i16>,
    data: W,
}

fn code_channel(x: &[i32], bps: u32, c: &Cookie) -> Coded {
    const DEN: u32 = 9;
    let mut best: Option<Coded> = None;
    for &order in &[4usize, 8, 16] {
        let start = fit(x, order, DEN);
        let mut run = start.clone();
        let res = residuals(x, &mut run, DEN, bps);
        let mut w = W::default();
        rice_write(
            &mut w,
            &res,
            u32::from(c.pb),
            u32::from(c.mb),
            u32::from(c.kb),
            bps,
        );
        let cost = w.bits() + order * 16;
        if best
            .as_ref()
            .is_none_or(|b| cost < b.data.bits() + b.order * 16)
        {
            best = Some(Coded {
                order,
                den: DEN,
                coefs: start,
                data: w,
            });
        }
    }
    best.unwrap_or(Coded {
        order: 0,
        den: 0,
        coefs: Vec::new(),
        data: W::default(),
    })
}

#[derive(Clone, Copy)]
enum Kind {
    Sce,
    Cpe,
    Lfe,
}

fn elements(channels: usize) -> &'static [Kind] {
    use Kind::*;
    match channels {
        1 => &[Sce],
        2 => &[Cpe],
        3 => &[Sce, Cpe],
        4 => &[Sce, Cpe, Sce],
        5 => &[Sce, Cpe, Cpe],
        6 => &[Sce, Cpe, Cpe, Lfe],
        7 => &[Sce, Cpe, Cpe, Sce, Lfe],
        _ => &[Sce, Cpe, Cpe, Cpe, Lfe],
    }
}

fn header(
    w: &mut W,
    kind: Kind,
    inst: u32,
    num: usize,
    frame_len: usize,
    shift: u32,
    escape: bool,
) {
    w.put(
        match kind {
            Kind::Sce => 0,
            Kind::Cpe => 1,
            Kind::Lfe => 3,
        },
        3,
    );
    w.put(inst, 4);
    w.put(0, 12);
    let partial = num != frame_len;
    w.put(u32::from(partial), 1);
    w.put(shift / 8, 2);
    w.put(u32::from(escape), 1);
    if partial {
        w.put(num as u32, 32);
    }
}

fn element(
    w: &mut W,
    kind: Kind,
    inst: u32,
    chans: &[&[i32]],
    depth: u32,
    c: &Cookie,
    escape: bool,
) {
    let num = chans[0].len();
    let n = chans.len();
    if escape {
        header(w, kind, inst, num, c.frame_len as usize, 0, true);
        for i in 0..num {
            for ch in chans {
                w.put(ch[i] as u32, depth);
            }
        }
        return;
    }
    let shift = if depth > 16 {
        (depth.div_ceil(8) - 2) * 8
    } else {
        0
    };
    let hi: Vec<Vec<i32>> = chans
        .iter()
        .map(|ch| ch.iter().map(|&s| s >> shift).collect())
        .collect();
    let lo: Vec<Vec<u32>> = chans
        .iter()
        .map(|ch| {
            ch.iter()
                .map(|&s| (s as u32) & ((1u32 << shift) - 1))
                .collect()
        })
        .collect();
    let bps = depth - shift + u32::from(n == 2);
    // candidates: (mix_bits, mix_res, channels)
    let mut variants: Vec<(u32, i32, Vec<Vec<i32>>)> = vec![(0, 0, hi.clone())];
    if n == 2 {
        let v: Vec<i32> = hi[0]
            .iter()
            .zip(&hi[1])
            .map(|(l, r)| l.wrapping_sub(*r))
            .collect();
        let u: Vec<i32> = hi[1]
            .iter()
            .zip(&v)
            .map(|(r, v)| r.wrapping_add(*v >> 1))
            .collect();
        variants.push((1, 1, vec![u, v]));
    }
    let mut best: Option<(W, usize)> = None;
    for (mix_bits, mix_res, data) in &variants {
        let coded: Vec<Coded> = data.iter().map(|d| code_channel(d, bps, c)).collect();
        let mut o = W::default();
        header(&mut o, kind, inst, num, c.frame_len as usize, shift, false);
        o.put(*mix_bits, 8);
        o.put(*mix_res as u32, 8);
        for cd in &coded {
            o.put(cd.den, 8);
            o.put((4 << 5) | cd.order as u32, 8);
            for &k in cd.coefs.iter().rev() {
                o.put(u32::from(k as u16), 16);
            }
        }
        if shift > 0 {
            for i in 0..num {
                for l in &lo {
                    o.put(l[i], shift);
                }
            }
        }
        for cd in &coded {
            o.append(&cd.data);
        }
        let bits = o.bits();
        if best.as_ref().is_none_or(|(_, b)| bits < *b) {
            best = Some((o, bits));
        }
    }
    if let Some((b, _)) = best {
        w.append(&b);
    }
}

pub struct Packet {
    pub bytes: Vec<u8>,
    pub frames: u32,
}

/// Encode a whole clip. Returns the cookie (with sizes filled in) and the packets.
pub fn encode(audio: &Audio) -> Result<(Cookie, Vec<Packet>)> {
    let (bits, data) = match &audio.samples {
        Samples::Int { bits, data } => (*bits, data),
        Samples::Float(_) => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "ALAC stores integer samples; convert the audio to 16, 20, 24 or 32 bits first",
            ))
        }
    };
    if !matches!(bits, 16 | 20 | 24 | 32) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!("ALAC cannot store {bits}-bit samples"),
        ));
    }
    let nch = usize::from(audio.channels);
    if nch == 0 || nch > 8 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "ALAC supports 1 to 8 channels",
        ));
    }
    let depth = u32::from(bits);
    let mut cookie = Cookie::new(nch as u8, bits, audio.sample_rate);
    let dec = Decoder::new(cookie.clone())?;
    let frame_len = cookie.frame_len as usize;
    let frames = data.len() / nch;
    let layout = CHANNEL_MAP[nch - 1];
    let kinds = elements(nch);
    let mut packets = Vec::new();
    let mut start = 0;
    while start < frames {
        let num = frame_len.min(frames - start);
        let mut src: Vec<Vec<i32>> = vec![Vec::with_capacity(num); nch];
        for i in 0..num {
            for (c, s) in src.iter_mut().enumerate() {
                s.push(data[(start + i) * nch + c]);
            }
        }
        let build = |escape: bool| -> Vec<u8> {
            let mut w = W::default();
            let mut at = 0usize;
            let mut inst = [0u32; 3];
            for &k in kinds {
                let n = if matches!(k, Kind::Cpe) { 2 } else { 1 };
                let chans: Vec<&[i32]> = (0..n).map(|j| &src[layout[at + j]][..]).collect();
                let slot = match k {
                    Kind::Sce => 0,
                    Kind::Cpe => 1,
                    Kind::Lfe => 2,
                };
                element(&mut w, k, inst[slot], &chans, depth, &cookie, escape);
                inst[slot] += 1;
                at += n;
            }
            w.put(7, 3);
            w.finish()
        };
        let mut bytes = build(false);
        let mut check = Vec::with_capacity(num * nch);
        let ok = dec.frame(&bytes, &mut check).is_ok_and(|n| {
            n == num
                && check
                    .iter()
                    .enumerate()
                    .all(|(i, &v)| v == data[start * nch + i])
        });
        if !ok {
            bytes = build(true);
        }
        packets.push(Packet {
            bytes,
            frames: num as u32,
        });
        start += num;
    }
    cookie.max_frame_bytes = packets
        .iter()
        .map(|p| p.bytes.len() as u32)
        .max()
        .unwrap_or(0);
    let total: usize = packets.iter().map(|p| p.bytes.len()).sum();
    if frames > 0 && audio.sample_rate > 0 {
        cookie.avg_bitrate =
            (total as f64 * 8.0 * f64::from(audio.sample_rate) / frames as f64) as u32;
    }
    Ok((cookie, packets))
}
