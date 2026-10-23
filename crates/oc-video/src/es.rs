//! Elementary-stream helpers shared by the broadcast containers (MPEG-TS, MPEG-PS):
//! Annex B NAL handling, parameter-set parsing, and frame splitting for ADTS, MPEG audio
//! and AC-3. Everything here works on bytes in memory and never touches a container.
use crate::{codec, Kind, Packet, Res, Stream};

/// One packetised-elementary-stream unit with 90 kHz timestamps (already unwrapped).
#[derive(Debug, Clone)]
pub struct Pes {
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub data: Vec<u8>,
}

/// One elementary stream pulled out of a transport: its codec and its PES units in order.
#[derive(Debug, Clone)]
pub struct Es {
    pub codec: &'static str,
    pub language: String,
    pub pes: Vec<Pes>,
}

pub(crate) struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    pub fn bit(&mut self) -> Option<u32> {
        let byte = *self.b.get(self.pos / 8)?;
        let v = u32::from((byte >> (7 - self.pos % 8)) & 1);
        self.pos += 1;
        Some(v)
    }

    pub fn bits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.pos += n;
        (self.pos <= self.b.len() * 8).then_some(())
    }

    pub fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 32 {
                return None;
            }
        }
        let rest = if zeros == 0 { 0 } else { self.bits(zeros)? };
        Some(((1u64 << zeros) - 1 + u64::from(rest)).min(u64::from(u32::MAX)) as u32)
    }

    pub fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        let m = (k as i64 + 1) / 2;
        Some(if k % 2 == 1 { m as i32 } else { -m as i32 })
    }
}

/// Removes emulation-prevention bytes (`00 00 03` becomes `00 00`).
pub fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// The NAL units of an Annex B byte stream, without start codes.
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |n| n - 3);
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

/// NAL units of a length-prefixed access unit.
pub fn split_avcc(data: &[u8], len_size: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + len_size <= data.len() {
        let n = data[i..i + len_size]
            .iter()
            .fold(0usize, |a, &b| (a << 8) | usize::from(b));
        i += len_size;
        if n == 0 || i + n > data.len() {
            break;
        }
        out.push(&data[i..i + n]);
        i += n;
    }
    out
}

#[derive(Debug, Default, Clone)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
}

pub fn h264_sps(nal: &[u8]) -> Option<VideoInfo> {
    let rbsp = unescape(nal.get(1..)?);
    let mut r = Bits::new(&rbsp);
    let profile = r.bits(8)?;
    r.skip(16)?;
    r.ue()?;
    let mut chroma = 1;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?;
        }
        r.ue()?;
        r.ue()?;
        r.bit()?;
        if r.bit()? == 1 {
            for i in 0..(if chroma == 3 { 12 } else { 8 }) {
                if r.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + r.se()? + 256) % 256;
                        }
                        last = if next == 0 { last } else { next };
                    }
                }
            }
        }
    }
    r.ue()?;
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?;
    r.bit()?;
    let w_mbs = r.ue()? + 1;
    let h_units = r.ue()? + 1;
    let frame_only = r.bit()?;
    if frame_only == 0 {
        r.bit()?;
    }
    r.bit()?;
    let (mut cl, mut cr, mut ct, mut cb) = (0, 0, 0, 0);
    if r.bit()? == 1 {
        cl = r.ue()?;
        cr = r.ue()?;
        ct = r.ue()?;
        cb = r.ue()?;
    }
    let (ux, uy) = match chroma {
        0 => (1, 2 - frame_only),
        1 => (2, 2 * (2 - frame_only)),
        2 => (2, 2 - frame_only),
        _ => (1, 2 - frame_only),
    };
    let width = (w_mbs * 16).checked_sub((cl + cr) * ux)?;
    let height = (h_units * 16 * (2 - frame_only)).checked_sub((ct + cb) * uy)?;
    Some(VideoInfo { width, height })
}

pub struct HevcSps {
    pub info: VideoInfo,
    /// general_profile_space..general_level_idc: 12 bytes copied into `hvcC`.
    pub ptl: [u8; 12],
    pub chroma: u32,
    pub depth_luma: u32,
    pub depth_chroma: u32,
    pub sub_layers: u32,
    pub nesting: u32,
}

