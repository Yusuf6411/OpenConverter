//! Frame decoders and encoders for the video codecs this build carries: Motion JPEG and raw
//! planar video (VP8 joins them in [`crate::vp8`]).
use crate::pic::Picture;
use crate::{codec, unsupported, Kind, Res, Stream};
use oc_core::{Error, ErrorCode, Limits, NoProgress};
use oc_image::jpeg::{self, EncodeOptions, Subsampling};
use oc_image::{ops, Depth, Layout};

/// Codecs this build can decode to pictures.
pub fn can_decode(c: &str) -> bool {
    matches!(
        c,
        codec::MJPEG | codec::RAW | codec::VP8 | codec::THEORA | codec::PRORES
    )
}

/// Codecs this build can encode from pictures.
pub fn can_encode(c: &str) -> bool {
    matches!(c, codec::MJPEG | codec::RAW | codec::VP8)
}

pub enum Decoder {
    Mjpeg(Limits),
    Raw { pix: String, w: usize, h: usize },
    Vp8(Box<crate::vp8::Vp8Decoder>),
    Theora(Box<crate::theora::TheoraDecoder>),
    ProRes(crate::prores::ProResDecoder),
}

impl Decoder {
    pub fn new(s: &Stream, limits: &Limits) -> Res<Self> {
        if s.kind != Kind::Video {
            return Err(unsupported("not a video stream"));
        }
        match s.codec.as_str() {
            codec::MJPEG => Ok(Self::Mjpeg(limits.clone())),
            codec::RAW => Ok(Self::Raw { pix: s.pix_fmt.clone(), w: s.width as usize, h: s.height as usize }),
            codec::VP8 => Ok(Self::Vp8(Box::new(crate::vp8::Vp8Decoder::new(limits.clone())))),
            codec::THEORA => Ok(Self::Theora(Box::new(crate::theora::TheoraDecoder::new(&s.private, limits)?))),
            codec::PRORES => Ok(Self::ProRes(crate::prores::ProResDecoder::new(limits))),
            other => Err(Error::new(
                ErrorCode::CodecNotInBuild,
                format!("{other} video cannot be decoded in this build; it can be copied between containers"),
            )),
        }
    }

    /// One packet in, at most one picture out (a hidden frame gives none).
    pub fn decode(&mut self, data: &[u8]) -> Res<Option<Picture>> {
        match self {
            Self::Mjpeg(limits) => {
                let (img, _) = jpeg::decode(data, limits, &NoProgress)?;
                let (img, _) = ops::convert_depth(&img, Depth::Eight);
                let (img, _) = ops::convert_layout(&img, Layout::Rgba, [0, 0, 0]);
                Ok(Some(Picture::from_rgba(
                    img.width as usize,
                    img.height as usize,
                    &img.data,
                )?))
            }
            Self::Raw { pix, w, h } => Ok(Some(Picture::from_raw(pix, *w, *h, data)?)),
            Self::Vp8(d) => d.decode(data),
            Self::Theora(d) => d.decode(data),
            Self::ProRes(d) => d.decode(data),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncodeParams {
    /// 1 to 100.
    pub quality: u8,
    pub fps: (u32, u32),
    /// Frames between key frames.
    pub keyint: u32,
    /// Target bit rate in kbit/s; 0 picks the rate from `quality`.
    pub bitrate_kbps: u32,
    /// 0 (fastest) to 6 (smallest output) for encoders that have the choice.
    pub effort: u8,
    /// Worker threads; 0 picks from the machine.
    pub threads: usize,
}

impl Default for EncodeParams {
    fn default() -> Self {
        Self {
            quality: 80,
            fps: (30, 1),
            keyint: 120,
            bitrate_kbps: 0,
            effort: 4,
            threads: 0,
        }
    }
}

pub enum Encoder {
    Mjpeg(u8),
    Raw,
    Vp8(Box<crate::vp8::Vp8Encoder>),
}

impl Encoder {
    pub fn new(c: &str, w: usize, h: usize, p: &EncodeParams) -> Res<Self> {
        match c {
            codec::MJPEG => Ok(Self::Mjpeg(p.quality.clamp(1, 100))),
            codec::RAW => Ok(Self::Raw),
            codec::VP8 => Ok(Self::Vp8(Box::new(crate::vp8::Vp8Encoder::new(w, h, p)?))),
            other => Err(Error::new(
                ErrorCode::CodecNotInBuild,
                format!("{other} video cannot be encoded in this build"),
            )),
        }
    }

    /// Fill in the stream fields that depend on the encoder.
    pub fn describe(&self, s: &mut Stream) {
        match self {
            Self::Mjpeg(_) => s.codec = codec::MJPEG.into(),
            Self::Raw => {
                s.codec = codec::RAW.into();
                s.pix_fmt = "yuv420p".into();
            }
            Self::Vp8(_) => s.codec = codec::VP8.into(),
        }
        s.private.clear();
    }

    /// The packet for one picture and whether it is a key frame.
    pub fn encode(&mut self, pic: &Picture) -> Res<(Vec<u8>, bool)> {
        match self {
            Self::Mjpeg(q) => {
                let img = oc_image::Image::new(
                    pic.w as u32,
                    pic.h as u32,
                    Layout::Rgba,
                    Depth::Eight,
                    pic.to_rgba(),
                )?;
                let (bytes, _) = jpeg::encode(
                    &img,
                    &EncodeOptions {
                        quality: *q,
                        subsampling: Subsampling::S420,
                        ..EncodeOptions::default()
                    },
                )?;
                Ok((bytes, true))
            }
            Self::Raw => Ok((pic.to_raw(), true)),
            Self::Vp8(e) => e.encode(pic),
        }
    }
}
