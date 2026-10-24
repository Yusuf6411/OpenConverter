//! VP8 video.
use crate::frames::EncodeParams;
use crate::pic::Picture;
use crate::{corrupt, Res};
use oc_core::{Limits, NoProgress};

pub struct Vp8Decoder {
    _limits: Limits,
    inner: oc_vp8::Decoder,
}

impl Vp8Decoder {
    pub fn new(limits: Limits) -> Self {
        Self {
            _limits: limits,
            inner: oc_vp8::Decoder::new(),
        }
    }

    pub fn decode(&mut self, data: &[u8]) -> Res<Option<Picture>> {
        let Some(f) = self.inner.decode(data, &NoProgress)? else {
            return Ok(None);
        };
        let cw = f.chroma_width();
        if f.u.len() != cw * f.chroma_height() {
            return Err(corrupt("VP8 chroma plane has the wrong size"));
        }
        Ok(Some(Picture {
            w: f.width,
            h: f.height,
            y: f.y,
            u: f.u,
            v: f.v,
        }))
    }
}

pub struct Vp8Encoder {
    inner: oc_vp8::VideoEncoder,
    pending_key: bool,
}

impl Vp8Encoder {
    pub fn new(w: usize, h: usize, p: &EncodeParams) -> Res<Self> {
        let params = oc_vp8::VideoParams {
            quality: f32::from(p.quality),
            effort: p.effort,
            bitrate_kbps: p.bitrate_kbps,
            fps: p.fps.0 as f32 / p.fps.1.max(1) as f32,
            keyint: p.keyint,
            threads: p.threads,
            partitions_log2: 0,
        };
        Ok(Self {
            inner: oc_vp8::VideoEncoder::new(w, h, params)?,
            pending_key: false,
        })
    }

    /// Makes the next picture a key frame.
    pub fn request_key(&mut self) {
        self.pending_key = true;
    }

    pub fn encode(&mut self, pic: &Picture) -> Res<(Vec<u8>, bool)> {
        let frame = oc_vp8::Frame {
            width: pic.w,
            height: pic.h,
            y: pic.y.clone(),
            u: pic.u.clone(),
            v: pic.v.clone(),
        };
        let force = std::mem::take(&mut self.pending_key);
        self.inner.encode(&frame, force, &NoProgress)
    }
}
