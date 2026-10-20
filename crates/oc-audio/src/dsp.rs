//! Signal processing: sample-format conversion with dither, channel mixing, resampling,
//! BS.1770 loudness, trimming and fades.
use crate::{default_mask, Audio, Samples};
use oc_core::{Error, ErrorCode, Result};
use std::borrow::Cow;

// ---------------------------------------------------------------- sample formats

/// Samples as floating point in about -1..1.
pub fn to_float(s: &Samples) -> Cow<'_, [f32]> {
    match s {
        Samples::Float(d) => Cow::Borrowed(d),
        Samples::Int { bits, data } => {
            let scale = 1.0 / (1u64 << (bits - 1)) as f32;
            Cow::Owned(data.iter().map(|&v| v as f32 * scale).collect())
        }
    }
}

/// Dither applied when the bit depth shrinks or float becomes integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dither {
    None,
    /// Triangular probability density, +-1 LSB.
    Tpdf,
    /// TPDF with a first-order error-feedback high-pass shaper.
    Shaped,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        // xorshift64*; deterministic so conversions are reproducible.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        v as f64 / (1u64 << 53) as f64
    }
}

/// Convert to integer samples at `bits` (8..=32).
pub fn to_int_dithered(s: &Samples, bits: u8, dither: Dither, channels: usize) -> Vec<i32> {
    let bits = bits.clamp(8, 32);
    let max = ((1i64 << (bits - 1)) - 1) as f64;
    let min = -((1i64 << (bits - 1)) as f64);
    let lsb = 1.0;
    match s {
        Samples::Int { bits: from, data } if *from == bits => data.clone(),
        Samples::Int { bits: from, data } if *from < bits => {
            let sh = bits - from;
            data.iter().map(|&v| v << sh).collect()
        }
        Samples::Int { bits: from, data } => {
            // Reducing depth: work in the target scale.
            let div = (1i64 << (from - bits)) as f64;
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
            let mut err = vec![0.0f64; channels.max(1)];
            data.iter()
                .enumerate()
                .map(|(i, &v)| {
                    let x = f64::from(v) / div;
                    quantize(
                        x,
                        dither,
                        &mut rng,
                        &mut err[i % channels.max(1)],
                        lsb,
                        min,
                        max,
                    )
                })
                .collect()
        }
        Samples::Float(d) => {
            let scale = (1i64 << (bits - 1)) as f64;
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
            let mut err = vec![0.0f64; channels.max(1)];
            d.iter()
                .enumerate()
                .map(|(i, &v)| {
                    let x = f64::from(v) * scale;
                    quantize(
                        x,
                        dither,
                        &mut rng,
                        &mut err[i % channels.max(1)],
                        lsb,
                        min,
                        max,
                    )
                })
                .collect()
        }
    }
}

fn quantize(
    x: f64,
    dither: Dither,
    rng: &mut Rng,
    err: &mut f64,
    lsb: f64,
    min: f64,
    max: f64,
) -> i32 {
    let (noise, fb) = match dither {
        Dither::None => (0.0, 0.0),
        Dither::Tpdf => (lsb * (rng.next() - rng.next()), 0.0),
        Dither::Shaped => (lsb * (rng.next() - rng.next()), *err),
    };
    let target = x - fb + noise;
    let q = target.round().clamp(min, max);
    if dither == Dither::Shaped {
        *err = q - (x - fb);
    }
    q as i32
}

/// Integer samples at `bits` with TPDF dither only when precision is lost.
pub fn to_int(s: &Samples, bits: u8) -> Vec<i32> {
    to_int_dithered(s, bits, Dither::None, 1)
}

/// Re-express a clip at another sample format.
pub fn convert_format(audio: &mut Audio, target: Option<u8>, dither: Dither) {
    // target: Some(bits) for integer, None for float
    let ch = usize::from(audio.channels);
    audio.samples = match target {
        None => Samples::Float(to_float(&audio.samples).into_owned()),
        Some(bits) => {
            let d = to_int_dithered(&audio.samples, bits, dither, ch);
            Samples::Int { bits, data: d }
        }
    };
}

