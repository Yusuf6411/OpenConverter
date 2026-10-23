//! Moving compressed audio between containers without decoding it: Opus and Vorbis between
//! Ogg, Matroska and WebM, and FLAC between its own file format and Matroska.
//!
//! A [`CopyStream`] is the codec's packets plus what a container needs to describe them. Each
//! container has a reader that fills one and a writer that drains one.
use crate::mka::{self, ns_to_samples, samples_to_ns, OPUS_RATE};
use crate::tags::{
    parse_flac_picture, parse_vorbis_comment, write_flac_picture, write_vorbis_comment, Tags,
};
use crate::{flac, ogg, opus, vorbis, Picture};
use oc_container::matroska::{self, AudioInfo, Packet, Track, TrackKind, WriteOptions};
use oc_core::{Error, ErrorCode, Limits, Notes, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Opus,
    Vorbis,
    Flac,
}

/// The containers a stream can be copied between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// `.opus`: Ogg holding Opus only.
    OggOpus,
    /// `.ogg`: Ogg holding Vorbis only.
    OggVorbis,
    /// `.flac`
    FlacFile,
    Matroska,
    WebM,
}

#[derive(Debug, Clone)]
pub struct CopyStream {
    pub kind: Kind,
    /// Opus: `[OpusHead]`. Vorbis: identification, comment and setup headers. FLAC: the 34-byte
    /// STREAMINFO.
    pub private: Vec<Vec<u8>>,
    pub rate: u64,
    pub channels: u32,
    pub bits: Option<u32>,
    /// Each packet with the samples (per channel) it adds.
    pub packets: Vec<(Vec<u8>, u64)>,
    /// Samples at the end of the last packet that are padding.
    pub trim_end: u64,
    pub pre_skip: u64,
    pub tags: Tags,
    pub pictures: Vec<Picture>,
}

const VENDOR: &str = "OpenConverter";

fn empty_vorbis_comment() -> Vec<u8> {
    let mut c = b"\x03vorbis".to_vec();
    c.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    c.extend_from_slice(VENDOR.as_bytes());
    c.extend_from_slice(&0u32.to_le_bytes());
    c.push(1);
    c
}

impl CopyStream {
    fn total(&self) -> u64 {
        self.packets.iter().map(|p| p.1).sum()
    }

    // -------------------------------------------------------------------------- readers

    /// An Opus or Vorbis stream in Ogg. Returns `None` for other codecs.
    pub fn from_ogg(data: &[u8]) -> Result<Self> {
        let (streams, _) = ogg::read(data)?;
        let s = streams
            .iter()
            .find(|s| {
                s.packets.first().is_some_and(|p| {
                    p.data.starts_with(b"OpusHead") || p.data.starts_with(b"\x01vorbis")
                })
            })
            .ok_or_else(|| Error::new(ErrorCode::UnsupportedFormat, "no Opus or Vorbis stream"))?;
        if s.packets[0].data.starts_with(b"OpusHead") {
            let head = opus::parse_head(&s.packets[0].data)?;
            let (tags, pictures, first) = match s.packets.get(1) {
                Some(p) if p.data.starts_with(b"OpusTags") => {
                    let (_, t, pics) = parse_vorbis_comment(&p.data[8..]);
                    (t, pics, 2)
                }
                _ => (Tags::default(), Vec::new(), 1),
            };
            let multi = head.streams > 1;
            let packets: Vec<(Vec<u8>, u64)> = s.packets[first..]
                .iter()
                .map(|p| {
                    (
                        p.data.clone(),
                        mka::opus_packet_samples(&p.data, multi) as u64,
                    )
                })
                .collect();
            let sum: u64 = packets.iter().map(|p| p.1).sum();
            let last = s.packets.last().map_or(0, |p| p.granule).max(0) as u64;
            let trim_end = (head.pre_skip as u64 + sum).saturating_sub(last);
            Ok(Self {
                kind: Kind::Opus,
                private: vec![s.packets[0].data.clone()],
                rate: OPUS_RATE,
                channels: head.channels as u32,
                bits: None,
                packets,
                trim_end,
                pre_skip: head.pre_skip as u64,
                tags,
                pictures,
            })
        } else {
            if s.packets.len() < 3 {
                return Err(Error::truncated("Vorbis stream ends inside its headers"));
            }
            let ident = &s.packets[0].data;
            let id = vorbis::parse_identification(ident)?;
            let (_, tags, pictures) =
                parse_vorbis_comment(s.packets[1].data.get(7..).unwrap_or_default());
            let datas: Vec<&[u8]> = s.packets[3..].iter().map(|p| p.data.as_slice()).collect();
            let durs = vorbis::packet_durations(ident, &s.packets[2].data, &datas)?;
            let packets: Vec<(Vec<u8>, u64)> = datas
                .iter()
                .zip(&durs)
                .map(|(d, &n)| (d.to_vec(), n as u64))
                .collect();
            let sum: u64 = packets.iter().map(|p| p.1).sum();
            let last = s.packets.last().map_or(0, |p| p.granule).max(0) as u64;
            Ok(Self {
                kind: Kind::Vorbis,
                private: vec![
                    ident.clone(),
                    empty_vorbis_comment(),
                    s.packets[2].data.clone(),
                ],
                rate: u64::from(id.sample_rate),
                channels: id.channels as u32,
                bits: None,
                packets,
                trim_end: if last > 0 {
                    sum.saturating_sub(last)
                } else {
                    0
                },
                pre_skip: 0,
                tags,
                pictures,
            })
        }
    }

