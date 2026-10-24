//! APEv2 tags (as found at the end of MP3, Musepack and Monkey's Audio files) and their mapping
//! onto the common tag model.
//!
//! Item keys are mapped to the Vorbis vocabulary the model uses: `Year` is `DATE`, `Track`
//! is `TRACKNUMBER`, `Album Artist` is `ALBUMARTIST`, and the binary `Cover Art (Front)` item
//! becomes a picture.
use crate::tags::{Picture, Tags};

const MAP: [(&str, &str); 6] = [
    ("YEAR", "DATE"),
    ("TRACK", "TRACKNUMBER"),
    ("DISC", "DISCNUMBER"),
    ("ALBUM ARTIST", "ALBUMARTIST"),
    ("COMMENT", "COMMENT"),
    ("RECORD DATE", "DATE"),
];

fn to_model(key: &str) -> String {
    let up = key.to_ascii_uppercase();
    MAP.iter()
        .find(|(a, _)| *a == up)
        .map_or_else(|| up.replace(' ', ""), |(_, b)| (*b).to_string())
}

fn from_model(key: &str) -> String {
    match key {
        "DATE" => "Year".into(),
        "TRACKNUMBER" => "Track".into(),
        "DISCNUMBER" => "Disc".into(),
        "ALBUMARTIST" => "Album Artist".into(),
        k => {
            // Title, Artist, Album, Genre, Comment: capitalised like other APE writers do.
            let mut c = k.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase())
                .unwrap_or_default()
        }
    }
}

/// Find the footer (before an optional ID3v1 tag) and return the item bytes and their count.
fn locate(data: &[u8]) -> Option<(&[u8], usize)> {
    let end = if data.len() >= 128 && &data[data.len() - 128..data.len() - 125] == b"TAG" {
        data.len() - 128
    } else {
        data.len()
    };
    if end < 32 || &data[end - 32..end - 24] != b"APETAGEX" {
        return None;
    }
    let f = &data[end - 32..end];
    let size = u32::from_le_bytes([f[12], f[13], f[14], f[15]]) as usize;
    let count = u32::from_le_bytes([f[16], f[17], f[18], f[19]]) as usize;
    // `size` covers the items and the footer, not a header.
    if size < 32 || size > end {
        return None;
    }
    Some((&data[end - size..end - 32], count.min(1024)))
}

/// Read the tag at the end of `data` into `tags` and `pictures`; returns whether one was found.
pub fn parse(data: &[u8], tags: &mut Tags, pictures: &mut Vec<Picture>) -> bool {
    let Some((items, count)) = locate(data) else {
        return false;
    };
    let mut p = 0usize;
    for _ in 0..count {
        let Some(h) = items.get(p..p + 8) else { break };
        let vlen = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
        let flags = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
        let rest = &items[p + 8..];
        let Some(kend) = rest.iter().position(|&c| c == 0) else {
            break;
        };
        let key = String::from_utf8_lossy(&rest[..kend]).into_owned();
        let Some(val) = rest.get(kend + 1..kend + 1 + vlen) else {
            break;
        };
        p += 8 + kend + 1 + vlen;
        match (flags >> 1) & 3 {
            0 => {
                for part in String::from_utf8_lossy(val).split('\0') {
                    tags.add(&to_model(&key), part);
                }
            }
            1 if key.to_ascii_lowercase().starts_with("cover art") => {
                // description, NUL, image bytes
                if let Some(z) = val.iter().position(|&c| c == 0) {
                    let img = &val[z + 1..];
                    let mime = if img.starts_with(&[0x89, b'P', b'N', b'G']) {
                        "image/png"
                    } else {
                        "image/jpeg"
                    };
                    pictures.push(Picture {
                        mime: mime.into(),
                        kind: if key.to_ascii_lowercase().contains("front") {
                            3
                        } else {
                            4
                        },
                        description: String::from_utf8_lossy(&val[..z]).into_owned(),
                        data: img.to_vec(),
                    });
                }
            }
            _ => {}
        }
    }
    true
}

/// An APEv2 tag (header, items, footer) for `tags` and `pictures`.
pub fn write(tags: &Tags, pictures: &[Picture]) -> Vec<u8> {
    let mut items = Vec::new();
    let mut count = 0u32;
    let mut item = |key: &str, flags: u32, val: &[u8]| {
        items.extend_from_slice(&(val.len() as u32).to_le_bytes());
        items.extend_from_slice(&flags.to_le_bytes());
        items.extend_from_slice(key.as_bytes());
        items.push(0);
        items.extend_from_slice(val);
        count += 1;
    };
    // Repeated keys become one item with NUL-separated values.
    let mut keys: Vec<&str> = Vec::new();
    for (k, _) in &tags.fields {
        if !keys.contains(&k.as_str()) {
            keys.push(k);
        }
    }
    for k in keys {
        let vals = tags.all(k).join("\0");
        item(&from_model(k), 0, vals.as_bytes());
    }
    for p in pictures {
        let mut v = p.description.as_bytes().to_vec();
        v.push(0);
        v.extend_from_slice(&p.data);
        let name = if p.kind == 3 {
            "Cover Art (Front)"
        } else {
            "Cover Art (Back)"
        };
        item(name, 2, &v);
    }
    let size = (items.len() + 32) as u32;
    let block = |flags: u32| {
        let mut b = b"APETAGEX".to_vec();
        b.extend_from_slice(&2000u32.to_le_bytes());
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&[0; 8]);
        b
    };
    let mut out = block(0xA000_0000); // header: has header and footer, is the header
    out.extend_from_slice(&items);
    out.extend_from_slice(&block(0x8000_0000));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_round_trip_through_the_vorbis_vocabulary() {
        let mut t = Tags::default();
        t.add("TITLE", "Song");
        t.add("ARTIST", "A");
        t.add("ARTIST", "B");
        t.add("DATE", "1999");
        t.add("TRACKNUMBER", "4/9");
        t.add("ALBUMARTIST", "Band");
        let pics = vec![Picture {
            mime: "image/png".into(),
            kind: 3,
            description: "front".into(),
            data: vec![0x89, b'P', b'N', b'G', 1, 2],
        }];
        let mut file = vec![0xFF, 0xFB, 0x90, 0x00, 0, 0, 0, 0];
        file.extend_from_slice(&write(&t, &pics));
        let mut t2 = Tags::default();
        let mut p2 = Vec::new();
        assert!(parse(&file, &mut t2, &mut p2));
        assert_eq!(t2.get("TITLE"), Some("Song"));
        assert_eq!(t2.all("ARTIST"), vec!["A", "B"]);
        assert_eq!(t2.get("DATE"), Some("1999"));
        assert_eq!(t2.get("TRACKNUMBER"), Some("4/9"));
        assert_eq!(t2.get("ALBUMARTIST"), Some("Band"));
        assert_eq!(p2, pics);
        // damaged footers are ignored
        let cut = &file[..file.len() - 5];
        assert!(!parse(cut, &mut Tags::default(), &mut Vec::new()));
    }
}
