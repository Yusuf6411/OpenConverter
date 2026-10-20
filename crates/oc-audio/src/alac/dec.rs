//! ALAC frame decoder: adaptive Rice residuals, sign-LMS linear prediction, stereo unmixing.
use super::{Cookie, CHANNEL_MAP};
use oc_core::{Error, ErrorCode, Result};

/// MSB-first bit reader over a byte slice. Reads past the end are errors.
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }
    /// The next up to 32 bits, zero-padded at the end, without consuming them.
    fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos / 8;
        let mut w = 0u64;
        for i in 0..8 {
            w = (w << 8) | u64::from(self.data.get(byte + i).copied().unwrap_or(0));
        }
        let shifted = w << (self.pos % 8);
        (shifted >> (64 - n)) as u32
    }
    pub fn read(&mut self, n: u32) -> Result<u32> {
        if n as usize > self.left() {
            return Err(Error::truncated("ALAC frame ended early"));
        }
        let v = self.peek(n);
        self.pos += n as usize;
        Ok(v)
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        if n > self.left() {
            return Err(Error::truncated("ALAC frame ended early"));
        }
        self.pos += n;
        Ok(())
    }
    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

pub(crate) fn sign_extend(v: i32, bits: u32) -> i32 {
    if bits >= 32 || bits == 0 {
        v
    } else {
        (v << (32 - bits)) >> (32 - bits)
    }
}

fn log2(v: u32) -> u32 {
    if v == 0 {
        0
    } else {
        31 - v.leading_zeros()
    }
}

/// One Rice-coded value in ALAC's variant: up to 9 unary ones, then a remainder of k or k-1
/// bits; nine ones escape to a raw `bps`-bit value.
fn scalar(b: &mut Bits<'_>, k: u32, bps: u32) -> Result<u32> {
    let mut x = 0u32;
    while x < 9 && b.read(1)? == 1 {
        x += 1;
    }
    if x > 8 {
        return b.read(bps);
    }
    if k != 1 {
        let extra = b.peek(k);
        x = (x << k) - x;
        if extra > 1 {
            x += extra - 1;
            b.skip(k as usize)?;
        } else {
            b.skip(k as usize - 1)?;
        }
    }
    Ok(x)
}

pub(crate) struct Params {
    pub mb: u32,
    pub pb: u32,
    pub kb: u32,
}

fn rice(b: &mut Bits<'_>, p: &Params, out: &mut [i32], bps: u32) -> Result<()> {
    let n = out.len();
    let mut history = p.mb;
    let mut sign_mod = 0u32;
    let mut i = 0usize;
    while i < n {
        let k = log2((history >> 9) + 3).min(p.kb);
        let x = scalar(b, k, bps)?;
        let xm = x.wrapping_add(sign_mod);
        out[i] = ((xm >> 1) ^ (xm & 1).wrapping_neg()) as i32;
        if x > 0xFFFF {
            history = 0xFFFF;
        } else {
            history = history
                .wrapping_add(xm.wrapping_mul(p.pb))
                .wrapping_sub((history.wrapping_mul(p.pb)) >> 9);
        }
        sign_mod = 0;
        if history < 128 && i + 1 < n {
            sign_mod = 1;
            let k = (7 + ((history + 16) >> 6)).saturating_sub(log2(history));
            let run = scalar(b, k, 16)? as usize;
            if run > 0 {
                if i + 1 + run > n {
                    return Err(Error::corrupt("ALAC zero run overruns the frame"));
                }
                for o in &mut out[i + 1..=i + run] {
                    *o = 0;
                }
                i += run;
            }
            if run >= 65535 {
                sign_mod = 0;
            }
            history = 0;
        }
        i += 1;
    }
    Ok(())
}