// ---------------------------------------------------------------- channels

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pos {
    Fl,
    Fr,
    Fc,
    Lfe,
    Bl,
    Br,
    Flc,
    Frc,
    Bc,
    Sl,
    Sr,
    Other,
}

fn positions(mask: u32, channels: usize) -> Vec<Pos> {
    const ORDER: [Pos; 11] = [
        Pos::Fl,
        Pos::Fr,
        Pos::Fc,
        Pos::Lfe,
        Pos::Bl,
        Pos::Br,
        Pos::Flc,
        Pos::Frc,
        Pos::Bc,
        Pos::Sl,
        Pos::Sr,
    ];
    let mut v = Vec::new();
    for (bit, p) in ORDER.iter().enumerate() {
        if mask & (1 << bit) != 0 {
            v.push(*p);
        }
    }
    // Higher mask bits (tops) are positions we do not mix by name.
    while v.len() < channels {
        v.push(Pos::Other);
    }
    v.truncate(channels);
    v
}

/// Mixing matrix rows = target channels, columns = source channels.
fn matrix(src: &[Pos], dst: &[Pos], normalize: bool) -> Vec<Vec<f32>> {
    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let has = |p: Pos| src.iter().position(|&s| s == p);
    let mut rows = Vec::new();
    for &t in dst {
        let mut row = vec![0.0f32; src.len()];
        if let Some(i) = has(t) {
            row[i] = 1.0;
        } else {
            match t {
                Pos::Fl | Pos::Fr => {
                    let left = t == Pos::Fl;
                    let (front, back, side, flc) = if left {
                        (Pos::Fl, Pos::Bl, Pos::Sl, Pos::Flc)
                    } else {
                        (Pos::Fr, Pos::Br, Pos::Sr, Pos::Frc)
                    };
                    let _ = front;
                    if let Some(i) = has(flc) {
                        row[i] += 1.0;
                    }
                    if let Some(i) = has(Pos::Fc) {
                        row[i] += H;
                    }
                    if let Some(i) = has(side) {
                        row[i] += H;
                    }
                    if let Some(i) = has(back) {
                        row[i] += H;
                    }
                    if let Some(i) = has(Pos::Bc) {
                        row[i] += 0.5;
                    }
                    if row.iter().all(|&w| w == 0.0) {
                        // A source with no left/right at all (mono, centre only): copy it.
                        if let Some(i) = has(Pos::Other).or_else(|| (src.len() == 1).then_some(0)) {
                            row[i] = 1.0;
                        }
                    }
                    if normalize {
                        let s: f32 = row.iter().sum();
                        if s > 1.0 {
                            for w in &mut row {
                                *w /= s;
                            }
                        }
                    }
                }
                Pos::Fc => {
                    let l = has(Pos::Fl);
                    let r = has(Pos::Fr);
                    if let (Some(l), Some(r)) = (l, r) {
                        row[l] = 0.5;
                        row[r] = 0.5;
                    } else if src.len() == 1 {
                        row[0] = 1.0;
                    }
                }
                Pos::Bl | Pos::Sl => {
                    if let Some(i) = has(Pos::Fl) {
                        row[i] = 1.0;
                    }
                }
                Pos::Br | Pos::Sr => {
                    if let Some(i) = has(Pos::Fr) {
                        row[i] = 1.0;
                    }
                }
                _ => {}
            }
        }
        rows.push(row);
    }
    rows
}

