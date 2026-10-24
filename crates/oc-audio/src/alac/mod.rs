//! Apple Lossless: the codec (`dec`, `enc`) and its magic cookie. Containers (M4A, CAF) live
//! in [`crate::m4a`] and [`crate::caf`].
use oc_core::{Error, Result};

pub mod dec;
pub mod enc;

/// Where each coded channel goes in the output, by channel count. Elements arrive as
/// centre, left-right pairs, surrounds, back, and LFE.
pub(crate) const CHANNEL_MAP: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[2, 0, 1],
    &[2, 0, 1, 3],
    &[2, 0, 1, 3, 4],
    &[2, 0, 1, 4, 5, 3],
    &[2, 0, 1, 4, 5, 6, 3],
    &[2, 6, 7, 0, 1, 4, 5, 3],
];

/// The 24-byte `ALACSpecificConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub frame_len: u32,
    pub bit_depth: u8,
    pub pb: u8,
    pub mb: u8,
    pub kb: u8,
    pub channels: u8,
    pub max_run: u16,
    pub max_frame_bytes: u32,
    pub avg_bitrate: u32,
    pub sample_rate: u32,
}

impl Cookie {
    pub fn new(channels: u8, bit_depth: u8, sample_rate: u32) -> Self {
        Self {
            frame_len: 4096,
            bit_depth,
            pb: 40,
            mb: 10,
            kb: 14,
            channels,
            max_run: 255,
            max_frame_bytes: 0,
            avg_bitrate: 0,
            sample_rate,
        }
    }

    /// Parse a cookie. Accepts the bare 24 bytes or the 36-byte form wrapped in an `alac`
    /// atom (size, `alac`, version and flags).
    pub fn parse(b: &[u8]) -> Result<Self> {
        // CAF written by Apple tools: a `frma` atom naming the codec comes first
        let b = if b.len() >= 48 && &b[4..8] == b"frma" {
            &b[12..]
        } else {
            b
        };
        let b = if b.len() >= 36 && &b[4..8] == b"alac" {
            &b[12..]
        } else {
            b
        };
        if b.len() < 24 {
            return Err(Error::corrupt("ALAC magic cookie is shorter than 24 bytes"));
        }
        let u32_at = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        Ok(Self {
            frame_len: u32_at(0),
            bit_depth: b[5],
            pb: b[6],
            mb: b[7],
            kb: b[8],
            channels: b[9],
            max_run: u16::from_be_bytes([b[10], b[11]]),
            max_frame_bytes: u32_at(12),
            avg_bitrate: u32_at(16),
            sample_rate: u32_at(20),
        })
    }

    pub fn to_bytes(&self) -> [u8; 24] {
        let mut o = [0u8; 24];
        o[0..4].copy_from_slice(&self.frame_len.to_be_bytes());
        o[4] = 0;
        o[5] = self.bit_depth;
        o[6] = self.pb;
        o[7] = self.mb;
        o[8] = self.kb;
        o[9] = self.channels;
        o[10..12].copy_from_slice(&self.max_run.to_be_bytes());
        o[12..16].copy_from_slice(&self.max_frame_bytes.to_be_bytes());
        o[16..20].copy_from_slice(&self.avg_bitrate.to_be_bytes());
        o[20..24].copy_from_slice(&self.sample_rate.to_be_bytes());
        o
    }
}
