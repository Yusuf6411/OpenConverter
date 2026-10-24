//! The CELT layer: frame decoder and encoder at 48 kHz.
use super::bands::{self, eb, quant_all_bands, NB};
use super::mdct::Mdct;
use super::rc::{Dec, Enc, Rc, BITRES};
use super::tables::*;

pub const OVERLAP: usize = 120;
const SHORT: usize = 120;
const HISTORY: usize = 2048;
pub(super) const PREEMPH: f32 = 0.850_006_1;

pub(super) fn window() -> &'static [f32; OVERLAP] {
    use std::sync::OnceLock;
    static W: OnceLock<[f32; OVERLAP]> = OnceLock::new();
    W.get_or_init(|| {
        let mut w = [0f32; OVERLAP];
        for (i, v) in w.iter_mut().enumerate() {
            let s = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / OVERLAP as f64).sin();
            *v = (0.5 * std::f64::consts::PI * s * s).sin() as f32;
        }
        w
    })
}

pub(super) fn mdct_for(lm: usize, short: bool) -> &'static Mdct {
    use std::sync::OnceLock;
    static M: OnceLock<Vec<Mdct>> = OnceLock::new();
    let all = M.get_or_init(|| (0..4).map(|l| Mdct::new(SHORT << l)).collect());
    &all[if short { 0 } else { lm }]
}

/// The reference decoder's polynomial `2^x` (matched so that output agrees to the last bits).
fn exp2(x: f32) -> f32 {
    let integer = x.floor();
    if integer < -50.0 {
        return 0.0;
    }
    let frac = x - integer;
    let p = 0.999_925_2 + frac * (0.695_833_54 + frac * (0.226_067_16 + frac * 0.078_024_52));
    p * (integer).exp2()
}

/// Add one windowed inverse block into `out[..n + OVERLAP]` (the first OVERLAP samples are
/// the overlap with the previous block).
fn imdct_block(m: &Mdct, coefs: &[f32], out: &mut [f32], n: usize) {
    let mut y = vec![0f32; 2 * n];
    m.inverse(coefs, &mut y);
    let w = window();
    let scale = n as f32;
    let base = n / 2 - OVERLAP / 2;
    for i in 0..OVERLAP {
        out[i] += y[base + i] * w[i] * scale;
    }
    for i in OVERLAP..n {
        out[i] += y[base + i] * scale;
    }
    for i in 0..OVERLAP {
        out[n + i] += y[base + n + i] * w[OVERLAP - 1 - i] * scale;
    }
}

#[allow(clippy::too_many_arguments)]
fn comb_filter(
    buf: &mut [f32],
    at: usize,
    t0: usize,
    t1: usize,
    n: usize,
    g0: f32,
    g1: f32,
    tap0: usize,
    tap1: usize,
    overlap: usize,
) {
    if g0 == 0.0 && g1 == 0.0 {
        return;
    }
    let t0 = t0.max(15);
    let t1 = t1.max(15);
    let (g00, g01, g02) = (
        g0 * COMB_GAINS[tap0][0],
        g0 * COMB_GAINS[tap0][1],
        g0 * COMB_GAINS[tap0][2],
    );
    let (g10, g11, g12) = (
        g1 * COMB_GAINS[tap1][0],
        g1 * COMB_GAINS[tap1][1],
        g1 * COMB_GAINS[tap1][2],
    );
    let x = |b: &[f32], i: isize| b[(at as isize + i) as usize];
    let mut x1 = x(buf, 1 - t1 as isize);
    let mut x2 = x(buf, -(t1 as isize));
    let mut x3 = x(buf, -(t1 as isize) - 1);
    let mut x4 = x(buf, -(t1 as isize) - 2);
    let overlap = if g0 == g1 && t0 == t1 && tap0 == tap1 {
        0
    } else {
        overlap
    };
    let w = window();
    let mut i = 0usize;
    let (t0i, t1i) = (t0 as isize, t1 as isize);
    while i < overlap.min(n) {
        let f = w[i] * w[i];
        let ii = i as isize;
        let x0 = x(buf, ii - t1i + 2);
        let v = x(buf, ii)
            + (1.0 - f) * g00 * x(buf, ii - t0i)
            + (1.0 - f) * g01 * (x(buf, ii - t0i + 1) + x(buf, ii - t0i - 1))
            + (1.0 - f) * g02 * (x(buf, ii - t0i + 2) + x(buf, ii - t0i - 2))
            + f * g10 * x2
            + f * g11 * (x1 + x3)
            + f * g12 * (x0 + x4);
        buf[at + i] = v;
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
        i += 1;
    }
    if g1 == 0.0 {
        return;
    }
    // constant filter for the rest
    let mut x4 = x(buf, i as isize - t1i - 2);
    let mut x3 = x(buf, i as isize - t1i - 1);
    let mut x2 = x(buf, i as isize - t1i);
    let mut x1 = x(buf, i as isize - t1i + 1);
    while i < n {
        let x0 = x(buf, i as isize - t1i + 2);
        buf[at + i] = x(buf, i as isize) + g10 * x2 + g11 * (x1 + x3) + g12 * (x0 + x4);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
        i += 1;
    }
}

