//! MPEG transport streams (ISO/IEC 13818-1): 188-byte packets, also the 192-byte M2TS form.
use crate::es::{self, Es, Pes};
use crate::{codec, corrupt, Kind, Media, Res};
use oc_core::{Error, Limits};
use std::collections::HashMap;

const PMT_PID: u16 = 0x1000;
const VIDEO_PID: u16 = 0x100;

/// Packet size and the offset of the 188 bytes inside each packet.
fn layout(data: &[u8]) -> Option<(usize, usize)> {
    for (size, off) in [(188, 0), (192, 4), (204, 0)] {
        let n = (data.len() / size).min(5);
        if n >= 2 && (0..n).all(|k| data[k * size + off] == 0x47) {
            return Some((size, off));
        }
    }
    None
}

pub fn sniff(data: &[u8]) -> bool {
    layout(data).is_some()
}

pub fn allows(c: &str) -> bool {
    matches!(
        c,
        codec::H264 | codec::HEVC | codec::AAC | codec::MP3 | codec::AC3
    )
}

fn ts33(b: &[u8]) -> i64 {
    (i64::from((b[0] >> 1) & 7) << 30)
        | (i64::from(b[1]) << 22)
        | (i64::from(b[2] >> 1) << 15)
        | (i64::from(b[3]) << 7)
        | i64::from(b[4] >> 1)
}

/// Picks the multiple of 2^33 that puts `v` nearest to `near`.
fn unwrap33(v: i64, near: i64) -> i64 {
    const W: i64 = 1 << 33;
    v + ((near - v + W / 2).div_euclid(W)) * W
}

struct Section {
    buf: Vec<u8>,
}

impl Section {
    /// Feeds one packet payload; yields a section once it is complete.
    fn push(&mut self, pusi: bool, payload: &[u8]) -> Option<Vec<u8>> {
        if pusi {
            let ptr = usize::from(*payload.first()?);
            self.buf = payload.get(1 + ptr..)?.to_vec();
        } else if self.buf.is_empty() {
            return None;
        } else {
            self.buf.extend_from_slice(payload);
        }
        if self.buf.len() >= 3 {
            let len = 3 + ((usize::from(self.buf[1] & 0xF) << 8) | usize::from(self.buf[2]));
            if self.buf.len() >= len {
                let s = self.buf[..len].to_vec();
                self.buf.clear();
                return Some(s);
            }
        }
        None
    }
}

struct Track {
    pid: u16,
    /// `None` for private streams whose content has to be looked at.
    codec: Option<&'static str>,
    language: String,
}

fn parse_pmt(s: &[u8]) -> Vec<Track> {
    let mut out = Vec::new();
    if s.len() < 16 || s[0] != 2 {
        return out;
    }
    let end = s.len() - 4;
    let mut i = 12 + ((usize::from(s[10] & 0xF) << 8) | usize::from(s[11]));
    while i + 5 <= end {
        let ty = s[i];
        let pid = (u16::from(s[i + 1] & 0x1F) << 8) | u16::from(s[i + 2]);
        let info = (usize::from(s[i + 3] & 0xF) << 8) | usize::from(s[i + 4]);
        let desc = s.get(i + 5..(i + 5 + info).min(end)).unwrap_or(&[]);
        i += 5 + info;
        let mut lang = String::from("und");
        let (mut ac3, mut j) = (false, 0);
        while j + 2 <= desc.len() {
            let (tag, len) = (desc[j], usize::from(desc[j + 1]));
            let body = desc.get(j + 2..j + 2 + len).unwrap_or(&[]);
            match tag {
                0x0A if body.len() >= 3 => {
                    lang = String::from_utf8_lossy(&body[..3]).to_string();
                }
                0x6A => ac3 = true,
                0x05 if body.starts_with(b"AC-3") => ac3 = true,
                _ => {}
            }
            j += 2 + len;
        }
        let codec = match ty {
            0x1B => Some(codec::H264),
            0x24 => Some(codec::HEVC),
            0x0F => Some(codec::AAC),
            0x03 | 0x04 => Some(codec::MP3),
            0x81 => Some(codec::AC3),
            0x06 if ac3 => Some(codec::AC3),
            0x06 => None,
            _ => continue,
        };
        out.push(Track {
            pid,
            codec,
            language: lang,
        });
    }
    out
}

