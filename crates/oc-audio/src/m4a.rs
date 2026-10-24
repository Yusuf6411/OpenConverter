//! MP4 audio (M4A): ALAC tracks and `ilst` iTunes-style tags.
use crate::alac::{dec::Decoder, enc, Cookie};
use crate::{check_samples, default_mask, Audio, Picture, Samples, Tags};
use oc_container::isobmff::{self, boxes, child, BoxWriter};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    data.len() >= 12 && &data[4..8] == b"ftyp" && {
        let brand = &data[8..12];
        matches!(
            brand,
            b"M4A " | b"M4B " | b"mp42" | b"isom" | b"iso2" | b"M4P " | b"dash"
        )
    }
}

/// Atom to tag key, for plain text items.
const ATOMS: [(&[u8; 4], &str); 17] = [
    (b"\xA9nam", "TITLE"),
    (b"\xA9ART", "ARTIST"),
    (b"aART", "ALBUMARTIST"),
    (b"\xA9alb", "ALBUM"),
    (b"\xA9day", "DATE"),
    (b"\xA9gen", "GENRE"),
    (b"\xA9cmt", "COMMENT"),
    (b"\xA9wrt", "COMPOSER"),
    (b"cprt", "COPYRIGHT"),
    (b"\xA9too", "ENCODER"),
    (b"\xA9grp", "GROUPING"),
    (b"\xA9lyr", "LYRICS"),
    (b"desc", "DESCRIPTION"),
    (b"\xA9con", "CONDUCTOR"),
    (b"soal", "ALBUMSORT"),
    (b"soar", "ARTISTSORT"),
    (b"sonm", "TITLESORT"),
];

fn data_of(item: &[u8]) -> Vec<(u32, &[u8])> {
    boxes(item)
        .into_iter()
        .filter(|b| &b.kind == b"data" && b.body.len() >= 8)
        .map(|b| {
            let t = isobmff::be32(b.body, 0).unwrap_or(0) & 0x00FF_FFFF;
            (t, &b.body[8..])
        })
        .collect()
}

/// Parse an `ilst` body into tags and pictures.
pub fn parse_ilst(ilst: &[u8], tags: &mut Tags, pictures: &mut Vec<Picture>) {
    for item in boxes(ilst) {
        let k = &item.kind;
        let datas = data_of(item.body);
        if let Some((_, key)) = ATOMS.iter().find(|(a, _)| *a == k) {
            for (_, d) in &datas {
                tags.add(key, String::from_utf8_lossy(d).trim().to_string());
            }
        } else if k == b"trkn" || k == b"disk" {
            for (_, d) in &datas {
                if d.len() >= 6 {
                    let n = u16::from_be_bytes([d[2], d[3]]);
                    let t = u16::from_be_bytes([d[4], d[5]]);
                    let (nk, tk) = if k == b"trkn" {
                        ("TRACKNUMBER", "TRACKTOTAL")
                    } else {
                        ("DISCNUMBER", "DISCTOTAL")
                    };
                    if n > 0 {
                        tags.add(nk, n.to_string());
                    }
                    if t > 0 {
                        tags.add(tk, t.to_string());
                    }
                }
            }
        } else if k == b"tmpo" {
            for (_, d) in &datas {
                if d.len() >= 2 {
                    tags.add("BPM", u16::from_be_bytes([d[0], d[1]]).to_string());
                }
            }
        } else if k == b"gnre" {
            for (_, d) in &datas {
                if d.len() >= 2 {
                    let n = usize::from(u16::from_be_bytes([d[0], d[1]]));
                    if let Some(g) = n
                        .checked_sub(1)
                        .and_then(|i| crate::tags::ID3_GENRES.get(i))
                    {
                        tags.add("GENRE", *g);
                    }
                }
            }
        } else if k == b"cpil" {
            if datas
                .iter()
                .any(|(_, d)| d.first().is_some_and(|&v| v != 0))
            {
                tags.add("COMPILATION", "1");
            }
        } else if k == b"covr" {
            for (t, d) in &datas {
                pictures.push(Picture {
                    mime: if *t == 14 { "image/png" } else { "image/jpeg" }.to_string(),
                    kind: 3,
                    description: String::new(),
                    data: d.to_vec(),
                });
            }
        } else if k == b"----" {
            let name = child(item.body, b"name").and_then(|b| b.body.get(4..));
            let mean = child(item.body, b"mean").and_then(|b| b.body.get(4..));
            if let (Some(name), Some(_)) = (name, mean) {
                let key = String::from_utf8_lossy(name).to_ascii_uppercase();
                if key != "ITUNSMPB" && key != "ITUNNORM" {
                    for (_, d) in &datas {
                        tags.add(&key, String::from_utf8_lossy(d).to_string());
                    }
                }
            }
        }
    }
}

