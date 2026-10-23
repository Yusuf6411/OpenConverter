//! Flash Video (`.flv`), read only: H.264 or HEVC video with AAC, MP3 or PCM audio.
use crate::es;
use crate::{codec, corrupt, estimate_fps, Kind, Media, Packet, Res, Stream};
use oc_core::{Error, Limits};

pub fn sniff(data: &[u8]) -> bool {
    data.len() >= 9 && &data[..3] == b"FLV" && data[3] == 1
}

const MS: i64 = 1_000_000;

pub fn read(data: &[u8], _limits: &Limits) -> Res<Media> {
    if !sniff(data) {
        return Err(corrupt("not an FLV file"));
    }
    let head = u32::from_be_bytes([data[5], data[6], data[7], data[8]]) as usize;
    let mut media = Media {
        source: "flv".into(),
        ..Media::default()
    };
    // Stream slots: video and audio are each created by their first tag.
    let (mut vs, mut au): (Option<usize>, Option<usize>) = (None, None);
    let mut video_len = 4usize;
    let mut skipped = 0;
    let mut pos = head + 4;
    while pos + 11 <= data.len() {
        let ty = data[pos];
        let size = (usize::from(data[pos + 1]) << 16)
            | (usize::from(data[pos + 2]) << 8)
            | usize::from(data[pos + 3]);
        let ts = (i64::from(data[pos + 7]) << 24)
            | (i64::from(data[pos + 4]) << 16)
            | (i64::from(data[pos + 5]) << 8)
            | i64::from(data[pos + 6]);
        let Some(body) = data.get(pos + 11..pos + 11 + size) else {
            break;
        };
        pos += 11 + size + 4;
        match ty {
            9 if !body.is_empty() => {
                let b0 = body[0];
                let key = b0 >> 4 & 7 == 1;
                // Classic tags: codec id in the low nibble, then packet type and a 24-bit
                // composition offset. Enhanced tags (high bit set) name the codec by FourCC.
                let (name, kind, cts, start) = if b0 & 0x80 != 0 {
                    let Some(four) = body.get(1..5) else {
                        continue;
                    };
                    let name = match four {
                        b"hvc1" => codec::HEVC,
                        b"avc1" => codec::H264,
                        _ => {
                            skipped += 1;
                            continue;
                        }
                    };
                    match b0 & 0xF {
                        0 => (name, 0, 0, 5),
                        1 if body.len() >= 8 => (name, 1, sint24(&body[5..8]), 8),
                        3 => (name, 1, 0, 5),
                        _ => continue,
                    }
                } else {
                    let name = match b0 & 0xF {
                        7 => codec::H264,
                        12 => codec::HEVC,
                        _ => {
                            skipped += 1;
                            continue;
                        }
                    };
                    if body.len() < 5 {
                        continue;
                    }
                    (name, body[1], sint24(&body[2..5]), 5)
                };
                let payload = &body[start..];
                if kind == 0 {
                    let si = *vs.get_or_insert_with(|| {
                        media.streams.push(Stream::new(Kind::Video, name));
                        media.streams.len() - 1
                    });
                    media.streams[si].private = payload.to_vec();
                    let (len_size, sets) = if name == codec::H264 {
                        crate::ts::avc_sets(payload)
                    } else {
                        crate::ts::hevc_sets(payload)
                    };
                    video_len = len_size;
                    let info = if name == codec::H264 {
                        sets.first().and_then(|n| es::h264_sps(n))
                    } else {
                        sets.iter()
                            .find(|n| n.first().is_some_and(|b| (b >> 1) & 0x3F == 33))
                            .and_then(|n| es::hevc_sps(n))
                            .map(|h| h.info)
                    };
                    if let Some(i) = info {
                        media.streams[si].width = i.width;
                        media.streams[si].height = i.height;
                    }
                } else if kind == 1 {
                    let Some(si) = vs else {
                        continue;
                    };
                    if video_len != 4 {
                        // Re-prefix with the 4-byte lengths the model uses.
                        let mut v = Vec::with_capacity(payload.len() + 8);
                        for n in es::split_avcc(payload, video_len) {
                            v.extend_from_slice(&(n.len() as u32).to_be_bytes());
                            v.extend_from_slice(n);
                        }
                        media.packets.push(vpacket(si, ts, cts, key, v));
                    } else {
                        media
                            .packets
                            .push(vpacket(si, ts, cts, key, payload.to_vec()));
                    }
                }
            }
            8 if body.len() >= 2 => {
                let fmt = body[0] >> 4;
                let rate = [5512u32, 11025, 22050, 44100][usize::from((body[0] >> 2) & 3)];
                let chans = u16::from(body[0] & 1) + 1;
                match fmt {
                    10 => {
                        let kind = body[1];
                        let payload = &body[2..];
                        if kind == 0 {
                            let si = *au.get_or_insert_with(|| {
                                media.streams.push(Stream::new(Kind::Audio, codec::AAC));
                                media.streams.len() - 1
                            });
                            media.streams[si].private = payload.to_vec();
                            if payload.len() >= 2 {
                                let v = (u16::from(payload[0]) << 8) | u16::from(payload[1]);
                                let idx = usize::from((v >> 7) & 0xF);
                                const R: [u32; 13] = [
                                    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000,
                                    12000, 11025, 8000, 7350,
                                ];
                                media.streams[si].sample_rate = R.get(idx).copied().unwrap_or(rate);
                                media.streams[si].channels = ((v >> 3) & 0xF).min(7);
                                if media.streams[si].channels == 7 {
                                    media.streams[si].channels = 8;
                                }
                            }
                        } else if let Some(si) = au {
                            media.packets.push(apacket(si, ts, payload.to_vec()));
                        }
                    }
                    2 | 14 => {
                        let payload = &body[1..];
                        let si = *au.get_or_insert_with(|| {
                            let mut s = Stream::new(Kind::Audio, codec::MP3);
                            if let Some(f) = es::mp3_frames(payload) {
                                s.sample_rate = f.sample_rate;
                                s.channels = f.channels;
                            } else {
                                s.sample_rate = rate;
                                s.channels = chans;
                            }
                            media.streams.push(s);
                            media.streams.len() - 1
                        });
                        media.packets.push(apacket(si, ts, payload.to_vec()));
                    }
                    3 => {
                        let bits = if body[0] & 2 != 0 { 16 } else { 8 };
                        let payload = &body[1..];
                        let si = *au.get_or_insert_with(|| {
                            let mut s = Stream::new(
                                Kind::Audio,
                                if bits == 16 {
                                    codec::PCM_S16
                                } else {
                                    codec::PCM_U8
                                },
                            );
                            s.sample_rate = rate;
                            s.channels = chans;
                            s.bits = bits;
                            media.streams.push(s);
                            media.streams.len() - 1
                        });
                        media.packets.push(apacket(si, ts, payload.to_vec()));
                    }
                    _ => skipped += 1,
                }
            }
            _ => {}
        }
    }
    if media.packets.is_empty() {
        return Err(if skipped > 0 {
            crate::unsupported("this FLV file uses a codec this build cannot carry")
        } else {
            Error::corrupt("FLV file has no media")
        });
    }
    // Video decode times are the tag times; audio is a plain sequence.
    for si in 0..media.streams.len() {
        if media.streams[si].kind == Kind::Video {
            let pts: Vec<i64> = media.packets_of(si).map(|p| p.pts).collect();
            media.streams[si].fps = estimate_fps(&pts);
        }
    }
    media.packets.retain(|p| p.stream < media.streams.len());
    media.sort_by_dts();
    media.fill_durations();
    Ok(media)
}

fn sint24(b: &[u8]) -> i64 {
    let v = (i32::from(b[0]) << 16) | (i32::from(b[1]) << 8) | i32::from(b[2]);
    i64::from((v << 8) >> 8)
}

fn vpacket(stream: usize, ts: i64, cts: i64, key: bool, data: Vec<u8>) -> Packet {
    Packet {
        stream,
        pts: (ts + cts) * MS,
        dts: ts * MS,
        duration: 0,
        key,
        discard_end: 0,
        data,
    }
}

fn apacket(stream: usize, ts: i64, data: Vec<u8>) -> Packet {
    Packet {
        stream,
        pts: ts * MS,
        dts: ts * MS,
        duration: 0,
        key: true,
        discard_end: 0,
        data,
    }
}