fn pes_unit(buf: &[u8], last: &mut (i64, i64)) -> Option<Pes> {
    if buf.len() < 9 || buf[0..3] != [0, 0, 1] {
        return None;
    }
    let flags = buf[7];
    let hdr = usize::from(buf[8]);
    let mut end = buf.len();
    let declared = (usize::from(buf[4]) << 8) | usize::from(buf[5]);
    if declared > 0 {
        end = end.min(6 + declared);
    }
    let data = buf.get(9 + hdr..end)?.to_vec();
    let (mut pts, mut dts) = (None, None);
    if flags & 0x80 != 0 && buf.len() >= 14 {
        let v = unwrap33(ts33(&buf[9..14]), last.0);
        last.0 = v;
        pts = Some(v);
        if flags & 0x40 != 0 && buf.len() >= 19 {
            dts = Some(unwrap33(ts33(&buf[14..19]), v));
        }
    }
    Some(Pes { pts, dts, data })
}

pub fn read(data: &[u8], _limits: &Limits) -> Res<Media> {
    let (size, off) = layout(data).ok_or_else(|| corrupt("not an MPEG transport stream"))?;
    let mut pat = Section { buf: Vec::new() };
    let mut pmt = Section { buf: Vec::new() };
    let mut pmt_pid: Option<u16> = None;
    let mut tracks: Option<Vec<Track>> = None;
    let mut bufs: HashMap<u16, Vec<u8>> = HashMap::new();
    let mut units: HashMap<u16, Vec<Pes>> = HashMap::new();
    let mut last: HashMap<u16, (i64, i64)> = HashMap::new();
    let mut pos = 0;
    while pos + off + 188 <= data.len() {
        let pkt = &data[pos + off..pos + off + 188];
        if pkt[0] != 0x47 {
            pos += 1;
            continue;
        }
        pos += size;
        if pkt[1] & 0x80 != 0 {
            continue;
        }
        let pusi = pkt[1] & 0x40 != 0;
        let pid = (u16::from(pkt[1] & 0x1F) << 8) | u16::from(pkt[2]);
        let afc = (pkt[3] >> 4) & 3;
        if afc & 1 == 0 {
            continue;
        }
        let start = if afc & 2 != 0 {
            5 + usize::from(pkt[4])
        } else {
            4
        };
        let Some(payload) = pkt.get(start..) else {
            continue;
        };
        if pid == 0 {
            if let Some(s) = pat.push(pusi, payload) {
                if s[0] == 0 && pmt_pid.is_none() {
                    let end = s.len().saturating_sub(4);
                    let mut i = 8;
                    while i + 4 <= end {
                        let prog = (u16::from(s[i]) << 8) | u16::from(s[i + 1]);
                        if prog != 0 {
                            pmt_pid = Some((u16::from(s[i + 2] & 0x1F) << 8) | u16::from(s[i + 3]));
                            break;
                        }
                        i += 4;
                    }
                }
            }
        } else if Some(pid) == pmt_pid {
            if tracks.is_none() {
                if let Some(s) = pmt.push(pusi, payload) {
                    let t = parse_pmt(&s);
                    if !t.is_empty() {
                        tracks = Some(t);
                    }
                }
            }
        } else if tracks
            .as_ref()
            .is_some_and(|t| t.iter().any(|x| x.pid == pid))
        {
            let buf = bufs.entry(pid).or_default();
            if pusi {
                if let Some(u) = pes_unit(buf, last.entry(pid).or_default()) {
                    units.entry(pid).or_default().push(u);
                }
                buf.clear();
            }
            if pusi || !buf.is_empty() {
                buf.extend_from_slice(payload);
            }
        }
    }
    for (pid, buf) in &bufs {
        if let Some(u) = pes_unit(buf, last.entry(*pid).or_default()) {
            units.entry(*pid).or_default().push(u);
        }
    }
    let tracks = tracks.ok_or_else(|| corrupt("transport stream has no program map"))?;
    let streams = tracks
        .into_iter()
        .filter_map(|t| {
            let pes = units.remove(&t.pid)?;
            let codec = match t.codec {
                Some(c) => c,
                None => {
                    let head: Vec<u8> = pes.iter().take(8).flat_map(|p| p.data.clone()).collect();
                    es::probe_audio(&head)?
                }
            };
            Some(Es {
                codec,
                language: t.language,
                pes,
            })
        })
        .collect();
    es::assemble(streams, "ts")
}