pub fn hevc_sps(nal: &[u8]) -> Option<HevcSps> {
    let rbsp = unescape(nal.get(2..)?);
    let mut r = Bits::new(&rbsp);
    r.bits(4)?;
    let max_sub = r.bits(3)?;
    let nesting = r.bit()?;
    let mut ptl = [0u8; 12];
    for b in &mut ptl {
        *b = r.bits(8)? as u8;
    }
    let mut present = Vec::new();
    for _ in 0..max_sub {
        present.push((r.bit()?, r.bit()?));
    }
    if max_sub > 0 {
        for _ in max_sub..8 {
            r.skip(2)?;
        }
    }
    for (p, l) in present {
        if p == 1 {
            r.skip(88)?;
        }
        if l == 1 {
            r.skip(8)?;
        }
    }
    r.ue()?;
    let chroma = r.ue()?;
    if chroma == 3 {
        r.bit()?;
    }
    let mut width = r.ue()?;
    let mut height = r.ue()?;
    if r.bit()? == 1 {
        let (l, rr, t, b) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
        let (sx, sy) = match chroma {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        width = width.checked_sub((l + rr) * sx)?;
        height = height.checked_sub((t + b) * sy)?;
    }
    let depth_luma = r.ue()? + 8;
    let depth_chroma = r.ue()? + 8;
    Some(HevcSps {
        info: VideoInfo { width, height },
        ptl,
        chroma,
        depth_luma,
        depth_chroma,
        sub_layers: max_sub + 1,
        nesting,
    })
}

pub fn build_avcc(sps: &[&[u8]], pps: &[&[u8]]) -> Option<Vec<u8>> {
    let first = sps.first()?;
    if first.len() < 4 {
        return None;
    }
    let mut out = vec![
        1,
        first[1],
        first[2],
        first[3],
        0xFF,
        0xE0 | sps.len() as u8,
    ];
    for s in sps {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s);
    }
    out.push(pps.len() as u8);
    for p in pps {
        out.extend_from_slice(&(p.len() as u16).to_be_bytes());
        out.extend_from_slice(p);
    }
    if [100, 110, 122, 144].contains(&first[1]) {
        let chroma = high_profile_chroma(first).unwrap_or((1, 8, 8));
        out.extend_from_slice(&[
            0xFC | chroma.0 as u8,
            0xF8 | (chroma.1 - 8) as u8,
            0xF8 | (chroma.2 - 8) as u8,
            0,
        ]);
    }
    Some(out)
}

fn high_profile_chroma(sps: &[u8]) -> Option<(u32, u32, u32)> {
    let rbsp = unescape(sps.get(1..)?);
    let mut r = Bits::new(&rbsp);
    r.skip(24)?;
    r.ue()?;
    let chroma = r.ue()?;
    if chroma == 3 {
        r.bit()?;
    }
    Some((chroma, r.ue()? + 8, r.ue()? + 8))
}

pub fn build_hvcc(vps: &[&[u8]], sps: &[&[u8]], pps: &[&[u8]]) -> Option<(Vec<u8>, VideoInfo)> {
    let info = hevc_sps(sps.first()?)?;
    let mut out = vec![1];
    out.extend_from_slice(&info.ptl);
    out.extend_from_slice(&[0xF0, 0x00, 0xFC, 0xFC | info.chroma as u8]);
    out.push(0xF8 | (info.depth_luma - 8) as u8);
    out.push(0xF8 | (info.depth_chroma - 8) as u8);
    out.extend_from_slice(&[0, 0]);
    out.push(((info.sub_layers as u8) << 3) | ((info.nesting as u8) << 2) | 3);
    let arrays: Vec<(u8, &[&[u8]])> = [(32u8, vps), (33, sps), (34, pps)]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect();
    out.push(arrays.len() as u8);
    for (ty, list) in arrays {
        out.push(0x80 | ty);
        out.extend_from_slice(&(list.len() as u16).to_be_bytes());
        for n in list {
            out.extend_from_slice(&(n.len() as u16).to_be_bytes());
            out.extend_from_slice(n);
        }
    }
    Some((out, info.info))
}

/// Parameter sets and picture type of one video stream while its access units go by.
#[derive(Default)]
pub struct VideoScan {
    hevc: bool,
    vps: Vec<Vec<u8>>,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
}

impl VideoScan {
    pub fn new(hevc: bool) -> Self {
        Self {
            hevc,
            ..Self::default()
        }
    }

    fn kind(&self, nal: &[u8]) -> u8 {
        if self.hevc {
            (nal[0] >> 1) & 0x3F
        } else {
            nal[0] & 0x1F
        }
    }

