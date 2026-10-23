//! MPEG audio layer III (MP3): frame scanner, tags, gapless information, decoder and encoder.

mod dec;
mod enc;
mod tables;

pub use enc::{write, Options};

use crate::{Audio, Picture, Samples, Tags};
use dec::{Decoder, Header};
use oc_core::{Error, ErrorCode, Limits, Notes, Result, Severity};

/// Frame synchronisation at the start of the data after any ID3v2 tag.
pub fn sniff(data: &[u8]) -> bool {
    let start = crate::id3::v2_len(data);
    let mut o = start;
    // Tolerate a little junk, then require two consecutive valid frames.
    while o < data.len().min(start + 8192) {
        if let Some(h) = Header::parse(data.get(o..).unwrap_or(&[])) {
            let next = o + h.frame_len();
            if next + 4 <= data.len() {
                if let Some(h2) = Header::parse(&data[next..]) {
                    if h2.version == h.version && h2.rate == h.rate {
                        return true;
                    }
                }
            } else if next <= data.len() && o == start {
                return true;
            }
        }
        o += 1;
    }
    false
}

struct Xing {
    frames: Option<u32>,
    delay: usize,
    padding: usize,
    lame: bool,
}

fn parse_xing(h: &Header, frame: &[u8]) -> Option<Xing> {
    let off = 4 + if h.crc { 2 } else { 0 } + h.side_info_len();
    let tag = frame.get(off..off + 4)?;
    if tag != b"Xing" && tag != b"Info" {
        return None;
    }
    let flags = u32::from_be_bytes(frame.get(off + 4..off + 8)?.try_into().ok()?);
    let mut p = off + 8;
    let mut frames = None;
    if flags & 1 != 0 {
        frames = Some(u32::from_be_bytes(frame.get(p..p + 4)?.try_into().ok()?));
        p += 4;
    }
    if flags & 2 != 0 {
        p += 4;
    }
    if flags & 4 != 0 {
        p += 100;
    }
    if flags & 8 != 0 {
        p += 4;
    }
    let mut x = Xing {
        frames,
        delay: 0,
        padding: 0,
        lame: false,
    };
    if let Some(ext) = frame.get(p..p + 24) {
        if ext.starts_with(b"LAME")
            || ext.starts_with(b"Lavf")
            || ext.starts_with(b"Lavc")
            || ext.starts_with(b"GOGO")
        {
            let d = u32::from(ext[21]) << 16 | u32::from(ext[22]) << 8 | u32::from(ext[23]);
            x.delay = (d >> 12) as usize;
            x.padding = (d & 0xFFF) as usize;
            x.lame = true;
        }
    }
    Some(x)
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    let mut notes = Notes::new();
    let mut tags = Tags::default();
    let mut pictures: Vec<Picture> = Vec::new();
    let start = crate::id3::v2_len(data);
    crate::id3::parse_v2(data, &mut tags, &mut pictures);
    crate::ape::parse(data, &mut tags, &mut pictures);
    let v1 = crate::id3::parse_v1(data, &mut tags);
    let end = if v1 { data.len() - 128 } else { data.len() };

    let mut dec = Decoder::new();
    let mut pcm: Vec<f32> = Vec::new();
    let mut o = start;
    let mut first: Option<Header> = None;
    let mut xing: Option<Xing> = None;
    let mut frames = 0usize;
    let mut skipped = 0usize;
    let mut sync_losses = 0usize;
    let mut channels = 0usize;
    while o + 4 <= end {
        let Some(h) = Header::parse(&data[o..]) else {
            o += 1;
            skipped += 1;
            continue;
        };
        let len = h.frame_len();
        if len < 4 + h.side_info_len() || o + len > end {
            // A truncated final frame is dropped; elsewhere it is a false sync.
            if o + len > end && first.is_some() {
                break;
            }
            o += 1;
            skipped += 1;
            continue;
        }
        // A real frame is followed by another header (or the end of the data).
        if o + len + 4 <= end && Header::parse(&data[o + len..]).is_none() && first.is_some() {
            // Try to continue anyway, but count the loss.
            sync_losses += 1;
        }
        match &first {
            None => {
                if o + len + 4 <= end && Header::parse(&data[o + len..]).is_none() {
                    o += 1;
                    skipped += 1;
                    continue; // not convincing enough to start on
                }
                first = Some(h);
                channels = h.channels();
                if let Some(x) = parse_xing(&h, &data[o..o + len]) {
                    xing = Some(x);
                    o += len;
                    continue;
                }
            }
            Some(f) => {
                if f.rate != h.rate || f.version != h.version {
                    notes.push(oc_core::Note::new(
                        Severity::Warning,
                        "mp3.format_change",
                        "the sample rate changes mid-stream; the rest of the file was ignored",
                    ));
                    break;
                }
            }
        }
        let before = pcm.len();
        dec.frame(&h, &data[o..o + len], &mut pcm);
        // Mono frames inside a stereo stream (or the reverse) are widened to the first layout.
        if h.channels() != channels {
            let n = (pcm.len() - before) / h.channels();
            let frame: Vec<f32> = pcm.split_off(before);
            for i in 0..n {
                if channels == 2 {
                    pcm.push(frame[i]);
                    pcm.push(frame[i]);
                } else {
                    pcm.push((frame[2 * i] + frame[2 * i + 1]) * 0.5);
                }
            }
        }
        frames += 1;
        crate::check_samples(pcm.len() as u64, limits)?;
        o += len;
    }
    let Some(h) = first else {
        return Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "no MPEG layer III frames found",
        ));
    };
    if frames == 0 {
        return Err(Error::new(
            ErrorCode::CorruptFile,
            "no decodable MP3 frames",
        ));
    }
    if skipped > 0 || sync_losses > 0 {
        notes.push(oc_core::Note::new(
            Severity::Warning,
            "mp3.resync",
            format!(
                "{} bytes of damaged or non-audio data were skipped",
                skipped.max(sync_losses)
            ),
        ));
    }

    // Gapless playback: the encoder delay and padding are removed as the LAME tag says.
    let spf = h.samples_per_frame();
    let nch = channels.max(1);
    if let Some(x) = &xing {
        if x.lame {
            let skip = (x.delay + 528 + 1) * nch;
            let total = x.frames.map_or(frames * spf, |f| f as usize * spf);
            let valid = total.saturating_sub(x.delay + x.padding);
            if skip < pcm.len() {
                pcm.drain(..skip);
            } else {
                pcm.clear();
            }
            pcm.truncate(valid * nch);
        }
    }
    let mut audio = Audio::new(h.rate, nch as u16, Samples::Float(pcm));
    audio.tags = tags;
    audio.pictures = pictures;
    Ok((audio, notes))
}
