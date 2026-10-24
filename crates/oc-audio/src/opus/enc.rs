//! Opus encoder: CELT-only frames of 2.5 to 20 ms, constant or variable bitrate, in Ogg
//! (RFC 7845). Stereo is coded as two independent channels (dual stereo); more than two
//! channels use mapping family 1 (coupled pairs plus mono streams). Output is always 48 kHz.
use super::bands::{eb, NB};
use super::celt::{self, Celt, EncInput, OVERLAP, PREEMPH};
use super::mdct::Mdct;
use super::tables::E_MEANS;
use crate::dsp::{self, Quality};
use crate::tags::write_vorbis_comment;
use crate::{ogg, Audio};
use oc_core::{Error, ErrorCode, Note, Notes, Result, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateMode {
    /// Every packet has the same size.
    Cbr,
    /// Packet sizes follow the signal; the average meets the target.
    Vbr,
    /// Like `Vbr` with a small bit reservoir, so short-term rate stays near the target.
    Cvbr,
}

impl RateMode {
    pub fn from_name(n: &str) -> Option<Self> {
        Some(match n {
            "cbr" => Self::Cbr,
            "vbr" => Self::Vbr,
            "cvbr" => Self::Cvbr,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Target bits per second for the whole file; 0 picks a default for the channel count.
    pub bitrate: u32,
    pub mode: RateMode,
    /// Frame duration in milliseconds: 2.5, 5, 10 or 20.
    pub frame_ms: f32,
    pub vendor: String,
    pub serial: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bitrate: 0,
            mode: RateMode::Vbr,
            frame_ms: 20.0,
            vendor: "OpenConverter".into(),
            serial: 0x4F43_5550,
        }
    }
}

pub const MIN_BITRATE: u32 = 6_000;
pub const MAX_BITRATE: u32 = 510_000;
const PRE_SKIP: usize = OVERLAP;
const MAX_FRAME_BYTES: usize = 1275;

/// (streams, coupled streams, channel mapping) of mapping family 1, indexed by channel count.
fn family1(ch: usize) -> (usize, usize, &'static [u8]) {
    match ch {
        3 => (2, 1, &[0, 2, 1]),
        4 => (2, 2, &[0, 1, 2, 3]),
        5 => (3, 2, &[0, 4, 1, 2, 3]),
        6 => (4, 2, &[0, 4, 1, 2, 3, 5]),
        7 => (5, 2, &[0, 4, 1, 2, 3, 5, 6]),
        _ => (6, 2, &[0, 6, 1, 2, 3, 4, 5, 7]),
    }
}

/// Number of bands to code for a bandwidth: (TOC bandwidth index, end band).
fn bandwidth_for(bits_per_channel_frame_rate: u32) -> (u8, usize) {
    match bits_per_channel_frame_rate {
        0..=15_999 => (0, 13),
        16_000..=23_999 => (2, 17),
        24_000..=31_999 => (3, 19),
        _ => (4, 21),
    }
}

struct Channel {
    tail: [f32; OVERLAP],
    pre_mem: f32,
}

/// One coded stream: one or two channels sharing a CELT state.
struct Stream {
    celt: Celt,
    ch: Vec<Channel>,
    first: bool,
    lm: usize,
    end: usize,
    toc: u8,
    mdct: &'static Mdct,
    activity: f32,
}

impl Stream {
    fn new(channels: usize, lm: usize, bandwidth: u8, end: usize) -> Self {
        let config = 16 + [0u8, 0, 1, 2, 3][usize::from(bandwidth)] * 4 + lm as u8;
        Self {
            celt: Celt::new(channels),
            ch: (0..channels)
                .map(|_| Channel {
                    tail: [0.0; OVERLAP],
                    pre_mem: 0.0,
                })
                .collect(),
            first: true,
            lm,
            end,
            toc: (config << 3) | if channels == 2 { 4 } else { 0 },
            mdct: celt::mdct_for(lm, false),
            activity: 0.0,
        }
    }

    /// Analyse `n` new samples per channel (`input[c]`, scaled to 16-bit range) into the
    /// coefficients, band energies and normalised shapes the frame writer needs.
    fn analyse(&mut self, input: &[&[f32]]) -> EncInput {
        let c = self.ch.len();
        let n = 120usize << self.lm;
        let m = 1usize << self.lm;
        let base = n / 2 - OVERLAP / 2;
        let w = celt::window();
        let scale = 2.0 / n as f32;
        let mut x = vec![0f32; c * n];
        let mut band_log_e = vec![-28.0f32; 2 * NB];
        let mut pe = 0f32;
        for (ch, st) in self.ch.iter_mut().enumerate() {
            let mut block = vec![0f32; n + OVERLAP];
            block[..OVERLAP].copy_from_slice(&st.tail);
            for (i, &s) in input[ch].iter().enumerate() {
                block[OVERLAP + i] = s - PREEMPH * st.pre_mem;
                st.pre_mem = s;
            }
            st.tail.copy_from_slice(&block[n..n + OVERLAP]);
            let mut seg = vec![0f32; 2 * n];
            for (j, &v) in block.iter().enumerate() {
                let win = if j < OVERLAP {
                    w[j]
                } else if j >= n {
                    w[n + OVERLAP - 1 - j]
                } else {
                    1.0
                };
                seg[base + j] = v * win;
            }
            let mut coef = vec![0f32; n];
            self.mdct.forward(&seg, &mut coef);
            for v in &mut coef {
                *v *= scale;
            }
            for i in 0..self.end {
                let (lo, hi) = (m * eb(i), m * eb(i + 1));
                let e: f32 = coef[lo..hi].iter().map(|v| v * v).sum();
                let amp = (e + 1e-27).sqrt();
                let lg = amp.log2() - E_MEANS[i];
                band_log_e[ch * NB + i] = lg.max(-28.0);
                // Rough measure of how many bits the band is worth.
                pe += (hi - lo) as f32 * (lg + 8.0).max(0.0).sqrt();
                for j in lo..hi {
                    x[ch * n + j] = coef[j] / amp;
                }
            }
        }
        self.activity = pe;
        let intra = self.first;
        self.first = false;
        EncInput {
            silence: false,
            intra,
            band_log_e,
            x,
        }
    }
}

fn put_len(out: &mut Vec<u8>, n: usize) {
    if n < 252 {
        out.push(n as u8);
    } else {
        out.push(252 + (n & 3) as u8);
        out.push(((n - (252 + (n & 3))) >> 2) as u8);
    }
}

pub fn write(audio: &Audio, opts: &Options, notes: &mut Notes) -> Result<Vec<u8>> {
    let ch = usize::from(audio.channels);
    if !(1..=8).contains(&ch) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "Opus holds 1 to 8 channels in this encoder",
        ));
    }
    let lm = match opts.frame_ms {
        x if (x - 2.5).abs() < 0.01 => 0usize,
        x if (x - 5.0).abs() < 0.01 => 1,
        x if (x - 10.0).abs() < 0.01 => 2,
        x if (x - 20.0).abs() < 0.01 => 3,
        _ => {
            return Err(Error::invalid_option(
                "frame_ms",
                "frame duration must be 2.5, 5, 10 or 20 ms",
            ))
        }
    };
    let n = 120usize << lm;
    let original_rate = audio.sample_rate;
    let resampled;
    let audio = if audio.sample_rate == 48000 {
        audio
    } else {
        resampled = dsp::resample(audio, 48000, Quality::High)?;
        notes.push(Note::new(
            Severity::Info,
            "opus.resampled",
            format!("Opus codes 48 kHz audio; the {original_rate} Hz input was resampled"),
        ));
        &resampled
    };
    let (streams_n, coupled, mapping): (usize, usize, Vec<u8>) = match ch {
        1 | 2 => (1, ch - 1, vec![]),
        _ => {
            let (s, c, m) = family1(ch);
            (s, c, m.to_vec())
        }
    };
    let family = if ch <= 2 { 0u8 } else { 1 };
    let target = if opts.bitrate == 0 {
        match ch {
            1 => 64_000,
            2 => 96_000,
            _ => 48_000 * ch as u32,
        }
    } else {
        opts.bitrate
    };
    if !(MIN_BITRATE..=MAX_BITRATE * streams_n as u32).contains(&target) {
        return Err(Error::invalid_option(
            "bitrate",
            "Opus bitrates run from 6 to 510 kbit/s per stream",
        ));
    }
    // Rate share per stream: a coupled stream carries two channels.
    let weights: Vec<usize> = (0..streams_n)
        .map(|s| if s < coupled { 2 } else { 1 })
        .collect();
    let total_w: usize = weights.iter().sum();
    let frames_per_sec = 48000.0 / n as f64;
    let per_channel_rate = target / ch as u32;
    let (bw_idx, end) = bandwidth_for(per_channel_rate);
    let mut streams: Vec<Stream> = weights
        .iter()
        .map(|&w| Stream::new(w, lm, bw_idx, end))
        .collect();

    // De-interleave into stream-channel order (inputs[m] = stream channel m).
    let pcm = dsp::to_float(&audio.samples);
    let frames = audio.frames();
    let order = if ch >= 3 {
        crate::vorbis::layout(ch).0
    } else {
        (0..ch).collect()
    };
    let mut inputs: Vec<Vec<f32>> = vec![vec![0.0; frames]; ch];
    for (k, &src_vorbis) in order.iter().enumerate() {
        // WAV channel k is Vorbis channel `src_vorbis`.
        let m = if ch <= 2 {
            k
        } else {
            usize::from(mapping[src_vorbis])
        };
        for (i, v) in inputs[m].iter_mut().enumerate() {
            *v = pcm[i * ch + k] * 32768.0;
        }
    }
    let total = frames + PRE_SKIP;
    let nframes = total.div_ceil(n).max(1);

    // Packet header pages.
    let mut head = Vec::new();
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(ch as u8);
    head.extend_from_slice(&(PRE_SKIP as u16).to_le_bytes());
    head.extend_from_slice(&original_rate.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(family);
    if family == 1 {
        head.push(streams_n as u8);
        head.push(coupled as u8);
        head.extend_from_slice(&mapping);
    }
    let mut tags_block = b"OpusTags".to_vec();
    tags_block.extend_from_slice(&write_vorbis_comment(
        &opts.vendor,
        &audio.tags,
        &audio.pictures,
        true,
    ));
    let mut w = ogg::Writer::new(opts.serial);
    w.packet(&head, 0, true, false);
    w.packet(&tags_block, 0, true, false);

    let bytes_per_frame_total = f64::from(target) / frames_per_sec / 8.0;
    let (bank_limit, bank_cap) = match opts.mode {
        RateMode::Cbr => (0.0, 0.0),
        RateMode::Cvbr => (bytes_per_frame_total * 3.0, bytes_per_frame_total * 3.0),
        RateMode::Vbr => (bytes_per_frame_total * 40.0, bytes_per_frame_total * 40.0),
    };
    let mut bank = 0f64;
    let mut mean_act = 0f32;
    for f in 0..nframes {
        let start = f * n;
        let mut first_ch = 0usize;
        let mut analysed = Vec::with_capacity(streams_n);
        let mut act = 0f32;
        for (s, st) in streams.iter_mut().enumerate() {
            let wch = weights[s];
            let input: Vec<Vec<f32>> = (first_ch..first_ch + wch)
                .map(|m| {
                    (0..n)
                        .map(|j| inputs[m].get(start + j).copied().unwrap_or(0.0))
                        .collect()
                })
                .collect();
            first_ch += wch;
            let refs: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
            let e = st.analyse(&refs);
            act += st.activity;
            analysed.push(e);
        }
        // Size of this frame, summed over the streams.
        let want = match opts.mode {
            RateMode::Cbr => bytes_per_frame_total,
            _ => {
                mean_act = if mean_act == 0.0 {
                    act.max(1.0)
                } else {
                    0.98 * mean_act + 0.02 * act
                };
                let ratio = f64::from((act / mean_act.max(1.0)).sqrt()).clamp(0.5, 1.8);
                let w = bytes_per_frame_total * ratio;
                w.min(bytes_per_frame_total + bank + bank_limit)
            }
        };
        let want = want.max(3.0 * streams_n as f64);
        let mut packet = Vec::new();
        for (s, st) in streams.iter_mut().enumerate() {
            let share = want * weights[s] as f64 / total_w as f64;
            let nb = (share.round() as usize).clamp(2, MAX_FRAME_BYTES);
            let frame = st.celt.encode(&analysed[s], st.lm, st.ch.len(), st.end, nb);
            packet.push(st.toc);
            if s + 1 < streams_n {
                put_len(&mut packet, frame.len());
            }
            packet.extend_from_slice(&frame);
        }
        bank = (bank + bytes_per_frame_total - packet.len() as f64).min(bank_cap);
        let granule = ((f + 1) * n).min(total) as i64;
        w.packet(&packet, granule, false, f + 1 == nframes);
    }
    Ok(w.finish())
}
