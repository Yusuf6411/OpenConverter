//! MPEG audio layer III encoder.
//!
//! Long blocks only, no psychoacoustic model: a polyphase analysis bank, an MDCT, a joint
//! stereo decision per frame and a single quantiser step per granule found by bisection so that
//! the Huffman-coded size fits the frame budget (constant bit rate, with a bit reservoir) or a
//! fixed step (variable bit rate). The result is ordinary, spec-conforming MPEG audio.

use super::dec::{
    alias_coefficients, big_entries, long_bands, synth_window, table_of, COUNT1_A_BITS,
    COUNT1_A_CODES,
};
use crate::{dsp, Audio, Tags};
use oc_core::{Error, ErrorCode, Note, Notes, Result, Severity};
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Options {
    /// Constant bit rate in kbit/s (snapped to a rate the format has); ignored when `vbr` is set.
    pub bitrate: u32,
    /// Variable bit rate quality, 0 (best) to 9 (smallest).
    pub vbr: Option<f32>,
    /// Allow mid/side coding of stereo frames.
    pub joint_stereo: bool,
    /// ID3v2 tag to write in front of the audio.
    pub id3: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bitrate: 128,
            vbr: None,
            joint_stereo: true,
            id3: true,
        }
    }
}

/// (rate, MPEG version 1/2/3 for 1/2/2.5, rate index in the header)
const RATES: [(u32, u8, u8); 9] = [
    (44100, 1, 0),
    (48000, 1, 1),
    (32000, 1, 2),
    (22050, 2, 0),
    (24000, 2, 1),
    (16000, 2, 2),
    (11025, 3, 0),
    (12000, 3, 1),
    (8000, 3, 2),
];
const BITRATES_V1: [u32; 14] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const BITRATES_V2: [u32; 14] = [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

// ---------------------------------------------------------------- bit writer

#[derive(Default)]
struct Bw {
    bytes: Vec<u8>,
    nbits: usize,
}

impl Bw {
    fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if self.nbits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (v >> i) & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.nbits % 8);
            }
            self.nbits += 1;
        }
    }
    fn align(&mut self) {
        self.nbits = self.bytes.len() * 8;
    }
}

// ---------------------------------------------------------------- Huffman encoding tables

struct Cand {
    select: usize,
    base: usize,
    linbits: u32,
    maxv: i32,
}

struct Tables {
    /// Per base table number: (length, code) by `x * 16 + y`.
    code: Vec<Vec<(u8, u32)>>,
    cands: Vec<Cand>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut code = vec![Vec::new(); 25];
        for n in 1..=24 {
            if let Some(e) = big_entries(n) {
                let mut v = vec![(0u8, 0u32); 256];
                for (len, c, sym) in e {
                    v[usize::from(sym)] = (len, c);
                }
                code[n] = v;
            }
        }
        let mut cands = Vec::new();
        for select in 1..32 {
            let Some((base, lin)) = table_of(select) else {
                continue;
            };
            let maxv = match base {
                1 => 1,
                2 | 3 => 2,
                5 | 6 => 3,
                7..=9 => 5,
                10..=12 => 7,
                13 | 15 => 15,
                _ => 15 + (1i32 << lin) - 1,
            };
            cands.push(Cand {
                select,
                base,
                linbits: u32::from(lin),
                maxv,
            });
        }
        Tables { code, cands }
    })
}

const BIG: u32 = 1 << 20;

fn pair_cost(t: &Tables, c: &Cand, x: i32, y: i32) -> u32 {
    if x > c.maxv || y > c.maxv {
        return BIG;
    }
    let esc = c.base == 16 || c.base == 24;
    let (cx, cy) = if esc { (x.min(15), y.min(15)) } else { (x, y) };
    let (len, _) = t.code[c.base][(cx * 16 + cy) as usize];
    let mut bits = u32::from(len) + u32::from(x > 0) + u32::from(y > 0);
    if esc {
        if x >= 15 {
            bits += c.linbits;
        }
        if y >= 15 {
            bits += c.linbits;
        }
    }
    bits
}

/// Everything needed to write one coded granule channel.
#[derive(Clone, Default)]
struct Coded {
    bits: usize,
    big_values: usize,
    gain: i32,
    tables: [usize; 3],
    r0: usize,
    r1: usize,
    count1_table: usize,
    count1_quads: usize,
    is: Vec<i32>,
}

