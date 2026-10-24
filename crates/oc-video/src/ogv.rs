//! Ogg video as [`Media`] (read only): Theora video with Vorbis audio.
//!
//! Theora packets are one frame each, so their times follow from the frame rate. Vorbis packet
//! times come from the block sizes the setup header declares.
use crate::theora::xiph_lace;
use crate::{codec, corrupt, div_round, frame_ns, Kind, Media, Packet, Res, Stream};
use oc_core::{Error, ErrorCode, Limits};

/// Whether the beginning-of-stream pages of an Ogg file include a Theora identification header.
pub fn sniff(data: &[u8]) -> bool {
    let mut o = 0usize;
    while data.get(o..o + 4) == Some(b"OggS") {
        let Some(&flags) = data.get(o + 5) else {
            return false;
        };
        let Some(&nseg) = data.get(o + 26) else {
            return false;
        };
        let start = o + 27 + usize::from(nseg);
        let Some(table) = data.get(o + 27..start) else {
            return false;
        };
        if flags & 2 == 0 {
            return false;
        }
        if data
            .get(start..start + 7)
            .is_some_and(oc_theora::is_identification)
        {
            return true;
        }
        o = start + table.iter().map(|&b| usize::from(b)).sum::<usize>();
    }
    false
}

pub fn read(data: &[u8], limits: &Limits) -> Res<Media> {
    let (logical, _damaged) = oc_audio::ogg::read(data)?;
    let mut media = Media {
        source: "ogg".into(),
        ..Media::default()
    };
    for ls in &logical {
        let Some(first) = ls.packets.first() else {
            continue;
        };
        if oc_theora::is_identification(&first.data) {
            add_theora(&mut media, &ls.packets, limits)?;
        } else if first.data.starts_with(b"\x01vorbis") {
            add_vorbis(&mut media, &ls.packets)?;
        }
    }
    if media.streams.is_empty() {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "this Ogg file holds no Theora or Vorbis stream",
        ));
    }
    media.sort_by_dts();
    media.fill_durations();
    Ok(media)
}

fn add_theora(media: &mut Media, packets: &[oc_audio::ogg::Packet], limits: &Limits) -> Res<()> {
    if packets.len() < 3 {
        return Err(corrupt("Theora stream has no setup header"));
    }
    let headers = oc_theora::parse_headers(&packets[0].data, &packets[1].data, &packets[2].data)?;
    let info = headers.info.clone();
    // Reject streams the decoder would refuse (and absurd sizes) before any packet is kept.
    oc_theora::decoder(headers, limits)?;
    let mut s = Stream::new(Kind::Video, codec::THEORA);
    s.private = xiph_lace(&[&packets[0].data, &packets[1].data, &packets[2].data]);
    s.width = info.pic_w;
    s.height = info.pic_h;
    s.pix_fmt = match info.pixel_format {
        0 => "yuv420p",
        2 => "yuv422p",
        _ => "yuv444p",
    }
    .into();
    let fps = (info.fps_num > 0 && info.fps_den > 0).then_some((info.fps_num, info.fps_den));
    s.fps = fps;
    let idx = media.streams.len();
    media.streams.push(s);
    let step = frame_ns(fps.unwrap_or((25, 1)));
    let mut n = 0i64;
    for p in &packets[3..] {
        // Further header packets (type bit set) carry nothing a decoder needs.
        if p.data.first().is_some_and(|b| b & 0x80 != 0) {
            continue;
        }
        let pts = div_round(
            i128::from(n) * 1_000_000_000 * i128::from(info.fps_den.max(1)),
            i128::from(info.fps_num.max(1)),
        );
        media.packets.push(Packet {
            stream: idx,
            pts,
            dts: pts,
            duration: step,
            key: p.data.first().is_some_and(|b| b & 0x40 == 0),
            discard_end: 0,
            data: p.data.clone(),
        });
        n += 1;
    }
    Ok(())
}

fn add_vorbis(media: &mut Media, packets: &[oc_audio::ogg::Packet]) -> Res<()> {
    if packets.len() < 3 {
        return Err(corrupt("Vorbis stream has no setup header"));
    }
    let id = oc_audio::vorbis::parse_identification(&packets[0].data)?;
    let body: Vec<&[u8]> = packets[3..].iter().map(|p| p.data.as_slice()).collect();
    let durations = oc_audio::vorbis::packet_durations(&packets[0].data, &packets[2].data, &body)?;
    let mut s = Stream::new(Kind::Audio, codec::VORBIS);
    s.private = xiph_lace(&[&packets[0].data, &packets[1].data, &packets[2].data]);
    s.sample_rate = id.sample_rate;
    s.channels = id.channels as u16;
    let idx = media.streams.len();
    media.streams.push(s);
    let rate = i128::from(id.sample_rate.max(1));
    let mut pos = 0i128;
    for (p, d) in packets[3..].iter().zip(durations) {
        let pts = div_round(pos * 1_000_000_000, rate);
        let end = div_round((pos + d as i128) * 1_000_000_000, rate);
        media.packets.push(Packet {
            stream: idx,
            pts,
            dts: pts,
            duration: end - pts,
            key: true,
            discard_end: 0,
            data: p.data.clone(),
        });
        pos += d as i128;
    }
    Ok(())
}
