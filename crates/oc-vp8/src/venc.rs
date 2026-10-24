//! VP8 video encoder: a sequence of frames coded as one key frame followed by inter frames,
//! with the quantiser steered towards a bit rate when one is asked for.
#![allow(clippy::indexing_slicing)]

use crate::dec::Frame;
use crate::enc::{encode_core, Core, ModeProbs, Params};
use crate::inter::{Mv, GOLDEN_FRAME, LAST_FRAME};
use crate::recon::Planes;
use crate::tables::COEFF_PROBS;
use crate::tokens::Probs;
use oc_core::{Error, ErrorCode, Progress, Result};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct VideoParams {
    /// 1 (smallest) to 100 (best): the quantiser when no bit rate is given.
    pub quality: f32,
    /// 0 (fastest) to 6 (best compression).
    pub effort: u8,
    /// Target bit rate in kilobits a second; 0 codes at constant quality.
    pub bitrate_kbps: u32,
    pub fps: f32,
    /// Frames between key frames at most.
    pub keyint: u32,
    /// Worker threads; 0 picks from the machine.
    pub threads: usize,
    /// Token partitions as a power of two exponent (0..=3).
    pub partitions_log2: u8,
}

impl Default for VideoParams {
    fn default() -> Self {
        Self {
            quality: 75.0,
            effort: 4,
            bitrate_kbps: 0,
            fps: 30.0,
            keyint: 120,
            threads: 0,
            partitions_log2: 0,
        }
    }
}

/// Steers the quantiser index so the stream approaches a target bit rate.
struct Rate {
    per_frame: f64,
    q: f64,
    debt: f64,
}

impl Rate {
    fn new(kbps: u32, fps: f32, w: usize, h: usize) -> Self {
        let per_frame = f64::from(kbps) * 1000.0 / f64::from(fps.max(1.0));
        let bpp = per_frame / (w * h).max(1) as f64;
        let q = (45.0 - 48.0 * (bpp / 0.1).max(1e-4).log10()).clamp(5.0, 120.0);
        Self {
            per_frame,
            q,
            debt: 0.0,
        }
    }

    fn target(&self, key: bool) -> f64 {
        if key {
            self.per_frame * 4.0
        } else {
            self.per_frame
        }
    }

    fn index(&self, key: bool) -> u8 {
        let q = if key { self.q - 6.0 } else { self.q };
        q.round().clamp(0.0, 127.0) as u8
    }

    fn update(&mut self, bits: f64, key: bool) {
        let target = self.target(key);
        let err = (bits.max(8.0) / target).log2();
        let gain = if key { 5.0 } else { 10.0 };
        self.q += (gain * err).clamp(-8.0, 8.0);
        self.debt = self.debt * 0.97 + (bits - target);
        self.q += (self.debt / (self.per_frame * 20.0)).clamp(-2.0, 2.0);
        self.q = self.q.clamp(0.0, 127.0);
    }
}

pub struct VideoEncoder {
    params: VideoParams,
    size: (usize, usize),
    threads: usize,
    last: Option<Arc<Planes>>,
    golden: Option<Arc<Planes>>,
    prev_mvs: Vec<Mv>,
    mp: ModeProbs,
    since_key: u32,
    since_golden: u32,
    rate: Option<Rate>,
    /// Token probabilities the decoder holds after the last frame.
    probs: Probs,
}

