//! Subtitle streams as [`oc_subtitle::Track`] and back, for extraction and conversion between
//! the text codecs a container can hold (SRT-style text and ASS).
use crate::{codec, Kind, Media, Packet, Stream};
use oc_core::Notes;
use oc_subtitle::{ass, inline_to_srt_text, srt_text_to_inline, Cue, Track};

const NS_PER_MS: i64 = 1_000_000;

/// Whether a subtitle codec is text that [`to_track`] understands.
pub fn is_text(codec: &str) -> bool {
    codec == codec::SRT || codec == codec::ASS
}

/// The cues of subtitle stream `si`, or `None` for other kinds of stream.
pub fn to_track(media: &Media, si: usize) -> Option<Track> {
    let s = media.streams.get(si)?;
    if s.kind != Kind::Subtitle {
        return None;
    }
    let mut notes = Notes::new();
    let mut packets: Vec<&Packet> = media.packets_of(si).collect();
    packets.sort_by_key(|p| p.pts);
    let mut track = match s.codec.as_str() {
        codec::SRT => {
            let mut t = Track::default();
            for p in packets {
                let start = p.pts / NS_PER_MS;
                t.cues.push(Cue {
                    start_ms: start,
                    end_ms: start.max((p.pts + p.duration) / NS_PER_MS),
                    text: srt_text_to_inline(&String::from_utf8_lossy(&p.data)),
                    ..Cue::default()
                });
            }
            t
        }
        codec::ASS => {
            let mut text = String::from_utf8_lossy(&s.private).replace("\r\n", "\n");
            if !text.contains("[Events]") {
                text.push_str("\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
            }
            if !text.ends_with('\n') {
                text.push('\n');
            }
            for p in packets {
                let line = String::from_utf8_lossy(&p.data);
                // ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text
                let Some((_, rest)) = line.split_once(',') else {
                    continue;
                };
                let Some((layer, rest)) = rest.split_once(',') else {
                    continue;
                };
                let t = |ns: i64| {
                    let cs = (ns.max(0) / NS_PER_MS + 5) / 10;
                    format!(
                        "{}:{:02}:{:02}.{:02}",
                        cs / 360_000,
                        cs / 6000 % 60,
                        cs / 100 % 60,
                        cs % 100
                    )
                };
                text.push_str(&format!(
                    "Dialogue: {layer},{},{},{rest}\n",
                    t(p.pts),
                    t(p.pts + p.duration)
                ));
            }
            ass::parse(&text, &mut notes)
        }
        _ => return None,
    };
    track.language = Some(s.language.clone()).filter(|l| l != "und");
    track.tidy(&mut notes);
    Some(track)
}

/// A subtitle stream of the given text codec holding `track`'s cues; packets are in time order.
pub fn from_track(track: &Track, target: &str) -> (Stream, Vec<Packet>) {
    let mut s = Stream::new(Kind::Subtitle, target);
    if let Some(l) = &track.language {
        s.language = l.clone();
    }
    let mut packets = Vec::new();
    let mut notes = Notes::new();
    if target == codec::ASS {
        let out = ass::write(track, &mut notes);
        let (head, events) = out.split_once("[Events]\n").unwrap_or((&out, ""));
        let mut lines = events.lines();
        let format = lines.next().unwrap_or("");
        s.private = format!("{head}[Events]\n{format}\n").into_bytes();
        for (i, l) in lines
            .filter_map(|l| l.strip_prefix("Dialogue: "))
            .enumerate()
        {
            let f: Vec<&str> = l.splitn(10, ',').collect();
            if f.len() < 10 {
                continue;
            }
            let cue = &track.cues[i.min(track.cues.len().saturating_sub(1))];
            packets.push(Packet {
                stream: 0,
                pts: cue.start_ms * NS_PER_MS,
                dts: cue.start_ms * NS_PER_MS,
                duration: (cue.end_ms - cue.start_ms).max(0) * NS_PER_MS,
                key: true,
                discard_end: 0,
                data: format!(
                    "{i},{},{},{},{},{},{},{},{}",
                    f[0], f[3], f[4], f[5], f[6], f[7], f[8], f[9]
                )
                .into_bytes(),
            });
        }
    } else {
        for c in &track.cues {
            packets.push(Packet {
                stream: 0,
                pts: c.start_ms * NS_PER_MS,
                dts: c.start_ms * NS_PER_MS,
                duration: (c.end_ms - c.start_ms).max(0) * NS_PER_MS,
                key: true,
                discard_end: 0,
                data: inline_to_srt_text(&c.text).into_bytes(),
            });
        }
    }
    (s, packets)
}
