//! Video containers as one packet-level model: streams, timed packets, demuxers and muxers.
//!
//! A [`Media`] holds the streams of a file and their packets in decode order, with times in
//! nanoseconds. `dts` can be negative: when a video track reorders frames, the first decode
//! time lies before the first presentation time, as in MP4 edit lists. Codec configuration
//! travels in the Matroska `CodecPrivate` form (`avcC`, `hvcC`, `AudioSpecificConfig`,
//! `OpusHead`, Xiph-laced Vorbis headers, `fLaC` plus metadata blocks) so a remux between
//! containers never touches the compressed data.
#![forbid(unsafe_code)]
#![allow(clippy::indexing_slicing)]

pub mod avi;
pub mod es;
pub mod flv;
pub mod frames;
pub mod mkv;
pub mod mp4;
pub mod ogv;
pub mod pic;
pub mod ps;
pub mod subs;
pub mod theora;
pub mod ts;
pub mod vp8;
pub mod y4m;

use oc_core::{Error, ErrorCode, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Video,
    Audio,
    Subtitle,
}

/// Canonical codec names used across containers.
pub mod codec {
    pub const H264: &str = "h264";
    pub const HEVC: &str = "hevc";
    pub const VP8: &str = "vp8";
    pub const VP9: &str = "vp9";
    pub const AV1: &str = "av1";
    pub const MJPEG: &str = "mjpeg";
    pub const THEORA: &str = "theora";
    pub const MPEG4: &str = "mpeg4";
    /// Uncompressed frames; the stream's `pix_fmt` says how.
    pub const RAW: &str = "rawvideo";
    pub const AAC: &str = "aac";
    pub const MP3: &str = "mp3";
    pub const OPUS: &str = "opus";
    pub const VORBIS: &str = "vorbis";
    pub const FLAC: &str = "flac";
    pub const AC3: &str = "ac3";
    pub const EAC3: &str = "eac3";
    pub const PCM_S16: &str = "pcm_s16le";
    pub const PCM_S24: &str = "pcm_s24le";
    pub const PCM_S32: &str = "pcm_s32le";
    pub const PCM_U8: &str = "pcm_u8";
    pub const PCM_F32: &str = "pcm_f32le";
    /// Plain UTF-8 text per cue (SRT without the numbering).
    pub const SRT: &str = "srt";
    /// ASS: `private` is the script header, packets are Matroska-style event lines.
    pub const ASS: &str = "ass";
    pub const WEBVTT: &str = "webvtt";

