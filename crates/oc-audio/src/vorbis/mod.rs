//! Vorbis I: decoder and encoder over Ogg.
mod dec;
mod enc;
mod mdct;

pub use enc::{write, Options};

use crate::tags::parse_vorbis_comment;
use crate::{check_samples, ogg, Audio, Samples};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

/// Vorbis channel order to WAVEFORMAT order (indices into the Vorbis frame), and the mask.
fn layout(ch: usize) -> (Vec<usize>, u32) {
    match ch {
        3 => (vec![0, 2, 1], 0x7),
        5 => (vec![0, 2, 1, 3, 4], 0x37),
        6 => (vec![0, 2, 1, 5, 3, 4], 0x3F),
        7 => (vec![0, 2, 1, 6, 5, 3, 4], 0x70F),
        8 => (vec![0, 2, 1, 7, 5, 6, 3, 4], 0x63F),
        n => ((0..n).collect(), crate::default_mask(n as u16)),
    }
}

pub fn sniff(data: &[u8]) -> bool {
    ogg::sniff(data)
        && data.len() > 40
        && data[..200.min(data.len())]
            .windows(7)
            .any(|w| w == b"\x01vorbis")
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    let (streams, bad) = ogg::read(data)?;
    let mut notes = Notes::new();
    if bad > 0 {
        notes.push(Note::new(
            Severity::Warning,
            "ogg.damaged_pages",
            format!("{bad} damaged Ogg page(s) were skipped"),
        ));
    }
    let mut result: Option<Audio> = None;
    for s in streams.iter().filter(|s| {
        s.packets
            .first()
            .is_some_and(|p| p.data.starts_with(b"\x01vorbis"))
    }) {
        if s.packets.len() < 3 {
            return Err(Error::truncated("Vorbis stream ends inside its headers"));
        }
        let id = dec::parse_identification(&s.packets[0].data)?;
        let (_, tags, pics) = parse_vorbis_comment(s.packets[1].data.get(7..).unwrap_or_default());
        let mut d = dec::Decoder::new(&id, &s.packets[2].data)?;
        let mut pcm: Vec<f32> = Vec::new();
        for p in &s.packets[3..] {
            d.decode(&p.data, &mut pcm)?;
            check_samples(pcm.len() as u64, limits)?;
        }
        let ch = id.channels;
        // The last page's granule position says how many samples the stream really holds.
        let granule = s
            .packets
            .iter()
            .rev()
            .find(|p| p.granule >= 0)
            .map(|p| p.granule);
        if let Some(g) = granule {
            let want = (g as usize).saturating_mul(ch);
            if want < pcm.len() {
                pcm.truncate(want);
            }
        }
        let (order, mask) = layout(ch);
        if ch >= 3 {
            let frames = pcm.len() / ch;
            let mut re = Vec::with_capacity(pcm.len());
            for f in 0..frames {
                for &src in &order {
                    re.push(pcm[f * ch + src]);
                }
            }
            pcm = re;
        }
        match &mut result {
            None => {
                let mut a = Audio::new(id.sample_rate, ch as u16, Samples::Float(pcm));
                a.channel_mask = mask;
                a.tags = tags;
                a.pictures = pics;
                result = Some(a);
            }
            Some(a) => {
                if a.sample_rate != id.sample_rate || usize::from(a.channels) != ch {
                    notes.push(Note::new(
                        Severity::Warning,
                        "ogg.chain_changed",
                        "a chained stream changes the format; only the first part was converted",
                    ));
                    break;
                }
                if let Samples::Float(v) = &mut a.samples {
                    v.extend_from_slice(&pcm);
                }
            }
        }
    }
    match result {
        Some(a) => Ok((a, notes)),
        None => Err(Error::new(
            ErrorCode::UnsupportedFormat,
            "no Vorbis stream in this Ogg file",
        )),
    }
}
