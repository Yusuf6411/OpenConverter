//! ID3v2 (2.2, 2.3, 2.4) and ID3v1 reading, ID3v2.4 writing, mapped onto the neutral tag model.

use crate::tags::{Picture, Tags, ID3_GENRES};

/// Total size of a leading ID3v2 tag (header included), or 0 when there is none.
pub fn v2_len(data: &[u8]) -> usize {
    if data.len() >= 10 && &data[..3] == b"ID3" && data[3] < 0xFF && data[4] < 0xFF {
        let s = &data[6..10];
        if s.iter().all(|&b| b < 0x80) {
            let size = (usize::from(s[0]) << 21)
                | (usize::from(s[1]) << 14)
                | (usize::from(s[2]) << 7)
                | usize::from(s[3]);
            let footer = if data[3] == 4 && data[5] & 0x10 != 0 {
                10
            } else {
                0
            };
            return (10 + size + footer).min(data.len());
        }
    }
    0
}

const FRAME_KEYS: [(&str, &str); 20] = [
    ("TIT2", "TITLE"),
    ("TPE1", "ARTIST"),
    ("TALB", "ALBUM"),
    ("TPE2", "ALBUMARTIST"),
    ("TCOM", "COMPOSER"),
    ("TPE3", "CONDUCTOR"),
    ("TCON", "GENRE"),
    ("TDRC", "DATE"),
    ("TRCK", "TRACKNUMBER"),
    ("TPOS", "DISCNUMBER"),
    ("TPUB", "ORGANIZATION"),
    ("TCOP", "COPYRIGHT"),
    ("TSSE", "ENCODER"),
    ("TBPM", "BPM"),
    ("TSRC", "ISRC"),
    ("TLAN", "LANGUAGE"),
    ("TIT3", "SUBTITLE"),
    ("TOPE", "ORIGINALARTIST"),
    ("TKEY", "INITIALKEY"),
    ("TSOP", "ARTISTSORT"),
];

fn v22_to_v24(id: &[u8]) -> Option<&'static str> {
    Some(match id {
        b"TT2" => "TIT2",
        b"TP1" => "TPE1",
        b"TAL" => "TALB",
        b"TP2" => "TPE2",
        b"TCM" => "TCOM",
        b"TP3" => "TPE3",
        b"TCO" => "TCON",
        b"TYE" => "TYER",
        b"TRK" => "TRCK",
        b"TPA" => "TPOS",
        b"TPB" => "TPUB",
        b"TCR" => "TCOP",
        b"TSS" => "TSSE",
        b"TBP" => "TBPM",
        b"TRC" => "TSRC",
        b"TLA" => "TLAN",
        b"COM" => "COMM",
        b"ULT" => "USLT",
        b"PIC" => "APIC",
        b"TXX" => "TXXX",
        _ => return None,
    })
}