fn data_box(w: &mut BoxWriter, kind: u32, payload: &[u8]) {
    w.boxed(b"data", |d| {
        d.u32(kind).u32(0).bytes(payload);
    });
}

/// Build an `ilst` body. Returns the body and the number of tags that had no atom.
pub fn build_ilst(tags: &Tags, pictures: &[Picture]) -> (Vec<u8>, usize) {
    let mut w = BoxWriter::new();
    let mut dropped = 0;
    let mut trk = (0u16, 0u16);
    let mut dsk = (0u16, 0u16);
    for (k, v) in &tags.fields {
        let num = |s: &str| -> (u16, u16) {
            let mut it = s.split('/');
            let a = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(0);
            let b = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(0);
            (a, b)
        };
        match k.as_str() {
            "TRACKNUMBER" => {
                let (a, b) = num(v);
                trk.0 = a;
                if b > 0 {
                    trk.1 = b;
                }
            }
            "TRACKTOTAL" => trk.1 = v.trim().parse().unwrap_or(0),
            "DISCNUMBER" => {
                let (a, b) = num(v);
                dsk.0 = a;
                if b > 0 {
                    dsk.1 = b;
                }
            }
            "DISCTOTAL" => dsk.1 = v.trim().parse().unwrap_or(0),
            "BPM" => {
                if let Ok(n) = v.trim().parse::<u16>() {
                    w.boxed(b"tmpo", |t| data_box(t, 21, &n.to_be_bytes()));
                }
            }
            "COMPILATION" => {
                w.boxed(b"cpil", |t| data_box(t, 21, &[u8::from(v != "0")]));
            }
            key => {
                if let Some((atom, _)) = ATOMS.iter().find(|(_, n)| *n == key) {
                    w.boxed(atom, |t| data_box(t, 1, v.as_bytes()));
                } else {
                    // free-form items keep their name under the iTunes namespace
                    w.boxed(b"----", |t| {
                        t.full(b"mean", 0, 0, |m| {
                            m.bytes(b"com.apple.iTunes");
                        });
                        t.full(b"name", 0, 0, |m| {
                            m.bytes(key.as_bytes());
                        });
                        data_box(t, 1, v.as_bytes());
                    });
                }
            }
        }
    }
    if trk != (0, 0) {
        let mut p = vec![0, 0];
        p.extend_from_slice(&trk.0.to_be_bytes());
        p.extend_from_slice(&trk.1.to_be_bytes());
        p.extend_from_slice(&[0, 0]);
        w.boxed(b"trkn", |t| data_box(t, 0, &p));
    }
    if dsk != (0, 0) {
        let mut p = vec![0, 0];
        p.extend_from_slice(&dsk.0.to_be_bytes());
        p.extend_from_slice(&dsk.1.to_be_bytes());
        p.extend_from_slice(&[0, 0]);
        w.boxed(b"disk", |t| data_box(t, 0, &p));
    }
    if !pictures.is_empty() {
        w.boxed(b"covr", |t| {
            for p in pictures {
                data_box(t, if p.mime.contains("png") { 14 } else { 13 }, &p.data);
            }
        });
    }
    let _ = &mut dropped;
    (w.buf, dropped)
}