    /// A FLAC file.
    pub fn from_flac(data: &[u8]) -> Result<Self> {
        if !flac::sniff(data) {
            return Err(Error::new(ErrorCode::UnsupportedFormat, "not a FLAC file"));
        }
        let mut o = if data.starts_with(b"ID3") && data.len() >= 10 {
            10 + ((usize::from(data[6] & 0x7F) << 21)
                | (usize::from(data[7] & 0x7F) << 14)
                | (usize::from(data[8] & 0x7F) << 7)
                | usize::from(data[9] & 0x7F))
        } else {
            0
        } + 4;
        let mut info_bytes: Option<Vec<u8>> = None;
        let mut tags = Tags::default();
        let mut pictures = Vec::new();
        loop {
            let h = *data
                .get(o)
                .ok_or_else(|| Error::truncated("FLAC metadata"))?;
            let b = data
                .get(o + 1..o + 4)
                .ok_or_else(|| Error::truncated("FLAC metadata"))?;
            let len = (usize::from(b[0]) << 16) | (usize::from(b[1]) << 8) | usize::from(b[2]);
            let body = data
                .get(o + 4..o + 4 + len)
                .ok_or_else(|| Error::truncated("FLAC metadata block"))?;
            match h & 0x7F {
                0 => info_bytes = Some(body.to_vec()),
                4 => {
                    let (_, t, p) = parse_vorbis_comment(body);
                    tags = t;
                    pictures.extend(p);
                }
                6 => pictures.extend(parse_flac_picture(body)),
                _ => {}
            }
            o += 4 + len;
            if h & 0x80 != 0 {
                break;
            }
        }
        let info_bytes = info_bytes.ok_or_else(|| Error::corrupt("FLAC without STREAMINFO"))?;
        let info = flac::parse_streaminfo(&info_bytes)
            .ok_or_else(|| Error::corrupt("short STREAMINFO"))?;
        let frames = &data[o..];
        let packets = flac::split_frames(frames, &info)
            .into_iter()
            .map(|(off, len, n)| (frames[off..off + len].to_vec(), n as u64))
            .collect();
        Ok(Self {
            kind: Kind::Flac,
            private: vec![info_bytes[..34].to_vec()],
            rate: u64::from(info.sample_rate),
            channels: u32::from(info.channels),
            bits: Some(u32::from(info.bits)),
            packets,
            trim_end: 0,
            pre_skip: 0,
            tags,
            pictures,
        })
    }