fn decode_text(enc: u8, b: &[u8]) -> String {
    match enc {
        1 | 2 => {
            let (big, body) = match b {
                [0xFF, 0xFE, rest @ ..] => (false, rest),
                [0xFE, 0xFF, rest @ ..] => (true, rest),
                _ => (enc == 2, b),
            };
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| {
                    if big {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
        3 => String::from_utf8_lossy(b).into_owned(),
        _ => b.iter().map(|&c| char::from(c)).collect(), // ISO-8859-1
    }
}

/// Split at the first terminator valid for the encoding; returns (before, after).
fn split_term(enc: u8, b: &[u8]) -> (&[u8], &[u8]) {
    if enc == 1 || enc == 2 {
        let mut i = 0;
        while i + 1 < b.len() {
            if b[i] == 0 && b[i + 1] == 0 {
                return (&b[..i], &b[i + 2..]);
            }
            i += 2;
        }
        (b, &[])
    } else {
        match b.iter().position(|&c| c == 0) {
            Some(p) => (&b[..p], &b[p + 1..]),
            None => (b, &[]),
        }
    }
}

fn unsync(b: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        o.push(b[i]);
        if b[i] == 0xFF && b.get(i + 1) == Some(&0) {
            i += 1;
        }
        i += 1;
    }
    o
}

fn genre_name(s: &str) -> String {
    // "(13)", "(13)Rock", "13"
    let t = s.trim();
    let digits = t.trim_start_matches('(').split(')').next().unwrap_or("");
    if let Ok(n) = digits.parse::<usize>() {
        let rest = t.split(')').nth(1).unwrap_or("").trim();
        if !rest.is_empty() && t.starts_with('(') {
            return rest.to_string();
        }
        if let Some(g) = ID3_GENRES.get(n) {
            return (*g).to_string();
        }
    }
    t.to_string()
}

fn parse_picture(v22: bool, enc: u8, b: &[u8]) -> Option<Picture> {
    let (mime, rest) = if v22 {
        let fmt = b.get(..3)?;
        let m = match fmt {
            b"PNG" => "image/png",
            b"JPG" => "image/jpeg",
            _ => "image/",
        };
        (m.to_string(), b.get(3..)?)
    } else {
        let (m, r) = split_term(0, b);
        (String::from_utf8_lossy(m).into_owned(), r)
    };
    let kind = *rest.first()?;
    let (desc, data) = split_term(enc, rest.get(1..)?);
    let mime = if mime.is_empty() || mime == "-->" {
        if data.starts_with(&[0x89, b'P', b'N', b'G']) {
            "image/png".to_string()
        } else {
            "image/jpeg".to_string()
        }
    } else {
        mime
    };
    Some(Picture {
        mime,
        kind,
        description: decode_text(enc, desc),
        data: data.to_vec(),
    })
}

/// Parse a leading ID3v2 tag into `tags` and `pictures`.
pub fn parse_v2(data: &[u8], tags: &mut Tags, pictures: &mut Vec<Picture>) {
    let total = v2_len(data);
    if total < 10 {
        return;
    }
    let ver = data[3];
    let flags = data[5];
    let mut body = data[10..total].to_vec();
    if flags & 0x80 != 0 && ver != 4 {
        body = unsync(&body);
    }
    let mut pos = 0usize;
    if flags & 0x40 != 0 && body.len() >= 4 {
        // Extended header.
        let sz = if ver == 4 {
            (usize::from(body[0] & 0x7F) << 21)
                | (usize::from(body[1] & 0x7F) << 14)
                | (usize::from(body[2] & 0x7F) << 7)
                | usize::from(body[3] & 0x7F)
        } else {
            4 + ((usize::from(body[0]) << 24)
                | (usize::from(body[1]) << 16)
                | (usize::from(body[2]) << 8)
                | usize::from(body[3]))
        };
        pos = sz.min(body.len());
    }
    let v22 = ver == 2;
    let hdr = if v22 { 6 } else { 10 };
    while pos + hdr <= body.len() {
        let id = &body[pos..pos + if v22 { 3 } else { 4 }];
        if id[0] == 0 {
            break;
        }
        let size = if v22 {
            (usize::from(body[pos + 3]) << 16)
                | (usize::from(body[pos + 4]) << 8)
                | usize::from(body[pos + 5])
        } else if ver == 4 {
            (usize::from(body[pos + 4] & 0x7F) << 21)
                | (usize::from(body[pos + 5] & 0x7F) << 14)
                | (usize::from(body[pos + 6] & 0x7F) << 7)
                | usize::from(body[pos + 7] & 0x7F)
        } else {
            (usize::from(body[pos + 4]) << 24)
                | (usize::from(body[pos + 5]) << 16)
                | (usize::from(body[pos + 6]) << 8)
                | usize::from(body[pos + 7])
        };
        let fflags = if v22 {
            0
        } else {
            u16::from_be_bytes([body[pos + 8], body[pos + 9]])
        };
        let start = pos + hdr;
        let Some(raw) = body.get(start..start + size) else {
            break;
        };
        pos = start + size;
        let mut payload = raw.to_vec();
        if ver == 4 {
            if fflags & 0x0002 != 0 {
                payload = unsync(&payload);
            }
            if fflags & 0x0001 != 0 && payload.len() >= 4 {
                payload.drain(..4); // data length indicator
            }
            if fflags & 0x0008 != 0 {
                continue; // compressed frames are not supported
            }
        } else if ver == 3 && fflags & 0x00C0 != 0 {
            continue; // compressed or encrypted
        }
        let id_str = std::str::from_utf8(id).unwrap_or("");
        let id24: &str = if v22 {
            v22_to_v24(id).unwrap_or("")
        } else {
            id_str
        };
        frame_to_tags(id24, v22, &payload, tags, pictures);
    }
}

fn frame_to_tags(id: &str, v22: bool, p: &[u8], tags: &mut Tags, pictures: &mut Vec<Picture>) {
    let Some((&enc, rest)) = p.split_first() else {
        return;
    };
    match id {
        "TXXX" => {
            let (d, v) = split_term(enc, rest);
            let key = decode_text(enc, d);
            let val = decode_text(enc, v);
            if !key.is_empty() {
                for part in val.split('\0') {
                    tags.add(&key, part.trim_end_matches('\0'));
                }
            }
        }
        "COMM" | "USLT" => {
            if rest.len() < 3 {
                return;
            }
            let (_, after) = rest.split_at(3); // language
            let (d, v) = split_term(enc, after);
            let key = if id == "COMM" { "COMMENT" } else { "LYRICS" };
            let desc = decode_text(enc, d);
            let val = decode_text(enc, v);
            // iTunes-style descriptions ("iTunNORM") are bookkeeping, not comments.
            if desc.is_empty() || desc == "Comment" || id == "USLT" {
                tags.add(key, val.trim_end_matches('\0'));
            }
        }
        "APIC" => {
            if let Some(pic) = parse_picture(v22, enc, rest) {
                pictures.push(pic);
            }
        }
        "TYER" => tags.add("DATE", decode_text(enc, rest).trim_end_matches('\0')),
        "TCON" => {
            for part in decode_text(enc, rest).split('\0') {
                if !part.is_empty() {
                    tags.add("GENRE", genre_name(part));
                }
            }
        }
        _ if id.starts_with('T') && id != "TXXX" => {
            if let Some((_, key)) = FRAME_KEYS.iter().find(|(f, _)| *f == id) {
                for part in decode_text(enc, rest).split('\0') {
                    let part = part.trim();
                    if !part.is_empty() {
                        tags.add(key, part);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Read a trailing 128-byte ID3v1 tag; returns the tag and whether it was present.
pub fn parse_v1(data: &[u8], tags: &mut Tags) -> bool {
    if data.len() < 128 || &data[data.len() - 128..data.len() - 125] != b"TAG" {
        return false;
    }
    let t = &data[data.len() - 128..];
    let text = |r: std::ops::Range<usize>| -> String {
        let s: String = t[r]
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| char::from(c))
            .collect();
        s.trim().to_string()
    };
    if tags.get("TITLE").is_none() {
        tags.add("TITLE", text(3..33));
    }
    if tags.get("ARTIST").is_none() {
        tags.add("ARTIST", text(33..63));
    }
    if tags.get("ALBUM").is_none() {
        tags.add("ALBUM", text(63..93));
    }
    if tags.get("DATE").is_none() {
        tags.add("DATE", text(93..97));
    }
    if t[125] == 0 && t[126] != 0 {
        if tags.get("TRACKNUMBER").is_none() {
            tags.add("TRACKNUMBER", t[126].to_string());
        }
        if tags.get("COMMENT").is_none() {
            tags.add("COMMENT", text(97..125));
        }
    } else if tags.get("COMMENT").is_none() {
        tags.add("COMMENT", text(97..127));
    }
    if tags.get("GENRE").is_none() {
        if let Some(g) = ID3_GENRES.get(usize::from(t[127])) {
            tags.add("GENRE", *g);
        }
    }
    true
}

// ---------------------------------------------------------------- writing

fn synchsafe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

fn frame(id: &str, body: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(10 + body.len());
    f.extend_from_slice(id.as_bytes());
    f.extend_from_slice(&synchsafe(body.len()));
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(body);
    f
}

fn text_frame(id: &str, values: &[&str]) -> Vec<u8> {
    let mut b = vec![3u8];
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            b.push(0);
        }
        b.extend_from_slice(v.as_bytes());
    }
    frame(id, &b)
}

/// An ID3v2.4 tag (UTF-8) holding `tags` and `pictures`; empty when there is nothing to store.
pub fn write_v2(tags: &Tags, pictures: &[Picture], padding: usize) -> Vec<u8> {
    let mut frames = Vec::new();
    let mut done: Vec<String> = Vec::new();
    let track_total = tags.get("TRACKTOTAL").or_else(|| tags.get("TOTALTRACKS"));
    let disc_total = tags.get("DISCTOTAL").or_else(|| tags.get("TOTALDISCS"));
    for (key, _) in &tags.fields {
        let key = key.as_str();
        if done.iter().any(|d| d == key) {
            continue;
        }
        done.push(key.to_string());
        let vals = tags.all(key);
        match key {
            "TRACKTOTAL" | "TOTALTRACKS" | "DISCTOTAL" | "TOTALDISCS" => {
                // Folded into TRCK / TPOS when the number is present, kept as text otherwise.
                let has_num = if key.contains("TRACK") {
                    tags.get("TRACKNUMBER").is_some()
                } else {
                    tags.get("DISCNUMBER").is_some()
                };
                if !has_num {
                    frames.extend(frame_txxx(key, &vals));
                }
            }
            "TRACKNUMBER" | "DISCNUMBER" => {
                let (id, total) = if key == "TRACKNUMBER" {
                    ("TRCK", track_total)
                } else {
                    ("TPOS", disc_total)
                };
                let v = vals[0];
                let joined = match total {
                    Some(t) if !v.contains('/') => format!("{v}/{t}"),
                    _ => v.to_string(),
                };
                frames.extend(text_frame(id, &[&joined]));
            }
            "COMMENT" | "LYRICS" => {
                for v in vals {
                    let mut b = vec![3u8];
                    b.extend_from_slice(b"XXX");
                    b.push(0);
                    b.extend_from_slice(v.as_bytes());
                    frames.extend(frame(if key == "COMMENT" { "COMM" } else { "USLT" }, &b));
                }
            }
            _ => match FRAME_KEYS.iter().find(|(_, k)| *k == key) {
                Some((id, _)) => frames.extend(text_frame(id, &vals)),
                None => frames.extend(frame_txxx(key, &vals)),
            },
        }
    }
    for p in pictures {
        let mut b = vec![3u8];
        b.extend_from_slice(p.mime.as_bytes());
        b.push(0);
        b.push(p.kind);
        b.extend_from_slice(p.description.as_bytes());
        b.push(0);
        b.extend_from_slice(&p.data);
        frames.extend(frame("APIC", &b));
    }
    if frames.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(10 + frames.len() + padding);
    out.extend_from_slice(b"ID3\x04\x00\x00");
    out.extend_from_slice(&synchsafe(frames.len() + padding));
    out.extend_from_slice(&frames);
    out.resize(out.len() + padding, 0);
    out
}

fn frame_txxx(key: &str, vals: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in vals {
        let mut b = vec![3u8];
        b.extend_from_slice(key.as_bytes());
        b.push(0);
        b.extend_from_slice(v.as_bytes());
        out.extend(frame("TXXX", &b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut t = Tags::default();
        t.add("TITLE", "Tést ♪");
        t.add("ARTIST", "A");
        t.add("ARTIST", "B");
        t.add("TRACKNUMBER", "3");
        t.add("TRACKTOTAL", "12");
        t.add("COMMENT", "hello");
        t.add("MYKEY", "mine");
        let pic = Picture {
            mime: "image/png".into(),
            kind: 3,
            description: "front".into(),
            data: vec![1, 2, 3],
        };
        let bytes = write_v2(&t, std::slice::from_ref(&pic), 16);
        assert_eq!(v2_len(&bytes), bytes.len());
        let mut back = Tags::default();
        let mut pics = Vec::new();
        parse_v2(&bytes, &mut back, &mut pics);
        assert_eq!(back.get("TITLE"), Some("Tést ♪"));
        assert_eq!(back.all("ARTIST"), vec!["A", "B"]);
        assert_eq!(back.get("TRACKNUMBER"), Some("3/12"));
        assert_eq!(back.get("COMMENT"), Some("hello"));
        assert_eq!(back.get("MYKEY"), Some("mine"));
        assert_eq!(pics, vec![pic]);
    }

    #[test]
    fn genre_numbers() {
        assert_eq!(genre_name("(13)"), "Pop");
        assert_eq!(genre_name("17"), "Rock");
        assert_eq!(genre_name("(17)Rock"), "Rock");
        assert_eq!(genre_name("Ambient"), "Ambient");
    }
}