/// Mix to `channels` (1..=8). Surround to stereo follows ITU-R BS.775 (centre and surrounds
/// at -3 dB, LFE dropped) and the sum is scaled down to avoid clipping unless `normalize` is off.
pub fn remix(audio: &Audio, channels: u16, normalize: bool) -> Result<Audio> {
    if channels == 0 || channels > 8 {
        return Err(Error::invalid_option(
            "channels",
            "channels must be between 1 and 8",
        ));
    }
    if channels == audio.channels {
        return Ok(audio.clone());
    }
    let src_n = usize::from(audio.channels);
    let dst_n = usize::from(channels);
    let src_pos = positions(
        if audio.channel_mask != 0 {
            audio.channel_mask
        } else {
            default_mask(audio.channels)
        },
        src_n,
    );
    let dst_pos = positions(default_mask(channels), dst_n);
    let float = to_float(&audio.samples);
    let frames = float.len() / src_n.max(1);
    let mut out = Vec::with_capacity(frames * dst_n);
    if dst_n == 1 && src_n > 2 {
        // Surround to mono goes through stereo.
        let st = remix(audio, 2, normalize)?;
        return remix(&st, 1, normalize);
    }
    if dst_n == 1 {
        let m = matrix(&src_pos, &[Pos::Fc], normalize);
        mix_into(&float, src_n, &m, &mut out);
    } else {
        let m = matrix(&src_pos, &dst_pos, normalize);
        mix_into(&float, src_n, &m, &mut out);
    }
    let mut a = audio.clone();
    a.channels = channels;
    a.channel_mask = default_mask(channels);
    a.samples = Samples::Float(out);
    Ok(a)
}

fn mix_into(src: &[f32], src_n: usize, m: &[Vec<f32>], out: &mut Vec<f32>) {
    for frame in src.chunks_exact(src_n) {
        for row in m {
            let mut acc = 0.0f32;
            for (w, s) in row.iter().zip(frame) {
                acc += w * s;
            }
            out.push(acc);
        }
    }
}

// ---------------------------------------------------------------- resampling

/// Resampler quality presets. `stopband` is the measured alias rejection, see the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Low,
    Medium,
    High,
    Best,
}

