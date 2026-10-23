//! Band shape coding: bit allocation, PVQ band quantisation with splitting and folding,
//! and the stereo angle. The same code decodes and encodes: every call takes the value to
//! write and returns the value that was read or written.
use super::pvq;
use super::rc::{Rc, BITRES};
use super::tables::*;

pub const NB: usize = 21;
pub const SPREAD_NONE: i32 = 0;
pub const SPREAD_AGGRESSIVE: i32 = 3;

pub fn eb(i: usize) -> usize {
    EBANDS[i] as usize
}

pub fn lcg(seed: u32) -> u32 {
    seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

pub fn get_pulses(i: i32) -> i32 {
    if i < 8 {
        i
    } else {
        (8 + (i & 7)) << ((i >> 3) - 1)
    }
}

fn cache(lm: i32, band: usize) -> &'static [u8] {
    let idx = CACHE_INDEX[(lm + 1) as usize * NB + band];
    &CACHE_BITS[idx.max(0) as usize..]
}

pub fn bits2pulses(band: usize, lm: i32, bits: i32) -> i32 {
    let c = cache(lm, band);
    let mut lo = 0usize;
    let mut hi = c[0] as usize;
    let bits = bits - 1;
    for _ in 0..6 {
        let mid = (lo + hi + 1) >> 1;
        if i32::from(c[mid]) >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let lo_bits = if lo == 0 { -1 } else { i32::from(c[lo]) };
    if bits - lo_bits <= i32::from(c[hi]) - bits {
        lo as i32
    } else {
        hi as i32
    }
}

pub fn pulses2bits(band: usize, lm: i32, pulses: i32) -> i32 {
    if pulses == 0 {
        0
    } else {
        i32::from(cache(lm, band)[pulses as usize]) + 1
    }
}

pub struct Alloc {
    pub coded_bands: usize,
    pub intensity: usize,
    pub dual_stereo: bool,
    pub balance: i32,
    pub pulses: [i32; NB],
    pub ebits: [i32; NB],
    pub fine_priority: [i32; NB],
}

#[allow(clippy::too_many_arguments)]
pub fn compute_allocation(
    rc: &mut Rc<'_>,
    start: usize,
    end: usize,
    offsets: &[i32; NB],
    cap: &[i32; NB],
    alloc_trim: i32,
    intensity_in: usize,
    dual_in: bool,
    total: i32,
    c: usize,
    lm: usize,
) -> Alloc {
    let mut total = total.max(0);
    let mut skip_start = start;
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let mut intensity_rsv = 0;
    let mut dual_rsv = 0;
    if c == 2 {
        intensity_rsv = i32::from(LOG2_FRAC[end - start]);
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_rsv;
        }
    }
    let ci = c as i32;
    let mut thresh = [0i32; NB];
    let mut trim_offset = [0i32; NB];
    for j in start..end {
        let w = (eb(j + 1) - eb(j)) as i32;
        thresh[j] = (ci << BITRES).max((3 * w << lm << BITRES) >> 4);
        trim_offset[j] = ci * w * (alloc_trim - 5 - lm as i32) * (end - j - 1) as i32 * (1 << (lm as i32 + BITRES)) >> 6;
        if (w << lm) == 1 {
            trim_offset[j] -= ci << BITRES;
        }
    }
    let mut lo = 1usize;
    let mut hi = 10usize;
    loop {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for j in (start..end).rev() {
            let n = (eb(j + 1) - eb(j)) as i32;
            let mut bitsj = ci * n * i32::from(BAND_ALLOC[mid * NB + j]) << lm >> 2;
            if bitsj > 0 {
                bitsj = (bitsj + trim_offset[j]).max(0);
            }
            bitsj += offsets[j];
            if bitsj >= thresh[j] || done {
                done = true;
                psum += bitsj.min(cap[j]);
            } else if bitsj >= ci << BITRES {
                psum += ci << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
        if lo > hi {
            break;
        }
    }
    let hi = lo;
    let lo = lo - 1;
    let mut bits1 = [0i32; NB];
    let mut bits2 = [0i32; NB];
    for j in start..end {
        let n = (eb(j + 1) - eb(j)) as i32;
        let mut b1 = ci * n * i32::from(BAND_ALLOC[lo * NB + j]) << lm >> 2;
        let mut b2 = if hi >= 11 { cap[j] } else { ci * n * i32::from(BAND_ALLOC[hi * NB + j]) << lm >> 2 };
        if b1 > 0 {
            b1 = (b1 + trim_offset[j]).max(0);
        }
        if b2 > 0 {
            b2 = (b2 + trim_offset[j]).max(0);
        }
        if lo > 0 {
            b1 += offsets[j];
        }
        b2 += offsets[j];
        if offsets[j] > 0 {
            skip_start = j;
        }
        b2 = (b2 - b1).max(0);
        bits1[j] = b1;
        bits2[j] = b2;
    }
    // interp_bits2pulses
    let alloc_floor = ci << BITRES;
    let stereo = c > 1;
    let log_m = (lm as i32) << BITRES;
    let (mut lo, mut hi) = (0i32, 1i32 << 6);
    for _ in 0..6 {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + (mid * bits2[j] >> 6);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut psum = 0;
    let mut done = false;
    let mut bits = [0i32; NB];
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + (lo * bits2[j] >> 6);
        if tmp < thresh[j] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }
    let mut coded = end;
    loop {
        let j = coded - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let left = total - psum;
        let span = (eb(coded) - eb(start)) as i32;
        let percoeff = left / span;
        let left = left - span * percoeff;
        let rem = (left - (eb(j) - eb(start)) as i32).max(0);
        let band_width = (eb(coded) - eb(j)) as i32;
        let mut band_bits = bits[j] + percoeff * band_width + rem;
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if rc.bit_logp(true, 1) {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = i32::from(LOG2_FRAC[j - start]);
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }
        coded -= 1;
    }
    let mut intensity = 0usize;
    if intensity_rsv > 0 {
        let want = intensity_in.min(coded) - start;
        intensity = start + rc.uint(want as u32, (coded + 1 - start) as u32) as usize;
    }
    let mut dual = false;
    if intensity <= start {
        total += dual_rsv;
        dual_rsv = 0;
    }
    if dual_rsv > 0 {
        dual = rc.bit_logp(dual_in, 1);
    }
    let left = total - psum;
    let span = (eb(coded) - eb(start)) as i32;
    let percoeff = left / span;
    let mut left = left - span * percoeff;
    for j in start..coded {
        bits[j] += percoeff * (eb(j + 1) - eb(j)) as i32;
    }
    for j in start..coded {
        let tmp = left.min((eb(j + 1) - eb(j)) as i32);
        bits[j] += tmp;
        left -= tmp;
    }
    let mut ebits = [0i32; NB];
    let mut fine_priority = [0i32; NB];
    let mut balance = 0;
    let mut j = start;
    while j < coded {
        let n0 = (eb(j + 1) - eb(j)) as i32;
        let n = n0 << lm;
        let bit = bits[j] + balance;
        let mut excess;
        if n > 1 {
            excess = (bit - cap[j]).max(0);
            bits[j] = bit - excess;
            let den = ci * n + i32::from(c == 2 && n > 2 && !dual && j < intensity);
            let nclogn = den * (i32::from(LOG_N[j]) + log_m);
            let mut offset = (nclogn >> 1) - den * 21;
            if n == 2 {
                offset += den << BITRES >> 2;
            }
            if bits[j] + offset < den * 2 << BITRES {
                offset += nclogn >> 2;
            } else if bits[j] + offset < den * 3 << BITRES {
                offset += nclogn >> 3;
            }
            ebits[j] = (bits[j] + offset + (den << (BITRES - 1))).max(0);
            ebits[j] = (ebits[j] / den) >> BITRES;
            if ci * ebits[j] > bits[j] >> BITRES {
                ebits[j] = bits[j] >> i32::from(stereo) >> BITRES;
            }
            ebits[j] = ebits[j].min(8);
            fine_priority[j] = i32::from(ebits[j] * (den << BITRES) >= bits[j] + offset);
            bits[j] -= ci * ebits[j] << BITRES;
        } else {
            excess = (bit - (ci << BITRES)).max(0);
            bits[j] = bit - excess;
            ebits[j] = 0;
            fine_priority[j] = 1;
        }
        if excess > 0 {
            let extra_fine = (excess >> (i32::from(stereo) + BITRES)).min(8 - ebits[j]);
            ebits[j] += extra_fine;
            let extra_bits = extra_fine * ci << BITRES;
            fine_priority[j] = i32::from(extra_bits >= excess - balance);
            excess -= extra_bits;
        }
        balance = excess;
        j += 1;
    }
    let out_balance = balance;
    while j < end {
        ebits[j] = bits[j] >> i32::from(stereo) >> BITRES;
        bits[j] = 0;
        fine_priority[j] = i32::from(ebits[j] < 1);
        j += 1;
    }
    Alloc {
        coded_bands: coded,
        intensity,
        dual_stereo: dual,
        balance: out_balance,
        pulses: bits,
        ebits,
        fine_priority,
    }
}

pub fn init_caps(lm: usize, c: usize) -> [i32; NB] {
    let mut cap = [0i32; NB];
    for i in 0..NB {
        let n = ((eb(i + 1) - eb(i)) << lm) as i32;
        cap[i] = (i32::from(CACHE_CAPS[NB * (2 * lm + c - 1) + i]) + 64) * c as i32 * n >> 2;
    }
    cap
}

// ---------------------------------------------------------------------------------- vectors

fn renormalise(x: &mut [f32], gain: f32) {
    let e: f32 = 1e-15 + x.iter().map(|v| v * v).sum::<f32>();
    let g = gain / e.sqrt();
    for v in x {
        *v *= g;
    }
}

fn exp_rotation1(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    let ms = -s;
    if len < stride + 1 {
        return;
    }
    for i in 0..len - stride {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 + ms * x2;
    }
    if len >= 2 * stride + 1 {
        for i in (0..=len - 2 * stride - 1).rev() {
            let x1 = x[i];
            let x2 = x[i + stride];
            x[i + stride] = c * x2 + s * x1;
            x[i] = c * x1 + ms * x2;
        }
    }
}

pub fn exp_rotation(x: &mut [f32], len: usize, dir: i32, stride: usize, k: usize, spread: i32) {
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    const FACTOR: [f32; 3] = [15.0, 10.0, 5.0];
    let factor = FACTOR[(spread - 1) as usize];
    let gain = len as f32 / (len as f32 + factor * k as f32);
    let theta = 0.5 * gain * gain;
    let c = (std::f32::consts::FRAC_PI_2 * theta).cos();
    let s = (std::f32::consts::FRAC_PI_2 * (1.0 - theta)).cos();
    let mut stride2 = 0usize;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let len = len / stride;
    for i in 0..stride {
        let b = &mut x[i * len..(i + 1) * len];
        if dir < 0 {
            if stride2 > 0 {
                exp_rotation1(b, len, stride2, s, c);
            }
            exp_rotation1(b, len, 1, c, s);
        } else {
            exp_rotation1(b, len, 1, c, -s);
            if stride2 > 0 {
                exp_rotation1(b, len, stride2, c, -s);
            }
        }
    }
}

fn collapse_mask(iy: &[i32], n: usize, b: usize) -> u32 {
    if b <= 1 {
        return 1;
    }
    let n0 = n / b;
    let mut m = 0u32;
    for i in 0..b {
        let mut t = 0;
        for j in 0..n0 {
            t |= iy[i * n0 + j];
        }
        m |= u32::from(t != 0) << i;
    }
    m
}

fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let n0 = n0 >> 1;
    for i in 0..stride {
        for j in 0..n0 {
            let t1 = x[stride * 2 * j + i] * std::f32::consts::FRAC_1_SQRT_2;
            let t2 = x[stride * (2 * j + 1) + i] * std::f32::consts::FRAC_1_SQRT_2;
            x[stride * 2 * j + i] = t1 + t2;
            x[stride * (2 * j + 1) + i] = t1 - t2;
        }
    }
}

fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let mut tmp = vec![0f32; n];
    if hadamard {
        let ordery = &ORDERY[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[ordery[i] as usize * n0 + j] = x[j * stride + i];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[i * n0 + j] = x[j * stride + i];
            }
        }
    }
    x[..n].copy_from_slice(&tmp);
}

fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let mut tmp = vec![0f32; n];
    if hadamard {
        let ordery = &ORDERY[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[ordery[i] as usize * n0 + j];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[i * n0 + j];
            }
        }
    }
    x[..n].copy_from_slice(&tmp);
}

fn frac_mul16(a: i32, b: i32) -> i32 {
    (16384 + a * b) >> 15
}

fn bitexact_cos(x: i32) -> i32 {
    let tmp = (4096 + x * x) >> 13;
    let x2 = tmp;
    let x2 = (32767 - x2) + frac_mul16(x2, -7651 + frac_mul16(x2, 8277 + frac_mul16(-626, x2)));
    1 + x2
}

fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = (32 - (icos as u32).leading_zeros()) as i32;
    let ls = (32 - (isin as u32).leading_zeros()) as i32;
    let icos = icos << (15 - lc);
    let isin = isin << (15 - ls);
    (ls - lc) * (1 << 11) + frac_mul16(isin, frac_mul16(isin, -2597) + 7932)
        - frac_mul16(icos, frac_mul16(icos, -2597) + 7932)
}

fn isqrt32(v: u32) -> u32 {
    let mut r = (f64::from(v)).sqrt() as u32;
    while r * r > v {
        r -= 1;
    }
    while (r + 1) * (r + 1) <= v {
        r += 1;
    }
    r
}

fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    const EXP2: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let mut qb = (b + n2 * offset) / n2;
    qb = qb.min(b - pulse_cap - (4 << BITRES));
    qb = qb.min(8 << BITRES);
    if qb < (1 << BITRES >> 1) {
        1
    } else {
        let qn = EXP2[(qb & 7) as usize] >> (14 - (qb >> BITRES));
        (qn + 1) >> 1 << 1
    }
}

struct Split {
    inv: bool,
    imid: i32,
    iside: i32,
    delta: i32,
    itheta: i32,
    qalloc: i32,
}

pub struct Quant<'a, 'b> {
    pub rc: &'a mut Rc<'b>,
    pub i: usize,
    pub intensity: usize,
    pub spread: i32,
    pub tf_change: i32,
    pub remaining_bits: i32,
    pub seed: u32,
    pub encode: bool,
}

impl Quant<'_, '_> {
    #[allow(clippy::too_many_arguments)]
    fn compute_theta(
        &mut self,
        x: &[f32],
        y: &[f32],
        n: usize,
        b: &mut i32,
        bk: usize,
        b0: usize,
        lm: i32,
        stereo: bool,
        fill: &mut u32,
    ) -> Split {
        let i = self.i;
        let pulse_cap = i32::from(LOG_N[i]) + lm * (1 << BITRES);
        let offset = (pulse_cap >> 1) - if stereo && n == 2 { 16 } else { 4 };
        let mut qn = compute_qn(n as i32, *b, offset, pulse_cap, stereo);
        if stereo && i >= self.intensity {
            qn = 1;
        }
        let mut itheta = 0i32;
        if self.encode {
            // angle between the halves' energies
            let (mut e1, mut e2) = (0f32, 0f32);
            for j in 0..n {
                if stereo {
                    let m = x[j] + y[j];
                    let s = x[j] - y[j];
                    e1 += m * m;
                    e2 += s * s;
                } else {
                    e1 += x[j] * x[j];
                    e2 += y[j] * y[j];
                }
            }
            let t = (0.5 + 16384.0 * 0.636_62 * e2.sqrt().atan2(e1.sqrt())).floor() as i32;
            itheta = t;
        }
        let tell = self.rc.tell_frac();
        let mut inv = false;
        if qn != 1 {
            if self.encode {
                itheta = (itheta * qn + 8192) >> 14;
            }
            if stereo && n > 2 {
                let p0 = 3;
                let x0 = qn / 2;
                let ft = p0 * (x0 + 1) + x0;
                let mut xv = itheta;
                let fs = self.rc.decode(ft as u32) as i32;
                if !self.encode {
                    xv = if fs < (x0 + 1) * p0 { fs / p0 } else { x0 + 1 + (fs - (x0 + 1) * p0) };
                }
                let (fl, fh) = if xv <= x0 {
                    (p0 * xv, p0 * (xv + 1))
                } else {
                    ((xv - 1 - x0) + (x0 + 1) * p0, (xv - x0) + (x0 + 1) * p0)
                };
                self.rc.update_or_encode(fl as u32, fh as u32, ft as u32);
                itheta = xv;
            } else if b0 > 1 || stereo {
                itheta = self.rc.uint(itheta as u32, (qn + 1) as u32) as i32;
            } else {
                let half = qn >> 1;
                let ft = (half + 1) * (half + 1);
                let (fl, fs);
                if self.encode {
                    let xv = itheta;
                    if xv <= half {
                        fs = xv + 1;
                        fl = xv * (xv + 1) >> 1;
                    } else {
                        fs = qn + 1 - xv;
                        fl = ft - ((qn + 1 - xv) * (qn + 2 - xv) >> 1);
                    }
                } else {
                    let fm = self.rc.decode(ft as u32) as i32;
                    if fm < (half * (half + 1) >> 1) {
                        itheta = (isqrt32((8 * fm + 1) as u32) as i32 - 1) >> 1;
                        fs = itheta + 1;
                        fl = itheta * (itheta + 1) >> 1;
                    } else {
                        itheta = (2 * (qn + 1) - isqrt32((8 * (ft - fm - 1) + 1) as u32) as i32) >> 1;
                        fs = qn + 1 - itheta;
                        fl = ft - ((qn + 1 - itheta) * (qn + 2 - itheta) >> 1);
                    }
                }
                self.rc.update_or_encode(fl as u32, (fl + fs) as u32, ft as u32);
            }
            itheta = itheta * 16384 / qn;
        } else if stereo {
            inv = false;
            if *b > 2 << BITRES && self.remaining_bits > 2 << BITRES {
                inv = self.rc.bit_logp(false, 2);
            }
            itheta = 0;
        }
        let qalloc = self.rc.tell_frac() - tell;
        *b -= qalloc;
        let (imid, iside, delta);
        if itheta == 0 {
            imid = 32767;
            iside = 0;
            *fill &= (1u32 << bk) - 1;
            delta = -16384;
        } else if itheta == 16384 {
            imid = 0;
            iside = 32767;
            *fill &= ((1u32 << bk) - 1) << bk;
            delta = 16384;
        } else {
            imid = bitexact_cos(itheta);
            iside = bitexact_cos(16384 - itheta);
            delta = frac_mul16((n as i32 - 1) << 7, bitexact_log2tan(iside, imid));
        }
        Split { inv, imid, iside, delta, itheta, qalloc }
    }

