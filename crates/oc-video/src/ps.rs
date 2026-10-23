//! MPEG program streams (`.mpg`, `.vob`), read only. Carries what the transport-stream
//! reader does: H.264/H.265 video and AAC (ADTS), MP3 or AC-3 audio.
use crate::es::{self, Es, Pes};
use crate::{codec, corrupt, Media, Res};
use oc_core::Limits;
use std::collections::BTreeMap;

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 1, 0xBA])
}

fn ts33(b: &[u8]) -> i64 {
    (i64::from((b[0] >> 1) & 7) << 30)
        | (i64::from(b[1]) << 22)
        | (i64::from(b[2] >> 1) << 15)
        | (i64::from(b[3]) << 7)
        | i64::from(b[4] >> 1)
}

/// What one elementary stream turned out to be, from its first payload bytes.
fn classify(id: u8, sub: Option<u8>, first: &[u8]) -> Option<&'static str> {
    match (id, sub) {
        (0xE0..=0xEF, _) => {
            let nals = es::split_annexb(first);
            let hevc = nals.iter().any(|n| matches!((n[0] >> 1) & 0x3F, 32..=34));
            let avc = nals.iter().any(|n| matches!(n[0] & 0x1F, 7..=9));
            if hevc && !avc {
                Some(codec::HEVC)
            } else if avc {
                Some(codec::H264)
            } else {
                None
            }
        }
        (0xC0..=0xDF, _) => es::probe_audio(first).filter(|c| *c != codec::AC3),
        (0xBD, Some(0x80..=0x87)) => Some(codec::AC3),
        _ => None,
    }
}

pub fn read(data: &[u8], _limits: &Limits) -> Res<Media> {
    if !sniff(data) {
        return Err(corrupt("not an MPEG program stream"));
    }
    // Key: (stream id, substream). Units are collected first, then classified.
    let mut units: BTreeMap<(u8, u8), Vec<Pes>> = BTreeMap::new();
    let mut last: BTreeMap<(u8, u8), i64> = BTreeMap::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        if data[i..i + 3] != [0, 0, 1] {
            i += 1;
            continue;
        }
        let id = data[i + 3];
        match id {
            0xBA => {
                // MPEG-2 packs carry a stuffing length; MPEG-1 packs are 12 bytes.
                let Some(b) = data.get(i + 4) else {
                    break;
                };
                i += if b >> 6 == 1 {
                    14 + usize::from(data.get(i + 13).map_or(0, |s| s & 7))
                } else {
                    12
                };
            }
            0xB9 => break,
            0xBB..=0xEF => {
                let Some(l) = data.get(i + 4..i + 6) else {
                    break;
                };
                let len = (usize::from(l[0]) << 8) | usize::from(l[1]);
                let end = (i + 6 + len).min(data.len());
                if len == 0 && id >= 0xE0 {
                    // Unbounded video PES: runs to the next start code of a PES or pack.
                    let mut e = i + 6;
                    while e + 4 <= data.len()
                        && !(data[e..e + 3] == [0, 0, 1] && (data[e + 3] >= 0xB9))
                    {
                        e += 1;
                    }
                    parse_pes(&data[i..e], id, &mut units, &mut last);
                    i = e;
                    continue;
                }
                if id == 0xBD || id >= 0xC0 {
                    parse_pes(&data[i..end], id, &mut units, &mut last);
                }
                i = end;
            }
            _ => i += 4,
        }
    }
    let mut streams = Vec::new();
    for ((id, sub), pes) in units {
        let mut joined = Vec::new();
        for p in pes.iter().take(8) {
            joined.extend_from_slice(&p.data);
        }
        let Some(name) = classify(id, (id == 0xBD).then_some(sub), &joined) else {
            continue;
        };
        streams.push(Es {
            codec: name,
            language: "und".into(),
            pes,
        });
    }
    es::assemble(streams, "ps")
}

fn parse_pes(
    pes: &[u8],
    id: u8,
    units: &mut BTreeMap<(u8, u8), Vec<Pes>>,
    last: &mut BTreeMap<(u8, u8), i64>,
) {
    if pes.len() < 9 {
        return;
    }
    let (mut pts, mut dts) = (None, None);
    let mut at;
    if pes[6] >> 6 == 2 {
        let flags = pes[7];
        at = 9 + usize::from(pes[8]);
        if flags & 0x80 != 0 && pes.len() >= 14 {
            pts = Some(ts33(&pes[9..14]));
            if flags & 0x40 != 0 && pes.len() >= 19 {
                dts = Some(ts33(&pes[14..19]));
            }
        }
    } else {
        // MPEG-1: stuffing, an optional STD field, then PTS or PTS and DTS.
        at = 6;
        while pes.get(at) == Some(&0xFF) {
            at += 1;
        }
        if pes.get(at).is_some_and(|b| b >> 6 == 1) {
            at += 2;
        }
        match pes.get(at).map(|b| b >> 4) {
            Some(2) if pes.len() >= at + 5 => {
                pts = Some(ts33(&pes[at..at + 5]));
                at += 5;
            }
            Some(3) if pes.len() >= at + 10 => {
                pts = Some(ts33(&pes[at..at + 5]));
                dts = Some(ts33(&pes[at + 5..at + 10]));
                at += 10;
            }
            _ => at += 1,
        }
    }
    let Some(mut payload) = pes.get(at..) else {
        return;
    };
    let mut sub = 0;
    if id == 0xBD {
        // Private stream 1: a substream id and, for AC-3, a 3-byte header.
        let Some(&s) = payload.first() else {
            return;
        };
        sub = s;
        if !(0x80..=0x87).contains(&s) || payload.len() < 4 {
            return;
        }
        payload = &payload[4..];
    }
    let key = (id, sub);
    let near = last.get(&key).copied().unwrap_or(0);
    const W: i64 = 1 << 33;
    let fix = |v: i64, n: i64| v + ((n - v + W / 2).div_euclid(W)) * W;
    let pts = pts.map(|v| fix(v, near));
    if let Some(p) = pts {
        last.insert(key, p);
    }
    let dts = dts.map(|v| fix(v, pts.unwrap_or(near)));
    units.entry(key).or_default().push(Pes {
        pts,
        dts,
        data: payload.to_vec(),
    });
}
