//! Audio: an in-memory intermediate representation, container readers and writers, codecs
//! and signal processing. Everything is our own code.
//!
//! The IR keeps integer PCM as `i32` at its native bit depth, so lossless routes (FLAC to WAV
//! at 24 bit, say) never pass through floating point.
#![allow(
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks,
    clippy::needless_range_loop
)]
// sample and byte offsets are bounds-checked against the input before use; the format tests feed hostile data
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod aiff;
pub mod alac;
pub mod ape;
pub mod caf;
pub mod copy;
pub mod dsp;
pub mod flac;
pub mod id3;
pub mod m4a;
pub mod mka;
pub mod mp3;
pub mod ogg;
pub mod opus;
pub mod tags;
pub mod vorbis;
pub mod wav;

pub use tags::{Picture, Tags};

use oc_core::{Error, Limits, Result};

/// The samples of a clip, interleaved by channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Samples {
    /// Signed integers in `bits` bits (8..=32), stored right-aligned in an `i32`.
    Int { bits: u8, data: Vec<i32> },
    /// Floating point in -1.0..=1.0 (values outside are legal and mean headroom).
    Float(Vec<f32>),
}

impl Samples {
    pub fn len(&self) -> usize {
        match self {
            Self::Int { data, .. } => data.len(),
            Self::Float(d) => d.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A decoded clip.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub sample_rate: u32,
    pub channels: u16,
    /// WAVEFORMAT_EXTENSIBLE speaker mask; 0 when the layout is unspecified.
    pub channel_mask: u32,
    pub samples: Samples,
    pub tags: Tags,
    pub pictures: Vec<Picture>,
}

impl Audio {
    pub fn new(sample_rate: u32, channels: u16, samples: Samples) -> Self {
        Self {
            sample_rate,
            channels,
            channel_mask: default_mask(channels),
            samples,
            tags: Tags::default(),
            pictures: Vec::new(),
        }
    }
    /// Frames (samples per channel).
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / f64::from(self.sample_rate)
        }
    }
    /// Native bit depth for integer audio, 32 for float.
    pub fn bits(&self) -> u8 {
        match &self.samples {
            Samples::Int { bits, .. } => *bits,
            Samples::Float(_) => 32,
        }
    }
    pub fn validate(&self, limits: &Limits) -> Result<()> {
        if self.channels == 0 || self.sample_rate == 0 {
            return Err(Error::corrupt("audio has no channels or no sample rate"));
        }
        if !self
            .samples
            .len()
            .is_multiple_of(usize::from(self.channels))
        {
            return Err(Error::corrupt(
                "sample count is not a multiple of the channel count",
            ));
        }
        limits.check_alloc(self.samples.len() as u64 * 4)
    }
}

/// Speaker masks (WAVEFORMAT_EXTENSIBLE bit order) that conventional layouts use.
pub fn default_mask(channels: u16) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        3 => 0x7,
        4 => 0x33,
        5 => 0x37,
        6 => 0x3F,
        7 => 0x13F,
        8 => 0x63F,
        _ => 0,
    }
}

/// Guard shared by readers: refuse a clip that would not fit the allocation limit.
pub(crate) fn check_samples(count: u64, limits: &Limits) -> Result<()> {
    limits.check_alloc(count.saturating_mul(4))
}