    fn band_n1(&mut self, x: &mut [f32], y: Option<&mut [f32]>, lowband_out: Option<&mut [f32]>) -> u32 {
        let mut chans: Vec<&mut [f32]> = vec![x];
        if let Some(y) = y {
            chans.push(y);
        }
        for ch in &mut chans {
            let mut sign = false;
            if self.remaining_bits >= 1 << BITRES {
                sign = self.rc.bits(u32::from(ch[0] < 0.0), 1) == 1;
                self.remaining_bits -= 1 << BITRES;
            }
            ch[0] = if sign { -1.0 } else { 1.0 };
        }
        if let Some(lo) = lowband_out {
            lo[0] = chans[0][0];
        }
        1
    }

    #[allow(clippy::too_many_arguments)]
    pub fn band<'l>(
        &mut self,
        x: &mut [f32],
        n: usize,
        b: i32,
        mut bk: usize,
        mut lowband: Option<&'l mut [f32]>,
        lm: i32,
        lowband_out: Option<&mut [f32]>,
        gain: f32,
        mut scratch: Option<&'l mut [f32]>,
        mut fill: u32,
    ) -> u32 {
        let n0 = n;
        let mut n_b = n / bk;
        let b0_in = bk;
        let mut time_divide = 0;
        let mut recombine = 0usize;
        let long_blocks = bk == 1;
        let mut tf_change = self.tf_change;
        if n == 1 {
            return self.band_n1(x, None, lowband_out);
        }
        if tf_change > 0 {
            recombine = tf_change as usize;
        }
        if scratch.is_some() && lowband.is_some() && (recombine > 0 || ((n_b & 1) == 0 && tf_change < 0) || b0_in > 1) {
            if let (Some(s), Some(l)) = (scratch.take(), lowband.take()) {
                s[..n].copy_from_slice(&l[..n]);
                lowband = Some(&mut s[..n]);
            }
        }
        for k in 0..recombine {
            if self.encode {
                haar1(x, n >> k, 1 << k);
            }
            if let Some(l) = lowband.as_deref_mut() {
                haar1(l, n >> k, 1 << k);
            }
            fill = u32::from(BIT_INTERLEAVE[(fill & 0xF) as usize]) | u32::from(BIT_INTERLEAVE[(fill >> 4) as usize]) << 2;
        }
        bk >>= recombine;
        n_b <<= recombine;
        while (n_b & 1) == 0 && tf_change < 0 {
            if self.encode {
                haar1(x, n_b, bk);
            }
            if let Some(l) = lowband.as_deref_mut() {
                haar1(l, n_b, bk);
            }
            fill |= fill << bk;
            bk <<= 1;
            n_b >>= 1;
            time_divide += 1;
            tf_change += 1;
        }
        let b0 = bk;
        let n_b0 = n_b;
        if b0 > 1 {
            if self.encode {
                deinterleave_hadamard(x, n_b >> recombine, b0 << recombine, long_blocks);
            }
            if let Some(l) = lowband.as_deref_mut() {
                deinterleave_hadamard(l, n_b >> recombine, b0 << recombine, long_blocks);
            }
        }
        let mut cm = self.partition(x, n, b, bk, lowband.as_deref(), lm, gain, fill);
        // undo the reorganisation (also needed by an encoder that resynthesises)
        if !self.encode {
            if b0 > 1 {
                interleave_hadamard(x, n_b >> recombine, b0 << recombine, long_blocks);
            }
            let mut n_b = n_b0;
            let mut bb = b0;
            for _ in 0..time_divide {
                bb >>= 1;
                n_b <<= 1;
                cm |= cm >> bb;
                haar1(x, n_b, bb);
            }
            for k in 0..recombine {
                cm = u32::from(BIT_DEINTERLEAVE[(cm & 0xF) as usize]);
                haar1(x, n0 >> k, 1 << k);
            }
            bb <<= recombine;
            if let Some(lo) = lowband_out {
                let s = (n0 as f32).sqrt();
                for j in 0..n0 {
                    lo[j] = s * x[j];
                }
            }
            cm &= (1u32 << bb) - 1;
        }
        cm
    }

    /// The recursive part of a band: split in two while the budget allows, else PVQ.
    #[allow(clippy::too_many_arguments)]
    fn partition(&mut self, x: &mut [f32], n: usize, mut b: i32, mut bk: usize, lowband: Option<&[f32]>, mut lm: i32, gain: f32, mut fill: u32) -> u32 {
        let b0 = bk;
        let mut n_cur = n;
        let c = cache(lm, self.i);
        let mut cm: u32;
        if lm != -1 && b > i32::from(c[c[0] as usize]) + 12 && n > 2 {
            n_cur >>= 1;
            lm -= 1;
            if bk == 1 {
                fill = (fill & 1) | (fill << 1);
            }
            bk = bk.div_ceil(2);
            let (xa, xb) = x.split_at_mut(n_cur);
            let sp = self.compute_theta(xa, xb, n_cur, &mut b, bk, b0, lm, false, &mut fill);
            let (imid, iside, mut delta, itheta, qalloc) = (sp.imid, sp.iside, sp.delta, sp.itheta, sp.qalloc);
            let mid = imid as f32 / 32768.0;
            let side = iside as f32 / 32768.0;
            if b0 > 1 && (itheta & 0x3fff) != 0 {
                if itheta > 8192 {
                    delta -= delta >> (4 - lm);
                } else {
                    delta = 0.min(delta + ((n_cur as i32) << BITRES >> (5 - lm)));
                }
            }
            let mut mbits = 0.max(b.min((b - delta) / 2));
            let mut sbits = b - mbits;
            self.remaining_bits -= qalloc;
            let (lb_mid, lb_side) = match lowband {
                Some(l) => (Some(&l[..n_cur]), Some(&l[n_cur..])),
                None => (None, None),
            };
            let mut rebalance = self.remaining_bits;
            if mbits >= sbits {
                cm = self.partition(xa, n_cur, mbits, bk, lb_mid, lm, gain * mid, fill);
                rebalance = mbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                cm |= self.partition(xb, n_cur, sbits, bk, lb_side, lm, gain * side, fill >> bk) << (b0 >> 1);
            } else {
                cm = self.partition(xb, n_cur, sbits, bk, lb_side, lm, gain * side, fill >> bk) << (b0 >> 1);
                rebalance = sbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && itheta != 16384 {
                    mbits += rebalance - (3 << BITRES);
                }
                cm |= self.partition(xa, n_cur, mbits, bk, lb_mid, lm, gain * mid, fill);
            }
        } else {
            let mut q = bits2pulses(self.i, lm, b);
            let mut curr_bits = pulses2bits(self.i, lm, q);
            self.remaining_bits -= curr_bits;
            while self.remaining_bits < 0 && q > 0 {
                self.remaining_bits += curr_bits;
                q -= 1;
                curr_bits = pulses2bits(self.i, lm, q);
                self.remaining_bits -= curr_bits;
            }
            if q != 0 {
                let k = get_pulses(q) as usize;
                cm = self.alg(x, n_cur, k, bk, gain);
            } else {
                cm = 0;
                let cm_mask = (1u32 << bk) - 1;
                fill &= cm_mask;
                if fill == 0 {
                    for v in &mut x[..n_cur] {
                        *v = 0.0;
                    }
                } else {
                    if let Some(lb) = lowband {
                        for j in 0..n_cur {
                            self.seed = lcg(self.seed);
                            let tmp = if self.seed & 0x8000 != 0 { 1.0 / 256.0 } else { -1.0 / 256.0 };
                            x[j] = lb[j] + tmp;
                        }
                        cm = fill;
                    } else {
                        for v in &mut x[..n_cur] {
                            self.seed = lcg(self.seed);
                            *v = ((self.seed as i32) >> 20) as f32;
                        }
                        cm = cm_mask;
                    }
                    renormalise(&mut x[..n_cur], gain);
                }
            }
        }
        cm
    }

    /// PVQ-code one vector of `n` values with `k` pulses.
    fn alg(&mut self, x: &mut [f32], n: usize, k: usize, bk: usize, gain: f32) -> u32 {
        let mut iy = vec![0i32; n];
        if self.encode {
            exp_rotation(x, n, 1, bk, k, self.spread);
            pvq::search(&x[..n], &mut iy, k);
            let idx = pvq::icwrs(&iy);
            self.rc.uint(idx, pvq::v(n, k) as u32);
        } else {
            let idx = self.rc.uint(0, pvq::v(n, k) as u32);
            let yy = pvq::cwrsi(n, k, idx, &mut iy);
            let g = gain / yy.max(1e-30).sqrt();
            for j in 0..n {
                x[j] = g * iy[j] as f32;
            }
            exp_rotation(x, n, -1, bk, k, self.spread);
        }
        collapse_mask(&iy, n, bk)
    }

    #[allow(clippy::too_many_arguments)]
    fn band_stereo(
        &mut self,
        x: &mut [f32],
        y: &mut [f32],
        n: usize,
        mut b: i32,
        bk: usize,
        lowband: Option<&mut [f32]>,
        lm: i32,
        lowband_out: Option<&mut [f32]>,
        scratch: Option<&mut [f32]>,
        mut fill: u32,
    ) -> u32 {
        let orig_fill = fill;
        if n == 1 {
            return self.band_n1(x, Some(y), lowband_out);
        }
        let sp = self.compute_theta(x, y, n, &mut b, bk, bk, lm, true, &mut fill);
        let (imid, iside, delta, itheta, qalloc, inv) = (sp.imid, sp.iside, sp.delta, sp.itheta, sp.qalloc, sp.inv);
        let mid = imid as f32 / 32768.0;
        let side = iside as f32 / 32768.0;
        let mut cm;
        if n == 2 {
            let mut mbits = b;
            let mut sbits = 0;
            if itheta != 0 && itheta != 16384 {
                sbits = 1 << BITRES;
            }
            mbits -= sbits;
            let c = itheta > 8192;
            self.remaining_bits -= qalloc + sbits;
            let mut sign = 0u32;
            if sbits > 0 {
                sign = self.rc.bits(0, 1);
            }
            let sg = 1.0 - 2.0 * sign as f32;
            let (x2, y2) = if c { (&mut *y, &mut *x) } else { (&mut *x, &mut *y) };
            cm = self.band(x2, n, mbits, bk, lowband, lm, lowband_out, 1.0, scratch, orig_fill);
            y2[0] = -sg * x2[1];
            y2[1] = sg * x2[0];
            if !self.encode {
                for j in 0..2 {
                    let m = mid * x[j];
                    let sd = side * y[j];
                    x[j] = m - sd;
                    y[j] = m + sd;
                }
            }
        } else {
            let mut mbits = 0.max(b.min((b - delta) / 2));
            let mut sbits = b - mbits;
            self.remaining_bits -= qalloc;
            let mut rebalance = self.remaining_bits;
            if mbits >= sbits {
                cm = self.band(x, n, mbits, bk, lowband, lm, lowband_out, 1.0, scratch, fill);
                rebalance = mbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                cm |= self.band(y, n, sbits, bk, None, lm, None, side, None, fill >> bk);
            } else {
                cm = self.band(y, n, sbits, bk, None, lm, None, side, None, fill >> bk);
                rebalance = sbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && itheta != 16384 {
                    mbits += rebalance - (3 << BITRES);
                }
                cm |= self.band(x, n, mbits, bk, lowband, lm, lowband_out, 1.0, scratch, fill);
            }
        }
        if n != 2 && !self.encode {
            // stereo_merge
            let mut xp = 0f32;
            let mut sd = 0f32;
            for j in 0..n {
                xp += y[j] * x[j];
                sd += y[j] * y[j];
            }
            xp *= mid;
            let el = mid * mid + sd - 2.0 * xp;
            let er = mid * mid + sd + 2.0 * xp;
            if er < 6e-4 || el < 6e-4 {
                y[..n].copy_from_slice(&x[..n]);
            } else {
                let lg = 1.0 / el.sqrt();
                let rg = 1.0 / er.sqrt();
                for j in 0..n {
                    let l = mid * x[j];
                    let r = y[j];
                    x[j] = lg * (l - r);
                    y[j] = rg * (l + r);
                }
            }
        }
        if inv {
            for v in &mut y[..n] {
                *v = -*v;
            }
        }
        cm
    }
}