fn crc32_mpeg(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

struct Out {
    bytes: Vec<u8>,
    cc: HashMap<u16, u8>,
}

impl Out {
    /// Splits `payload` over transport packets for `pid`; `pcr` goes into the first one.
    fn emit(&mut self, pid: u16, payload: &[u8], pcr: Option<u64>) {
        let mut rest = payload;
        let mut first = true;
        while !rest.is_empty() || first {
            let want_pcr = if first { pcr } else { None };
            let af_fixed = if want_pcr.is_some() { 8 } else { 0 };
            let room = 184 - af_fixed;
            let take = rest.len().min(room);
            let stuff = room - take;
            let cc = self.cc.entry(pid).or_insert(0);
            let counter = *cc;
            *cc = (*cc + 1) & 0xF;
            let has_af = af_fixed + stuff > 0;
            self.bytes.push(0x47);
            self.bytes
                .push(((first as u8) << 6) | ((pid >> 8) as u8 & 0x1F));
            self.bytes.push(pid as u8);
            self.bytes
                .push((if has_af { 0x30 } else { 0x10 }) | counter);
            if has_af {
                let af_len = af_fixed + stuff - 1;
                self.bytes.push(af_len as u8);
                if af_len > 0 {
                    self.bytes.push(if want_pcr.is_some() { 0x10 } else { 0 });
                    let mut used = 1;
                    if let Some(p) = want_pcr {
                        let base = p / 300;
                        let ext = p % 300;
                        self.bytes.push((base >> 25) as u8);
                        self.bytes.push((base >> 17) as u8);
                        self.bytes.push((base >> 9) as u8);
                        self.bytes.push((base >> 1) as u8);
                        self.bytes
                            .push((((base & 1) << 7) as u8) | 0x7E | ((ext >> 8) as u8 & 1));
                        self.bytes.push(ext as u8);
                        used += 6;
                    }
                    self.bytes.resize(self.bytes.len() + (af_len - used), 0xFF);
                }
            }
            self.bytes.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            first = false;
        }
    }

    fn section(&mut self, pid: u16, mut body: Vec<u8>) {
        let crc = crc32_mpeg(&body);
        body.extend_from_slice(&crc.to_be_bytes());
        let mut payload = vec![0];
        payload.extend_from_slice(&body);
        self.emit(pid, &payload, None);
    }
}

fn pes_header(id: u8, len: usize, pts: i64, dts: Option<i64>) -> Vec<u8> {
    let mut h = vec![0, 0, 1, id];
    let hdr = if dts.is_some() { 10 } else { 5 };
    let total = if len == 0 { 0 } else { 3 + hdr + len };
    h.extend_from_slice(&(total.min(0xFFFF) as u16).to_be_bytes());
    h.push(0x80);
    h.push(if dts.is_some() { 0xC0 } else { 0x80 });
    h.push(hdr as u8);
    let stamp = |marker: u8, t: i64| {
        let t = t as u64 & 0x1_FFFF_FFFF;
        [
            (marker << 4) | (((t >> 30) as u8 & 7) << 1) | 1,
            (t >> 22) as u8,
            (((t >> 15) as u8) << 1) | 1,
            (t >> 7) as u8,
            ((t as u8) << 1) | 1,
        ]
    };
    if let Some(d) = dts {
        h.extend_from_slice(&stamp(3, pts));
        h.extend_from_slice(&stamp(1, d));
    } else {
        h.extend_from_slice(&stamp(2, pts));
    }
    h
}

fn ticks(ns: i64) -> i64 {
    crate::div_round(i128::from(ns) * 90_000, 1_000_000_000)
}

pub fn write(media: &Media) -> Res<Vec<u8>> {
    if media.streams.is_empty() {
        return Err(Error::corrupt("no streams to write"));
    }
    for s in &media.streams {
        if !allows(&s.codec) {
            return Err(crate::unsupported(format!(
                "MPEG-TS cannot carry {} streams",
                s.codec
            )));
        }
    }
    if media.streams.len() > 15 {
        return Err(crate::unsupported("too many streams for one program"));
    }
    let pids: Vec<u16> = (0..media.streams.len())
        .map(|i| VIDEO_PID + i as u16)
        .collect();
    let pcr_pid = media
        .streams
        .iter()
        .position(|s| s.kind == Kind::Video)
        .map_or(pids[0], |i| pids[i]);
    // Timestamps start 1.4 s in, as broadcast tools do, so the PCR can lead by 0.7 s and
    // decode times that precede presentation times stay positive.
    let min_dts = media
        .packets
        .iter()
        .map(|p| ticks(p.dts).clamp(-(1 << 40), 1 << 40))
        .min()
        .unwrap_or(0);
    let base = 126_000 + min_dts.saturating_neg().clamp(0, 1 << 40);
    let mut out = Out {
        bytes: Vec::new(),
        cc: HashMap::new(),
    };
    let tables = |out: &mut Out| {
        let mut pat = vec![0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1];
        pat.extend_from_slice(&[0xE0 | (PMT_PID >> 8) as u8, PMT_PID as u8]);
        out.section(0, pat);
        let mut es_loop = Vec::new();
        for (s, &pid) in media.streams.iter().zip(&pids) {
            let ty = match s.codec.as_str() {
                codec::H264 => 0x1B,
                codec::HEVC => 0x24,
                codec::AAC => 0x0F,
                codec::MP3 if s.sample_rate >= 32000 => 0x03,
                codec::MP3 => 0x04,
                _ => 0x81,
            };
            es_loop.extend_from_slice(&[ty, 0xE0 | (pid >> 8) as u8, pid as u8]);
            let mut desc = Vec::new();
            if s.codec == codec::AC3 {
                desc.extend_from_slice(&[0x05, 4, b'A', b'C', b'-', b'3']);
            }
            if s.kind == Kind::Audio {
                let l = s.language.as_bytes();
                if l.len() == 3 && s.language != "und" {
                    desc.extend_from_slice(&[0x0A, 4, l[0], l[1], l[2], 0]);
                }
            }
            es_loop.extend_from_slice(&[0xF0 | (desc.len() >> 8) as u8, desc.len() as u8]);
            es_loop.extend_from_slice(&desc);
        }
        let len = 9 + es_loop.len() + 4;
        let mut pmt = vec![0x02, 0xB0 | (len >> 8) as u8, len as u8, 0, 1, 0xC1, 0, 0];
        pmt.extend_from_slice(&[0xE0 | (pcr_pid >> 8) as u8, pcr_pid as u8, 0xF0, 0]);
        pmt.extend_from_slice(&es_loop);
        out.section(PMT_PID, pmt);
    };
    let mut next_tables = i64::MIN;
    let mut last_pcr = i64::MIN;
    for p in &media.packets {
        let s = &media.streams[p.stream];
        let pts = ticks(p.pts).clamp(-(1 << 40), 1 << 40) + base;
        let dts = ticks(p.dts).clamp(-(1 << 40), 1 << 40) + base;
        // Tables lead the stream and recur about every 0.3 s.
        if dts >= next_tables {
            tables(&mut out);
            next_tables = dts.saturating_add(27_000);
        }
        let pid = pids[p.stream];
        let payload = match s.codec.as_str() {
            codec::H264 | codec::HEVC => annexb(s, p),
            codec::AAC => adts(s, &p.data),
            _ => p.data.clone(),
        };
        let id = match s.codec.as_str() {
            codec::H264 | codec::HEVC => 0xE0,
            codec::AC3 => 0xBD,
            _ => 0xC0,
        };
        let video = s.kind == Kind::Video;
        let with_dts = (dts != pts).then_some(dts);
        let mut pes = pes_header(id, if video { 0 } else { payload.len() }, pts, with_dts);
        pes.extend_from_slice(&payload);
        let pcr = (pid == pcr_pid && (p.key || dts.saturating_sub(last_pcr) >= 3_600))
            .then(|| (dts - 63_000).max(0) as u64 * 300);
        if pcr.is_some() {
            last_pcr = dts;
        }
        out.emit(pid, &pes, pcr);
    }
    Ok(out.bytes)
}

/// Length-prefixed access unit to Annex B with a delimiter and, before key frames, the
/// parameter sets from the codec configuration.
fn annexb(s: &crate::Stream, p: &crate::Packet) -> Vec<u8> {
    let hevc = s.codec == codec::HEVC;
    let (len_size, sets): (usize, Vec<Vec<u8>>) = if hevc {
        hevc_sets(&s.private)
    } else {
        avc_sets(&s.private)
    };
    let nals = es::split_avcc(&p.data, len_size);
    let ty = |n: &[u8]| {
        if hevc {
            (n[0] >> 1) & 0x3F
        } else {
            n[0] & 0x1F
        }
    };
    let has_sps = nals.iter().any(|n| ty(n) == if hevc { 33 } else { 7 });
    let mut out = vec![0, 0, 0, 1];
    if hevc {
        out.extend_from_slice(&[0x46, 0x01, 0x50]);
    } else {
        out.extend_from_slice(&[0x09, 0xF0]);
    }
    if p.key && !has_sps {
        for set in &sets {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(set);
        }
    }
    for n in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(n);
    }
    out
}