impl Quality {
    pub fn from_name(n: &str) -> Option<Self> {
        Some(match n {
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "best" => Self::Best,
            _ => return None,
        })
    }
    /// (zero crossings on each side of the centre, Kaiser beta, cutoff as a fraction of Nyquist)
    fn params(self) -> (usize, f64, f64) {
        match self {
            Self::Low => (8, 6.0, 0.90),
            Self::Medium => (16, 8.6, 0.94),
            Self::High => (32, 11.0, 0.96),
            Self::Best => (64, 13.0, 0.98),
        }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..60 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Resample every channel from `from` to `to` Hz with a polyphase windowed-sinc filter.
pub fn resample(audio: &Audio, to: u32, quality: Quality) -> Result<Audio> {
    if to == 0 || audio.sample_rate == 0 {
        return Err(Error::invalid_option(
            "sample_rate",
            "sample rate must be positive",
        ));
    }
    if to == audio.sample_rate {
        return Ok(audio.clone());
    }
    let ch = usize::from(audio.channels);
    let data = to_float(&audio.samples);
    let frames = data.len() / ch;
    let g = gcd(u64::from(to), u64::from(audio.sample_rate));
    let up = u64::from(to) / g; // L
    let down = u64::from(audio.sample_rate) / g; // M
    let ratio = to as f64 / f64::from(audio.sample_rate);
    let (zc, beta, cut_frac) = quality.params();
    // Cutoff relative to the input rate: Nyquist of the lower of the two rates.
    let fc = cut_frac * 0.5 * ratio.min(1.0); // cycles per input sample
    let scale = 2.0 * fc; // sinc argument scale: sinc(2*fc*t)
                          // Half width in input samples so that `zc` zero crossings of the cutoff fit.
    let half = (zc as f64 / scale).ceil() as isize;
    let taps = (2 * half) as usize;
    const MAX_PHASES: u64 = 2048;
    let phases = up.min(MAX_PHASES) as usize;
    let i0_beta = bessel_i0(beta);
    let mut table = vec![0.0f32; (phases + 1) * taps];
    for p in 0..=phases {
        let frac = p as f64 / phases as f64; // fractional input position within the sample
        let mut sum = 0.0;
        let mut row = vec![0.0f64; taps];
        for (k, w) in row.iter_mut().enumerate() {
            // tap k covers input offset j = k - half + 1 relative to floor(position)
            let t = (k as isize - half + 1) as f64 - frac;
            let x = t / half as f64;
            let win = if x.abs() >= 1.0 {
                0.0
            } else {
                bessel_i0(beta * (1.0 - x * x).sqrt()) / i0_beta
            };
            let a = std::f64::consts::PI * scale * t;
            let s = if a.abs() < 1e-12 { 1.0 } else { a.sin() / a };
            *w = s * win;
            sum += *w;
        }
        // Unity gain at DC for every phase.
        for (k, w) in row.iter().enumerate() {
            table[p * taps + k] = (*w / sum) as f32;
        }
    }
    let out_frames = ((frames as u64 * up) / down) as usize;
    let mut out = vec![0.0f32; out_frames * ch];
    for n in 0..out_frames {
        let pos_num = n as u64 * down; // position in units of 1/up input samples
        let i = (pos_num / up) as isize;
        let rem = pos_num % up;
        let (row_a, row_b, mix) = if up <= MAX_PHASES {
            (rem as usize, rem as usize, 0.0f32)
        } else {
            let f = rem as f64 / up as f64 * phases as f64;
            let a = f.floor() as usize;
            (a, (a + 1).min(phases), (f - a as f64) as f32)
        };
        let ra = &table[row_a * taps..(row_a + 1) * taps];
        let rb = &table[row_b * taps..(row_b + 1) * taps];
        let start = i - half + 1;
        for c in 0..ch {
            let mut acc = 0.0f32;
            for k in 0..taps {
                let idx = start + k as isize;
                if idx < 0 || idx as usize >= frames {
                    continue;
                }
                let w = if mix == 0.0 {
                    ra[k]
                } else {
                    ra[k] * (1.0 - mix) + rb[k] * mix
                };
                acc += w * data[idx as usize * ch + c];
            }
            out[n * ch + c] = acc;
        }
    }
    let mut a = audio.clone();
    a.sample_rate = to;
    a.samples = Samples::Float(out);
    Ok(a)
}

// ---------------------------------------------------------------- loudness (ITU-R BS.1770-4)

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn run(&self, x: &[f32]) -> Vec<f64> {
        let (mut z1, mut z2) = (0.0, 0.0);
        x.iter()
            .map(|&v| {
                let v = f64::from(v);
                let y = self.b[0] * v + z1;
                z1 = self.b[1] * v - self.a[0] * y + z2;
                z2 = self.b[2] * v - self.a[1] * y;
                y
            })
            .collect()
    }
}

/// The two K-weighting stages for any sample rate (the 48 kHz coefficients of the standard
/// fall out of the same analogue prototypes).
fn k_weighting(rate: f64) -> [Biquad; 2] {
    // Stage 1: high shelf, +4 dB around 1.68 kHz.
    let f0 = 1_681.974_450_955_533;
    let g = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let s1 = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    // Stage 2: RLB high-pass at 38 Hz.
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let s2 = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    [s1, s2]
}

/// Loudness and peak measurements.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// Integrated loudness in LUFS, `None` for silence.
    pub integrated: Option<f64>,
    /// Loudness range in LU (EBU Tech 3342), `None` when too short.
    pub range: Option<f64>,
    pub sample_peak: f32,
    /// Highest peak in dBFS after 4x oversampling.
    pub true_peak_db: f64,
}

