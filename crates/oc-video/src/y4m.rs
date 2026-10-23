//! YUV4MPEG2 (`.y4m`): uncompressed planar video with a text header.
use crate::{codec, corrupt, div_round, frame_ns, unsupported, Kind, Media, Packet, Res, Stream};
use oc_core::{Error, ErrorCode, Limits};

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(b"YUV4MPEG2 ") || data.starts_with(b"YUV4MPEG2\n")
}

/// Bytes of one frame, and the pixel format name, for a chroma tag.
pub fn frame_size(pix_fmt: &str, w: usize, h: usize) -> Option<usize> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    match pix_fmt {
        "yuv420p" => Some(w * h + 2 * cw * ch),
        "yuv422p" => Some(w * h + 2 * cw * h),
        "yuv444p" => Some(3 * w * h),
        "gray" => Some(w * h),
        _ => None,
    }
}

pub fn read(data: &[u8], limits: &Limits) -> Res<Media> {
    let eol = data
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| Error::truncated("Y4M header"))?;
    let head = std::str::from_utf8(&data[..eol]).map_err(|_| corrupt("Y4M header is not text"))?;
    let (mut w, mut h) = (0usize, 0usize);
    let mut fps = (25u32, 1u32);
    let mut pix = "yuv420p";
    for tok in head.split_whitespace().skip(1) {
        let (k, v) = tok.split_at(1);
        match k {
            "W" => w = v.parse().map_err(|_| corrupt("bad Y4M width"))?,
            "H" => h = v.parse().map_err(|_| corrupt("bad Y4M height"))?,
            "F" => {
                let (n, d) = v.split_once(':').ok_or_else(|| corrupt("bad Y4M frame rate"))?;
                fps = (n.parse().map_err(|_| corrupt("bad Y4M rate"))?, d.parse().map_err(|_| corrupt("bad Y4M rate"))?);
            }
            "C" => {
                pix = match v {
                    "mono" => "gray",
                    s if s.starts_with("420") && !s.starts_with("420p1") => "yuv420p",
                    s if s == "422" => "yuv422p",
                    s if s == "444" => "yuv444p",
                    _ => return Err(unsupported(format!("Y4M colour space {v} is not supported (8-bit only)"))),
                }
            }
            _ => {}
        }
    }
    if w == 0 || h == 0 || fps.0 == 0 || fps.1 == 0 {
        return Err(corrupt("Y4M header lacks size or frame rate"));
    }
    limits.check_pixels(w as u32, h as u32)?;
    let size = frame_size(pix, w, h).ok_or_else(|| unsupported("Y4M pixel format"))?;
    let mut s = Stream::new(Kind::Video, codec::RAW);
    s.width = w as u32;
    s.height = h as u32;
    s.fps = Some(fps);
    s.pix_fmt = pix.into();
    let mut media = Media {
        streams: vec![s],
        source: "y4m".into(),
        ..Media::default()
    };
    let per = frame_ns(fps);
    let mut pos = eol + 1;
    let mut n = 0i64;
    while pos < data.len() {
        if !data[pos..].starts_with(b"FRAME") {
            return Err(corrupt("expected FRAME marker"));
        }
        let e = data[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .ok_or_else(|| Error::truncated("FRAME line"))?;
        let start = pos + e + 1;
        let Some(body) = data.get(start..start + size) else {
            break; // a truncated last frame is dropped
        };
        limits.check_entries(n as u64)?;
        let t = div_round(i128::from(n) * 1_000_000_000 * i128::from(fps.1), i128::from(fps.0));
        media.packets.push(Packet {
            stream: 0,
            pts: t,
            dts: t,
            duration: per,
            key: true,
            discard_end: 0,
            data: body.to_vec(),
        });
        pos = start + size;
        n += 1;
    }
    if media.packets.is_empty() {
        return Err(Error::new(ErrorCode::TruncatedFile, "Y4M has no complete frame"));
    }
    Ok(media)
}

pub fn write(media: &Media) -> Res<Vec<u8>> {
    let s = media
        .streams
        .iter()
        .find(|s| s.kind == Kind::Video && s.codec == codec::RAW)
        .ok_or_else(|| unsupported("Y4M holds one uncompressed video stream"))?;
    if media.streams.len() != 1 {
        return Err(unsupported("Y4M holds exactly one stream"));
    }
    let fps = s.fps.unwrap_or((25, 1));
    let tag = match s.pix_fmt.as_str() {
        "yuv420p" => "420jpeg",
        "yuv422p" => "422",
        "yuv444p" => "444",
        "gray" => "mono",
        other => return Err(unsupported(format!("Y4M cannot hold {other}"))),
    };
    let mut out = format!("YUV4MPEG2 W{} H{} F{}:{} Ip A1:1 C{tag}\n", s.width, s.height, fps.0, fps.1).into_bytes();
    let want = frame_size(&s.pix_fmt, s.width as usize, s.height as usize).ok_or_else(|| unsupported("pixel format"))?;
    for p in &media.packets {
        if p.data.len() != want {
            return Err(corrupt("a raw frame has the wrong size"));
        }
        out.extend_from_slice(b"FRAME\n");
        out.extend_from_slice(&p.data);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut s = Stream::new(Kind::Video, codec::RAW);
        s.width = 4;
        s.height = 2;
        s.fps = Some((30000, 1001));
        s.pix_fmt = "yuv420p".into();
        let mut m = Media { streams: vec![s], ..Media::default() };
        for i in 0..3u8 {
            m.packets.push(Packet { stream: 0, pts: 0, dts: 0, duration: 0, key: true, discard_end: 0, data: vec![i; 8 + 2 * 2 * 1] });
        }
        let bytes = write(&m).unwrap_or_default();
        assert!(sniff(&bytes));
        let back = read(&bytes, &Limits::default()).unwrap_or_default();
        assert_eq!(back.packets.len(), 3);
        assert_eq!(back.streams[0].fps, Some((30000, 1001)));
        assert_eq!(back.packets[2].data, m.packets[2].data);
        assert_eq!(back.packets[1].pts, 33_366_667);
    }
}