/// Code (or decode) all bands of a frame. `x` holds `c` channels of `n` values each.
#[allow(clippy::too_many_arguments)]
pub fn quant_all_bands(
    rc: &mut Rc<'_>,
    start: usize,
    end: usize,
    x: &mut [f32],
    c: usize,
    collapse_masks: &mut [u8],
    pulses: &[i32; NB],
    short_blocks: bool,
    spread: i32,
    mut dual_stereo: bool,
    intensity: usize,
    tf_res: &[i32; NB],
    total_bits: i32,
    mut balance: i32,
    lm: usize,
    coded_bands: usize,
    seed: &mut u32,
) {
    let m = 1usize << lm;
    let n_total = m * EBANDS[NB] as usize;
    let nn = 120usize << lm;
    let bk = if short_blocks { m } else { 1 };
    let norm_offset = m * eb(start);
    let norm_len = m * eb(NB - 1) - norm_offset;
    let mut norm = vec![0f32; norm_len.max(1) + 8];
    let mut norm2 = vec![0f32; norm_len.max(1) + 8];
    let mut scratch = vec![0f32; m * (eb(NB) - eb(NB - 1))];
    let encode = rc.encoding();
    let _ = n_total;
    let mut q = Quant { rc, i: 0, intensity, spread, tf_change: 0, remaining_bits: 0, seed: *seed, encode };
    let mut update_lowband = true;
    let mut lowband_offset = 0usize;
    let (xa, xb) = x.split_at_mut(nn);
    for i in start..end {
        q.i = i;
        let last = i + 1 == end;
        let n = m * eb(i + 1) - m * eb(i);
        let tell = q.rc.tell_frac();
        if i != start {
            balance -= tell;
        }
        let remaining_bits = total_bits - tell - 1;
        q.remaining_bits = remaining_bits;
        let b = if i < coded_bands {
            let curr_balance = balance / 3.min((coded_bands - i) as i32);
            0.max(16383.min((remaining_bits + 1).min(pulses[i] + curr_balance)))
        } else {
            0
        };
        if (m * eb(i) >= m * eb(start) + n || i == start + 1) && (update_lowband || lowband_offset == 0) {
            lowband_offset = i;
        }
        if i == start + 1 {
            // special_hybrid_folding
            let n1 = m * (eb(start + 1) - eb(start));
            let n2 = m * (eb(start + 2) - eb(start + 1));
            if n2 > n1 {
                let (src, dst) = (2 * n1 - n2, n1);
                for buf in [&mut norm, &mut norm2] {
                    for j in 0..n2 - n1 {
                        buf[dst + j] = buf[src + j];
                    }
                }
            }
        }
        q.tf_change = tf_res[i];
        let mut effective_lowband: Option<usize> = None;
        let (mut x_cm, mut y_cm);
        if lowband_offset != 0 && (spread != SPREAD_AGGRESSIVE || bk > 1 || tf_res[i] < 0) {
            let eff = (m * eb(lowband_offset) as usize).saturating_sub(norm_offset + n);
            effective_lowband = Some(eff);
            let mut fold_start = lowband_offset;
            loop {
                fold_start -= 1;
                if m * eb(fold_start) <= eff + norm_offset {
                    break;
                }
            }
            let mut fold_end = lowband_offset - 1;
            loop {
                fold_end += 1;
                if !(fold_end < i && m * eb(fold_end) < eff + norm_offset + n) {
                    break;
                }
            }
            x_cm = 0u32;
            y_cm = 0u32;
            for f in fold_start..fold_end {
                x_cm |= u32::from(collapse_masks[f * c]);
                y_cm |= u32::from(collapse_masks[f * c + c - 1]);
            }
        } else {
            x_cm = (1u32 << bk) - 1;
            y_cm = x_cm;
        }
        if dual_stereo && i == intensity {
            dual_stereo = false;
            for j in 0..m * eb(i) - norm_offset {
                norm[j] = 0.5 * (norm[j] + norm2[j]);
            }
        }
        let out_pos = m * eb(i) - norm_offset;
        let band_x = m * eb(i)..m * eb(i + 1);
        if dual_stereo {
            let (lo, hi) = norm.split_at_mut(out_pos);
            let lb = effective_lowband.map(|e| &mut lo[e..]);
            let out = if last { None } else { Some(&mut hi[..n]) };
            x_cm = q.band(&mut xa[band_x.clone()], n, b / 2, bk, lb, lm as i32, out, 1.0, Some(&mut scratch), x_cm);
            let (lo2, hi2) = norm2.split_at_mut(out_pos);
            let lb2 = effective_lowband.map(|e| &mut lo2[e..]);
            let out2 = if last { None } else { Some(&mut hi2[..n]) };
            y_cm = q.band(&mut xb[band_x], n, b / 2, bk, lb2, lm as i32, out2, 1.0, Some(&mut scratch), y_cm);
        } else {
            let (lo, hi) = norm.split_at_mut(out_pos);
            let lb = effective_lowband.map(|e| &mut lo[e..]);
            let out = if last { None } else { Some(&mut hi[..n]) };
            if c == 2 {
                x_cm = q.band_stereo(&mut xa[band_x.clone()], &mut xb[band_x], n, b, bk, lb, lm as i32, out, Some(&mut scratch), x_cm | y_cm);
            } else {
                x_cm = q.band(&mut xa[band_x], n, b, bk, lb, lm as i32, out, 1.0, Some(&mut scratch), x_cm | y_cm);
            }
            y_cm = x_cm;
        }
        collapse_masks[i * c] = x_cm as u8;
        collapse_masks[i * c + c - 1] = y_cm as u8;
        balance += pulses[i] + tell;
        update_lowband = b > (n as i32) << BITRES;
    }
    *seed = q.seed;
}