pub fn loudness(audio: &Audio) -> Loudness {
    let ch = usize::from(audio.channels);
    let data = to_float(&audio.samples);
    let frames = data.len() / ch.max(1);
    let rate = f64::from(audio.sample_rate);
    let peak = data.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let pos = positions(
        if audio.channel_mask != 0 {
            audio.channel_mask
        } else {
            default_mask(audio.channels)
        },
        ch,
    );
    let kw = k_weighting(rate);
    // Per-channel squared K-weighted signal.
    let mut sq: Vec<(f64, Vec<f64>)> = Vec::new();
    for (c, p) in pos.iter().enumerate() {
        let weight = match p {
            Pos::Lfe => continue,
            Pos::Bl | Pos::Br | Pos::Sl | Pos::Sr => 1.41,
            _ => 1.0,
        };
        let x: Vec<f32> = (0..frames).map(|i| data[i * ch + c]).collect();
        let y = kw[1].run(&kw[0].run(&x).iter().map(|&v| v as f32).collect::<Vec<_>>());
        // Squared prefix sums make every block mean O(1).
        let mut acc = Vec::with_capacity(frames + 1);
        acc.push(0.0);
        let mut s = 0.0;
        for v in y {
            s += v * v;
            acc.push(s);
        }
        sq.push((weight, acc));
    }
    let block = (0.4 * rate) as usize;
    let step = (0.1 * rate) as usize;
    let mut blocks = Vec::new();
    if block > 0 && step > 0 && frames >= block {
        let mut start = 0;
        while start + block <= frames {
            let z: f64 = sq
                .iter()
                .map(|(w, acc)| w * (acc[start + block] - acc[start]) / block as f64)
                .sum();
            blocks.push(z);
            start += step;
        }
    }
    let to_lufs = |z: f64| -0.691 + 10.0 * z.log10();
    let integrated = gated(&blocks, &to_lufs);
    // Loudness range from 3 s windows.
    let range = {
        let w = (3.0 * rate) as usize;
        if w > 0 && frames >= w {
            let mut vals = Vec::new();
            let mut start = 0;
            while start + w <= frames {
                let z: f64 = sq
                    .iter()
                    .map(|(wt, acc)| wt * (acc[start + w] - acc[start]) / w as f64)
                    .sum();
                vals.push(z);
                start += step;
            }
            let abs: Vec<f64> = vals
                .iter()
                .copied()
                .filter(|&z| to_lufs(z) > -70.0)
                .collect();
            if abs.is_empty() {
                None
            } else {
                let mean = abs.iter().sum::<f64>() / abs.len() as f64;
                let rel = to_lufs(mean) - 20.0;
                let mut l: Vec<f64> = abs
                    .iter()
                    .map(|&z| to_lufs(z))
                    .filter(|&v| v > rel)
                    .collect();
                l.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                if l.len() < 2 {
                    None
                } else {
                    let lo = l[((l.len() - 1) as f64 * 0.10) as usize];
                    let hi = l[((l.len() - 1) as f64 * 0.95) as usize];
                    Some(hi - lo)
                }
            }
        } else {
            None
        }
    };
    Loudness {
        integrated,
        range,
        sample_peak: peak,
        true_peak_db: true_peak_db(&data, ch),
    }
}

fn gated(blocks: &[f64], to_lufs: &dyn Fn(f64) -> f64) -> Option<f64> {
    let abs: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|&z| to_lufs(z) > -70.0)
        .collect();
    if abs.is_empty() {
        return None;
    }
    let mean = abs.iter().sum::<f64>() / abs.len() as f64;
    let rel = to_lufs(mean) - 10.0;
    let kept: Vec<f64> = abs.into_iter().filter(|&z| to_lufs(z) > rel).collect();
    if kept.is_empty() {
        return None;
    }
    Some(to_lufs(kept.iter().sum::<f64>() / kept.len() as f64))
}

/// 4x oversampled peak with a short windowed-sinc interpolator (BS.1770 Annex 2 style).
fn true_peak_db(data: &[f32], ch: usize) -> f64 {
    const TAPS: isize = 12; // per side, in input samples
    let mut kern = [[0.0f64; (2 * TAPS) as usize]; 4];
    for (ph, row) in kern.iter_mut().enumerate() {
        let frac = ph as f64 / 4.0;
        for (k, w) in row.iter_mut().enumerate() {
            let t = (k as isize - TAPS + 1) as f64 - frac;
            let a = std::f64::consts::PI * t;
            let s = if a.abs() < 1e-12 { 1.0 } else { a.sin() / a };
            let x = t / TAPS as f64;
            let win = if x.abs() >= 1.0 {
                0.0
            } else {
                0.5 + 0.5 * (std::f64::consts::PI * x).cos()
            };
            *w = s * win;
        }
    }
    let frames = data.len() / ch.max(1);
    let mut peak = 0.0f64;
    for c in 0..ch {
        for i in 0..frames {
            peak = peak.max(f64::from(data[i * ch + c]).abs());
            for row in &kern[1..] {
                let mut acc = 0.0;
                for (k, w) in row.iter().enumerate() {
                    let idx = i as isize - TAPS + 1 + k as isize;
                    if idx >= 0 && (idx as usize) < frames {
                        acc += w * f64::from(data[idx as usize * ch + c]);
                    }
                }
                peak = peak.max(acc.abs());
            }
        }
    }
    if peak <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * peak.log10()
    }
}

