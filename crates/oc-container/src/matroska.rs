//! Matroska and WebM (EBML, RFC 8794; Matroska, RFC 9559): a demuxer that flattens clusters
//! into a packet list and a muxer that writes SeekHead, Cues, Tags, Chapters and Attachments.
//!
//! The model is codec-blind: a track carries its codec ID and private data, a packet carries
//! bytes and a time in nanoseconds. Lacing (Xiph, EBML, fixed-size) and header-stripping
//! compression are undone on read; the muxer writes one frame per block.
use oc_core::{Error, ErrorCode, Limits, Result};

pub mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const SEEK: u32 = 0x4DBB;
    pub const SEEK_ID: u32 = 0x53AB;
    pub const SEEK_POSITION: u32 = 0x53AC;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub const DURATION: u32 = 0x4489;
    pub const MUXING_APP: u32 = 0x4D80;
    pub const WRITING_APP: u32 = 0x5741;
    pub const TITLE: u32 = 0x7BA9;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const FLAG_ENABLED: u32 = 0xB9;
    pub const FLAG_DEFAULT: u32 = 0x88;
    pub const FLAG_FORCED: u32 = 0x55AA;
    pub const DEFAULT_DURATION: u32 = 0x23_E383;
    pub const NAME: u32 = 0x536E;
    pub const LANGUAGE: u32 = 0x22_B59C;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const CODEC_DELAY: u32 = 0x56AA;
    pub const SEEK_PRE_ROLL: u32 = 0x56BB;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const DISPLAY_WIDTH: u32 = 0x54B0;
    pub const DISPLAY_HEIGHT: u32 = 0x54BA;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const OUTPUT_SAMPLING_FREQUENCY: u32 = 0x78B5;
    pub const CHANNELS: u32 = 0x9F;
    pub const BIT_DEPTH: u32 = 0x6264;
    pub const CONTENT_ENCODINGS: u32 = 0x6D80;
    pub const CONTENT_ENCODING: u32 = 0x6240;
    pub const CONTENT_ENCODING_TYPE: u32 = 0x5033;
    pub const CONTENT_COMPRESSION: u32 = 0x5034;
    pub const CONTENT_COMP_ALGO: u32 = 0x4254;
    pub const CONTENT_COMP_SETTINGS: u32 = 0x4255;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const BLOCK_GROUP: u32 = 0xA0;
    pub const BLOCK: u32 = 0xA1;
    pub const BLOCK_DURATION: u32 = 0x9B;
    pub const REFERENCE_BLOCK: u32 = 0xFB;
    pub const DISCARD_PADDING: u32 = 0x75A2;
    pub const CUES: u32 = 0x1C53_BB6B;
    pub const CUE_POINT: u32 = 0xBB;
    pub const CUE_TIME: u32 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
    pub const CUE_TRACK: u32 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
    pub const TAGS: u32 = 0x1254_C367;
    pub const TAG: u32 = 0x7373;
    pub const TARGETS: u32 = 0x63C0;
    pub const TARGET_TYPE_VALUE: u32 = 0x68CA;
    pub const TARGET_TYPE: u32 = 0x63CA;
    pub const TAG_TRACK_UID: u32 = 0x63C5;
    pub const SIMPLE_TAG: u32 = 0x67C8;
    pub const TAG_NAME: u32 = 0x45A3;
    pub const TAG_LANGUAGE: u32 = 0x447A;
    pub const TAG_DEFAULT: u32 = 0x4484;
    pub const TAG_STRING: u32 = 0x4487;
    pub const TAG_BINARY: u32 = 0x4485;
    pub const CHAPTERS: u32 = 0x1043_A770;
    pub const EDITION_ENTRY: u32 = 0x45B9;
    pub const EDITION_UID: u32 = 0x45BC;
    pub const EDITION_FLAG_HIDDEN: u32 = 0x45BD;
    pub const EDITION_FLAG_DEFAULT: u32 = 0x45DB;
    pub const CHAPTER_ATOM: u32 = 0xB6;
    pub const CHAPTER_UID: u32 = 0x73C4;
    pub const CHAPTER_TIME_START: u32 = 0x91;
    pub const CHAPTER_TIME_END: u32 = 0x92;
    pub const CHAPTER_FLAG_HIDDEN: u32 = 0x98;
    pub const CHAPTER_FLAG_ENABLED: u32 = 0x4598;
    pub const CHAPTER_DISPLAY: u32 = 0x80;
    pub const CHAP_STRING: u32 = 0x85;
    pub const CHAP_LANGUAGE: u32 = 0x437C;
    pub const ATTACHMENTS: u32 = 0x1941_A469;
    pub const ATTACHED_FILE: u32 = 0x61A7;
    pub const FILE_DESCRIPTION: u32 = 0x467E;
    pub const FILE_NAME: u32 = 0x466E;
    pub const FILE_MIME_TYPE: u32 = 0x4660;
    pub const FILE_DATA: u32 = 0x465C;
    pub const FILE_UID: u32 = 0x46AE;
    pub const VOID: u32 = 0xEC;
}

// ------------------------------------------------------------------------------- EBML basics

/// An element ID, with its length marker bits kept (as the specification writes IDs).
fn read_id(b: &[u8], o: &mut usize) -> Option<u32> {
    let first = *b.get(*o)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return None;
    }
    let mut v = 0u32;
    for i in 0..len {
        v = (v << 8) | u32::from(*b.get(*o + i)?);
    }
    *o += len;
    Some(v)
}

/// A data size; `Some(None)` is the reserved "unknown" value.
fn read_size(b: &[u8], o: &mut usize) -> Option<Option<u64>> {
    let first = *b.get(*o)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut v = u64::from(first) & ((1u64 << (8 - len)) - 1);
    let mut all_ones = v == (1u64 << (8 - len)) - 1;
    for i in 1..len {
        let byte = *b.get(*o + i)?;
        all_ones &= byte == 0xFF;
        v = (v << 8) | u64::from(byte);
    }
    *o += len;
    Some(if all_ones { None } else { Some(v) })
}

/// A signed variable-length integer (lace sizes).
fn read_svint(b: &[u8], o: &mut usize) -> Option<i64> {
    let first = *b.get(*o)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let start = *o;
    let mut v = u64::from(first) & ((1u64 << (8 - len)) - 1);
    for i in 1..len {
        v = (v << 8) | u64::from(*b.get(start + i)?);
    }
    *o += len;
    let bias = (1i64 << (7 * len - 1)) - 1;
    Some(v as i64 - bias)
}

#[derive(Clone, Copy)]
struct El<'a> {
    id: u32,
    body: &'a [u8],
}