/// The inverse of the predictor. `out` receives the samples; `err` the residuals.
pub(crate) fn predict(err: &[i32], out: &mut [i32], coefs: &mut [i16], quant: u32, bps: u32) {
    let n = err.len();
    if n == 0 {
        return;
    }
    out[0] = err[0];
    if n == 1 {
        return;
    }
    let order = coefs.len();
    if order == 0 {
        out[1..n].copy_from_slice(&err[1..n]);
        return;
    }
    if order == 31 {
        for i in 1..n {
            out[i] = sign_extend(out[i - 1].wrapping_add(err[i]), bps);
        }
        return;
    }
    let mut i = 1;
    while i <= order && i < n {
        out[i] = sign_extend(out[i - 1].wrapping_add(err[i]), bps);
        i += 1;
    }
    while i < n {
        let d = out[i - order - 1];
        let mut val = 0i32;
        for j in 0..order {
            val = val.wrapping_add(
                out[i - order + j]
                    .wrapping_sub(d)
                    .wrapping_mul(i32::from(coefs[j])),
            );
        }
        let half = if quant > 0 { 1i32 << (quant - 1) } else { 0 };
        val = val.wrapping_add(half) >> quant;
        let mut e = err[i];
        val = val.wrapping_add(d).wrapping_add(e);
        out[i] = sign_extend(val, bps);
        let sgn = e.signum();
        if sgn != 0 {
            let mut j = 0;
            while j < order && e.wrapping_mul(sgn) > 0 {
                let v = d.wrapping_sub(out[i - order + j]);
                let s = v.signum() * sgn;
                coefs[j] = coefs[j].wrapping_sub(s as i16);
                e = e.wrapping_sub((v.wrapping_mul(s) >> quant).wrapping_mul(j as i32 + 1));
                j += 1;
            }
        }
        i += 1;
    }
}

pub struct Decoder {
    pub cookie: Cookie,
}