    /// Converts one Annex B access unit to 4-byte length prefixes, drops delimiters and
    /// parameter sets already known, and reports whether it is a random-access point.
    pub fn access_unit(&mut self, annexb: &[u8]) -> Option<(Vec<u8>, bool)> {
        let mut out = Vec::with_capacity(annexb.len() + 16);
        let mut key = false;
        for nal in split_annexb(annexb) {
            let t = self.kind(nal);
            let (aud, vps, sps, pps, irap) = if self.hevc {
                (35, 32, 33, 34, (16..=21).contains(&t))
            } else {
                (9, 0xFF, 7, 8, t == 5)
            };
            if t == aud {
                continue;
            }
            let set = if t == vps {
                Some(&mut self.vps)
            } else if t == sps {
                Some(&mut self.sps)
            } else if t == pps {
                Some(&mut self.pps)
            } else {
                None
            };
            if let Some(list) = set {
                if list.iter().any(|n| n == nal) {
                    continue;
                }
                // The first set of each kind goes into the codec configuration; a changed
                // one stays in the stream where a decoder expects it.
                let first = list.is_empty();
                list.push(nal.to_vec());
                if first {
                    continue;
                }
            }
            key |= irap;
            out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            out.extend_from_slice(nal);
        }
        (!out.is_empty()).then_some((out, key))
    }

    /// The codec configuration and picture size gathered so far.
    pub fn config(&self) -> Option<(Vec<u8>, VideoInfo)> {
        fn refs(v: &[Vec<u8>]) -> Vec<&[u8]> {
            v.iter().map(|n| n.as_slice()).collect()
        }
        if self.hevc {
            build_hvcc(&refs(&self.vps), &refs(&self.sps), &refs(&self.pps))
        } else {
            let sps = refs(&self.sps);
            let info = h264_sps(sps.first()?)?;
            Some((build_avcc(&sps, &refs(&self.pps))?, info))
        }
    }
}

const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];
const AAC_CHANNELS: [u16; 8] = [0, 1, 2, 3, 4, 5, 6, 8];

pub struct Frame {
    pub offset: usize,
    pub len: usize,
    /// Where the payload (the part kept in a packet) begins inside the frame.
    pub skip: usize,
}

pub struct AudioFrames {
    pub frames: Vec<Frame>,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: u32,
    pub private: Vec<u8>,
}

pub fn adts_frames(data: &[u8]) -> Option<AudioFrames> {
    let mut frames = Vec::new();
    let mut i = 0;
    let (mut rate, mut chans, mut asc) = (0, 0, Vec::new());
    while i + 7 <= data.len() {
        if data[i] != 0xFF || data[i + 1] & 0xF6 != 0xF0 {
            i += 1;
            continue;
        }
        let hdr = if data[i + 1] & 1 == 1 { 7 } else { 9 };
        let len = (usize::from(data[i + 3] & 3) << 11)
            | (usize::from(data[i + 4]) << 3)
            | usize::from(data[i + 5] >> 5);
        if len < hdr || i + len > data.len() {
            break;
        }
        if asc.is_empty() {
            let profile = (data[i + 2] >> 6) + 1;
            let idx = (data[i + 2] >> 2) & 0xF;
            let cfg = ((data[i + 2] & 1) << 2) | (data[i + 3] >> 6);
            rate = *AAC_RATES.get(usize::from(idx))?;
            chans = AAC_CHANNELS[usize::from(cfg)];
            let v = (u16::from(profile) << 11) | (u16::from(idx) << 7) | (u16::from(cfg) << 3);
            asc = v.to_be_bytes().to_vec();
        }
        frames.push(Frame {
            offset: i,
            len,
            skip: hdr,
        });
        i += len;
    }
    (!frames.is_empty()).then_some(AudioFrames {
        frames,
        sample_rate: rate,
        channels: chans,
        samples: 1024,
        private: asc,
    })
}