    pub fn is_pcm(c: &str) -> bool {
        c.starts_with("pcm_")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub kind: Kind,
    pub codec: String,
    pub private: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Frame rate as numerator/denominator, when the container states one.
    pub fps: Option<(u32, u32)>,
    /// For raw video: `yuv420p`, `yuv422p`, `yuv444p`, `gray`, `rgb24`, `rgba`.
    pub pix_fmt: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub language: String,
    pub name: String,
    pub default: bool,
    pub forced: bool,
    /// Rotation in degrees clockwise (MP4 `tkhd` matrix), 0 when upright.
    pub rotation: u16,
}

impl Stream {
    pub fn new(kind: Kind, codec: &str) -> Self {
        Self {
            kind,
            codec: codec.to_string(),
            private: Vec::new(),
            width: 0,
            height: 0,
            fps: None,
            pix_fmt: String::new(),
            sample_rate: 0,
            channels: 0,
            bits: 0,
            language: "und".into(),
            name: String::new(),
            default: true,
            forced: false,
            rotation: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Packet {
    pub stream: usize,
    /// Presentation time, nanoseconds.
    pub pts: i64,
    /// Decode time, nanoseconds (not after `pts`).
    pub dts: i64,
    /// Nanoseconds; 0 when unknown (the muxer then uses the gap to the next packet).
    pub duration: i64,
    pub key: bool,
    /// Nanoseconds to drop from the end of the decoded audio (Opus trimming).
    pub discard_end: i64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Media {
    pub streams: Vec<Stream>,
    /// In decode order, interleaved by `dts`.
    pub packets: Vec<Packet>,
    pub title: String,
    /// Container the file came from, for the report.
    pub source: String,
}

impl Media {
    pub fn packets_of(&self, stream: usize) -> impl Iterator<Item = &Packet> {
        self.packets.iter().filter(move |p| p.stream == stream)
    }

    /// Duration of the whole file in nanoseconds.
    pub fn duration(&self) -> i64 {
        self.packets
            .iter()
            .map(|p| p.pts + p.duration)
            .max()
            .unwrap_or(0)
            .max(0)
    }

    /// Give every packet a duration: the distance to the next packet of its stream in
    /// presentation order (the last one repeats the previous distance).
    pub fn fill_durations(&mut self) {
        for s in 0..self.streams.len() {
            let mut idx: Vec<usize> = (0..self.packets.len())
                .filter(|&i| self.packets[i].stream == s)
                .collect();
            idx.sort_by_key(|&i| self.packets[i].pts);
            let mut last = 0;
            for w in 0..idx.len() {
                let next = idx
                    .get(w + 1)
                    .map(|&n| self.packets[n].pts - self.packets[idx[w]].pts);
                let p = &mut self.packets[idx[w]];
                if p.duration <= 0 {
                    p.duration = next.unwrap_or(last).max(0);
                }
                last = p.duration;
            }
        }
    }

    /// Order packets by decode time (stable, streams tie-broken by index).
    pub fn sort_by_dts(&mut self) {
        self.packets.sort_by_key(|p| (p.dts, p.stream));
    }
}

/// Decode times for streams that only carry presentation times (Matroska, AVI with B-frames):
/// the k-th decode time is the k-th smallest presentation time, moved back by the largest
/// reordering distance so that no packet decodes after it is shown.
pub fn assign_dts(pts_in_decode_order: &[i64]) -> Vec<i64> {
    let mut sorted = pts_in_decode_order.to_vec();
    sorted.sort_unstable();
    let shift = pts_in_decode_order
        .iter()
        .zip(&sorted)
        .map(|(p, s)| s - p)
        .max()
        .unwrap_or(0)
        .max(0);
    sorted.iter().map(|s| s - shift).collect()
}

pub(crate) fn corrupt(msg: &str) -> Error {
    Error::corrupt(msg)
}

pub(crate) fn unsupported(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::UnsupportedVariant, msg)
}

pub(crate) type Res<T> = Result<T>;

/// Ceil-free rounded division for timestamps.
pub(crate) fn div_round(a: i128, b: i128) -> i64 {
    if b == 0 {
        return 0;
    }
    let q = if (a >= 0) == (b > 0) {
        (a + b.abs() / 2) / b
    } else {
        (a - b.abs() / 2) / b
    };
    q as i64
}

/// A frame rate from a per-frame duration in nanoseconds, snapped to the usual broadcast rates.
pub fn fps_from_ns(frame_ns: i64) -> Option<(u32, u32)> {
    fps_from_ns_tol(frame_ns, 0.0008)
}

/// Frame rate from presentation times: the average spacing, snapped to a standard rate when
/// it agrees within the rounding of millisecond timestamps.
pub fn estimate_fps(pts: &[i64]) -> Option<(u32, u32)> {
    let mut v: Vec<i64> = pts.to_vec();
    v.sort_unstable();
    v.dedup();
    let gaps = v.len().checked_sub(1).filter(|g| *g > 0)? as i64;
    let span = v.last()? - v.first()?;
    if span <= 0 {
        return None;
    }
    let tol = (1.0e6 / span as f64).max(0.0008);
    fps_from_ns_tol(span / gaps, tol)
}

fn fps_from_ns_tol(frame_ns: i64, tol: f64) -> Option<(u32, u32)> {
    if frame_ns <= 0 {
        return None;
    }
    let fps = 1e9 / frame_ns as f64;
    for (n, d) in [
        (24000u32, 1001u32),
        (30000, 1001),
        (60000, 1001),
        (120000, 1001),
        (24, 1),
        (25, 1),
        (30, 1),
        (48, 1),
        (50, 1),
        (60, 1),
        (120, 1),
    ] {
        if (fps / (f64::from(n) / f64::from(d)) - 1.0).abs() < tol {
            return Some((n, d));
        }
    }
    let (mut a, mut b) = (1_000_000_000u64, frame_ns as u64);
    let g = {
        let (mut x, mut y) = (a, b);
        while y != 0 {
            (x, y) = (y, x % y);
        }
        x
    };
    a /= g;
    b /= g;
    (a <= u64::from(u32::MAX) && b <= u64::from(u32::MAX)).then_some((a as u32, b as u32))
}

/// Nanoseconds per frame for a frame rate.
pub fn frame_ns(fps: (u32, u32)) -> i64 {
    div_round(
        1_000_000_000i128 * i128::from(fps.1),
        i128::from(fps.0.max(1)),
    )
}
