//! A format-neutral tag model. Keys follow the Vorbis comment convention (upper case), which
//! is also the vocabulary the other tag systems map onto.

/// Embedded picture (cover art).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub mime: String,
    /// ID3v2 picture type (3 = front cover).
    pub kind: u8,
    pub description: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags {
    /// (KEY, value) pairs in file order; keys may repeat.
    pub fields: Vec<(String, String)>,
}

impl Tags {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
    pub fn add(&mut self, key: &str, value: impl Into<String>) {
        let v = value.into();
        if !v.is_empty() {
            self.fields.push((key.to_ascii_uppercase(), v));
        }
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
    pub fn all(&self, key: &str) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// ID3v1 genre names, by number.
pub const ID3_GENRES: [&str; 80] = [
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "AlternRock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychadelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
];

fn le32(b: &[u8], o: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?) as usize)
}

/// Parse a Vorbis comment block (vendor string, then `KEY=value` entries). Truncated blocks
/// give what was readable.
pub fn parse_vorbis_comment(b: &[u8]) -> (String, Tags, Vec<Picture>) {
    let mut tags = Tags::default();
    let mut pics = Vec::new();
    let Some(vlen) = le32(b, 0) else {
        return (String::new(), tags, pics);
    };
    let vendor = b
        .get(4..4 + vlen)
        .map(|v| String::from_utf8_lossy(v).to_string())
        .unwrap_or_default();
    let mut o = 4 + vlen;
    let n = le32(b, o).unwrap_or(0);
    o += 4;
    for _ in 0..n {
        let Some(len) = le32(b, o) else { break };
        let Some(e) = b.get(o + 4..o + 4 + len) else {
            break;
        };
        o += 4 + len;
        let text = String::from_utf8_lossy(e);
        if let Some((k, v)) = text.split_once('=') {
            if k.eq_ignore_ascii_case("METADATA_BLOCK_PICTURE") {
                if let Some(p) = base64_decode(v).and_then(|d| parse_flac_picture(&d)) {
                    pics.push(p);
                }
            } else if !k.is_empty() {
                tags.fields.push((k.to_ascii_uppercase(), v.to_string()));
            }
        }
    }
    (vendor, tags, pics)
}

pub fn write_vorbis_comment(
    vendor: &str,
    tags: &Tags,
    pics: &[Picture],
    with_pictures: bool,
) -> Vec<u8> {
    let mut entries: Vec<Vec<u8>> = tags
        .fields
        .iter()
        .map(|(k, v)| format!("{k}={v}").into_bytes())
        .collect();
    if with_pictures {
        for p in pics {
            let enc = base64_encode(&write_flac_picture(p));
            entries.push(format!("METADATA_BLOCK_PICTURE={enc}").into_bytes());
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor.as_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        out.extend_from_slice(&(e.len() as u32).to_le_bytes());
        out.extend_from_slice(&e);
    }
    out
}

fn be32(b: &[u8], o: usize) -> Option<usize> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?) as usize)
}

/// The FLAC PICTURE block layout (also used by Ogg's METADATA_BLOCK_PICTURE).
pub fn parse_flac_picture(b: &[u8]) -> Option<Picture> {
    let kind = be32(b, 0)? as u8;
    let ml = be32(b, 4)?;
    let mime = String::from_utf8_lossy(b.get(8..8 + ml)?).to_string();
    let mut o = 8 + ml;
    let dl = be32(b, o)?;
    let description = String::from_utf8_lossy(b.get(o + 4..o + 4 + dl)?).to_string();
    o += 4 + dl + 16; // width, height, depth, colours
    let n = be32(b, o)?;
    let data = b.get(o + 4..o + 4 + n)?.to_vec();
    Some(Picture {
        mime,
        kind,
        description,
        data,
    })
}

pub fn write_flac_picture(p: &Picture) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&u32::from(p.kind).to_be_bytes());
    o.extend_from_slice(&(p.mime.len() as u32).to_be_bytes());
    o.extend_from_slice(p.mime.as_bytes());
    o.extend_from_slice(&(p.description.len() as u32).to_be_bytes());
    o.extend_from_slice(p.description.as_bytes());
    o.extend_from_slice(&[0; 16]);
    o.extend_from_slice(&(p.data.len() as u32).to_be_bytes());
    o.extend_from_slice(&p.data);
    o
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(d: &[u8]) -> String {
    let mut s = String::with_capacity(d.len().div_ceil(3) * 4);
    for c in d.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for n in 0..20 {
            let d: Vec<u8> = (0..n).map(|i| (i * 31 + 7) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&d)).unwrap(), d);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }

    #[test]
    fn comment_round_trip() {
        let mut t = Tags::default();
        t.add("title", "Song");
        t.add("ARTIST", "Me");
        let pic = Picture {
            mime: "image/png".into(),
            kind: 3,
            description: "front".into(),
            data: vec![1, 2, 3],
        };
        let b = write_vorbis_comment("oc", &t, std::slice::from_ref(&pic), true);
        let (v, t2, p2) = parse_vorbis_comment(&b);
        assert_eq!(v, "oc");
        assert_eq!(t2, t);
        assert_eq!(p2, vec![pic]);
    }
}