    /// The first audio track of a Matroska or WebM file, when it holds Opus, Vorbis or FLAC.
    pub fn from_mka(data: &[u8], limits: &Limits) -> Result<Option<Self>> {
        let m = matroska::parse(data, limits)?;
        let Some(t) = m
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio && t.enabled)
        else {
            return Ok(None);
        };
        let pk: Vec<&Packet> = m.packets_of(t.number).collect();
        let info = t.audio.clone().unwrap_or_default();
        let last_discard = pk.last().map_or(0, |p| p.discard_padding_ns.max(0));
        let tags = mka::tags_from(&m, t.uid);
        let pictures = mka::pictures_from(&m);
        let cs = match t.codec_id.as_str() {
            "A_OPUS" => {
                let head = opus::parse_head(&t.codec_private)?;
                let multi = head.streams > 1;
                Self {
                    kind: Kind::Opus,
                    private: vec![t.codec_private.clone()],
                    rate: OPUS_RATE,
                    channels: head.channels as u32,
                    bits: None,
                    packets: pk
                        .iter()
                        .map(|p| {
                            (
                                p.data.clone(),
                                mka::opus_packet_samples(&p.data, multi) as u64,
                            )
                        })
                        .collect(),
                    trim_end: ns_to_samples(last_discard, OPUS_RATE).max(0) as u64,
                    pre_skip: head.pre_skip as u64,
                    tags,
                    pictures,
                }
            }
            "A_VORBIS" => {
                let hdr = mka::xiph_unlace(&t.codec_private)?;
                if hdr.len() != 3 {
                    return Err(Error::corrupt("Vorbis codec private data needs 3 headers"));
                }
                let id = vorbis::parse_identification(hdr[0])?;
                let datas: Vec<&[u8]> = pk.iter().map(|p| p.data.as_slice()).collect();
                let durs = vorbis::packet_durations(hdr[0], hdr[2], &datas)?;
                Self {
                    kind: Kind::Vorbis,
                    private: vec![hdr[0].to_vec(), empty_vorbis_comment(), hdr[2].to_vec()],
                    rate: u64::from(id.sample_rate),
                    channels: id.channels as u32,
                    bits: None,
                    packets: datas
                        .iter()
                        .zip(&durs)
                        .map(|(d, &n)| (d.to_vec(), n as u64))
                        .collect(),
                    trim_end: ns_to_samples(last_discard, u64::from(id.sample_rate)).max(0) as u64,
                    pre_skip: 0,
                    tags,
                    pictures,
                }
            }
            "A_FLAC" => {
                let cp = &t.codec_private;
                let info_bytes = if cp.starts_with(b"fLaC") {
                    cp.get(8..42)
                } else if cp.len() >= 34 {
                    cp.get(..34)
                } else {
                    None
                }
                .ok_or_else(|| Error::corrupt("FLAC track without STREAMINFO"))?;
                let info = flac::parse_streaminfo(info_bytes)
                    .ok_or_else(|| Error::corrupt("short STREAMINFO"))?;
                let mut packets = Vec::new();
                for p in &pk {
                    let n = flac::split_frames(&p.data, &info)
                        .iter()
                        .map(|f| f.2 as u64)
                        .sum();
                    packets.push((p.data.clone(), n));
                }
                Self {
                    kind: Kind::Flac,
                    private: vec![info_bytes.to_vec()],
                    rate: u64::from(info.sample_rate),
                    channels: u32::from(info.channels),
                    bits: Some(u32::from(info.bits)),
                    packets,
                    trim_end: 0,
                    pre_skip: 0,
                    tags,
                    pictures,
                }
            }
            _ => {
                let _ = info;
                return Ok(None);
            }
        };
        Ok(Some(cs))
    }

    // -------------------------------------------------------------------------- writers

    pub fn to_mka(&self, webm: bool, notes: &mut Notes) -> Result<Vec<u8>> {
        if webm && self.kind == Kind::Flac {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "WebM audio holds Opus or Vorbis only",
            ));
        }
        let mut m = mka::base(webm);
        let mut track = Track::new(
            1,
            TrackKind::Audio,
            match self.kind {
                Kind::Opus => "A_OPUS",
                Kind::Vorbis => "A_VORBIS",
                Kind::Flac => "A_FLAC",
            },
        );
        track.audio = Some(AudioInfo {
            sample_rate: self.rate as f64,
            output_sample_rate: None,
            channels: self.channels,
            bit_depth: self.bits,
        });
        match self.kind {
            Kind::Opus => {
                track.codec_private = self.private[0].clone();
                track.codec_delay_ns = samples_to_ns(self.pre_skip, OPUS_RATE);
                track.seek_pre_roll_ns = 80_000_000;
            }
            Kind::Vorbis => {
                let parts: Vec<&[u8]> = self.private.iter().map(Vec::as_slice).collect();
                track.codec_private = mka::xiph_lace(&parts);
            }
            Kind::Flac => {
                let mut cp = b"fLaC\x80\x00\x00\x22".to_vec();
                cp.extend_from_slice(&self.private[0]);
                track.codec_private = cp;
            }
        }
        let mut pos = 0u64;
        for (i, (data, n)) in self.packets.iter().enumerate() {
            let last = i + 1 == self.packets.len();
            m.packets.push(Packet {
                track: 1,
                pts_ns: samples_to_ns(pos, self.rate) as i64,
                duration_ns: Some(samples_to_ns(*n, self.rate)),
                keyframe: true,
                discard_padding_ns: if last && self.trim_end > 0 {
                    samples_to_ns(self.trim_end, self.rate) as i64
                } else {
                    0
                },
                data: data.clone(),
            });
            pos += n;
        }
        m.duration_ns = Some(samples_to_ns(
            self.total().saturating_sub(self.trim_end),
            self.rate,
        ));
        m.tracks.push(track);
        mka::finish(&mut m, &self.tags, &self.pictures, webm, notes);
        matroska::write(&m, &WriteOptions::default())
    }

    /// Opus or Vorbis in Ogg; the caller has checked that the container takes this codec.
    pub fn to_ogg(&self, serial: u32) -> Result<Vec<u8>> {
        let mut w = ogg::Writer::new(serial);
        let sum = self.total();
        match self.kind {
            Kind::Opus => {
                w.packet(&self.private[0], 0, true, false);
                let mut t = b"OpusTags".to_vec();
                t.extend_from_slice(&write_vorbis_comment(
                    VENDOR,
                    &self.tags,
                    &self.pictures,
                    true,
                ));
                w.packet(&t, 0, true, false);
                let mut acc = 0u64;
                for (i, (d, n)) in self.packets.iter().enumerate() {
                    acc += n;
                    let last = i + 1 == self.packets.len();
                    let g = self.pre_skip
                        + if last {
                            sum.saturating_sub(self.trim_end)
                        } else {
                            acc
                        };
                    w.packet(d, g as i64, false, last);
                }
                if self.packets.is_empty() {
                    w.packet(&[], self.pre_skip as i64, true, true);
                }
            }
            Kind::Vorbis => {
                w.packet(&self.private[0], 0, true, false);
                let mut c = b"\x03vorbis".to_vec();
                c.extend_from_slice(&write_vorbis_comment(
                    VENDOR,
                    &self.tags,
                    &self.pictures,
                    true,
                ));
                c.push(1);
                w.packet(&c, 0, true, false);
                w.packet(&self.private[2], 0, true, false);
                let mut acc = 0u64;
                for (i, (d, n)) in self.packets.iter().enumerate() {
                    acc += n;
                    let last = i + 1 == self.packets.len();
                    let g = if last {
                        sum.saturating_sub(self.trim_end)
                    } else {
                        acc
                    };
                    w.packet(d, g as i64, false, last);
                }
            }
            Kind::Flac => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "FLAC does not go in Ogg here",
                ))
            }
        }
        Ok(w.finish())
    }

    pub fn to_flac(&self) -> Result<Vec<u8>> {
        if self.kind != Kind::Flac {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "only FLAC streams go in a FLAC file",
            ));
        }
        let mut blocks: Vec<(u8, Vec<u8>)> = vec![(0, self.private[0].clone())];
        if !self.tags.is_empty() {
            blocks.push((4, write_vorbis_comment(VENDOR, &self.tags, &[], false)));
        }
        for p in &self.pictures {
            blocks.push((6, write_flac_picture(p)));
        }
        let mut out = b"fLaC".to_vec();
        let n = blocks.len();
        for (i, (ty, body)) in blocks.into_iter().enumerate() {
            out.push(ty | if i + 1 == n { 0x80 } else { 0 });
            out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
            out.extend_from_slice(&body);
        }
        for (d, _) in &self.packets {
            out.extend_from_slice(d);
        }
        Ok(out)
    }

    /// Whether `to` can hold this codec.
    pub fn fits(&self, to: Container) -> bool {
        match to {
            Container::OggOpus => self.kind == Kind::Opus,
            Container::OggVorbis => self.kind == Kind::Vorbis,
            Container::FlacFile => self.kind == Kind::Flac,
            Container::Matroska => true,
            Container::WebM => self.kind != Kind::Flac,
        }
    }
}

