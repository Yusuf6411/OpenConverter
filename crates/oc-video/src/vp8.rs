//! VP8 video: key frames today, inter frames next.
use crate::frames::EncodeParams;
use crate::pic::Picture;
use crate::{corrupt, Res};
use oc_core::{Limits, NoProgress};

pub struct Vp8Decoder {
    _limits: Limits,
}

impl Vp8Decoder {
    pub fn new(limits: Limits) -> Self {
        Self { _limits: limits }
    }

    pub fn decode(&mut self, data: &[u8]) -> Res<Option<Picture>> {
        let f = oc_vp8::decode(data, &NoProgress)?;
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
    params: oc_vp8::Params,
}

impl Vp8Encoder {
    pub fn new(_w: usize, _h: usize, p: &EncodeParams) -> Self {
        let params = oc_vp8::Params {
            quality: f32::from(p.quality),
            ..Default::default()
        };
        Self { params }
    }

    pub fn encode(&mut self, pic: &Picture) -> Res<(Vec<u8>, bool)> {
        let frame = oc_vp8::Frame {
            width: pic.w,
            height: pic.h,
            y: pic.y.clone(),
            u: pic.u.clone(),
            v: pic.v.clone(),
        };
        let bytes = oc_vp8::encode(&frame, &self.params, &NoProgress)?;
        Ok((bytes, true))
    }
}