fn tf_code(
    rc: &mut Rc<'_>,
    start: usize,
    end: usize,
    transient: bool,
    lm: usize,
    tf_in: &[i32; NB],
) -> [i32; NB] {
    let mut budget = rc.storage() as i32 * 8;
    let mut tell = rc.tell();
    let mut logp = if transient { 2 } else { 4 };
    let sel_rsv = lm > 0 && tell + logp < budget;
    if sel_rsv {
        budget -= 1;
    }
    let mut tf_res = [0i32; NB];
    let mut changed = 0;
    let mut curr = 0;
    for i in start..end {
        if tell + logp <= budget {
            let bit = rc.bit_logp((tf_in[i] ^ curr) != 0, logp as u32);
            curr ^= i32::from(bit);
            tell = rc.tell();
            changed |= curr;
        }
        tf_res[i] = curr;
        logp = if transient { 4 } else { 5 };
    }
    let t = usize::from(transient) * 4;
    let mut select = 0;
    if sel_rsv && TF_SELECT[lm][t + changed as usize] != TF_SELECT[lm][t + 2 + changed as usize] {
        select = i32::from(rc.bit_logp(false, 1));
    }
    for i in start..end {
        tf_res[i] = i32::from(TF_SELECT[lm][t + 2 * select as usize + tf_res[i] as usize]);
    }
    tf_res
}

pub struct Celt {
    pub channels: usize,
    old_band_e: [f32; 2 * NB],
    old_log_e: [f32; 2 * NB],
    old_log_e2: [f32; 2 * NB],
    rng: u32,
    mem: Vec<Vec<f32>>,
    deemph: [f32; 2],
    pf_period: usize,
    pf_gain: f32,
    pf_tapset: usize,
    pf_period_old: usize,
    pf_gain_old: f32,
    pf_tapset_old: usize,
}

impl Celt {
    pub fn new(channels: usize) -> Self {
        Self {
            channels,
            old_band_e: [0.0; 2 * NB],
            old_log_e: [-28.0; 2 * NB],
            old_log_e2: [-28.0; 2 * NB],
            rng: 0,
            mem: vec![vec![0.0; HISTORY + OVERLAP]; channels],
            deemph: [0.0; 2],
            pf_period: 0,
            pf_gain: 0.0,
            pf_tapset: 0,
            pf_period_old: 0,
            pf_gain_old: 0.0,
            pf_tapset_old: 0,
        }
    }

    /// Encode one frame into `nbytes` bytes (the range coder pads to exactly that size).
    pub fn encode(
        &mut self,
        input: &EncInput,
        lm: usize,
        stream_ch: usize,
        end: usize,
        nbytes: usize,
    ) -> Vec<u8> {
        let mut rc = Rc::Enc(Enc::new(nbytes));
        self.frame(&mut rc, lm, stream_ch, end, Some(input));
        match rc {
            Rc::Enc(e) => e.finish().0,
            Rc::Dec(_) => Vec::new(),
        }
    }