pub(crate) fn avc_sets(avcc: &[u8]) -> (usize, Vec<Vec<u8>>) {
    let mut sets = Vec::new();
    let len_size = avcc.get(4).map_or(4, |b| usize::from(b & 3) + 1);
    let mut i = 5;
    let take = |i: &mut usize, n: usize, sets: &mut Vec<Vec<u8>>| {
        for _ in 0..n {
            let Some(l) = avcc.get(*i..*i + 2) else {
                return;
            };
            let l = (usize::from(l[0]) << 8) | usize::from(l[1]);
            if let Some(b) = avcc.get(*i + 2..*i + 2 + l) {
                sets.push(b.to_vec());
            }
            *i += 2 + l;
        }
    };
    let n_sps = avcc.get(i).map_or(0, |b| usize::from(b & 0x1F));
    i += 1;
    take(&mut i, n_sps, &mut sets);
    let n_pps = avcc.get(i).map_or(0, |b| usize::from(*b));
    i += 1;
    take(&mut i, n_pps, &mut sets);
    (len_size, sets)
}

pub(crate) fn hevc_sets(hvcc: &[u8]) -> (usize, Vec<Vec<u8>>) {
    let mut sets = Vec::new();
    let len_size = hvcc.get(21).map_or(4, |b| usize::from(b & 3) + 1);
    let arrays = hvcc.get(22).copied().unwrap_or(0);
    let mut i = 23;
    for _ in 0..arrays {
        let Some(h) = hvcc.get(i..i + 3) else {
            break;
        };
        let n = (usize::from(h[1]) << 8) | usize::from(h[2]);
        i += 3;
        for _ in 0..n {
            let Some(l) = hvcc.get(i..i + 2) else {
                break;
            };
            let l = (usize::from(l[0]) << 8) | usize::from(l[1]);
            if let Some(b) = hvcc.get(i + 2..i + 2 + l) {
                sets.push(b.to_vec());
            }
            i += 2 + l;
        }
    }
    (len_size, sets)
}