/// Elements of a buffer. An unknown-size element extends to the end of the buffer here;
/// the segment loop handles unknown-size clusters itself.
fn elements(b: &[u8]) -> Vec<El<'_>> {
    let mut v = Vec::new();
    let mut o = 0;
    while o < b.len() {
        let Some(id) = read_id(b, &mut o) else { break };
        let Some(size) = read_size(b, &mut o) else {
            break;
        };
        let end = match size {
            Some(n) => (o as u64).saturating_add(n).min(b.len() as u64) as usize,
            None => b.len(),
        };
        v.push(El {
            id,
            body: &b[o..end],
        });
        o = end;
    }
    v
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0u64, |a, &x| (a << 8) | u64::from(x))
}
fn sint(b: &[u8]) -> i64 {
    if b.is_empty() {
        return 0;
    }
    let mut v = if b[0] & 0x80 != 0 { -1i64 } else { 0 };
    for &x in b.iter().take(8) {
        v = (v << 8) | i64::from(x);
    }
    v
}
fn float(b: &[u8]) -> f64 {
    match b.len() {
        4 => f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]])),
        8 => f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        _ => 0.0,
    }
}
fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

// ----------------------------------------------------------------------------------- model

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Other(u8),
}

impl TrackKind {
    fn from_code(c: u8) -> Self {
        match c {
            1 => Self::Video,
            2 => Self::Audio,
            17 => Self::Subtitle,
            n => Self::Other(n),
        }
    }
    fn code(self) -> u8 {
        match self {
            Self::Video => 1,
            Self::Audio => 2,
            Self::Subtitle => 17,
            Self::Other(n) => n,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioInfo {
    pub sample_rate: f64,
    pub output_sample_rate: Option<f64>,
    pub channels: u32,
    pub bit_depth: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub number: u64,
    pub uid: u64,
    pub kind: TrackKind,
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    pub language: String,
    pub name: String,
    pub default_duration_ns: Option<u64>,
    pub codec_delay_ns: u64,
    pub seek_pre_roll_ns: u64,
    pub enabled: bool,
    pub default: bool,
    pub forced: bool,
    pub audio: Option<AudioInfo>,
    pub video: Option<VideoInfo>,
    /// Bytes to put in front of every frame (header-stripping compression), already applied
    /// to the packets on read.
    pub stripped: Vec<u8>,
}

impl Track {
    pub fn new(number: u64, kind: TrackKind, codec_id: &str) -> Self {
        Self {
            number,
            uid: number,
            kind,
            codec_id: codec_id.to_string(),
            codec_private: Vec::new(),
            language: "und".into(),
            name: String::new(),
            default_duration_ns: None,
            codec_delay_ns: 0,
            seek_pre_roll_ns: 0,
            enabled: true,
            default: true,
            forced: false,
            audio: None,
            video: None,
            stripped: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Packet {
    pub track: u64,
    pub pts_ns: i64,
    pub duration_ns: Option<u64>,
    pub keyframe: bool,
    /// Nanoseconds to drop from the end (Opus end trimming); negative values pad the start.
    pub discard_padding_ns: i64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimpleTag {
    pub name: String,
    pub language: String,
    pub value: Option<String>,
    pub binary: Option<Vec<u8>>,
    pub children: Vec<SimpleTag>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tag {
    pub target_type_value: u64,
    pub target_type: String,
    pub track_uids: Vec<u64>,
    pub simple: Vec<SimpleTag>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chapter {
    pub uid: u64,
    pub start_ns: u64,
    pub end_ns: Option<u64>,
    pub hidden: bool,
    pub enabled: bool,
    /// (language, title)
    pub names: Vec<(String, String)>,
    pub children: Vec<Chapter>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Edition {
    pub uid: u64,
    pub hidden: bool,
    pub default: bool,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attachment {
    pub uid: u64,
    pub name: String,
    pub mime: String,
    pub description: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Matroska {
    /// "matroska" or "webm".
    pub doc_type: String,
    pub timestamp_scale: u64,
    pub duration_ns: Option<u64>,
    pub title: String,
    pub muxing_app: String,
    pub writing_app: String,
    pub tracks: Vec<Track>,
    pub packets: Vec<Packet>,
    pub tags: Vec<Tag>,
    pub editions: Vec<Edition>,
    pub attachments: Vec<Attachment>,
}

impl Default for Matroska {
    fn default() -> Self {
        Self {
            doc_type: "matroska".into(),
            timestamp_scale: 1_000_000,
            duration_ns: None,
            title: String::new(),
            muxing_app: String::new(),
            writing_app: String::new(),
            tracks: Vec::new(),
            packets: Vec::new(),
            tags: Vec::new(),
            editions: Vec::new(),
            attachments: Vec::new(),
        }
    }
}

impl Matroska {
    pub fn track(&self, number: u64) -> Option<&Track> {
        self.tracks.iter().find(|t| t.number == number)
    }
    pub fn packets_of(&self, number: u64) -> impl Iterator<Item = &Packet> {
        self.packets.iter().filter(move |p| p.track == number)
    }
}

// --------------------------------------------------------------------------------- reading

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}

/// The DocType in the EBML header (`matroska` or `webm`), without parsing the rest.
pub fn doc_type(data: &[u8]) -> Option<String> {
    if !sniff(data) {
        return None;
    }
    elements(data)
        .into_iter()
        .find(|e| e.id == id::EBML)
        .and_then(|e| elements(e.body).into_iter().find(|c| c.id == id::DOC_TYPE))
        .map(|c| text(c.body))
}

/// Whether the file declares a video track (read from the track headers only).
pub fn has_video(data: &[u8]) -> bool {
    elements(data)
        .into_iter()
        .filter(|e| e.id == id::SEGMENT)
        .flat_map(|seg| elements(seg.body))
        .filter(|e| e.id == id::TRACKS)
        .flat_map(|t| elements(t.body))
        .filter(|e| e.id == id::TRACK_ENTRY)
        .any(|t| {
            elements(t.body)
                .into_iter()
                .any(|c| c.id == id::TRACK_TYPE && uint(c.body) == 1)
        })
}

fn parse_track(body: &[u8]) -> Result<Track> {
    let mut t = Track::new(0, TrackKind::Other(0), "");
    t.language = "eng".into();
    for e in elements(body) {
        match e.id {
            id::TRACK_NUMBER => t.number = uint(e.body),
            id::TRACK_UID => t.uid = uint(e.body),
            id::TRACK_TYPE => t.kind = TrackKind::from_code(uint(e.body) as u8),
            id::FLAG_ENABLED => t.enabled = uint(e.body) != 0,
            id::FLAG_DEFAULT => t.default = uint(e.body) != 0,
            id::FLAG_FORCED => t.forced = uint(e.body) != 0,
            id::DEFAULT_DURATION => t.default_duration_ns = Some(uint(e.body)),
            id::NAME => t.name = text(e.body),
            id::LANGUAGE => t.language = text(e.body),
            id::CODEC_ID => t.codec_id = text(e.body),
            id::CODEC_PRIVATE => t.codec_private = e.body.to_vec(),
            id::CODEC_DELAY => t.codec_delay_ns = uint(e.body),
            id::SEEK_PRE_ROLL => t.seek_pre_roll_ns = uint(e.body),
            id::VIDEO => {
                let mut v = VideoInfo::default();
                for c in elements(e.body) {
                    match c.id {
                        id::PIXEL_WIDTH => v.width = uint(c.body) as u32,
                        id::PIXEL_HEIGHT => v.height = uint(c.body) as u32,
                        id::DISPLAY_WIDTH => v.display_width = Some(uint(c.body) as u32),
                        id::DISPLAY_HEIGHT => v.display_height = Some(uint(c.body) as u32),
                        _ => {}
                    }
                }
                t.video = Some(v);
            }
            id::AUDIO => {
                let mut a = AudioInfo {
                    sample_rate: 8000.0,
                    channels: 1,
                    ..AudioInfo::default()
                };
                for c in elements(e.body) {
                    match c.id {
                        id::SAMPLING_FREQUENCY => a.sample_rate = float(c.body),
                        id::OUTPUT_SAMPLING_FREQUENCY => a.output_sample_rate = Some(float(c.body)),
                        id::CHANNELS => a.channels = uint(c.body) as u32,
                        id::BIT_DEPTH => a.bit_depth = Some(uint(c.body) as u32),
                        _ => {}
                    }
                }
                t.audio = Some(a);
            }
            id::CONTENT_ENCODINGS => {
                for enc in elements(e.body)
                    .into_iter()
                    .filter(|x| x.id == id::CONTENT_ENCODING)
                {
                    let mut kind = 0;
                    let mut algo = 0;
                    let mut settings = Vec::new();
                    for c in elements(enc.body) {
                        match c.id {
                            id::CONTENT_ENCODING_TYPE => kind = uint(c.body),
                            id::CONTENT_COMPRESSION => {
                                for d in elements(c.body) {
                                    match d.id {
                                        id::CONTENT_COMP_ALGO => algo = uint(d.body),
                                        id::CONTENT_COMP_SETTINGS => settings = d.body.to_vec(),
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if kind != 0 || algo != 3 {
                        return Err(Error::new(
                            ErrorCode::UnsupportedVariant,
                            "this Matroska track is encrypted or uses zlib/bzlib/lzo compression, which is not supported",
                        ));
                    }
                    t.stripped = settings;
                }
            }
            _ => {}
        }
    }
    Ok(t)
}

fn parse_simple_tag(body: &[u8], depth: u32) -> SimpleTag {
    let mut s = SimpleTag {
        language: "und".into(),
        ..SimpleTag::default()
    };
    for e in elements(body) {
        match e.id {
            id::TAG_NAME => s.name = text(e.body),
            id::TAG_LANGUAGE => s.language = text(e.body),
            id::TAG_STRING => {
                s.value = Some(
                    String::from_utf8_lossy(e.body)
                        .trim_end_matches('\0')
                        .to_string(),
                )
            }
            id::TAG_BINARY => s.binary = Some(e.body.to_vec()),
            id::SIMPLE_TAG if depth < 8 => s.children.push(parse_simple_tag(e.body, depth + 1)),
            _ => {}
        }
    }
    s
}

fn parse_chapter(body: &[u8], depth: u32) -> Chapter {
    let mut c = Chapter {
        enabled: true,
        ..Chapter::default()
    };
    for e in elements(body) {
        match e.id {
            id::CHAPTER_UID => c.uid = uint(e.body),
            id::CHAPTER_TIME_START => c.start_ns = uint(e.body),
            id::CHAPTER_TIME_END => c.end_ns = Some(uint(e.body)),
            id::CHAPTER_FLAG_HIDDEN => c.hidden = uint(e.body) != 0,
            id::CHAPTER_FLAG_ENABLED => c.enabled = uint(e.body) != 0,
            id::CHAPTER_DISPLAY => {
                let mut lang = "eng".to_string();
                let mut name = String::new();
                for d in elements(e.body) {
                    match d.id {
                        id::CHAP_STRING => name = text(d.body),
                        id::CHAP_LANGUAGE => lang = text(d.body),
                        _ => {}
                    }
                }
                c.names.push((lang, name));
            }
            id::CHAPTER_ATOM if depth < 8 => c.children.push(parse_chapter(e.body, depth + 1)),
            _ => {}
        }
    }
    c
}

struct Block<'a> {
    track: u64,
    rel: i16,
    flags: u8,
    frames: Vec<&'a [u8]>,
}

fn parse_block(b: &[u8]) -> Result<Block<'_>> {
    let bad = || Error::corrupt("damaged Matroska block");
    let mut o = 0usize;
    let track = read_size(b, &mut o).flatten().ok_or_else(bad)?;
    let rel = i16::from_be_bytes([*b.get(o).ok_or_else(bad)?, *b.get(o + 1).ok_or_else(bad)?]);
    let flags = *b.get(o + 2).ok_or_else(bad)?;
    o += 3;
    let lacing = (flags >> 1) & 3;
    let mut frames = Vec::new();
    if lacing == 0 {
        frames.push(&b[o..]);
    } else {
        let count = usize::from(*b.get(o).ok_or_else(bad)?) + 1;
        o += 1;
        let mut sizes = Vec::with_capacity(count);
        match lacing {
            1 => {
                for _ in 0..count - 1 {
                    let mut n = 0usize;
                    loop {
                        let x = *b.get(o).ok_or_else(bad)?;
                        o += 1;
                        n += usize::from(x);
                        if x != 255 {
                            break;
                        }
                    }
                    sizes.push(n);
                }
            }
            3 => {
                let first = read_size(b, &mut o).flatten().ok_or_else(bad)? as i64;
                sizes.push(first as usize);
                let mut prev = first;
                for _ in 1..count - 1 {
                    let d = read_svint(b, &mut o).ok_or_else(bad)?;
                    prev += d;
                    if prev < 0 {
                        return Err(bad());
                    }
                    sizes.push(prev as usize);
                }
            }
            _ => {
                let total = b.len().checked_sub(o).ok_or_else(bad)?;
                if total % count != 0 {
                    return Err(bad());
                }
                for _ in 0..count - 1 {
                    sizes.push(total / count);
                }
            }
        }
        let used: usize = sizes.iter().sum();
        let rest = b
            .len()
            .checked_sub(o)
            .and_then(|r| r.checked_sub(used))
            .ok_or_else(bad)?;
        sizes.push(rest);
        for s in sizes {
            frames.push(b.get(o..o + s).ok_or_else(bad)?);
            o += s;
        }
    }
    Ok(Block {
        track,
        rel,
        flags,
        frames,
    })
}

struct ClusterCtx<'a> {
    scale: u64,
    out: &'a mut Vec<Packet>,
    tracks: &'a [Track],
    limits: &'a Limits,
}

fn parse_cluster(body: &[u8], cx: &mut ClusterCtx<'_>) -> Result<()> {
    let mut cluster_ts = 0i64;
    let scale = cx.scale as i64;
    let push = |cx: &mut ClusterCtx<'_>,
                cluster_ts: i64,
                blk: Block<'_>,
                dur: Option<u64>,
                keyframe: bool,
                discard: i64,
                in_group: bool| {
        let _ = in_group;
        let track = cx.tracks.iter().find(|t| t.number == blk.track);
        let default_dur = track.and_then(|t| t.default_duration_ns);
        let stripped = track.map(|t| t.stripped.as_slice()).unwrap_or(&[]);
        let n = blk.frames.len();
        for (i, f) in blk.frames.into_iter().enumerate() {
            let pts = (cluster_ts + i64::from(blk.rel)) * scale
                + default_dur.unwrap_or(0) as i64 * i as i64;
            let mut data = Vec::with_capacity(stripped.len() + f.len());
            data.extend_from_slice(stripped);
            data.extend_from_slice(f);
            cx.out.push(Packet {
                track: blk.track,
                pts_ns: pts,
                duration_ns: if n == 1 {
                    dur.or(default_dur)
                } else {
                    default_dur
                },
                keyframe,
                discard_padding_ns: if i + 1 == n { discard } else { 0 },
                data,
            });
        }
    };
    for e in elements(body) {
        if cx.out.len() as u64 > cx.limits.max_entries {
            return Err(Error::new(
                ErrorCode::LimitExceeded,
                "too many packets in this Matroska file",
            ));
        }
        match e.id {
            id::TIMESTAMP => cluster_ts = uint(e.body) as i64,
            id::SIMPLE_BLOCK => {
                if let Ok(blk) = parse_block(e.body) {
                    let key = blk.flags & 0x80 != 0;
                    push(cx, cluster_ts, blk, None, key, 0, false);
                }
            }
            id::BLOCK_GROUP => {
                let mut block = None;
                let mut dur = None;
                let mut refs = 0;
                let mut discard = 0i64;
                for c in elements(e.body) {
                    match c.id {
                        id::BLOCK => block = parse_block(c.body).ok(),
                        id::BLOCK_DURATION => dur = Some(uint(c.body) * cx.scale),
                        id::REFERENCE_BLOCK => refs += 1,
                        id::DISCARD_PADDING => discard = sint(c.body),
                        _ => {}
                    }
                }
                if let Some(blk) = block {
                    push(cx, cluster_ts, blk, dur, refs == 0, discard, true);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn parse(data: &[u8], limits: &Limits) -> Result<Matroska> {
    if !sniff(data) {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "not a Matroska or WebM file",
        ));
    }
    let mut m = Matroska::default();
    let top = elements(data);
    for e in &top {
        if e.id == id::EBML {
            for c in elements(e.body) {
                if c.id == id::DOC_TYPE {
                    m.doc_type = text(c.body);
                }
            }
        }
    }
    if m.doc_type != "matroska" && m.doc_type != "webm" {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            format!("EBML document type '{}' is not Matroska", m.doc_type),
        ));
    }
    let seg = top
        .iter()
        .find(|e| e.id == id::SEGMENT)
        .ok_or_else(|| Error::corrupt("Matroska file without a Segment"))?;
    // Level-1 loop. Clusters of unknown size end where the next level-1 element starts.
    let level1 = [
        id::CLUSTER,
        id::CUES,
        id::TAGS,
        id::CHAPTERS,
        id::ATTACHMENTS,
        id::SEEK_HEAD,
        id::INFO,
        id::TRACKS,
    ];
    let b = seg.body;
    let mut o = 0usize;
    let mut clusters: Vec<&[u8]> = Vec::new();
    while o < b.len() {
        let start = o;
        let Some(eid) = read_id(b, &mut o) else { break };
        let Some(size) = read_size(b, &mut o) else {
            break;
        };
        let end = match size {
            Some(n) => (o as u64).saturating_add(n).min(b.len() as u64) as usize,
            None => {
                // Scan forward for the next level-1 ID at an element boundary of the cluster.
                let mut p = o;
                let mut found = b.len();
                while p < b.len() {
                    let at = p;
                    let Some(cid) = read_id(b, &mut p) else { break };
                    let Some(csz) = read_size(b, &mut p) else {
                        break;
                    };
                    if level1.contains(&cid) {
                        found = at;
                        break;
                    }
                    match csz {
                        Some(n) => p = (p as u64).saturating_add(n).min(b.len() as u64) as usize,
                        None => break,
                    }
                }
                found
            }
        };
        let body = &b[o..end];
        o = end.max(start + 1);
        match eid {
            id::INFO => {
                for c in elements(body) {
                    match c.id {
                        id::TIMESTAMP_SCALE => m.timestamp_scale = uint(c.body).max(1),
                        id::DURATION => m.duration_ns = Some(float(c.body) as u64),
                        id::MUXING_APP => m.muxing_app = text(c.body),
                        id::WRITING_APP => m.writing_app = text(c.body),
                        id::TITLE => m.title = text(c.body),
                        _ => {}
                    }
                }
            }
            id::TRACKS => {
                for c in elements(body)
                    .into_iter()
                    .filter(|c| c.id == id::TRACK_ENTRY)
                {
                    m.tracks.push(parse_track(c.body)?);
                    if m.tracks.len() as u64 > limits.max_entries {
                        return Err(Error::new(
                            ErrorCode::LimitExceeded,
                            "too many Matroska tracks",
                        ));
                    }
                }
            }
            id::TAGS => {
                for c in elements(body).into_iter().filter(|c| c.id == id::TAG) {
                    let mut t = Tag {
                        target_type_value: 50,
                        ..Tag::default()
                    };
                    for d in elements(c.body) {
                        match d.id {
                            id::TARGETS => {
                                for x in elements(d.body) {
                                    match x.id {
                                        id::TARGET_TYPE_VALUE => t.target_type_value = uint(x.body),
                                        id::TARGET_TYPE => t.target_type = text(x.body),
                                        id::TAG_TRACK_UID => t.track_uids.push(uint(x.body)),
                                        _ => {}
                                    }
                                }
                            }
                            id::SIMPLE_TAG => t.simple.push(parse_simple_tag(d.body, 0)),
                            _ => {}
                        }
                    }
                    m.tags.push(t);
                }
            }
            id::CHAPTERS => {
                for c in elements(body)
                    .into_iter()
                    .filter(|c| c.id == id::EDITION_ENTRY)
                {
                    let mut ed = Edition::default();
                    for d in elements(c.body) {
                        match d.id {
                            id::EDITION_UID => ed.uid = uint(d.body),
                            id::EDITION_FLAG_HIDDEN => ed.hidden = uint(d.body) != 0,
                            id::EDITION_FLAG_DEFAULT => ed.default = uint(d.body) != 0,
                            id::CHAPTER_ATOM => ed.chapters.push(parse_chapter(d.body, 0)),
                            _ => {}
                        }
                    }
                    m.editions.push(ed);
                }
            }
            id::ATTACHMENTS => {
                for c in elements(body)
                    .into_iter()
                    .filter(|c| c.id == id::ATTACHED_FILE)
                {
                    let mut a = Attachment::default();
                    for d in elements(c.body) {
                        match d.id {
                            id::FILE_NAME => a.name = text(d.body),
                            id::FILE_MIME_TYPE => a.mime = text(d.body),
                            id::FILE_DESCRIPTION => a.description = text(d.body),
                            id::FILE_UID => a.uid = uint(d.body),
                            id::FILE_DATA => a.data = d.body.to_vec(),
                            _ => {}
                        }
                    }
                    m.attachments.push(a);
                }
            }
            id::CLUSTER => clusters.push(body),
            _ => {}
        }
    }
    let tracks = m.tracks.clone();
    let mut packets = Vec::new();
    for body in clusters {
        let mut cx = ClusterCtx {
            scale: m.timestamp_scale,
            out: &mut packets,
            tracks: &tracks,
            limits,
        };
        parse_cluster(body, &mut cx)?;
    }
    // File order is decode order; B-frame video is not in presentation order, so no sorting.
    m.packets = packets;
    Ok(m)
}

// --------------------------------------------------------------------------------- writing

fn put_id(out: &mut Vec<u8>, id: u32) {
    let n = if id > 0xFF_FFFF {
        4
    } else if id > 0xFFFF {
        3
    } else if id > 0xFF {
        2
    } else {
        1
    };
    out.extend_from_slice(&id.to_be_bytes()[4 - n..]);
}

fn size_bytes(n: u64) -> usize {
    let mut len = 1;
    while len < 8 && n >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    len
}

fn put_size(out: &mut Vec<u8>, n: u64) {
    let len = size_bytes(n);
    let marked = n | (1u64 << (7 * len));
    out.extend_from_slice(&marked.to_be_bytes()[8 - len..]);
}

/// A size that always takes 8 bytes (so it can be patched in place).
fn put_size8(out: &mut Vec<u8>, n: u64) {
    out.push(0x01);
    out.extend_from_slice(&n.to_be_bytes()[1..]);
}

fn el(out: &mut Vec<u8>, id: u32, body: &[u8]) {
    put_id(out, id);
    put_size(out, body.len() as u64);
    out.extend_from_slice(body);
}

fn el_uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let n = ((64 - v.leading_zeros()).div_ceil(8)).max(1) as usize;
    el(out, id, &v.to_be_bytes()[8 - n..]);
}

fn el_sint(out: &mut Vec<u8>, id: u32, v: i64) {
    let mut n = 8;
    while n > 1 {
        let hi = (v >> (8 * (n - 1) - 1)) & 0x1FF;
        if hi == 0 || hi == 0x1FF {
            n -= 1;
        } else {
            break;
        }
    }
    el(out, id, &v.to_be_bytes()[8 - n..]);
}

fn el_float(out: &mut Vec<u8>, id: u32, v: f64) {
    el(out, id, &v.to_be_bytes());
}

fn el_str(out: &mut Vec<u8>, id: u32, s: &str) {
    el(out, id, s.as_bytes());
}

fn write_simple_tag(out: &mut Vec<u8>, s: &SimpleTag) {
    let mut b = Vec::new();
    el_str(&mut b, id::TAG_NAME, &s.name);
    el_str(
        &mut b,
        id::TAG_LANGUAGE,
        if s.language.is_empty() {
            "und"
        } else {
            &s.language
        },
    );
    el_uint(&mut b, id::TAG_DEFAULT, 1);
    if let Some(v) = &s.value {
        el_str(&mut b, id::TAG_STRING, v);
    }
    if let Some(v) = &s.binary {
        el(&mut b, id::TAG_BINARY, v);
    }
    for c in &s.children {
        write_simple_tag(&mut b, c);
    }
    el(out, id::SIMPLE_TAG, &b);
}

fn write_chapter(out: &mut Vec<u8>, c: &Chapter) {
    let mut b = Vec::new();
    el_uint(&mut b, id::CHAPTER_UID, c.uid);
    el_uint(&mut b, id::CHAPTER_TIME_START, c.start_ns);
    if let Some(e) = c.end_ns {
        el_uint(&mut b, id::CHAPTER_TIME_END, e);
    }
    el_uint(&mut b, id::CHAPTER_FLAG_HIDDEN, u64::from(c.hidden));
    el_uint(&mut b, id::CHAPTER_FLAG_ENABLED, u64::from(c.enabled));
    for (lang, name) in &c.names {
        let mut d = Vec::new();
        el_str(&mut d, id::CHAP_STRING, name);
        el_str(&mut d, id::CHAP_LANGUAGE, lang);
        el(&mut b, id::CHAPTER_DISPLAY, &d);
    }
    for k in &c.children {
        write_chapter(&mut b, k);
    }
    el(out, id::CHAPTER_ATOM, &b);
}

fn write_track(out: &mut Vec<u8>, t: &Track) {
    let mut b = Vec::new();
    el_uint(&mut b, id::TRACK_NUMBER, t.number);
    el_uint(&mut b, id::TRACK_UID, t.uid.max(1));
    el_uint(&mut b, id::TRACK_TYPE, u64::from(t.kind.code()));
    el_uint(&mut b, id::FLAG_ENABLED, u64::from(t.enabled));
    el_uint(&mut b, id::FLAG_DEFAULT, u64::from(t.default));
    el_uint(&mut b, id::FLAG_FORCED, u64::from(t.forced));
    if let Some(d) = t.default_duration_ns {
        el_uint(&mut b, id::DEFAULT_DURATION, d);
    }
    if !t.name.is_empty() {
        el_str(&mut b, id::NAME, &t.name);
    }
    el_str(&mut b, id::LANGUAGE, &t.language);
    el_str(&mut b, id::CODEC_ID, &t.codec_id);
    if !t.codec_private.is_empty() {
        el(&mut b, id::CODEC_PRIVATE, &t.codec_private);
    }
    if t.codec_delay_ns > 0 {
        el_uint(&mut b, id::CODEC_DELAY, t.codec_delay_ns);
    }
    if t.seek_pre_roll_ns > 0 {
        el_uint(&mut b, id::SEEK_PRE_ROLL, t.seek_pre_roll_ns);
    }
    if let Some(v) = &t.video {
        let mut c = Vec::new();
        el_uint(&mut c, id::PIXEL_WIDTH, u64::from(v.width));
        el_uint(&mut c, id::PIXEL_HEIGHT, u64::from(v.height));
        if let Some(w) = v.display_width {
            el_uint(&mut c, id::DISPLAY_WIDTH, u64::from(w));
        }
        if let Some(h) = v.display_height {
            el_uint(&mut c, id::DISPLAY_HEIGHT, u64::from(h));
        }
        el(&mut b, id::VIDEO, &c);
    }
    if let Some(a) = &t.audio {
        let mut c = Vec::new();
        el_float(&mut c, id::SAMPLING_FREQUENCY, a.sample_rate);
        if let Some(o) = a.output_sample_rate {
            el_float(&mut c, id::OUTPUT_SAMPLING_FREQUENCY, o);
        }
        el_uint(&mut c, id::CHANNELS, u64::from(a.channels));
        if let Some(d) = a.bit_depth {
            el_uint(&mut c, id::BIT_DEPTH, u64::from(d));
        }
        el(&mut b, id::AUDIO, &c);
    }
    el(out, id::TRACK_ENTRY, &b);
}

#[derive(Debug, Clone)]
pub struct WriteOptions {
    /// Longest span of one cluster, in nanoseconds (blocks hold 16-bit offsets in ticks).
    pub cluster_ns: i64,
    /// Write Cues (an index for seeking).
    pub cues: bool,
    pub writing_app: String,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            cluster_ns: 5_000_000_000,
            cues: true,
            writing_app: "OpenConverter".into(),
        }
    }
}

fn block_bytes(track: u64, rel: i16, flags: u8, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(data.len() + 12);
    put_size(&mut b, track);
    b.extend_from_slice(&rel.to_be_bytes());
    b.push(flags);
    b.extend_from_slice(data);
    b
}

/// Serialise a file. Packets are written in the order given (decode order); each cluster starts at a keyframe of
/// the first video track (or after `cluster_ns` for audio-only files).
/// A cluster being filled while writing: its start time, body, and the cue points inside it.
type OpenCluster = (i64, Vec<u8>, Vec<(u64, i64)>);

pub fn write(m: &Matroska, opts: &WriteOptions) -> Result<Vec<u8>> {
    if m.tracks.is_empty() {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "a Matroska file needs at least one track",
        ));
    }
    let scale = m.timestamp_scale.max(1);
    // Blocks go out in the order given: that is decode order, which differs from presentation
    // order when a video track has B-frames. Callers pass them interleaved.
    let packets: Vec<&Packet> = m.packets.iter().collect();
    let video = m
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .map(|t| t.number);

    // Header
    let mut head = Vec::new();
    {
        let mut b = Vec::new();
        el_uint(&mut b, 0x4286, 1);
        el_uint(&mut b, 0x42F7, 1);
        el_uint(&mut b, 0x42F2, 4);
        el_uint(&mut b, 0x42F3, 8);
        el_str(&mut b, id::DOC_TYPE, &m.doc_type);
        el_uint(&mut b, 0x4287, 4);
        el_uint(&mut b, 0x4285, 2);
        el(&mut head, id::EBML, &b);
    }

    let mut info = Vec::new();
    el_uint(&mut info, id::TIMESTAMP_SCALE, scale);
    let dur = m.duration_ns.or_else(|| {
        packets
            .iter()
            .map(|p| p.pts_ns.max(0) as u64 + p.duration_ns.unwrap_or(0))
            .max()
    });
    if let Some(d) = dur {
        el_float(&mut info, id::DURATION, d as f64 / scale as f64);
    }
    el_str(&mut info, id::MUXING_APP, &opts.writing_app);
    el_str(&mut info, id::WRITING_APP, &opts.writing_app);
    if !m.title.is_empty() {
        el_str(&mut info, id::TITLE, &m.title);
    }
    let mut info_el = Vec::new();
    el(&mut info_el, id::INFO, &info);

    let mut tracks = Vec::new();
    for t in &m.tracks {
        write_track(&mut tracks, t);
    }
    let mut tracks_el = Vec::new();
    el(&mut tracks_el, id::TRACKS, &tracks);

    let mut chapters_el = Vec::new();
    if !m.editions.is_empty() {
        let mut eds = Vec::new();
        for ed in &m.editions {
            let mut b = Vec::new();
            el_uint(&mut b, id::EDITION_UID, ed.uid.max(1));
            el_uint(&mut b, id::EDITION_FLAG_HIDDEN, u64::from(ed.hidden));
            el_uint(&mut b, id::EDITION_FLAG_DEFAULT, u64::from(ed.default));
            for c in &ed.chapters {
                write_chapter(&mut b, c);
            }
            el(&mut eds, id::EDITION_ENTRY, &b);
        }
        el(&mut chapters_el, id::CHAPTERS, &eds);
    }

    let mut tags_el = Vec::new();
    if !m.tags.is_empty() {
        let mut ts = Vec::new();
        for t in &m.tags {
            let mut b = Vec::new();
            let mut tg = Vec::new();
            el_uint(&mut tg, id::TARGET_TYPE_VALUE, t.target_type_value.max(1));
            if !t.target_type.is_empty() {
                el_str(&mut tg, id::TARGET_TYPE, &t.target_type);
            }
            for u in &t.track_uids {
                el_uint(&mut tg, id::TAG_TRACK_UID, *u);
            }
            el(&mut b, id::TARGETS, &tg);
            for s in &t.simple {
                write_simple_tag(&mut b, s);
            }
            el(&mut ts, id::TAG, &b);
        }
        el(&mut tags_el, id::TAGS, &ts);
    }

    let mut att_el = Vec::new();
    if !m.attachments.is_empty() {
        let mut at = Vec::new();
        for (i, a) in m.attachments.iter().enumerate() {
            let mut b = Vec::new();
            if !a.description.is_empty() {
                el_str(&mut b, id::FILE_DESCRIPTION, &a.description);
            }
            el_str(&mut b, id::FILE_NAME, &a.name);
            el_str(&mut b, id::FILE_MIME_TYPE, &a.mime);
            el(&mut b, id::FILE_DATA, &a.data);
            el_uint(
                &mut b,
                id::FILE_UID,
                if a.uid == 0 { i as u64 + 1 } else { a.uid },
            );
            el(&mut at, id::ATTACHED_FILE, &b);
        }
        el(&mut att_el, id::ATTACHMENTS, &at);
    }

    // Clusters, remembering where each starts (relative to the segment body) for the Cues.
    struct ClusterInfo {
        time_ticks: i64,
        body: Vec<u8>,
        // (track, time ticks) of blocks worth indexing
        cue_hits: Vec<(u64, i64)>,
    }
    let mut clusters: Vec<ClusterInfo> = Vec::new();
    let mut cur: Option<OpenCluster> = None;
    let cue_track = video
        .or_else(|| m.tracks.first().map(|t| t.number))
        .unwrap_or(1);
    let mut last_cue_ns = i64::MIN / 2;
    let flush = |cur: &mut Option<OpenCluster>, clusters: &mut Vec<ClusterInfo>| {
        if let Some((t, mut body, hits)) = cur.take() {
            let mut full = Vec::new();
            el_uint(&mut full, id::TIMESTAMP, t as u64);
            full.append(&mut body);
            clusters.push(ClusterInfo {
                time_ticks: t,
                body: full,
                cue_hits: hits,
            });
        }
    };
    for p in &packets {
        let ticks = div_round(p.pts_ns, scale as i64);
        let key = p.keyframe;
        let starts_new = match &cur {
            None => true,
            Some((t, ..)) => {
                let span_ok =
                    (ticks - *t) * (scale as i64) < opts.cluster_ns && (ticks - *t) < 32_000;
                let video_key = video == Some(p.track) && key && (ticks - *t) > 0;
                !span_ok || video_key
            }
        };
        if starts_new {
            flush(&mut cur, &mut clusters);
            cur = Some((ticks.max(0), Vec::new(), Vec::new()));
        }
        let (ct, body, hits) = cur
            .as_mut()
            .ok_or_else(|| Error::corrupt("cluster state"))?;
        let rel = (ticks - *ct).clamp(-32768, 32767) as i16;
        let needs_group = p.duration_ns.is_some_and(|_| {
            m.track(p.track)
                .is_some_and(|t| t.kind == TrackKind::Subtitle)
        }) || p.discard_padding_ns != 0;
        if needs_group {
            let mut g = Vec::new();
            el(&mut g, id::BLOCK, &block_bytes(p.track, rel, 0, &p.data));
            if let Some(d) = p.duration_ns {
                el_uint(
                    &mut g,
                    id::BLOCK_DURATION,
                    div_round(d as i64, scale as i64) as u64,
                );
            }
            if !key {
                el_sint(&mut g, id::REFERENCE_BLOCK, -1);
            }
            if p.discard_padding_ns != 0 {
                el_sint(&mut g, id::DISCARD_PADDING, p.discard_padding_ns);
            }
            el(body, id::BLOCK_GROUP, &g);
        } else {
            el(
                body,
                id::SIMPLE_BLOCK,
                &block_bytes(p.track, rel, if key { 0x80 } else { 0 }, &p.data),
            );
        }
        let cue_here = p.track == cue_track && (key || video != Some(cue_track));
        if cue_here && p.pts_ns - last_cue_ns >= if video.is_some() { 0 } else { 1_000_000_000 } {
            hits.push((p.track, ticks));
            last_cue_ns = p.pts_ns;
        }
    }
    flush(&mut cur, &mut clusters);

    // Layout: SeekHead, Info, Tracks, Chapters, Tags, Attachments, Clusters, Cues.
    // Positions are relative to the start of the Segment body.
    let seek_entry = |target: u32, pos: u64| {
        let mut b = Vec::new();
        let mut idb = Vec::new();
        put_id(&mut idb, target);
        el(&mut b, id::SEEK_ID, &idb);
        el(&mut b, id::SEEK_POSITION, &pos.to_be_bytes()[4..]);
        let mut e = Vec::new();
        el(&mut e, id::SEEK, &b);
        e
    };
    let targets: Vec<(u32, bool)> = vec![
        (id::INFO, true),
        (id::TRACKS, true),
        (id::CHAPTERS, !chapters_el.is_empty()),
        (id::TAGS, !tags_el.is_empty()),
        (id::ATTACHMENTS, !att_el.is_empty()),
        (id::CUES, opts.cues),
    ];
    let seek_len: usize = targets
        .iter()
        .filter(|(_, on)| *on)
        .map(|(t, _)| seek_entry(*t, 0).len())
        .sum();
    let mut seek_head_len = 0;
    {
        let mut tmp = Vec::new();
        put_id(&mut tmp, id::SEEK_HEAD);
        put_size(&mut tmp, seek_len as u64);
        seek_head_len += tmp.len() + seek_len;
    }
    let mut pos = seek_head_len as u64;
    let info_pos = pos;
    pos += info_el.len() as u64;
    let tracks_pos = pos;
    pos += tracks_el.len() as u64;
    let chapters_pos = pos;
    pos += chapters_el.len() as u64;
    let tags_pos = pos;
    pos += tags_el.len() as u64;
    let att_pos = pos;
    pos += att_el.len() as u64;
    let mut cluster_pos = Vec::with_capacity(clusters.len());
    let mut cluster_els: Vec<Vec<u8>> = Vec::with_capacity(clusters.len());
    for c in &clusters {
        cluster_pos.push(pos);
        let mut e = Vec::new();
        el(&mut e, id::CLUSTER, &c.body);
        pos += e.len() as u64;
        cluster_els.push(e);
    }
    let cues_pos = pos;
    let mut cues_el = Vec::new();
    if opts.cues {
        let mut cp = Vec::new();
        for (i, c) in clusters.iter().enumerate() {
            for (track, ticks) in &c.cue_hits {
                let mut b = Vec::new();
                el_uint(&mut b, id::CUE_TIME, (*ticks).max(0) as u64);
                let mut tp = Vec::new();
                el_uint(&mut tp, id::CUE_TRACK, *track);
                el_uint(
                    &mut tp,
                    id::CUE_CLUSTER_POSITION,
                    cluster_pos[i] - seek_head_len as u64 + seek_head_len as u64,
                );
                el(&mut b, id::CUE_TRACK_POSITIONS, &tp);
                el(&mut cp, id::CUE_POINT, &b);
            }
            let _ = c.time_ticks;
        }
        el(&mut cues_el, id::CUES, &cp);
    }
    let mut seek = Vec::new();
    for (t, on) in &targets {
        if !on {
            continue;
        }
        let p = match *t {
            x if x == id::INFO => info_pos,
            x if x == id::TRACKS => tracks_pos,
            x if x == id::CHAPTERS => chapters_pos,
            x if x == id::TAGS => tags_pos,
            x if x == id::ATTACHMENTS => att_pos,
            _ => cues_pos,
        };
        seek.extend(seek_entry(*t, p));
    }
    let mut seek_head = Vec::new();
    el(&mut seek_head, id::SEEK_HEAD, &seek);
    debug_assert_eq!(seek_head.len(), seek_head_len);

    let mut body = Vec::new();
    body.extend(seek_head);
    body.extend(info_el);
    body.extend(tracks_el);
    body.extend(chapters_el);
    body.extend(tags_el);
    body.extend(att_el);
    for c in cluster_els {
        body.extend(c);
    }
    body.extend(cues_el);

    let mut out = head;
    put_id(&mut out, id::SEGMENT);
    put_size8(&mut out, body.len() as u64);
    out.extend(body);
    Ok(out)
}

fn div_round(a: i64, b: i64) -> i64 {
    if a >= 0 {
        (a + b / 2) / b
    } else {
        -((-a + b / 2) / b)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample() -> Matroska {
        let mut m = Matroska {
            title: "T".into(),
            ..Matroska::default()
        };
        let mut a = Track::new(1, TrackKind::Audio, "A_OPUS");
        a.audio = Some(AudioInfo {
            sample_rate: 48000.0,
            channels: 2,
            ..AudioInfo::default()
        });
        a.codec_private = vec![1, 2, 3];
        m.tracks.push(a);
        let mut v = Track::new(2, TrackKind::Video, "V_VP8");
        v.video = Some(VideoInfo {
            width: 64,
            height: 48,
            ..VideoInfo::default()
        });
        m.tracks.push(v);
        for i in 0..300i64 {
            m.packets.push(Packet {
                track: 1,
                pts_ns: i * 20_000_000,
                duration_ns: Some(20_000_000),
                keyframe: true,
                discard_padding_ns: 0,
                data: vec![i as u8; 10 + (i % 7) as usize],
            });
            if i % 2 == 0 {
                m.packets.push(Packet {
                    track: 2,
                    pts_ns: i * 20_000_000,
                    duration_ns: None,
                    keyframe: i % 50 == 0,
                    discard_padding_ns: 0,
                    data: vec![0xAA; 100],
                });
            }
        }
        m.tags.push(Tag {
            target_type_value: 30,
            simple: vec![SimpleTag {
                name: "TITLE".into(),
                language: "und".into(),
                value: Some("Song".into()),
                ..SimpleTag::default()
            }],
            ..Tag::default()
        });
        m.attachments.push(Attachment {
            uid: 5,
            name: "cover.jpg".into(),
            mime: "image/jpeg".into(),
            description: String::new(),
            data: vec![9; 33],
        });
        m.editions.push(Edition {
            uid: 1,
            chapters: vec![Chapter {
                uid: 7,
                start_ns: 0,
                enabled: true,
                names: vec![("eng".into(), "One".into())],
                ..Chapter::default()
            }],
            ..Edition::default()
        });
        m
    }

    #[test]
    fn write_then_parse_round_trips() {
        let m = sample();
        let bytes = write(&m, &WriteOptions::default()).unwrap();
        let back = parse(&bytes, &Limits::default()).unwrap();
        assert_eq!(back.tracks.len(), 2);
        assert_eq!(back.tracks[0].codec_private, vec![1, 2, 3]);
        assert_eq!(back.packets.len(), m.packets.len());
        let a: Vec<_> = back
            .packets_of(1)
            .map(|p| (p.pts_ns, p.data.clone()))
            .collect();
        let b: Vec<_> = m
            .packets_of(1)
            .map(|p| (p.pts_ns, p.data.clone()))
            .collect();
        assert_eq!(a, b);
        assert_eq!(back.tags[0].simple[0].value.as_deref(), Some("Song"));
        assert_eq!(back.attachments[0].data, vec![9; 33]);
        assert_eq!(back.editions[0].chapters[0].names[0].1, "One");
        assert!(back.packets_of(2).filter(|p| p.keyframe).count() >= 3);
    }

    #[test]
    fn laced_blocks_and_unknown_size_clusters() {
        // Hand-built: EBML header, Segment (unknown size) with Tracks and an unknown-size cluster
        // holding a Xiph-laced block of three frames.
        let mut t = Vec::new();
        let mut e = Vec::new();
        el_uint(&mut e, id::TRACK_NUMBER, 1);
        el_uint(&mut e, id::TRACK_TYPE, 2);
        el_str(&mut e, id::CODEC_ID, "A_PCM/INT/LIT");
        el(&mut t, id::TRACK_ENTRY, &e);
        let mut tracks = Vec::new();
        el(&mut tracks, id::TRACKS, &t);
        let mut blk = vec![0x81, 0, 0, 0x02, 2, 3, 4];
        blk.extend_from_slice(&[1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 3]);
        // lace sizes 3, 4 then 5 (rest): total payload 12
        let mut cl = Vec::new();
        el_uint(&mut cl, id::TIMESTAMP, 10);
        el(&mut cl, id::SIMPLE_BLOCK, &blk);
        let mut seg = tracks;
        put_id(&mut seg, id::CLUSTER);
        seg.push(0xFF); // unknown size
        seg.extend(cl);
        let mut head = Vec::new();
        let mut b = Vec::new();
        el_str(&mut b, id::DOC_TYPE, "matroska");
        el(&mut head, id::EBML, &b);
        put_id(&mut head, id::SEGMENT);
        head.push(0xFF);
        head.extend(seg);
        let m = parse(&head, &Limits::default()).unwrap();
        assert_eq!(m.packets.len(), 3);
        assert_eq!(m.packets[0].data, vec![1, 1, 1]);
        assert_eq!(m.packets[1].data, vec![2, 2, 2, 2]);
        assert_eq!(m.packets[2].data, vec![3, 3, 3, 3, 3]);
        assert_eq!(m.packets[0].pts_ns, 10_000_000);
    }

    #[test]
    fn damaged_files_never_panic() {
        let bytes = write(&sample(), &WriteOptions::default()).unwrap();
        let mut seed = 99u32;
        for _ in 0..300 {
            let mut b = bytes.clone();
            for _ in 0..4 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let i = (seed >> 8) as usize % b.len();
                b[i] ^= (seed >> 3) as u8 | 1;
            }
            let cut = (seed as usize >> 4) % b.len();
            let _ = parse(&b[..cut.max(8)], &Limits::default());
            let _ = parse(&b, &Limits::default());
        }
    }
}