impl Decoder {
    pub fn new(cookie: Cookie) -> Result<Self> {
        if cookie.channels == 0 || cookie.channels > 8 {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "ALAC supports 1 to 8 channels",
            ));
        }
        if !matches!(cookie.bit_depth, 16 | 20 | 24 | 32) {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!("ALAC bit depth {} is not supported", cookie.bit_depth),
            ));
        }
        if cookie.frame_len == 0 || cookie.frame_len > 1 << 20 {
            return Err(Error::corrupt("ALAC frame length is out of range"));
        }
        Ok(Self { cookie })
    }

    /// Decode one packet, appending interleaved samples to `out`. Returns the frame count.
    pub fn frame(&self, data: &[u8], out: &mut Vec<i32>) -> Result<usize> {
        let c = &self.cookie;
        let total_ch = usize::from(c.channels);
        let layout = CHANNEL_MAP[total_ch - 1];
        let mut b = Bits::new(data);
        let mut done = 0usize;
        let mut frames: Option<usize> = None;
        let mut chans: Vec<Vec<i32>> = vec![Vec::new(); total_ch];
        loop {
            let tag = b.read(3)?;
            match tag {
                0 | 1 | 3 => {
                    let n = if tag == 1 { 2 } else { 1 };
                    if done + n > total_ch {
                        return Err(Error::corrupt("ALAC frame has more channels than declared"));
                    }
                    b.skip(4 + 12)?;
                    let partial = b.read(1)? == 1;
                    let shift = b.read(2)? * 8;
                    let escape = b.read(1)? == 1;
                    let num = if partial {
                        b.read(32)? as usize
                    } else {
                        c.frame_len as usize
                    };
                    if num > c.frame_len as usize {
                        return Err(Error::corrupt(
                            "ALAC frame is longer than the declared frame length",
                        ));
                    }
                    if *frames.get_or_insert(num) != num {
                        return Err(Error::corrupt("ALAC elements disagree on the frame length"));
                    }
                    let depth = u32::from(c.bit_depth);
                    let mut el: Vec<Vec<i32>> = vec![vec![0; num]; n];
                    if escape {
                        for i in 0..num {
                            for ch in &mut el {
                                ch[i] = sign_extend(b.read(depth)? as i32, depth);
                            }
                        }
                    } else {
                        let mix_bits = b.read(8)?;
                        let mix_res = b.read(8)? as u8 as i8;
                        let mut hdr = Vec::with_capacity(n);
                        for _ in 0..n {
                            let h1 = b.read(8)?;
                            let h2 = b.read(8)?;
                            let order = (h2 & 31) as usize;
                            let mut coefs = Vec::with_capacity(order);
                            for _ in 0..order {
                                coefs.push(b.read(16)? as u16 as i16);
                            }
                            // the file lists the newest tap first; the predictor works oldest first
                            coefs.reverse();
                            hdr.push((h1 >> 4, h1 & 15, h2 >> 5, coefs));
                        }
                        if shift > 0 && shift < 32 {
                            // the low bytes are stored raw, ahead of the entropy-coded data
                            b.skip(shift as usize * num * n)?;
                        }
                        let after_shift = b.pos;
                        let bps = depth - shift + u32::from(n == 2);
                        let mut err = vec![0i32; num];
                        for (ch, (mode, den, pbf, coefs)) in hdr.iter_mut().enumerate() {
                            let p = Params {
                                mb: u32::from(c.mb),
                                pb: *pbf * u32::from(c.pb) / 4,
                                kb: u32::from(c.kb),
                            };
                            rice(&mut b, &p, &mut err, bps)?;
                            if *mode != 0 {
                                let mut tmp = vec![0i32; num];
                                predict(&err, &mut tmp, &mut [0i16; 31], 0, bps);
                                err.copy_from_slice(&tmp);
                            }
                            predict(&err, &mut el[ch], coefs, *den, bps);
                        }
                        if n == 2 && mix_res != 0 {
                            let w = i32::from(mix_res);
                            let (l, r) = el.split_at_mut(1);
                            for (a, bb) in l[0].iter_mut().zip(r[0].iter_mut()) {
                                let u = *a;
                                let v = *bb;
                                let u2 = u.wrapping_sub(v.wrapping_mul(w) >> mix_bits);
                                *a = u2.wrapping_add(v);
                                *bb = u2;
                            }
                        }
                        if shift > 0 && shift < 32 {
                            let end = b.pos;
                            b.pos = after_shift - shift as usize * num * n;
                            for i in 0..num {
                                for ch in &mut el {
                                    let lo = b.read(shift)? as i32;
                                    ch[i] = (ch[i] << shift) | lo;
                                }
                            }
                            b.pos = end;
                        }
                    }
                    for (j, ch) in el.into_iter().enumerate() {
                        let dst = layout.get(done + j).copied().unwrap_or(done + j);
                        if let Some(slot) = chans.get_mut(dst) {
                            *slot = ch;
                        }
                    }
                    done += n;
                }
                4 => {
                    // data stream element: skipped
                    b.skip(4)?;
                    let align = b.read(1)? == 1;
                    let mut count = b.read(8)? as usize;
                    if count == 255 {
                        count += b.read(8)? as usize;
                    }
                    if align {
                        b.align();
                    }
                    b.skip(count * 8)?;
                }
                6 => {
                    // fill element
                    let mut count = b.read(4)? as usize;
                    if count == 15 {
                        count += b.read(8)? as usize - 1;
                    }
                    b.skip(count * 8)?;
                }
                7 => break,
                _ => {
                    return Err(Error::new(
                        ErrorCode::UnsupportedVariant,
                        "ALAC coupling and program-config elements are not supported",
                    ))
                }
            }
            if b.left() == 0 {
                break;
            }
        }
        let n = frames.unwrap_or(0);
        if done != total_ch {
            return Err(Error::corrupt(
                "ALAC frame has fewer channels than declared",
            ));
        }
        out.reserve(n * total_ch);
        for i in 0..n {
            for ch in &chans {
                out.push(ch.get(i).copied().unwrap_or(0));
            }
        }
        Ok(n)
    }
}