/// MPEG audio layer III frames (layers I and II are not carried).
pub fn mp3_frames(data: &[u8]) -> Option<AudioFrames> {
    const V1: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let mut frames = Vec::new();
    let mut i = 0;
    let (mut rate, mut chans, mut samples) = (0, 0, 0);
    while i + 4 <= data.len() {
        if data[i] != 0xFF || data[i + 1] & 0xE0 != 0xE0 {
            i += 1;
            continue;
        }
        let ver = (data[i + 1] >> 3) & 3;
        let layer = (data[i + 1] >> 1) & 3;
        let br = usize::from(data[i + 2] >> 4);
        let sr = usize::from((data[i + 2] >> 2) & 3);
        if ver == 1 || layer != 1 || br == 0 || br == 15 || sr == 3 {
            i += 1;
            continue;
        }
        let v1 = ver == 3;
        let sample_rate = [44100, 48000, 32000][sr]
            >> (if v1 {
                0
            } else if ver == 2 {
                1
            } else {
                2
            });
        let kbps = if v1 { V1[br] } else { V2[br] };
        let pad = usize::from((data[i + 2] >> 1) & 1);
        let len = (if v1 { 144 } else { 72 }) * kbps as usize * 1000 / sample_rate as usize + pad;
        if i + len > data.len() {
            break;
        }
        if frames.is_empty() {
            rate = sample_rate;
            chans = if data[i + 3] >> 6 == 3 { 1 } else { 2 };
            samples = if v1 { 1152 } else { 576 };
        }
        frames.push(Frame {
            offset: i,
            len,
            skip: 0,
        });
        i += len;
    }
    (!frames.is_empty()).then_some(AudioFrames {
        frames,
        sample_rate: rate,
        channels: chans,
        samples,
        private: Vec::new(),
    })
}

pub fn ac3_frames(data: &[u8]) -> Option<AudioFrames> {
    const KBPS: [u32; 19] = [
        32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
    ];
    let mut frames = Vec::new();
    let mut i = 0;
    let (mut rate, mut chans) = (0, 0);
    while i + 8 <= data.len() {
        if data[i] != 0x0B || data[i + 1] != 0x77 {
            i += 1;
            continue;
        }
        let fscod = data[i + 4] >> 6;
        let code = usize::from(data[i + 4] & 0x3F);
        if fscod == 3 || code >= 38 || data[i + 5] >> 3 > 8 {
            i += 1;
            continue;
        }
        let kbps = KBPS[code / 2];
        let words = match fscod {
            0 => kbps * 2,
            1 => (kbps as f64 * 2.176_870_748_299_32) as u32 + (code as u32 & 1),
            _ => kbps * 3,
        };
        let len = words as usize * 2;
        if i + len > data.len() {
            break;
        }
        if frames.is_empty() {
            rate = [48000, 44100, 32000][usize::from(fscod)];
            let acmod = data[i + 6] >> 5;
            let mut bit = 3;
            if acmod & 1 == 1 && acmod != 1 {
                bit += 2;
            }
            if acmod & 4 != 0 {
                bit += 2;
            }
            if acmod == 2 {
                bit += 2;
            }
            let word = u16::from(data[i + 6]) << 8 | u16::from(data[i + 7]);
            let lfe = (word >> (15 - bit)) & 1;
            chans = [2u16, 1, 2, 3, 3, 4, 4, 5][usize::from(acmod)] + lfe;
        }
        frames.push(Frame {
            offset: i,
            len,
            skip: 0,
        });
        i += len;
    }
    (!frames.is_empty()).then_some(AudioFrames {
        frames,
        sample_rate: rate,
        channels: chans,
        samples: 1536,
        private: Vec::new(),
    })
}

/// Which audio codec a run of bytes holds, when the container did not say.
pub fn probe_audio(data: &[u8]) -> Option<&'static str> {
    if adts_frames(data).is_some() {
        Some(codec::AAC)
    } else if ac3_frames(data).is_some() {
        Some(codec::AC3)
    } else if mp3_frames(data).is_some() {
        Some(codec::MP3)
    } else {
        None
    }
}

fn ticks_to_ns(t: i64) -> i64 {
    crate::div_round(i128::from(t) * 1_000_000_000, 90_000)
}