impl VideoEncoder {
    pub fn new(width: usize, height: usize, params: VideoParams) -> Result<Self> {
        if width == 0 || height == 0 || width > 16383 || height > 16383 {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "VP8 frames are at most 16383 pixels on a side",
            ));
        }
        let threads = if params.threads == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get().min(8))
        } else {
            params.threads
        };
        let rate = (params.bitrate_kbps > 0)
            .then(|| Rate::new(params.bitrate_kbps, params.fps, width, height));
        Ok(Self {
            params,
            size: (width, height),
            threads,
            last: None,
            golden: None,
            prev_mvs: Vec::new(),
            mp: ModeProbs::default(),
            since_key: 0,
            since_golden: 0,
            rate,
            probs: COEFF_PROBS,
        })
    }

    /// Codes the next picture. Returns the frame's bytes and whether it is a key frame.
    pub fn encode(
        &mut self,
        frame: &Frame,
        force_key: bool,
        progress: &dyn Progress,
    ) -> Result<(Vec<u8>, bool)> {
        if (frame.width, frame.height) != self.size {
            return Err(Error::new(
                ErrorCode::InternalError,
                "VP8 frame size changed mid-stream",
            ));
        }
        let key = force_key || self.last.is_none() || self.since_key >= self.params.keyint.max(1);
        let refresh_golden = !key && self.since_golden >= 10;
        let mut attempts = 0;
        loop {
            attempts += 1;
            let index = match &self.rate {
                Some(r) => r.index(key),
                None => crate::enc::quality_to_index(self.params.quality).clamp(0, 127) as u8,
            };
            let params = Params {
                quality: self.params.quality,
                effort: self.params.effort,
                filter_level: None,
                filter_sharpness: 0,
                partitions_log2: self.params.partitions_log2,
                qindex: Some(index),
            };
            let mut core = Core::new(&params, key, frame, self.threads);
            core.mp = self.mp.clone();
            core.persist = true;
            core.base_probs = if key { COEFF_PROBS } else { self.probs };
            core.cost_probs = core.base_probs;
            if !key {
                core.refs[usize::from(LAST_FRAME)] = self.last.clone();
                core.refs[usize::from(GOLDEN_FRAME)] = self.golden.clone();
                crate::enc_inter::estimate_motion(&mut core, &self.prev_mvs);
            }
            let enc = encode_core(
                &mut core,
                frame.width,
                frame.height,
                refresh_golden,
                progress,
            )?;
            let bits = enc.bytes.len() as f64 * 8.0;
            if let Some(r) = &mut self.rate {
                if key
                    && attempts < 4
                    && (bits > r.target(true) * 2.5 || bits < r.target(true) * 0.3)
                {
                    // A key frame far off its budget would drag the whole stream along: try
                    // again at a better guess.
                    r.q = (r.q + 24.0 * (bits / r.target(true)).log2()).clamp(0.0, 127.0);
                    continue;
                }
                r.update(bits, key);
            }
            let rec = Arc::new(core.filtered_planes(enc.level, 0));
            self.prev_mvs = core
                .mbs
                .iter()
                .map(|m| if m.inter { m.mv } else { Mv::ZERO })
                .collect();
            self.mp = enc.mp;
            self.probs = enc.probs;
            self.last = Some(rec.clone());
            if key {
                self.since_key = 1;
                self.since_golden = 0;
                self.golden = Some(rec);
            } else {
                self.since_key += 1;
                if refresh_golden {
                    self.since_golden = 0;
                    self.golden = Some(rec);
                } else {
                    self.since_golden += 1;
                }
            }
            return Ok((enc.bytes, key));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::dec::Decoder;
    use crate::enc::tests::test_frame;
    use oc_core::NoProgress;

    /// A picture that moves by a few samples each step, with a little fresh noise.
    fn moving(w: usize, h: usize, step: usize) -> Frame {
        let base = test_frame(w + 40, h + 40, 5);
        let (dx, dy) = ((step * 3) % 40, (step * 2) % 40);
        let crop = |p: &[u8], pw: usize, x0: usize, y0: usize, cw: usize, ch: usize| {
            let mut out = Vec::with_capacity(cw * ch);
            for r in 0..ch {
                out.extend_from_slice(&p[(y0 + r) * pw + x0..(y0 + r) * pw + x0 + cw]);
            }
            out
        };
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let bcw = (w + 40).div_ceil(2);
        Frame {
            width: w,
            height: h,
            y: crop(&base.y, w + 40, dx, dy, w, h),
            u: crop(&base.u, bcw, dx / 2, dy / 2, cw, ch),
            v: crop(&base.v, bcw, dx / 2, dy / 2, cw, ch),
        }
    }

    fn cropped(p: &Planes, w: usize, h: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for (plane, stride, pw, ph) in [
            (&p.y, p.ys, w, h),
            (&p.u, p.uvs, w.div_ceil(2), h.div_ceil(2)),
            (&p.v, p.uvs, w.div_ceil(2), h.div_ceil(2)),
        ] {
            for r in 0..ph {
                let s = (r + 1) * stride + 1;
                out.extend_from_slice(&plane[s..s + pw]);
            }
        }
        out
    }

    #[test]
    fn the_decoder_follows_the_encoder_frame_by_frame() {
        for (w, h) in [(16, 16), (70, 50), (33, 17), (1, 1), (5, 40)] {
            for (effort, kbps, keyint) in [(0, 0, 100), (2, 0, 4), (4, 0, 100), (6, 200, 7)] {
                let params = VideoParams {
                    quality: 60.0,
                    effort,
                    bitrate_kbps: kbps,
                    fps: 25.0,
                    keyint,
                    threads: 2,
                    partitions_log2: 0,
                };
                let mut enc = VideoEncoder::new(w, h, params).unwrap();
                let mut dec = Decoder::new();
                for step in 0..12 {
                    let frame = moving(w, h, step);
                    let (bytes, key) = enc.encode(&frame, false, &NoProgress).unwrap();
                    assert_eq!(key, step % keyint as usize == 0, "{w}x{h} step {step}");
                    let got = dec.decode(&bytes, &NoProgress).unwrap().unwrap();
                    let mut flat = got.y.clone();
                    flat.extend_from_slice(&got.u);
                    flat.extend_from_slice(&got.v);
                    let want = cropped(enc.last.as_ref().unwrap(), w, h);
                    assert_eq!(flat, want, "{w}x{h} effort {effort} step {step}");
                }
            }
        }
    }

    #[test]
    fn a_requested_key_frame_is_a_key_frame() {
        let mut enc = VideoEncoder::new(32, 32, VideoParams::default()).unwrap();
        let f = moving(32, 32, 0);
        assert!(enc.encode(&f, false, &NoProgress).unwrap().1);
        assert!(!enc.encode(&f, false, &NoProgress).unwrap().1);
        assert!(enc.encode(&f, true, &NoProgress).unwrap().1);
    }

    #[test]
    fn a_bit_rate_is_roughly_met() {
        let (w, h) = (96, 64);
        let mut enc = VideoEncoder::new(
            w,
            h,
            VideoParams {
                bitrate_kbps: 120,
                fps: 25.0,
                keyint: 1000,
                threads: 1,
                ..VideoParams::default()
            },
        )
        .unwrap();
        let mut bits = 0usize;
        for step in 0..60 {
            bits += enc
                .encode(&moving(w, h, step), false, &NoProgress)
                .unwrap()
                .0
                .len()
                * 8;
        }
        let kbps = bits as f64 / (60.0 / 25.0) / 1000.0;
        assert!((70.0..=200.0).contains(&kbps), "{kbps} kbit/s");
    }
}