/// Copy the audio of `data` (in container `from`) into container `to` without decoding it.
/// `None` means the codec in the source does not fit the target; the caller should transcode.
pub fn copy(
    data: &[u8],
    from: Container,
    to: Container,
    only: Option<Kind>,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Option<Vec<u8>>> {
    let cs = match from {
        Container::OggOpus | Container::OggVorbis => Some(CopyStream::from_ogg(data)?),
        Container::FlacFile => Some(CopyStream::from_flac(data)?),
        Container::Matroska | Container::WebM => CopyStream::from_mka(data, limits)?,
    };
    let Some(cs) = cs else { return Ok(None) };
    let wants = match from {
        Container::OggOpus => cs.kind == Kind::Opus,
        Container::OggVorbis => cs.kind == Kind::Vorbis,
        _ => true,
    };
    if !wants || !cs.fits(to) || only.is_some_and(|k| k != cs.kind) {
        return Ok(None);
    }
    Ok(Some(match to {
        Container::OggOpus | Container::OggVorbis => cs.to_ogg(0x4F43_4350)?,
        Container::FlacFile => cs.to_flac()?,
        Container::Matroska => cs.to_mka(false, notes)?,
        Container::WebM => cs.to_mka(true, notes)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Audio, Samples};

    fn tone() -> Audio {
        let mut d = Vec::new();
        for i in 0..24000 {
            let v = ((i as f32 * 0.05).sin() * 9000.0) as i32;
            d.push(v);
            d.push(-v / 2);
        }
        let mut a = Audio::new(48000, 2, Samples::Int { bits: 16, data: d });
        a.tags.add("TITLE", "Copy");
        a
    }

    #[test]
    fn packets_survive_every_container_hop() {
        let lim = Limits::default();
        let mut n = Notes::new();
        let a = tone();
        let ogg_opus = opus::write(&a, &opus::Options::default(), &mut n).unwrap();
        let direct = CopyStream::from_ogg(&ogg_opus).unwrap();
        let mka = copy(
            &ogg_opus,
            Container::OggOpus,
            Container::Matroska,
            None,
            &lim,
            &mut n,
        )
        .unwrap()
        .unwrap();
        let webm = copy(
            &mka,
            Container::Matroska,
            Container::WebM,
            None,
            &lim,
            &mut n,
        )
        .unwrap()
        .unwrap();
        let back = copy(
            &webm,
            Container::WebM,
            Container::OggOpus,
            None,
            &lim,
            &mut n,
        )
        .unwrap()
        .unwrap();
        let again = CopyStream::from_ogg(&back).unwrap();
        assert_eq!(again.packets, direct.packets);
        assert_eq!(again.trim_end, direct.trim_end);
        assert_eq!(again.pre_skip, direct.pre_skip);
        // and it still plays: same length as the original file decodes to
        let (x, _) = opus::read(&ogg_opus, &lim).unwrap();
        let (y, _) = opus::read(&back, &lim).unwrap();
        assert_eq!(x.samples, y.samples);

        let flac_file = flac::write(&a, &flac::Options::default(), &mut n).unwrap();
        let mka = copy(
            &flac_file,
            Container::FlacFile,
            Container::Matroska,
            None,
            &lim,
            &mut n,
        )
        .unwrap()
        .unwrap();
        assert!(copy(
            &mka,
            Container::Matroska,
            Container::WebM,
            None,
            &lim,
            &mut n
        )
        .unwrap()
        .is_none());
        let back = copy(
            &mka,
            Container::Matroska,
            Container::FlacFile,
            None,
            &lim,
            &mut n,
        )
        .unwrap()
        .unwrap();
        let (z, _) = flac::read(&back, &lim).unwrap();
        assert_eq!(z.samples, a.samples);
        assert_eq!(z.tags.get("TITLE"), Some("Copy"));

        let ogg_vorbis = vorbis::write(&a, &vorbis::Options::default(), &mut n).unwrap();
        let mka = copy(
            &ogg_vorbis,
            Container::OggVorbis,
            Container::Matroska,
            None,
            &lim,
            &mut n,
        )
        .unwrap()
        .unwrap();
        let back = copy(
            &mka,
            Container::Matroska,
            Container::OggVorbis,
            None,
            &lim,
            &mut n,
        )
        .unwrap()
        .unwrap();
        let (p, _) = vorbis::read(&ogg_vorbis, &lim).unwrap();
        let (q, _) = vorbis::read(&back, &lim).unwrap();
        assert_eq!(p.samples, q.samples);
        // an Opus file does not fit the Vorbis extension
        assert!(copy(
            &ogg_opus,
            Container::OggOpus,
            Container::OggVorbis,
            None,
            &lim,
            &mut n
        )
        .unwrap()
        .is_none());
    }
}