/// Access units of a video stream with the time stamps of the PES they begin in. Units are
/// cut at access-unit delimiters, since a PES may end mid-picture (program streams do); a
/// stream without delimiters is taken to hold one picture per time-stamped PES.
fn video_units(pes: &[Pes], hevc: bool) -> Vec<(Vec<u8>, Option<i64>, Option<i64>)> {
    let mut joined = Vec::new();
    let mut marks = Vec::new();
    for p in pes {
        if p.pts.is_some() || p.dts.is_some() || marks.is_empty() {
            marks.push((joined.len(), p.pts, p.dts));
        }
        joined.extend_from_slice(&p.data);
    }
    let aud = if hevc { 35 } else { 9 };
    let mut cuts = Vec::new();
    let mut i = 0;
    while i + 4 <= joined.len() {
        if joined[i] == 0 && joined[i + 1] == 0 && joined[i + 2] == 1 {
            let t = if hevc {
                (joined[i + 3] >> 1) & 0x3F
            } else {
                joined[i + 3] & 0x1F
            };
            if t == aud {
                cuts.push(i);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    if cuts.is_empty() {
        cuts = marks.iter().map(|m| m.0).collect();
    } else if cuts[0] > 3 {
        cuts.insert(0, 0);
    } else {
        cuts[0] = 0;
    }
    cuts.dedup();
    let mut stamps = vec![(None, None); cuts.len()];
    for (off, pts, dts) in &marks {
        if let Some(k) = cuts.iter().position(|c| c >= off) {
            if stamps[k] == (None, None) {
                stamps[k] = (*pts, *dts);
            }
        }
    }
    let mut out = Vec::new();
    for (k, &c) in cuts.iter().enumerate() {
        let end = cuts.get(k + 1).copied().unwrap_or(joined.len());
        out.push((joined[c..end].to_vec(), stamps[k].0, stamps[k].1));
    }
    out
}

/// Builds a [`crate::Media`] from elementary streams cut out of a broadcast container.
pub fn assemble(streams: Vec<Es>, source: &str) -> Res<crate::Media> {
    let mut media = crate::Media {
        source: source.to_string(),
        ..crate::Media::default()
    };
    // (stream index, pts, dts) in ticks so the common origin can be taken off at the end.
    let mut raw: Vec<(Packet, i64, i64)> = Vec::new();
    for es in streams {
        let video = matches!(es.codec, codec::H264 | codec::HEVC);
        let kind = if video { Kind::Video } else { Kind::Audio };
        let mut stream = Stream::new(kind, es.codec);
        stream.language = es.language.clone();
        let si = media.streams.len();
        if video {
            let hevc = es.codec == codec::HEVC;
            let mut scan = VideoScan::new(hevc);
            let mut parsed = Vec::new();
            for (au, pts, dts) in video_units(&es.pes, hevc) {
                if let Some((data, key)) = scan.access_unit(&au) {
                    parsed.push((data, key, pts, dts));
                }
            }
            // Pictures that start inside a PES without a time stamp get one from the
            // spacing of the stamped ones (exact unless pictures are reordered).
            let stamped: Vec<(usize, i64)> = parsed
                .iter()
                .enumerate()
                .filter_map(|(i, u)| u.3.or(u.2).map(|t| (i, t)))
                .collect();
            let step = match (stamped.first(), stamped.last()) {
                (Some(a), Some(b)) if b.0 > a.0 => (b.1 - a.1) / (b.0 - a.0) as i64,
                _ => 0,
            };
            let (mut last_dts, mut offset) = (0i64, 0i64);
            let mut units = Vec::new();
            for (data, key, pts, dts) in parsed {
                let (pts, dts) = match (pts, dts) {
                    (None, None) => (last_dts + step + offset, last_dts + step),
                    (Some(p), None) => (p, p),
                    (Some(p), Some(d)) => (p, d.min(p)),
                    (None, Some(d)) => (d, d),
                };
                last_dts = dts;
                offset = pts - dts;
                units.push((data, key, pts, dts));
            }
            let Some((private, info)) = scan.config() else {
                continue;
            };
            stream.private = private;
            stream.width = info.width;
            stream.height = info.height;
            for (data, key, pts, dts) in units {
                raw.push((
                    Packet {
                        stream: si,
                        pts: 0,
                        dts: 0,
                        duration: 0,
                        key,
                        discard_end: 0,
                        data,
                    },
                    pts,
                    dts,
                ));
            }
        } else {
            // Frames can straddle PES boundaries: parse the joined payload and take each
            // frame's time from the PES it starts in.
            let mut joined = Vec::new();
            let mut marks: Vec<(usize, i64)> = Vec::new();
            let mut last = 0;
            for p in &es.pes {
                let pts = p.pts.unwrap_or(last);
                marks.push((joined.len(), pts));
                last = pts;
                joined.extend_from_slice(&p.data);
            }
            let parsed = match es.codec {
                codec::AAC => adts_frames(&joined),
                codec::MP3 => mp3_frames(&joined),
                _ => ac3_frames(&joined),
            };
            let Some(a) = parsed else {
                continue;
            };
            stream.sample_rate = a.sample_rate;
            stream.channels = a.channels;
            stream.private = a.private.clone();
            let step = i64::from(a.samples) * 90_000 / i64::from(a.sample_rate.max(1));
            let mut group = 0;
            let mut n = 0i64;
            for f in &a.frames {
                let mut moved = false;
                while group + 1 < marks.len() && marks[group + 1].0 <= f.offset {
                    group += 1;
                    moved = true;
                }
                if moved {
                    n = 0;
                }
                let t = marks[group].1 + n * step;
                n += 1;
                raw.push((
                    Packet {
                        stream: si,
                        pts: 0,
                        dts: 0,
                        duration: 0,
                        key: true,
                        discard_end: 0,
                        data: joined[f.offset + f.skip..f.offset + f.len].to_vec(),
                    },
                    t,
                    t,
                ));
            }
        }
        media.streams.push(stream);
    }
    if raw.is_empty() {
        return Err(crate::unsupported(
            "no stream in this file uses a codec this build can carry",
        ));
    }
    let origin = raw.iter().map(|r| r.1).min().unwrap_or(0);
    for (mut p, pts, dts) in raw {
        p.pts = ticks_to_ns(pts - origin);
        p.dts = ticks_to_ns(dts - origin);
        media.packets.push(p);
    }
    for si in 0..media.streams.len() {
        if media.streams[si].kind == Kind::Video {
            let pts: Vec<i64> = media.packets_of(si).map(|p| p.pts).collect();
            media.streams[si].fps = crate::estimate_fps(&pts);
        }
    }
    media.sort_by_dts();
    media.fill_durations();
    Ok(media)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn annexb_units_are_cut_at_start_codes() {
        let data = [
            0, 0, 0, 1, 9, 0xF0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 0, 1, 0x68, 5,
        ];
        let nals = split_annexb(&data);
        assert_eq!(
            nals,
            vec![&[9u8, 0xF0][..], &[0x67, 1, 2, 3][..], &[0x68, 5][..]]
        );
    }

    #[test]
    fn emulation_prevention_bytes_are_removed() {
        assert_eq!(unescape(&[1, 0, 0, 3, 1, 0, 0, 3]), vec![1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn exp_golomb_codes_read_back() {
        // 1 | 010 | 011 | 00100 -> 0, 1, 2, 3
        let bits = 0b1010_0110_0100_0000u16.to_be_bytes();
        let mut r = Bits::new(&bits);
        assert_eq!(
            (r.ue(), r.ue(), r.ue(), r.ue()),
            (Some(0), Some(1), Some(2), Some(3))
        );
    }

    #[test]
    fn a_baseline_sps_gives_its_size() {
        // 320x240 Baseline level 1.3 as x264 writes it.
        let sps = [
            0x67, 0x42, 0xC0, 0x0D, 0xDA, 0x05, 0x07, 0xE8, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
            0x00, 0x00, 0x0C, 0x83, 0xC5, 0x0A, 0x92,
        ];
        let i = h264_sps(&sps).unwrap();
        assert_eq!((i.width % 16, i.height % 8), (0, 0));
        assert!(i.width >= 16 && i.height >= 16);
    }

    #[test]
    fn mpeg_audio_frames_are_found_by_their_lengths() {
        // MPEG-1 layer III, 128 kbit/s, 44.1 kHz: 417 bytes (418 with padding).
        let mut data = Vec::new();
        for _ in 0..4 {
            let mut f = vec![0u8; 417];
            f[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            data.extend_from_slice(&f);
        }
        let a = mp3_frames(&data).unwrap();
        assert_eq!(
            (a.frames.len(), a.sample_rate, a.channels, a.samples),
            (4, 44100, 2, 1152)
        );
        assert_eq!(a.frames[1].offset, 417);
    }

    #[test]
    fn adts_frames_give_the_audio_specific_config() {
        let mut f = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC];
        f.extend_from_slice(&[7u8; 9]);
        let a = adts_frames(&f).unwrap();
        assert_eq!((a.sample_rate, a.channels), (44100, 2));
        assert_eq!(a.private, vec![0x12, 0x10]);
        assert_eq!(&f[a.frames[0].offset + a.frames[0].skip..], &[7u8; 9]);
    }
}