/// Choose the partition into regions and tables for quantised values `is` and count the bits.
fn plan(is: &[i32; 576], rate: u32) -> Coded {
    let t = tables();
    let last = is.iter().rposition(|&v| v != 0).map_or(0, |p| p + 1);
    let big_last = is.iter().rposition(|&v| v.abs() > 1).map_or(0, |p| p + 1);
    let mut bv = big_last.div_ceil(2) * 2;
    let mut c1 = last.saturating_sub(bv).div_ceil(4);
    while bv + 4 * c1 > 576 {
        bv += 2;
        c1 = last.saturating_sub(bv).div_ceil(4);
    }
    let nb = bv / 2;
    // Cumulative cost of every candidate table over the pairs.
    let ncand = t.cands.len();
    let mut cum = vec![0u32; ncand * (nb + 1)];
    for (ci, c) in t.cands.iter().enumerate() {
        let row = &mut cum[ci * (nb + 1)..(ci + 1) * (nb + 1)];
        for p in 0..nb {
            let cost = pair_cost(t, c, is[2 * p].abs(), is[2 * p + 1].abs());
            row[p + 1] = row[p] + cost;
        }
    }
    let best = |a: usize, b: usize| -> (u32, usize) {
        if a >= b {
            return (0, 0);
        }
        let mut r = (BIG, 0usize);
        for (ci, c) in t.cands.iter().enumerate() {
            let cost = cum[ci * (nb + 1) + b] - cum[ci * (nb + 1) + a];
            if cost < r.0 {
                r = (cost, c.select);
            }
        }
        r
    };
    let bands = long_bands(rate);
    let mut winner = (u32::MAX, 0usize, 0usize, [0usize; 3]);
    for r0 in 0..16usize {
        let p0 = (usize::from(bands[(r0 + 1).min(22)]) / 2).min(nb);
        let (c0, t0) = best(0, p0);
        if c0 >= BIG {
            continue;
        }
        for r1 in 0..8usize {
            let p1 = (usize::from(bands[(r0 + r1 + 2).min(22)]) / 2)
                .min(nb)
                .max(p0);
            let (c1_, t1) = best(p0, p1);
            let (c2, t2) = best(p1, nb);
            let total = c0 + c1_ + c2;
            if c1_ < BIG && c2 < BIG && total < winner.0 {
                winner = (total, r0, r1, [t0, t1, t2]);
            }
        }
    }
    // Count-1 region.
    let (mut ca, mut cb) = (0usize, 0usize);
    for q in 0..c1 {
        let v = &is[bv + 4 * q..bv + 4 * q + 4];
        let idx = (usize::from(v[0] != 0) << 3)
            | (usize::from(v[1] != 0) << 2)
            | (usize::from(v[2] != 0) << 1)
            | usize::from(v[3] != 0);
        let signs = v.iter().filter(|&&x| x != 0).count();
        ca += usize::from(COUNT1_A_BITS[idx]) + signs;
        cb += 4 + signs;
    }
    let (c1bits, c1t) = if ca <= cb { (ca, 0) } else { (cb, 1) };
    Coded {
        bits: if winner.0 == u32::MAX {
            usize::MAX / 4
        } else {
            winner.0 as usize + c1bits
        },
        big_values: nb,
        gain: 0,
        tables: winner.3,
        r0: winner.1,
        r1: winner.2,
        count1_table: c1t,
        count1_quads: c1,
        is: is.to_vec(),
    }
}

fn emit(w: &mut Bw, c: &Coded, rate: u32) {
    let t = tables();
    let bands = long_bands(rate);
    let b0 = usize::from(bands[(c.r0 + 1).min(22)]);
    let b1 = usize::from(bands[(c.r0 + c.r1 + 2).min(22)]);
    for p in 0..c.big_values {
        let line = 2 * p;
        let region = if line < b0 {
            0
        } else if line < b1 {
            1
        } else {
            2
        };
        let sel = c.tables[region];
        let Some((base, lin)) = table_of(sel) else {
            continue;
        };
        let (x, y) = (c.is[line], c.is[line + 1]);
        let (ax, ay) = (x.abs(), y.abs());
        let esc = base == 16 || base == 24;
        let (cx, cy) = if esc {
            (ax.min(15), ay.min(15))
        } else {
            (ax, ay)
        };
        let (len, code) = t.code[base][(cx * 16 + cy) as usize];
        w.put(code, u32::from(len));
        if esc && ax >= 15 {
            w.put((ax - 15) as u32, u32::from(lin));
        }
        if ax > 0 {
            w.put(u32::from(x < 0), 1);
        }
        if esc && ay >= 15 {
            w.put((ay - 15) as u32, u32::from(lin));
        }
        if ay > 0 {
            w.put(u32::from(y < 0), 1);
        }
    }
    let start = 2 * c.big_values;
    for q in 0..c.count1_quads {
        let v = &c.is[start + 4 * q..start + 4 * q + 4];
        let idx = (usize::from(v[0] != 0) << 3)
            | (usize::from(v[1] != 0) << 2)
            | (usize::from(v[2] != 0) << 1)
            | usize::from(v[3] != 0);
        if c.count1_table == 0 {
            w.put(
                u32::from(COUNT1_A_CODES[idx]),
                u32::from(COUNT1_A_BITS[idx]),
            );
        } else {
            w.put(15 - idx as u32, 4);
        }
        for &x in v {
            if x != 0 {
                w.put(u32::from(x < 0), 1);
            }
        }
    }
}