/// Scale the clip by `db`. Integer audio is converted to float first.
pub fn apply_gain(audio: &mut Audio, db: f64) {
    let g = 10f64.powf(db / 20.0) as f32;
    let mut f = to_float(&audio.samples).into_owned();
    for v in &mut f {
        *v *= g;
    }
    audio.samples = Samples::Float(f);
}

// ---------------------------------------------------------------- editing

/// Keep `start..end` seconds (`end` of 0 means to the end).
pub fn trim(audio: &mut Audio, start: f64, end: f64) -> Result<()> {
    if start < 0.0 || end < 0.0 || (end > 0.0 && end <= start) {
        return Err(Error::invalid_option(
            "trim_start",
            "start must be before end",
        ));
    }
    let ch = usize::from(audio.channels);
    let frames = audio.frames();
    let a = ((start * f64::from(audio.sample_rate)) as usize).min(frames);
    let b = if end > 0.0 {
        ((end * f64::from(audio.sample_rate)) as usize).min(frames)
    } else {
        frames
    };
    match &mut audio.samples {
        Samples::Int { data, .. } => *data = data[a * ch..b * ch].to_vec(),
        Samples::Float(d) => *d = d[a * ch..b * ch].to_vec(),
    }
    Ok(())
}

/// Remove leading and trailing frames quieter than `threshold_db` (peak across channels).
/// Returns the frames removed (start, end).
pub fn trim_silence(audio: &mut Audio, threshold_db: f64) -> (usize, usize) {
    let ch = usize::from(audio.channels);
    let f = to_float(&audio.samples);
    let th = 10f64.powf(threshold_db / 20.0) as f32;
    let frames = f.len() / ch;
    let loud = |i: usize| f[i * ch..(i + 1) * ch].iter().any(|v| v.abs() > th);
    let first = (0..frames).find(|&i| loud(i)).unwrap_or(frames);
    let last = (0..frames)
        .rev()
        .find(|&i| loud(i))
        .map_or(first, |i| i + 1);
    drop(f);
    match &mut audio.samples {
        Samples::Int { data, .. } => *data = data[first * ch..last * ch].to_vec(),
        Samples::Float(d) => *d = d[first * ch..last * ch].to_vec(),
    }
    (first, frames - last)
}

/// Linear fades in and out over the given seconds.
pub fn fade(audio: &mut Audio, fade_in: f64, fade_out: f64) {
    if fade_in <= 0.0 && fade_out <= 0.0 {
        return;
    }
    let ch = usize::from(audio.channels);
    let frames = audio.frames();
    let fi = ((fade_in * f64::from(audio.sample_rate)) as usize).min(frames);
    let fo = ((fade_out * f64::from(audio.sample_rate)) as usize).min(frames);
    let mut f = to_float(&audio.samples).into_owned();
    for i in 0..fi {
        let g = i as f32 / fi as f32;
        for c in 0..ch {
            f[i * ch + c] *= g;
        }
    }
    for i in 0..fo {
        let g = i as f32 / fo as f32;
        let idx = frames - 1 - i;
        for c in 0..ch {
            f[idx * ch + c] *= g;
        }
    }
    audio.samples = Samples::Float(f);
}

