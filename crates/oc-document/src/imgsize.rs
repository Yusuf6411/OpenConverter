//! Picture dimensions and type from the first bytes (PNG, JPEG, GIF, BMP, WebP).

pub fn mime_of(d: &[u8]) -> Option<&'static str> {
    if d.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if d.starts_with(&[0xFF, 0xD8]) {
        Some("image/jpeg")
    } else if d.starts_with(b"GIF8") {
        Some("image/gif")
    } else if d.starts_with(b"BM") {
        Some("image/bmp")
    } else if d.len() > 12 && &d[..4] == b"RIFF" && &d[8..12] == b"WEBP" {
        Some("image/webp")
    } else if d.starts_with(b"<svg") || d.starts_with(b"<?xml") {
        Some("image/svg+xml")
    } else {
        None
    }
}

fn be16(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?])))
}
fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*d.get(o)?, *d.get(o + 1)?, *d.get(o + 2)?, *d.get(o + 3)?]))
}
fn le16(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes([*d.get(o)?, *d.get(o + 1)?])))
}
fn le32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*d.get(o)?, *d.get(o + 1)?, *d.get(o + 2)?, *d.get(o + 3)?]))
}

pub fn size(d: &[u8]) -> Option<(u32, u32)> {
    match mime_of(d)? {
        "image/png" => Some((be32(d, 16)?, be32(d, 20)?)),
        "image/gif" => Some((le16(d, 6)?, le16(d, 8)?)),
        "image/bmp" => Some((le32(d, 18)?, le32(d, 22)?.min(0x7FFF_FFFF))),
        "image/jpeg" => {
            let mut i = 2;
            while i + 9 < d.len() {
                if d[i] != 0xFF {
                    i += 1;
                    continue;
                }
                let m = d[i + 1];
                if m == 0xFF {
                    i += 1;
                    continue;
                }
                if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
                    return Some((be16(d, i + 7)?, be16(d, i + 5)?));
                }
                if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
                    i += 2;
                    continue;
                }
                i += 2 + be16(d, i + 2)? as usize;
            }
            None
        }
        "image/webp" => {
            let tag = d.get(12..16)?;
            match tag {
                b"VP8 " => Some((le16(d, 26)? & 0x3FFF, le16(d, 28)? & 0x3FFF)),
                b"VP8L" => {
                    let b = le32(d, 21)?;
                    Some(((b & 0x3FFF) + 1, ((b >> 14) & 0x3FFF) + 1))
                }
                b"VP8X" => {
                    let w = le32(d, 24)? & 0xFF_FFFF;
                    let h = le32(d, 27)? & 0xFF_FFFF;
                    Some((w + 1, h + 1))
                }
                _ => None,
            }
        }
        _ => None,
    }
}