// ---------------------------------------------------------------- filter banks

struct Analysis {
    window: Vec<f32>, // C[i] = D[i] / 32
    matrix: Vec<f32>, // 32 x 64
    imdct: Vec<f32>,  // 18 x 36 (forward kernel)
    win: [f32; 36],
}

fn analysis() -> &'static Analysis {
    static A: OnceLock<Analysis> = OnceLock::new();
    A.get_or_init(|| {
        let pi = std::f64::consts::PI;
        let window = synth_window().iter().map(|&d| d / 32.0).collect();
        let mut matrix = vec![0.0f32; 32 * 64];
        for k in 0..32 {
            for i in 0..64 {
                matrix[k * 64 + i] =
                    (((2 * k + 1) as f64) * (i as f64 - 16.0) * pi / 64.0).cos() as f32;
            }
        }
        let mut imdct = vec![0.0f32; 18 * 36];
        for k in 0..18 {
            for i in 0..36 {
                imdct[k * 36 + i] =
                    (pi / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64).cos() as f32;
            }
        }
        let mut win = [0.0f32; 36];
        for (i, w) in win.iter_mut().enumerate() {
            *w = (pi / 36.0 * (i as f64 + 0.5)).sin() as f32;
        }
        Analysis {
            window,
            matrix,
            imdct,
            win,
        }
    })
}

/// Per-channel analysis state.
struct Channel {
    x: [f32; 512],
    prev: [[f32; 18]; 32],
}

impl Channel {
    fn new() -> Self {
        Self {
            x: [0.0; 512],
            prev: [[0.0; 18]; 32],
        }
    }

    /// 32 new samples to 32 subband samples.
    fn polyphase(&mut self, new: &[f32]) -> [f32; 32] {
        let a = analysis();
        self.x.copy_within(0..480, 32);
        for i in 0..32 {
            self.x[31 - i] = new[i];
        }
        let mut y = [0.0f32; 64];
        for (i, yv) in y.iter_mut().enumerate() {
            let mut acc = 0.0;
            for j in 0..8 {
                acc += a.window[i + 64 * j] * self.x[i + 64 * j];
            }
            *yv = acc;
        }
        let mut s = [0.0f32; 32];
        for (k, sv) in s.iter_mut().enumerate() {
            let row = &a.matrix[k * 64..k * 64 + 64];
            let mut acc = 0.0;
            for i in 0..64 {
                acc += row[i] * y[i];
            }
            *sv = acc;
        }
        s
    }

    /// One granule of 576 samples to 576 MDCT lines (alias butterflies applied).
    fn granule(&mut self, pcm: &[f32]) -> [f32; 576] {
        let a = analysis();
        let mut sb = [[0.0f32; 18]; 32]; // [subband][slot]
        for slot in 0..18 {
            let s = self.polyphase(&pcm[slot * 32..slot * 32 + 32]);
            for k in 0..32 {
                // Frequency inversion: undone by the decoder.
                sb[k][slot] = if k % 2 == 1 && slot % 2 == 1 {
                    -s[k]
                } else {
                    s[k]
                };
            }
        }
        let mut xr = [0.0f32; 576];
        for k in 0..32 {
            let mut z = [0.0f32; 36];
            for i in 0..18 {
                z[i] = self.prev[k][i] * a.win[i];
                z[18 + i] = sb[k][i] * a.win[18 + i];
            }
            for m in 0..18 {
                let row = &a.imdct[m * 36..m * 36 + 36];
                let mut acc = 0.0;
                for i in 0..36 {
                    acc += row[i] * z[i];
                }
                xr[k * 18 + m] = acc * (1.0 / 9.0);
            }
            self.prev[k] = sb[k];
        }
        // Alias butterflies (the transpose of the decoder's).
        let (cs, ca) = alias_coefficients();
        for k in 1..32 {
            for i in 0..8 {
                let lo = k * 18 - 1 - i;
                let hi = k * 18 + i;
                let (l, h) = (xr[lo], xr[hi]);
                xr[lo] = l * cs[i] + h * ca[i];
                xr[hi] = h * cs[i] - l * ca[i];
            }
        }
        xr
    }
}

// ---------------------------------------------------------------- quantisation

struct Quantizer {
    mag: Vec<f32>, // |x|^0.75
    neg: Vec<bool>,
}