    pub fn final_range(&self) -> u32 {
        self.rng
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.channels);
    }

    /// Decode one frame of `lm` (0..=3: 2.5, 5, 10, 20 ms) coding `stream_ch` channels, using
    /// bands below `end`. Appends `120 << lm` interleaved frames of `self.channels` channels.
    pub fn decode(
        &mut self,
        data: &[u8],
        lm: usize,
        stream_ch: usize,
        end: usize,
        out: &mut Vec<f32>,
    ) {
        let cc = self.channels;
        let n = SHORT << lm;
        if data.len() <= 1 {
            out.extend(std::iter::repeat_n(0.0, n * cc));
            return;
        }
        let mut rc = Rc::Dec(Dec::new(data));
        let (o, ..) = self.frame(&mut rc, lm, stream_ch, end, None);
        self.synth_out(o, n, out);
    }

    fn synth_out(&mut self, o: usize, n: usize, out: &mut Vec<f32>) {
        let cc = self.channels;
        for i in 0..n {
            for c in 0..cc {
                let tmp = self.mem[c][o + i] + self.deemph[c];
                self.deemph[c] = PREEMPH * tmp;
                out.push(tmp * (1.0 / 32768.0));
            }
        }
    }

    /// Run the frame syntax in either direction. For encoding, `enc` supplies the analysis
    /// (band energies and normalised spectra) and the frame is built by the writer.
    pub(super) fn frame(
        &mut self,
        rc: &mut Rc<'_>,
        lm: usize,
        c: usize,
        end: usize,
        enc: Option<&EncInput>,
    ) -> (usize,) {
        let cc = self.channels;
        let m = 1usize << lm;
        let n = SHORT << lm;
        let start = 0usize;
        let len = rc.storage();
        let encoding = rc.encoding();
        if c == 1 && cc == 2 {
            for i in 0..NB {
                self.old_band_e[i] = self.old_band_e[i].max(self.old_band_e[NB + i]);
            }
        }
        for m in &mut self.mem {
            m.copy_within(n.., 0);
        }
        let total_bits = len as i32 * 8;
        let mut tell = rc.tell();
        let silence_in = enc.is_some_and(|e| e.silence);
        let mut silence = false;
        if tell >= total_bits {
            silence = true;
        } else if tell == 1 {
            silence = rc.bit_logp(silence_in, 15);
        }
        if silence {
            if let Rc::Dec(d) = rc {
                d.exhaust();
            }
            tell = len as i32 * 8;
        }
        let mut pf_gain = 0.0f32;
        let mut pf_pitch = 0usize;
        let mut pf_tapset = 0usize;
        if !silence && tell + 16 <= total_bits {
            if rc.bit_logp(false, 1) {
                let octave = rc.uint(0, 6);
                pf_pitch = (16usize << octave) + rc.bits(0, 4 + octave) as usize - 1;
                let qg = rc.bits(0, 3);
                if rc.tell() + 2 <= total_bits {
                    pf_tapset = rc.icdf(0, &TAPSET_ICDF, 2);
                }
                pf_gain = 0.093_75 * (qg + 1) as f32;
            }
            tell = rc.tell();
        }
        let mut transient = false;
        if lm > 0 && tell + 3 <= total_bits {
            transient = rc.bit_logp(false, 3);
            tell = rc.tell();
        }
        let short_blocks = transient;
        let mut intra = false;
        if tell + 3 <= total_bits {
            intra = rc.bit_logp(enc.is_some_and(|e| e.intra), 3);
        }
        // coarse energy
        let mut error = [0f32; 2 * NB];
        {
            let (coef, beta) = if intra {
                (0.0, BETA_INTRA)
            } else {
                (PRED_COEF[lm], BETA_COEF[lm])
            };
            let model = &E_PROB_MODEL[(lm * 2 + usize::from(intra)) * 42..][..42];
            let budget = len as i32 * 8;
            let mut prev = [0f32; 2];
            for i in start..end {
                for ch in 0..c {
                    let tell = rc.tell();
                    let x = enc.map_or(0.0, |e| e.band_log_e[ch * NB + i]);
                    let old_e = self.old_band_e[ch * NB + i].max(-9.0);
                    let f = x - coef * old_e - prev[ch];
                    let mut qi = (0.5 + f).floor() as i32;
                    if encoding {
                        let decay_bound = self.old_band_e[ch * NB + i].max(-28.0) - 16.0;
                        if qi < 0 && x < decay_bound {
                            qi += (decay_bound - x) as i32;
                            if qi > 0 {
                                qi = 0;
                            }
                        }
                        let bits_left = budget - tell - 3 * c as i32 * (end - i) as i32;
                        if i != start && bits_left < 30 {
                            if bits_left < 24 {
                                qi = qi.min(1);
                            }
                            if bits_left < 16 {
                                qi = qi.max(-1);
                            }
                        }
                    }
                    if budget - tell >= 15 {
                        let pi = 2 * i.min(20);
                        qi = rc.laplace(
                            qi,
                            u32::from(model[pi]) << 7,
                            u32::from(model[pi + 1]) << 6,
                        );
                    } else if budget - tell >= 2 {
                        qi = qi.clamp(-1, 1);
                        let sym = rc.icdf(
                            (2 * qi ^ -i32::from(qi < 0)) as usize,
                            &SMALL_ENERGY_ICDF,
                            2,
                        ) as i32;
                        qi = (sym >> 1) ^ -(sym & 1);
                    } else if budget - tell >= 1 {
                        qi = -i32::from(rc.bit_logp(qi < 0, 1));
                    } else {
                        qi = -1;
                    }
                    let q = qi as f32;
                    error[ch * NB + i] = f - q;
                    let tmp = (coef * old_e + prev[ch] + q).max(-28.0);
                    self.old_band_e[ch * NB + i] = tmp;
                    prev[ch] = prev[ch] + q - beta * q;
                }
            }
        }
        let tf_in = enc.map_or([0i32; NB], |_| [0i32; NB]);
        let tf_res = tf_code(rc, start, end, transient, lm, &tf_in);
        tell = rc.tell();
        let mut spread = 2;
        if tell + 4 <= total_bits {
            spread = rc.icdf(2, &SPREAD_ICDF, 5) as i32;
        }
        let cap = bands::init_caps(lm, c);
        let mut offsets = [0i32; NB];
        let mut dynalloc_logp = 6i32;
        let mut total_frac = total_bits << BITRES;
        let mut tellf = rc.tell_frac();
        for i in start..end {
            let width = (c * (eb(i + 1) - eb(i)) << lm) as i32;
            let quanta = (width << BITRES).min(width.max(6 << BITRES));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0;
            while tellf + (loop_logp << BITRES) < total_frac && boost < cap[i] {
                let flag = rc.bit_logp(false, loop_logp as u32);
                tellf = rc.tell_frac();
                if !flag {
                    break;
                }
                boost += quanta;
                total_frac -= quanta;
                loop_logp = 1;
            }
            offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = 2.max(dynalloc_logp - 1);
            }
        }
        let mut alloc_trim = 5;
        if tellf + (6 << BITRES) <= total_frac {
            alloc_trim = rc.icdf(5, &TRIM_ICDF, 7) as i32;
        }
        let mut bits = ((len as i32 * 8) << BITRES) - rc.tell_frac() - 1;
        let anti_collapse_rsv = if transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;
        let al = bands::compute_allocation(
            rc, start, end, &offsets, &cap, alloc_trim, end, true, bits, c, lm,
        );
        // fine energy
        for i in start..end {
            if al.ebits[i] <= 0 {
                continue;
            }
            for ch in 0..c {
                let frac = 1i32 << al.ebits[i];
                let mut q2 = ((error[ch * NB + i] + 0.5) * frac as f32).floor() as i32;
                q2 = q2.clamp(0, frac - 1);
                let q2 = rc.bits(q2 as u32, al.ebits[i] as u32) as i32;
                let off =
                    (q2 as f32 + 0.5) * (1 << (14 - al.ebits[i])) as f32 * (1.0 / 16384.0) - 0.5;
                self.old_band_e[ch * NB + i] += off;
                error[ch * NB + i] -= off;
            }
        }
        // shapes
        let mut x = vec![0f32; c * n];
        if let Some(e) = enc {
            for ch in 0..c {
                x[ch * n..(ch + 1) * n].copy_from_slice(&e.x[ch * n..(ch + 1) * n]);
            }
        }
        let mut collapse = vec![0u8; c * NB];
        let mut seed = self.rng;
        quant_all_bands(
            rc,
            start,
            end,
            &mut x,
            c,
            &mut collapse,
            &al.pulses,
            short_blocks,
            spread,
            al.dual_stereo,
            al.intensity,
            &tf_res,
            (len as i32) * (8 << BITRES) - anti_collapse_rsv,
            al.balance,
            lm,
            al.coded_bands,
            &mut seed,
        );
        let mut anti_collapse_on = false;
        if anti_collapse_rsv > 0 {
            anti_collapse_on = rc.bits(0, 1) == 1;
        }
        // leftover fine bits
        {
            let mut bits_left = len as i32 * 8 - rc.tell();
            for prio in 0..2 {
                let mut i = start;
                while i < end && bits_left >= c as i32 {
                    if al.ebits[i] >= 8 || al.fine_priority[i] != prio {
                        i += 1;
                        continue;
                    }
                    for ch in 0..c {
                        let q2 = rc.bits(u32::from(error[ch * NB + i] >= 0.0), 1) as i32;
                        let off = (q2 as f32 - 0.5)
                            * (1 << (14 - al.ebits[i] - 1)) as f32
                            * (1.0 / 16384.0);
                        self.old_band_e[ch * NB + i] += off;
                        error[ch * NB + i] -= off;
                        bits_left -= 1;
                    }
                    i += 1;
                }
            }
        }
        if encoding {
            self.rng = rc.rng();
            return (0,);
        }
        if anti_collapse_on {
            self.anti_collapse(&mut x, &collapse, lm, c, n, start, end, &al.pulses, seed);
        }
        if silence {
            for v in &mut self.old_band_e {
                *v = -28.0;
            }
        }
        // synthesis
        let o = HISTORY - n;
        for ch in 0..cc {
            for v in &mut self.mem[ch][o + OVERLAP..] {
                *v = 0.0;
            }
        }
        let (blocks, nb) = if transient {
            (m, SHORT)
        } else {
            (1, SHORT << lm)
        };
        let denorm = |xs: &[f32], e: &[f32], freq: &mut [f32]| {
            if silence {
                for v in freq.iter_mut() {
                    *v = 0.0;
                }
                return;
            }
            for v in freq.iter_mut() {
                *v = 0.0;
            }
            for i in start..end {
                let lg = (e[i] + E_MEANS[i]).min(32.0);
                let g = exp2(lg);
                for j in m * eb(i)..m * eb(i + 1) {
                    freq[j] = xs[j] * g;
                }
            }
        };
        let synth = |this: &mut Self, ch: usize, freq: &[f32]| {
            let md = mdct_for(lm, transient);
            for b in 0..blocks {
                let coefs: Vec<f32> = (0..nb).map(|k| freq[b + k * blocks]).collect();
                imdct_block(md, &coefs, &mut this.mem[ch][o + nb * b..], nb);
            }
        };
        let mut freq = vec![0f32; n];
        if cc == 2 && c == 1 {
            denorm(&x[..n], &self.old_band_e[..NB], &mut freq);
            synth(self, 0, &freq);
            synth(self, 1, &freq);
        } else if cc == 1 && c == 2 {
            let mut f2 = vec![0f32; n];
            denorm(&x[..n], &self.old_band_e[..NB], &mut freq);
            denorm(&x[n..2 * n], &self.old_band_e[NB..], &mut f2);
            for i in 0..n {
                freq[i] = 0.5 * freq[i] + 0.5 * f2[i];
            }
            synth(self, 0, &freq);
        } else {
            for ch in 0..cc {
                denorm(
                    &x[ch * n..(ch + 1) * n],
                    &self.old_band_e[ch * NB..(ch + 1) * NB],
                    &mut freq,
                );
                synth(self, ch, &freq);
            }
        }
        // post-filter
        for ch in 0..cc {
            comb_filter(
                &mut self.mem[ch],
                o,
                self.pf_period_old,
                self.pf_period,
                SHORT,
                self.pf_gain_old,
                self.pf_gain,
                self.pf_tapset_old,
                self.pf_tapset,
                OVERLAP,
            );
            if lm != 0 {
                comb_filter(
                    &mut self.mem[ch],
                    o + SHORT,
                    self.pf_period,
                    pf_pitch,
                    n - SHORT,
                    self.pf_gain,
                    pf_gain,
                    self.pf_tapset,
                    pf_tapset,
                    OVERLAP,
                );
            }
        }
        self.pf_period_old = self.pf_period;
        self.pf_gain_old = self.pf_gain;
        self.pf_tapset_old = self.pf_tapset;
        self.pf_period = pf_pitch;
        self.pf_gain = pf_gain;
        self.pf_tapset = pf_tapset;
        if lm != 0 {
            self.pf_period_old = self.pf_period;
            self.pf_gain_old = self.pf_gain;
            self.pf_tapset_old = self.pf_tapset;
        }
        if !transient {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_band_e;
        } else {
            for i in 0..2 * NB {
                self.old_log_e[i] = self.old_log_e[i].min(self.old_band_e[i]);
            }
        }
        for ch in 0..2 {
            for i in (0..start).chain(end..NB) {
                self.old_band_e[ch * NB + i] = 0.0;
                self.old_log_e[ch * NB + i] = -28.0;
                self.old_log_e2[ch * NB + i] = -28.0;
            }
        }
        self.rng = rc.rng();
        let _ = cc;
        (o,)
    }

    #[allow(clippy::too_many_arguments)]
    fn anti_collapse(
        &self,
        x: &mut [f32],
        masks: &[u8],
        lm: usize,
        c: usize,
        size: usize,
        start: usize,
        end: usize,
        pulses: &[i32; NB],
        mut seed: u32,
    ) {
        for i in start..end {
            let n0 = eb(i + 1) - eb(i);
            let depth = ((1 + pulses[i]) / n0 as i32) >> lm;
            let thresh = 0.5 * exp2(-0.125 * depth as f32);
            let sqrt_1 = 1.0 / ((n0 << lm) as f32).sqrt();
            for ch in 0..c {
                let mut prev1 = self.old_log_e[ch * NB + i];
                let mut prev2 = self.old_log_e2[ch * NB + i];
                if c == 1 {
                    prev1 = prev1.max(self.old_log_e[NB + i]);
                    prev2 = prev2.max(self.old_log_e2[NB + i]);
                }
                let ediff = (self.old_band_e[ch * NB + i] - prev1.min(prev2)).max(0.0);
                let mut r = 2.0 * exp2(-ediff);
                if lm == 3 {
                    r *= std::f32::consts::SQRT_2;
                }
                r = thresh.min(r) * sqrt_1;
                let base = ch * size + (eb(i) << lm);
                let mut renorm = false;
                for k in 0..1usize << lm {
                    if masks[i * c + ch] & (1 << k) == 0 {
                        for j in 0..n0 {
                            seed = bands::lcg(seed);
                            x[base + (j << lm) + k] = if seed & 0x8000 != 0 { r } else { -r };
                        }
                        renorm = true;
                    }
                }
                if renorm {
                    let band = &mut x[base..base + (n0 << lm)];
                    let e: f32 = 1e-15 + band.iter().map(|v| v * v).sum::<f32>();
                    let g = 1.0 / e.sqrt();
                    for v in band {
                        *v *= g;
                    }
                }
            }
        }
    }
}

/// What the encoder analysis hands to the frame writer.
pub struct EncInput {
    pub silence: bool,
    pub intra: bool,
    pub band_log_e: Vec<f32>,
    pub x: Vec<f32>,
}

#[allow(dead_code)]
pub fn unused(_: Enc) {}