/// Reject sample rates the formats cannot carry.
pub fn check_rate(rate: u32) -> Result<()> {
    if (1..=768_000).contains(&rate) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!("{rate} Hz is not a usable sample rate"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, secs: f64, amp: f32) -> Audio {
        let n = (rate as f64 * secs) as usize;
        let d: Vec<f32> = (0..n)
            .map(|i| {
                amp * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin() as f32
            })
            .collect();
        Audio::new(rate, 1, Samples::Float(d))
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn dither_keeps_mean_and_noise_small() {
        let a = Audio::new(8000, 1, Samples::Float(vec![0.3; 4000]));
        for d in [Dither::None, Dither::Tpdf, Dither::Shaped] {
            let q = to_int_dithered(&a.samples, 8, d, 1);
            let mean = q.iter().map(|&v| f64::from(v)).sum::<f64>() / q.len() as f64;
            assert!((mean / 128.0 - 0.3).abs() < 0.01, "{d:?} {mean}");
        }
    }

    #[test]
    fn resampler_passband_and_stopband() {
        // Passband: a 1 kHz tone keeps its level; stopband: a tone above the new Nyquist is gone.
        for (q, min_att) in [
            (Quality::Low, 60.0),
            (Quality::Medium, 85.0),
            (Quality::High, 110.0),
            (Quality::Best, 130.0),
        ] {
            let pass = resample(&sine(96000, 1000.0, 0.5, 0.5), 44100, q).unwrap();
            let f = to_float(&pass.samples);
            let mid = &f[2000..f.len() - 2000];
            let gain_db = 20.0 * (rms(mid) / (0.5 / 2f64.sqrt())).log10();
            assert!(gain_db.abs() < 0.01, "{q:?} passband {gain_db} dB");
            // 96 kHz to 44.1 kHz: a 25 kHz tone would fold to 19.1 kHz, inside the passband of every
            // preset, so the filter has to have removed it already.
            let stop = resample(&sine(96000, 25000.0, 0.5, 0.5), 44100, q).unwrap();
            let f = to_float(&stop.samples);
            let mid = &f[2000..f.len() - 2000];
            let att = -20.0 * (rms(mid) / (0.5 / 2f64.sqrt())).log10();
            eprintln!("{q:?} passband {gain_db:.4} dB, 25 kHz rejection {att:.1} dB");
            assert!(att > min_att, "{q:?} stopband {att} dB");
        }
    }

    #[test]
    fn resample_length() {
        let a = resample(&sine(44100, 440.0, 1.0, 0.5), 48000, Quality::Medium).unwrap();
        assert_eq!(a.frames(), 48000);
        let b = resample(&sine(48000, 440.0, 1.0, 0.5), 8000, Quality::Medium).unwrap();
        assert_eq!(b.frames(), 8000);
    }

    #[test]
    fn loudness_of_reference_sine() {
        // BS.1770: a full-scale 997 Hz sine in one channel reads -3.01 LUFS, so the same sine
        // at -20 dBFS in two channels reads -3.01 - 20 + 3.01 = -20.0 LUFS.
        let mono = sine(48000, 997.0, 5.0, 0.1);
        let mut st = mono.clone();
        st.channels = 2;
        st.channel_mask = 3;
        if let Samples::Float(d) = &mut st.samples {
            *d = d.iter().flat_map(|&v| [v, v]).collect();
        }
        let l = loudness(&st).integrated.unwrap();
        assert!((l + 20.0).abs() < 0.05, "{l}");
    }

    #[test]
    fn silence_has_no_loudness() {
        let a = Audio::new(48000, 2, Samples::Float(vec![0.0; 48000 * 2]));
        assert_eq!(loudness(&a).integrated, None);
    }

    #[test]
    fn downmix_5_1_is_bounded() {
        let mut a = Audio::new(8000, 6, Samples::Float(vec![1.0; 6 * 100]));
        a.channel_mask = 0x3F;
        let s = remix(&a, 2, true).unwrap();
        let f = to_float(&s.samples);
        assert!(f.iter().all(|&v| v <= 1.0 + 1e-6));
        let m = remix(&a, 1, true).unwrap();
        assert_eq!(m.channels, 1);
    }

    #[test]
    fn trim_and_fade() {
        let mut a = Audio::new(1000, 1, Samples::Float(vec![1.0; 1000]));
        trim(&mut a, 0.25, 0.75).unwrap();
        assert_eq!(a.frames(), 500);
        fade(&mut a, 0.1, 0.1);
        let f = to_float(&a.samples);
        assert_eq!(f[0], 0.0);
        assert!((f[250] - 1.0).abs() < 1e-6);
    }
}