impl Quantizer {
    fn new(xr: &[f32; 576]) -> Self {
        Self {
            mag: xr.iter().map(|v| v.abs().powf(0.75)).collect(),
            neg: xr.iter().map(|&v| v < 0.0).collect(),
        }
    }
    /// Quantise with the step of `gain`; None when a value does not fit the largest table.
    fn run(&self, gain: i32) -> Option<[i32; 576]> {
        let step = 2f64.powf(f64::from(gain - 210) / 4.0);
        let mult = step.powf(-0.75) as f32;
        let mut out = [0i32; 576];
        for k in 0..576 {
            let q = (self.mag[k] * mult + 0.4054).floor();
            if q > 8206.0 {
                return None;
            }
            let q = q as i32;
            out[k] = if self.neg[k] { -q } else { q };
        }
        Some(out)
    }
}

// ---------------------------------------------------------------- the encoder proper

struct GranuleOut {
    ch: Vec<Coded>,
}

fn code_granule(
    quants: &[Quantizer],
    rate: u32,
    budget_bits: usize,
    fixed_gain: Option<i32>,
) -> GranuleOut {
    let eval = |gain: i32| -> Option<Vec<Coded>> {
        let mut v = Vec::with_capacity(quants.len());
        for q in quants {
            let is = q.run(gain)?;
            let mut c = plan(&is, rate);
            c.gain = gain;
            v.push(c);
        }
        Some(v)
    };
    let fits = |v: &Vec<Coded>| {
        v.iter().map(|c| c.bits).sum::<usize>() <= budget_bits && v.iter().all(|c| c.bits < 4096)
    };
    if let Some(g) = fixed_gain {
        // Fixed step: raise it only if the data cannot be represented or does not fit a granule.
        let mut gain = g;
        loop {
            if let Some(v) = eval(gain) {
                if v.iter().all(|c| c.bits < 4096) || gain >= 255 {
                    return GranuleOut { ch: v };
                }
            }
            gain = (gain + 2).min(255);
        }
    }
    let (mut lo, mut hi) = (0i32, 255i32);
    // hi always fits; find the smallest fitting gain by bisection.
    while lo < hi {
        let mid = (lo + hi) / 2;
        match eval(mid) {
            Some(v) if fits(&v) => hi = mid,
            _ => lo = mid + 1,
        }
    }
    let ch = eval(lo).unwrap_or_else(|| {
        quants
            .iter()
            .map(|q| {
                let is = q.run(255).unwrap_or([0; 576]);
                let mut c = plan(&is, rate);
                c.gain = 255;
                c
            })
            .collect()
    });
    GranuleOut { ch }
}

fn side_info(v1: bool, nch: usize, mdb: usize, grs: &[GranuleOut]) -> Vec<u8> {
    let mut w = Bw::default();
    if v1 {
        w.put(mdb as u32, 9);
        w.put(0, if nch == 1 { 5 } else { 3 });
        w.put(0, 4 * nch as u32); // scfsi
    } else {
        w.put(mdb as u32, 8);
        w.put(0, if nch == 1 { 1 } else { 2 });
    }
    for g in grs {
        for c in &g.ch {
            w.put(c.bits as u32, 12);
            w.put(c.big_values as u32, 9);
            w.put(c.gain as u32, 8);
            w.put(0, if v1 { 4 } else { 9 }); // scalefac_compress: no scalefactors
            w.put(0, 1); // window switching
            for t in c.tables {
                w.put(t as u32, 5);
            }
            w.put(c.r0 as u32, 4);
            w.put(c.r1 as u32, 3);
            if v1 {
                w.put(0, 1); // preflag
            }
            w.put(0, 1); // scalefac_scale
            w.put(c.count1_table as u32, 1);
        }
    }
    w.bytes
}