/// Raw AAC frame with an ADTS header built from the AudioSpecificConfig.
fn adts(s: &crate::Stream, frame: &[u8]) -> Vec<u8> {
    let asc = s.private.as_slice();
    let (profile, idx, cfg) = if asc.len() >= 2 {
        let v = (u16::from(asc[0]) << 8) | u16::from(asc[1]);
        (
            ((v >> 11) as u8).saturating_sub(1).min(3),
            ((v >> 7) & 0xF) as u8,
            ((v >> 3) & 0xF) as u8,
        )
    } else {
        (1, 4, s.channels.min(7) as u8)
    };
    let len = frame.len() + 7;
    let mut h = vec![
        0xFF,
        0xF1,
        (profile << 6) | (idx << 2) | (cfg >> 2),
        ((cfg & 3) << 6) | ((len >> 11) as u8 & 3),
        (len >> 3) as u8,
        (((len & 7) as u8) << 5) | 0x1F,
        0xFC,
    ];
    h.extend_from_slice(frame);
    h
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{Media, Packet, Stream};

    #[test]
    fn crc_matches_the_mpeg_check_value() {
        assert_eq!(crc32_mpeg(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn time_stamps_unwrap_across_the_33_bit_boundary() {
        let top = (1i64 << 33) - 100;
        assert_eq!(unwrap33(50, top), (1 << 33) + 50);
        assert_eq!(unwrap33(top, 10), top - (1 << 33));
        assert_eq!(unwrap33(1000, 900), 1000);
    }

    fn aac_media() -> Media {
        let mut s = Stream::new(Kind::Audio, codec::AAC);
        s.sample_rate = 44100;
        s.channels = 2;
        s.private = vec![0x12, 0x10];
        let packets = (0..40)
            .map(|i| {
                let t = i * 1_000_000_000 * 1024 / 44100;
                Packet {
                    stream: 0,
                    pts: t,
                    dts: t,
                    duration: 0,
                    key: true,
                    discard_end: 0,
                    data: (0..100 + i as usize)
                        .map(|b| (b * 7 + i as usize) as u8)
                        .collect(),
                }
            })
            .collect();
        Media {
            streams: vec![s],
            packets,
            ..Media::default()
        }
    }

    #[test]
    fn aac_survives_a_trip_through_a_transport_stream() {
        let m = aac_media();
        let bytes = write(&m).unwrap();
        assert_eq!(bytes.len() % 188, 0);
        assert!(sniff(&bytes));
        let back = read(&bytes, &Limits::default()).unwrap();
        assert_eq!(back.streams.len(), 1);
        assert_eq!(back.streams[0].codec, codec::AAC);
        assert_eq!(back.streams[0].sample_rate, 44100);
        assert_eq!(back.streams[0].channels, 2);
        assert_eq!(back.streams[0].private, vec![0x12, 0x10]);
        assert_eq!(back.packets.len(), 40);
        for (a, b) in m.packets.iter().zip(&back.packets) {
            assert_eq!(a.data, b.data);
            // 90 kHz ticks: within one tick of the original.
            assert!((a.pts - b.pts).abs() < 20_000, "{} vs {}", a.pts, b.pts);
        }
    }

    #[test]
    fn the_192_byte_form_is_read_too() {
        let bytes = write(&aac_media()).unwrap();
        let mut m2ts = Vec::new();
        for p in bytes.chunks(188) {
            m2ts.extend_from_slice(&[0, 0, 0, 0]);
            m2ts.extend_from_slice(p);
        }
        let back = read(&m2ts, &Limits::default()).unwrap();
        assert_eq!(back.packets.len(), 40);
    }

    #[test]
    fn damaged_streams_are_errors_not_panics() {
        let bytes = write(&aac_media()).unwrap();
        for cut in [0, 1, 187, 188, 189, 700, bytes.len() - 5] {
            let _ = read(&bytes[..cut], &Limits::default());
        }
        let mut bad = bytes.clone();
        for (i, b) in bad.iter_mut().enumerate() {
            if i % 97 == 3 {
                *b ^= 0xFF;
            }
        }
        let _ = read(&bad, &Limits::default());
    }

    #[test]
    fn codecs_a_transport_stream_cannot_hold_are_refused() {
        let mut m = aac_media();
        m.streams[0].codec = codec::OPUS.into();
        assert!(write(&m).is_err());
    }
}