pub fn read(data: &[u8], limits: &Limits) -> Result<(Audio, Notes)> {
    let movie = isobmff::parse(data, limits)?;
    let track = movie
        .tracks
        .iter()
        .find(|t| &t.handler == b"soun")
        .ok_or_else(|| Error::new(ErrorCode::UnsupportedFormat, "the file has no audio track"))?;
    let mut notes = Notes::new();
    let mut audio = match &track.codec {
        b"alac" => read_alac(data, track, limits)?,
        b"mp4a" | b"ac-3" | b"ec-3" => {
            return Err(Error::new(
                ErrorCode::CodecNotInBuild,
                "this audio is AAC or AC-3, codecs that may be covered by patents and are not in this build; remux it or use Apple Lossless instead",
            ))
        }
        other => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!(
                    "MP4 audio codec '{}' is not supported",
                    String::from_utf8_lossy(other)
                ),
            ))
        }
    };
    if let Some(ilst) = movie.ilst {
        parse_ilst(ilst, &mut audio.tags, &mut audio.pictures);
    }
    let _ = &mut notes;
    Ok((audio, notes))
}

/// Find the `alac` configuration inside an audio sample entry (28 bytes of fixed fields, then
/// child boxes; the cookie sits in a nested `alac` full box).
fn alac_cookie(entry: &[u8]) -> Result<Cookie> {
    let kids = entry.get(28..).unwrap_or(&[]);
    let inner = child(kids, b"alac").ok_or_else(|| Error::corrupt("no ALAC configuration box"))?;
    Cookie::parse(inner.body.get(4..).unwrap_or(&[]))
}

fn read_alac(file: &[u8], t: &isobmff::Track<'_>, limits: &Limits) -> Result<Audio> {
    let cookie = alac_cookie(t.entry)?;
    let dec = Decoder::new(cookie.clone())?;
    let total: u64 = t.samples.iter().map(|s| u64::from(s.duration)).sum();
    check_samples(total * u64::from(cookie.channels), limits)?;
    let mut out: Vec<i32> = Vec::new();
    for s in &t.samples {
        let bytes = isobmff::sample_bytes(file, s)?;
        dec.frame(bytes, &mut out)?;
        check_samples(out.len() as u64, limits)?;
    }
    let ch = u16::from(cookie.channels);
    let mut a = Audio::new(
        cookie.sample_rate,
        ch,
        Samples::Int {
            bits: cookie.bit_depth,
            data: out,
        },
    );
    a.channel_mask = default_mask(ch);
    Ok(a)
}

/// Write an M4A holding ALAC.
pub fn write(audio: &Audio, notes: &mut Notes) -> Result<Vec<u8>> {
    let (cookie, packets) = enc::encode(audio)?;
    let mut payload = Vec::new();
    let mut samples = Vec::new();
    for p in &packets {
        samples.push((p.bytes.len() as u32, p.frames));
        payload.extend_from_slice(&p.bytes);
    }
    let mut extra = BoxWriter::new();
    extra.full(b"alac", 0, 0, |b| {
        b.bytes(&cookie.to_bytes());
    });
    let (ilst, _) = build_ilst(&audio.tags, &audio.pictures);
    let has_tags = !ilst.is_empty();
    let spec = isobmff::AudioTrackSpec {
        timescale: audio.sample_rate,
        codec: *b"alac",
        channels: audio.channels,
        sample_bits: u16::from(cookie.bit_depth),
        sample_rate: audio.sample_rate,
        entry_extra: &extra.buf,
        samples: &samples,
        ilst: has_tags.then_some(&ilst[..]),
        brand: *b"M4A ",
    };
    if audio.sample_rate > 0xFFFF {
        notes.push(Note::new(
            Severity::Info,
            "m4a.rate_field",
            "the sample entry rate field holds 16 bits; the exact rate is in the ALAC configuration",
        ));
    }
    Ok(isobmff::write_audio(&spec, &payload))
}