fn header(
    version: u8,
    br_idx: usize,
    sr_idx: u8,
    padding: bool,
    mode: u8,
    mode_ext: u8,
) -> [u8; 4] {
    let v = match version {
        1 => 3u32,
        2 => 2,
        _ => 0,
    };
    let h = (0x7FFu32 << 21)
        | (v << 19)
        | (1 << 17) // layer III
        | (1 << 16) // no CRC
        | ((br_idx as u32 + 1) << 12)
        | (u32::from(sr_idx) << 10)
        | (u32::from(padding) << 9)
        | (u32::from(mode) << 6)
        | (u32::from(mode_ext) << 4)
        | (1 << 2); // original
    h.to_be_bytes()
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// Delay of the analysis and synthesis filter banks plus the MDCT overlap, in samples.
const CHAIN_DELAY: usize = 481 + 576;

pub fn write(audio: &Audio, opts: &Options, notes: &mut Notes) -> Result<Vec<u8>> {
    if audio.channels == 0 {
        return Err(Error::new(ErrorCode::InvalidOptions, "no audio channels"));
    }
    // Layout: mono or stereo; other layouts are mixed down.
    let mut a = audio.clone();
    if a.channels > 2 {
        a = dsp::remix(&a, 2, true)?;
        notes.push(Note::new(
            Severity::Warning,
            "mp3.downmix",
            "MP3 holds one or two channels; the audio was mixed down to stereo",
        ));
    }
    // Sample rate: one MPEG audio has.
    let rate = if RATES.iter().any(|r| r.0 == a.sample_rate) {
        a.sample_rate
    } else {
        let target = RATES
            .iter()
            .map(|r| r.0)
            .min_by_key(|&r| {
                (i64::from(r) - i64::from(a.sample_rate)).abs()
                    + if r < a.sample_rate { 1 } else { 0 }
            })
            .unwrap_or(44100);
        a = dsp::resample(&a, target, dsp::Quality::High)?;
        notes.push(Note::new(
            Severity::Info,
            "mp3.resampled",
            format!(
                "MP3 has no {} Hz mode; resampled to {target} Hz",
                audio.sample_rate
            ),
        ));
        target
    };
    let (_, version, sr_idx) = *RATES.iter().find(|r| r.0 == rate).unwrap_or(&RATES[0]);
    let v1 = version == 1;
    let nch = usize::from(a.channels);
    let ngr = if v1 { 2 } else { 1 };
    let spf = 576 * ngr;
    let pcm = dsp::to_float(&a.samples);
    let n = pcm.len() / nch;

    let list: &[u32; 14] = if v1 { &BITRATES_V1 } else { &BITRATES_V2 };
    let snap = |kbps: u32| -> usize {
        (0..14)
            .min_by_key(|&i| (i64::from(list[i]) - i64::from(kbps)).abs())
            .unwrap_or(8)
    };
    let cbr_idx = snap(opts.bitrate.max(1));
    // Variable bit rate keeps one quantiser step for the whole file, set relative to its level
    // so that a quality setting means about the same signal-to-noise ratio on quiet and loud files.
    let vbr_gain = opts.vbr.map(|q| {
        let rms = (pcm.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>()
            / pcm.len().max(1) as f64)
            .sqrt()
            .max(1e-4);
        (158.5 + 1.5 * f64::from(q.clamp(0.0, 9.0)) + 4.0 * (rms / 0.057).log2())
            .round()
            .clamp(0.0, 255.0) as i32
    });

    let base = if v1 { 144u64 } else { 72 };
    let max_reservoir = if v1 { 511usize } else { 255 };
    let side_len = match (v1, nch) {
        (true, 1) => 17,
        (true, _) => 32,
        (false, 1) => 9,
        (false, _) => 17,
    };

    // Chain delay: filter banks (481) plus the MDCT overlap (576); the decoder removes 529.
    let enc_delay = CHAIN_DELAY - 529;
    let frames = (n + CHAIN_DELAY).div_ceil(spf).max(1);
    let padding_samples = frames * spf - enc_delay - n;

    let mut chans: Vec<Channel> = (0..nch).map(|_| Channel::new()).collect();
    let mut stream = Bw::default();
    let mut cap_total = 0usize; // C: bytes of capacity of all frames so far
    let mut data_total = 0usize; // D: bytes of main data written so far
    let mut out_frames: Vec<Vec<u8>> = Vec::with_capacity(frames);
    let mut slot_ranges: Vec<(usize, usize)> = Vec::with_capacity(frames);
    let mut frame_bitrates: Vec<usize> = Vec::with_capacity(frames);
    let mut slot_frac = 0u64;
    let rate64 = u64::from(rate);

    let mut ms_frames = 0usize;
    for f in 0..frames {
        // Bit rate and padding of this frame (constant rate: from the table; variable: below).
        let fetch = |lo: usize, hi: usize, ch: usize| -> Vec<f32> {
            (lo..hi)
                .map(|i| {
                    let t = i as isize;
                    if t >= 0 && (t as usize) < n {
                        pcm[t as usize * nch + ch]
                    } else {
                        0.0
                    }
                })
                .collect()
        };
        // Analysis of all granules of this frame.
        let mut spectra: Vec<Vec<[f32; 576]>> = Vec::new(); // [gr][ch]
        for g in 0..ngr {
            let lo = f * spf + g * 576;
            let mut per = Vec::new();
            for c in 0..nch {
                let block = fetch(lo, lo + 576, c);
                per.push(chans[c].granule(&block));
            }
            spectra.push(per);
        }
        // Stereo decision for the frame.
        let mut ms = false;
        if nch == 2 && opts.joint_stereo {
            let (mut lr, mut mscost) = (0.0f64, 0.0f64);
            for per in &spectra {
                for k in 0..576 {
                    let (l, r) = (per[0][k], per[1][k]);
                    lr += f64::from(l.abs().powf(0.75) + r.abs().powf(0.75));
                    let (m, s) = (
                        (l + r) * std::f32::consts::FRAC_1_SQRT_2,
                        (l - r) * std::f32::consts::FRAC_1_SQRT_2,
                    );
                    mscost += f64::from(m.abs().powf(0.75) + s.abs().powf(0.75));
                }
            }
            ms = mscost < lr * 0.95;
        }
        if ms {
            ms_frames += 1;
            for per in spectra.iter_mut() {
                for k in 0..576 {
                    let (l, r) = (per[0][k], per[1][k]);
                    per[0][k] = (l + r) * std::f32::consts::FRAC_1_SQRT_2;
                    per[1][k] = (l - r) * std::f32::consts::FRAC_1_SQRT_2;
                }
            }
        }
        let quants: Vec<Vec<Quantizer>> = spectra
            .iter()
            .map(|per| per.iter().map(Quantizer::new).collect())
            .collect();

        // Choose the frame's bit rate and code the granules.
        let mdb_now = cap_total - data_total.min(cap_total);
        let (br_idx, grs): (usize, Vec<GranuleOut>) = if let Some(g0) = vbr_gain {
            // Raise the step for the rare frame that would not fit even the largest bit rate.
            let largest = ((base * u64::from(list[13]) * 1000) / rate64) as usize;
            let mut g = g0;
            loop {
                let grs: Vec<GranuleOut> = quants
                    .iter()
                    .map(|q| code_granule(q, rate, usize::MAX / 4, Some(g)))
                    .collect();
                let bits: usize = grs.iter().flat_map(|g| g.ch.iter()).map(|c| c.bits).sum();
                let need = bits.div_ceil(8) + 4 + side_len + 1;
                if need <= largest || g >= 255 {
                    let idx = (0..14)
                        .find(|&i| ((base * u64::from(list[i]) * 1000) / rate64) as usize >= need)
                        .unwrap_or(13);
                    break (idx, grs);
                }
                g += 1;
            }
        } else {
            let slot = (base * u64::from(list[cbr_idx]) * 1000) / rate64;
            let _ = slot;
            let capacity =
                frame_capacity(base, list[cbr_idx], rate64, &mut slot_frac.clone()) - 4 - side_len;
            let avail = capacity * 8 + mdb_now.min(max_reservoir) * 8 / 2;
            let mut remaining = avail;
            let mut grs = Vec::new();
            for (gi, q) in quants.iter().enumerate() {
                let share = remaining / (ngr - gi);
                let g = code_granule(q, rate, share, None);
                let used: usize = g.ch.iter().map(|c| c.bits).sum();
                remaining = remaining.saturating_sub(used);
                grs.push(g);
            }
            (cbr_idx, grs)
        };

        // Frame size with padding (accumulated so that the average rate is exact).
        let num = base * u64::from(list[br_idx]) * 1000;
        let mut slots = (num / rate64) as usize;
        slot_frac += num % rate64;
        let padding = slot_frac >= rate64;
        if padding {
            slot_frac -= rate64;
            slots += 1;
        }
        let capacity = slots - 4 - side_len;
        // Main data of this frame, appended to the stream.
        let mdb = (cap_total - data_total).min(max_reservoir);
        // The reservoir pointer may not exceed what exists.
        let mdb = mdb.min(cap_total);
        for g in &grs {
            for c in &g.ch {
                emit(&mut stream, c, rate);
            }
        }
        stream.align();
        let mut used_total = stream.bytes.len();
        cap_total += capacity;
        // Stuffing keeps the pointer within its range and the data inside the slots.
        if used_total + max_reservoir < cap_total {
            used_total = cap_total - max_reservoir;
        }
        if used_total > cap_total {
            // Does not happen with a fitting budget; if it does, fail loudly instead of corrupting.
            return Err(Error::new(
                ErrorCode::InternalError,
                "MP3 encoder overran a frame",
            ));
        }
        stream.bytes.resize(used_total, 0);
        stream.nbits = used_total * 8;
        data_total = used_total;

        // Assemble the frame.
        let mode = if nch == 1 {
            3
        } else if opts.joint_stereo {
            1
        } else {
            0
        };
        let mut frame = Vec::with_capacity(slots);
        frame.extend_from_slice(&header(
            version,
            br_idx,
            sr_idx,
            padding,
            mode,
            if ms { 2 } else { 0 },
        ));
        frame.extend_from_slice(&side_info(v1, nch, mdb, &grs));
        // The slot is filled once the whole stream is known: the data of later frames may
        // begin inside this one (bit reservoir).
        slot_ranges.push((cap_total - capacity, capacity));
        out_frames.push(frame);
        frame_bitrates.push(br_idx);
    }
    if ms_frames > 0 {
        notes.push(Note::new(
            Severity::Info,
            "mp3.joint_stereo",
            format!("{ms_frames} of {frames} frames use mid/side coding"),
        ));
    }

    stream.bytes.resize(cap_total, 0);
    for (fr, &(start, len)) in out_frames.iter_mut().zip(&slot_ranges) {
        fr.extend_from_slice(&stream.bytes[start..start + len]);
    }

    // The information frame in front of the audio.
    let audio_bytes: usize = out_frames.iter().map(Vec::len).sum();
    // The tag needs room for its fields: the smallest bit rate whose frame holds it (the
    // constant rate itself when that is enough, as decoders skip this frame by its own length).
    let info_need = 4 + side_len + 120 + 36;
    let fits = |i: usize| ((base * u64::from(list[i]) * 1000) / rate64) as usize >= info_need;
    let info_br = if vbr_gain.is_none() && fits(cbr_idx) {
        cbr_idx
    } else {
        (0..14).find(|&i| fits(i)).unwrap_or(13)
    };
    let info_len = ((base * u64::from(list[info_br]) * 1000) / rate64) as usize;
    let mut info = vec![0u8; info_len];
    info[..4].copy_from_slice(&header(
        version,
        info_br,
        sr_idx,
        false,
        if nch == 1 { 3 } else { 1 },
        0,
    ));
    let off = 4 + side_len;
    info[off..off + 4].copy_from_slice(if vbr_gain.is_some() { b"Xing" } else { b"Info" });
    info[off + 4..off + 8].copy_from_slice(&0x0Fu32.to_be_bytes());
    info[off + 8..off + 12].copy_from_slice(&(frames as u32).to_be_bytes());
    info[off + 12..off + 16].copy_from_slice(&((audio_bytes + info_len) as u32).to_be_bytes());
    // Table of contents: file position at each percent of the duration.
    let mut pos = Vec::with_capacity(frames);
    let mut acc = info_len;
    for fr in &out_frames {
        pos.push(acc);
        acc += fr.len();
    }
    let total_bytes = (audio_bytes + info_len).max(1);
    for p in 0..100 {
        let fi = (p * frames / 100).min(frames - 1);
        let v = (pos[fi] * 256 / total_bytes).min(255);
        info[off + 16 + p] = v as u8;
    }
    let quality = opts
        .vbr
        .map_or(100, |q| (100.0 - 10.0 * q.clamp(0.0, 9.0)) as i32);
    info[off + 116..off + 120].copy_from_slice(&(quality as u32).to_be_bytes());
    let lame = off + 120;
    info[lame..lame + 9].copy_from_slice(b"LAMEOC2.2");
    info[lame + 9] = if vbr_gain.is_some() { 3 } else { 1 }; // info tag revision 0, method: CBR 1 / VBR 3
    info[lame + 10] = 0xFF;
    info[lame + 20] = if vbr_gain.is_some() {
        0
    } else {
        (list[cbr_idx].min(255)) as u8
    };
    let delay_bytes = ((enc_delay as u32) << 12) | (padding_samples as u32 & 0xFFF);
    info[lame + 21] = (delay_bytes >> 16) as u8;
    info[lame + 22] = (delay_bytes >> 8) as u8;
    info[lame + 23] = delay_bytes as u8;
    info[lame + 28..lame + 32].copy_from_slice(&(audio_bytes as u32).to_be_bytes());
    let audio_crc = {
        let mut c = 0u16;
        for fr in &out_frames {
            // CRC over the audio: chained by feeding the previous value.
            for &b in fr {
                c ^= u16::from(b);
                for _ in 0..8 {
                    c = if c & 1 == 1 {
                        (c >> 1) ^ 0xA001
                    } else {
                        c >> 1
                    };
                }
            }
        }
        c
    };
    info[lame + 32..lame + 34].copy_from_slice(&audio_crc.to_be_bytes());
    let tag_crc = crc16(&info[..lame + 34]);
    info[lame + 34..lame + 36].copy_from_slice(&tag_crc.to_be_bytes());

    let mut out = Vec::with_capacity(audio_bytes + info_len + 1024);
    let empty = Tags::default();
    let tags = if opts.id3 { &a.tags } else { &empty };
    let pics: &[crate::Picture] = if opts.id3 { &a.pictures } else { &[] };
    out.extend(crate::id3::write_v2(tags, pics, 0));
    out.extend_from_slice(&info);
    for fr in out_frames {
        out.extend_from_slice(&fr);
    }
    Ok(out)
}

fn frame_capacity(base: u64, kbps: u32, rate: u64, frac: &mut u64) -> usize {
    let num = base * u64::from(kbps) * 1000;
    let mut slots = (num / rate) as usize;
    *frac += num % rate;
    if *frac >= rate {
        slots += 1;
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_lands_in_the_right_lines() {
        let mut ch = Channel::new();
        let rate = 44100.0f32;
        let mut last = [0.0f32; 576];
        for g in 0..8 {
            let pcm: Vec<f32> = (0..576)
                .map(|i| {
                    0.5 * (2.0 * std::f32::consts::PI * 1000.0 * (g * 576 + i) as f32 / rate).sin()
                })
                .collect();
            last = ch.granule(&pcm);
        }
        let mut e = [0.0f32; 32];
        for k in 0..32 {
            e[k] = last[k * 18..k * 18 + 18].iter().map(|v| v * v).sum();
        }
        // 1 kHz is in the second subband (689 to 1378 Hz).
        let total: f32 = e.iter().sum();
        assert!(e[1] > 0.99 * total, "{e:?}");
        let peak = last.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!((0.3..0.6).contains(&peak), "peak {peak}");
    }

    #[test]
    fn huffman_data_parses_back() {
        let mut s = 12345u64;
        for case in 0..400 {
            // Varied spectra: decaying magnitudes with occasional large values.
            let mut is = [0i32; 576];
            let scale = [3.0, 12.0, 60.0, 400.0, 6000.0][case % 5];
            let width = 40 + (case * 37) % 536;
            for (k, v) in is.iter_mut().enumerate().take(width) {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let r = (s >> 11) as f64 / (1u64 << 53) as f64;
                let m = (r * r * r * scale * (1.0 - k as f64 / width as f64)).floor() as i32;
                let m = m.min(8206);
                *v = if s & 1 == 0 { m } else { -m };
            }
            let rate = [44100, 48000, 32000][case % 3];
            let c = plan(&is, rate);
            let mut w = Bw::default();
            emit(&mut w, &c, rate);
            assert_eq!(w.nbits, c.bits, "case {case} bit count");
            let back = super::super::dec::parse_spectrum_for_test(
                &w.bytes,
                c.bits,
                c.big_values,
                c.tables,
                c.r0,
                c.r1,
                c.count1_table,
                rate,
            );
            assert_eq!(&back[..], &is[..], "case {case}");
        }
    }

    #[test]
    fn analysis_then_synthesis_is_transparent() {
        let rate = 44100.0f32;
        let n = 576 * 12;
        let src: Vec<f32> = (0..n)
            .map(|i| {
                0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / rate).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * 6000.0 * i as f32 / rate).sin()
            })
            .collect();
        let mut ch = Channel::new();
        let spectra: Vec<[f32; 576]> = src.chunks(576).map(|c| ch.granule(c)).collect();
        let out = super::super::dec::reconstruct_long(&spectra);
        // Find the delay and the gain.
        let mut best = (0usize, 0f64);
        for lag in 0..1500 {
            let mut c = 0f64;
            for i in 3000..5000 {
                c += f64::from(src[i]) * f64::from(out[i + lag]);
            }
            if c.abs() > best.1.abs() {
                best = (lag, c);
            }
        }
        let e: f64 = (3000..5000).map(|i| f64::from(src[i]).powi(2)).sum();
        assert!((best.1 / e - 1.0).abs() < 1e-3, "gain {}", best.1 / e);
        let mut err_best = (0usize, f64::MAX);
        for lag in 900..1200 {
            let err: f64 = (3000..5000)
                .map(|i| (f64::from(src[i]) - f64::from(out[i + lag])).powi(2))
                .sum();
            if err < err_best.1 {
                err_best = (lag, err);
            }
        }
        // The chain delay the encoder's gapless header relies on.
        assert_eq!(err_best.0, CHAIN_DELAY);
        assert!(err_best.1 / e < 1e-6);
        // Quantise and dequantise at a fine step: the error must stay small.
        for gain in [150, 180, 200] {
            let step = 2f64.powf(f64::from(gain - 210) / 4.0);
            let deq: Vec<[f32; 576]> = spectra
                .iter()
                .map(|xr| {
                    let q = Quantizer::new(xr);
                    let is = q.run(gain).unwrap_or([0; 576]);
                    let mut o = [0.0f32; 576];
                    for k in 0..576 {
                        let m = f64::from(is[k].abs()).powf(4.0 / 3.0) * step;
                        o[k] = if is[k] < 0 { -m as f32 } else { m as f32 };
                    }
                    o
                })
                .collect();
            let out2 = super::super::dec::reconstruct_long(&deq);
            let err: f64 = (3000..5000)
                .map(|i| (f64::from(src[i]) - f64::from(out2[i + err_best.0])).powi(2))
                .sum();
            let snr = 10.0 * (e / err.max(1e-30)).log10();
            let want = match gain {
                150 => 60.0,
                180 => 28.0,
                _ => 10.0,
            };
            assert!(snr > want, "gain {gain}: {snr:.1} dB");
        }
    }
}
