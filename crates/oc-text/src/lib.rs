//! Text encodings, charset detection and line-ending handling.
//!
//! Supported: UTF-8, UTF-16 LE/BE, UTF-32 LE/BE, US-ASCII, ISO-8859-1, windows-1252.
//! Unicode normalisation and further legacy code pages are not implemented yet.
#![forbid(unsafe_code)]

use oc_core::{Error, ErrorCode, Note, Notes, Result, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
    Ascii,
    Latin1,
    Windows1252,
}

impl Encoding {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
            Self::Utf32Le => "utf-32le",
            Self::Utf32Be => "utf-32be",
            Self::Ascii => "us-ascii",
            Self::Latin1 => "iso-8859-1",
            Self::Windows1252 => "windows-1252",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let n = name.trim().to_ascii_lowercase().replace('_', "-");
        Some(match n.as_str() {
            "utf-8" | "utf8" => Self::Utf8,
            "utf-16le" | "utf16le" | "utf-16" => Self::Utf16Le,
            "utf-16be" | "utf16be" => Self::Utf16Be,
            "utf-32le" | "utf32le" | "utf-32" => Self::Utf32Le,
            "utf-32be" | "utf32be" => Self::Utf32Be,
            "ascii" | "us-ascii" => Self::Ascii,
            "iso-8859-1" | "latin1" | "latin-1" => Self::Latin1,
            "windows-1252" | "cp1252" => Self::Windows1252,
            _ => return None,
        })
    }
}

/// What to do with bytes or characters the encoding cannot represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Fail with a typed error.
    Strict,
    /// Substitute U+FFFD when decoding and `?` when encoding, and record a loss note.
    Replace,
}

const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0xFFFD, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0xFFFD, 0x017D, 0xFFFD, 0xFFFD, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0xFFFD, 0x017E, 0x0178,
];

/// Result of detecting the encoding of a byte buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    pub encoding: Encoding,
    /// Length of the byte-order mark to skip, 0 if none.
    pub bom_len: usize,
    /// True if the choice came from a BOM, false if guessed.
    pub certain: bool,
}

/// Detect by BOM, then by UTF-16 zero-byte patterns, then valid UTF-8, else windows-1252.
pub fn detect(data: &[u8]) -> Detected {
    let d = |encoding, bom_len, certain| Detected {
        encoding,
        bom_len,
        certain,
    };
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return d(Encoding::Utf8, 3, true);
    }
    if data.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return d(Encoding::Utf32Le, 4, true);
    }
    if data.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return d(Encoding::Utf32Be, 4, true);
    }
    if data.starts_with(&[0xFF, 0xFE]) {
        return d(Encoding::Utf16Le, 2, true);
    }
    if data.starts_with(&[0xFE, 0xFF]) {
        return d(Encoding::Utf16Be, 2, true);
    }
    let sample = data.get(..data.len().min(4096)).unwrap_or(&[]);
    let (mut even0, mut odd0) = (0usize, 0usize);
    for (i, &b) in sample.iter().enumerate() {
        if b == 0 {
            if i % 2 == 0 {
                even0 += 1;
            } else {
                odd0 += 1;
            }
        }
    }
    let half = sample.len() / 2;
    if half >= 2 && odd0 * 10 > half * 6 && even0 * 10 < half {
        return d(Encoding::Utf16Le, 0, false);
    }
    if half >= 2 && even0 * 10 > half * 6 && odd0 * 10 < half {
        return d(Encoding::Utf16Be, 0, false);
    }
    // A buffer cut mid-sequence is still UTF-8; only reject real invalid bytes.
    let utf8_ok = match std::str::from_utf8(data) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    };
    if utf8_ok {
        return d(Encoding::Utf8, 0, false);
    }
    d(Encoding::Windows1252, 0, false)
}

fn bad(enc: Encoding, offset: usize, what: &str) -> Error {
    Error::new(
        ErrorCode::CorruptFile,
        format!("invalid {what} in {}", enc.name()),
    )
    .at(offset as u64)
}

fn replaced(notes: &mut Notes, count: usize, dir: &str) {
    if count > 0 {
        notes.push(
            Note::new(
                Severity::Loss,
                "text_approximated",
                format!("{count} {dir} replaced with a substitute character"),
            )
            .with_detail(dir),
        );
    }
}

/// Decode `data` (BOM, if any, must already be stripped) to a `String`.
pub fn decode(data: &[u8], enc: Encoding, policy: Policy) -> Result<(String, Notes)> {
    let mut notes = Notes::new();
    let mut out = String::with_capacity(data.len());
    let mut subs = 0usize;
    match enc {
        Encoding::Utf8 => match std::str::from_utf8(data) {
            Ok(s) => out.push_str(s),
            Err(e) if policy == Policy::Strict => {
                return Err(bad(enc, e.valid_up_to(), "byte sequence"));
            }
            Err(_) => {
                let lossy = String::from_utf8_lossy(data);
                subs = lossy.matches('\u{FFFD}').count();
                out = lossy.into_owned();
            }
        },
        Encoding::Ascii | Encoding::Latin1 | Encoding::Windows1252 => {
            for (i, &b) in data.iter().enumerate() {
                let c = match (enc, b) {
                    (_, 0..=0x7F) => char::from(b),
                    (Encoding::Ascii, _) => {
                        if policy == Policy::Strict {
                            return Err(bad(enc, i, "byte"));
                        }
                        subs += 1;
                        '\u{FFFD}'
                    }
                    (Encoding::Windows1252, 0x80..=0x9F) => {
                        let u = CP1252_HIGH
                            .get(usize::from(b) - 0x80)
                            .copied()
                            .unwrap_or(0xFFFD);
                        if u == 0xFFFD {
                            if policy == Policy::Strict {
                                return Err(bad(enc, i, "byte"));
                            }
                            subs += 1;
                        }
                        char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}')
                    }
                    _ => char::from(b),
                };
                out.push(c);
            }
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let be = enc == Encoding::Utf16Be;
            let mut units = Vec::with_capacity(data.len() / 2);
            let (pairs, rest) = data.as_chunks::<2>();
            for pair in pairs {
                units.push(if be {
                    u16::from_be_bytes(*pair)
                } else {
                    u16::from_le_bytes(*pair)
                });
            }
            let mut offset = 0usize;
            for r in char::decode_utf16(units) {
                match r {
                    Ok(c) => {
                        offset += 2 * c.len_utf16();
                        out.push(c);
                    }
                    Err(_) if policy == Policy::Strict => {
                        return Err(bad(enc, offset, "surrogate"));
                    }
                    Err(_) => {
                        offset += 2;
                        subs += 1;
                        out.push('\u{FFFD}');
                    }
                }
            }
            if !rest.is_empty() {
                if policy == Policy::Strict {
                    return Err(Error::truncated("odd number of bytes in UTF-16 data"));
                }
                subs += 1;
                out.push('\u{FFFD}');
            }
        }
        Encoding::Utf32Le | Encoding::Utf32Be => {
            let be = enc == Encoding::Utf32Be;
            let (words, rest) = data.as_chunks::<4>();
            for (i, a) in words.iter().enumerate() {
                let v = if be {
                    u32::from_be_bytes(*a)
                } else {
                    u32::from_le_bytes(*a)
                };
                match char::from_u32(v) {
                    Some(ch) => out.push(ch),
                    None if policy == Policy::Strict => return Err(bad(enc, i * 4, "code point")),
                    None => {
                        subs += 1;
                        out.push('\u{FFFD}');
                    }
                }
            }
            if !rest.is_empty() {
                if policy == Policy::Strict {
                    return Err(Error::truncated("partial UTF-32 code unit"));
                }
                subs += 1;
                out.push('\u{FFFD}');
            }
        }
    }
    replaced(&mut notes, subs, "undecodable bytes");
    Ok((out, notes))
}

/// Encode `text`. No BOM is written; use [`bom`] if the caller wants one.
pub fn encode(text: &str, enc: Encoding, policy: Policy) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let mut out = Vec::with_capacity(text.len());
    let mut subs = 0usize;
    let single_byte = |c: char, out: &mut Vec<u8>, subs: &mut usize| -> Result<()> {
        let v = u32::from(c);
        let byte = match enc {
            Encoding::Ascii if v < 0x80 => Some(v as u8),
            Encoding::Latin1 if v < 0x100 => Some(v as u8),
            Encoding::Windows1252 => {
                if v < 0x80 || (0xA0..0x100).contains(&v) {
                    Some(v as u8)
                } else {
                    CP1252_HIGH
                        .iter()
                        .position(|&u| u32::from(u) == v && u != 0xFFFD)
                        .map(|i| 0x80 + i as u8)
                }
            }
            _ => None,
        };
        match byte {
            Some(b) => out.push(b),
            None if policy == Policy::Strict => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    format!("U+{v:04X} cannot be represented in {}", enc.name()),
                ));
            }
            None => {
                *subs += 1;
                out.push(b'?');
            }
        }
        Ok(())
    };
    match enc {
        Encoding::Utf8 => out.extend_from_slice(text.as_bytes()),
        Encoding::Ascii | Encoding::Latin1 | Encoding::Windows1252 => {
            for c in text.chars() {
                single_byte(c, &mut out, &mut subs)?;
            }
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            for u in text.encode_utf16() {
                let b = if enc == Encoding::Utf16Be {
                    u.to_be_bytes()
                } else {
                    u.to_le_bytes()
                };
                out.extend_from_slice(&b);
            }
        }
        Encoding::Utf32Le | Encoding::Utf32Be => {
            for c in text.chars() {
                let b = if enc == Encoding::Utf32Be {
                    u32::from(c).to_be_bytes()
                } else {
                    u32::from(c).to_le_bytes()
                };
                out.extend_from_slice(&b);
            }
        }
    }
    replaced(&mut notes, subs, "unencodable characters");
    Ok((out, notes))
}

/// The byte-order mark for an encoding, empty for single-byte encodings.
pub fn bom(enc: Encoding) -> &'static [u8] {
    match enc {
        Encoding::Utf8 => &[0xEF, 0xBB, 0xBF],
        Encoding::Utf16Le => &[0xFF, 0xFE],
        Encoding::Utf16Be => &[0xFE, 0xFF],
        Encoding::Utf32Le => &[0xFF, 0xFE, 0, 0],
        Encoding::Utf32Be => &[0, 0, 0xFE, 0xFF],
        _ => &[],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

/// Detect the dominant line ending. `None` if the text has no line break.
pub fn detect_line_ending(text: &str) -> Option<LineEnding> {
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' if it.peek() == Some(&'\n') => {
                it.next();
                crlf += 1;
            }
            '\r' => cr += 1,
            '\n' => lf += 1,
            _ => {}
        }
    }
    let max = lf.max(crlf).max(cr);
    if max == 0 {
        None
    } else if crlf == max {
        Some(LineEnding::CrLf)
    } else if lf == max {
        Some(LineEnding::Lf)
    } else {
        Some(LineEnding::Cr)
    }
}

/// Rewrite every line break (LF, CRLF or lone CR) as `to`.
pub fn convert_line_endings(text: &str, to: LineEnding) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push_str(to.as_str());
            }
            '\n' => out.push_str(to.as_str()),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_unicode_encodings() {
        let s = "héllo wörld € 日本語 😀\r\nline2";
        for enc in [
            Encoding::Utf8,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
            Encoding::Utf32Le,
            Encoding::Utf32Be,
        ] {
            let (b, _) = encode(s, enc, Policy::Strict).unwrap_or_default();
            let (back, notes) = decode(&b, enc, Policy::Strict).unwrap_or_default();
            assert_eq!(back, s, "{enc:?}");
            assert!(notes.is_empty());
        }
    }

    #[test]
    fn windows_1252_specials() {
        let (s, _) = decode(
            &[0x80, 0x93, 0x94, 0xE9],
            Encoding::Windows1252,
            Policy::Strict,
        )
        .unwrap_or_default();
        assert_eq!(s, "€“”é");
        let (b, _) = encode("€“”é", Encoding::Windows1252, Policy::Strict).unwrap_or_default();
        assert_eq!(b, vec![0x80, 0x93, 0x94, 0xE9]);
        assert!(decode(&[0x81], Encoding::Windows1252, Policy::Strict).is_err());
        assert!(encode("日", Encoding::Windows1252, Policy::Strict).is_err());
    }

    #[test]
    fn replace_policy_records_a_loss_note() {
        let (b, notes) = encode("a日b", Encoding::Latin1, Policy::Replace).unwrap_or_default();
        assert_eq!(b, b"a?b");
        assert!(notes.has_loss());
        let (s, notes) =
            decode(&[b'a', 0xFF, b'b'], Encoding::Utf8, Policy::Replace).unwrap_or_default();
        assert_eq!(s, "a\u{FFFD}b");
        assert!(notes.has_loss());
        assert_eq!(
            decode(&[b'a', 0xFF], Encoding::Utf8, Policy::Strict)
                .err()
                .and_then(|e| e.offset),
            Some(1)
        );
    }

    #[test]
    fn detection() {
        assert_eq!(detect(b"\xEF\xBB\xBFhi").encoding, Encoding::Utf8);
        assert_eq!(detect(b"\xEF\xBB\xBFhi").bom_len, 3);
        assert_eq!(detect(b"\xFF\xFEh\0i\0").encoding, Encoding::Utf16Le);
        assert_eq!(detect(b"\xFF\xFE\0\0h\0\0\0").encoding, Encoding::Utf32Le);
        assert_eq!(
            detect(b"h\0i\0 \0t\0h\0e\0r\0e\0").encoding,
            Encoding::Utf16Le
        );
        assert_eq!(
            detect(b"\0h\0i\0 \0t\0h\0e\0r\0e").encoding,
            Encoding::Utf16Be
        );
        assert_eq!(detect("héllo".as_bytes()).encoding, Encoding::Utf8);
        assert_eq!(detect(b"caf\xE9 au lait").encoding, Encoding::Windows1252);
        assert!(!detect(b"plain").certain);
    }

    #[test]
    fn utf16_edge_cases() {
        // Lone high surrogate, then odd trailing byte.
        let data = [0x00, 0xD8, 0x41, 0x00, 0x42];
        assert!(decode(&data, Encoding::Utf16Le, Policy::Strict).is_err());
        let (s, notes) = decode(&data, Encoding::Utf16Le, Policy::Replace).unwrap_or_default();
        assert_eq!(s, "\u{FFFD}A\u{FFFD}");
        assert!(notes.has_loss());
        assert!(decode(&[0, 0, 0x11, 0], Encoding::Utf32Le, Policy::Strict).is_err());
    }

    #[test]
    fn line_endings() {
        assert_eq!(detect_line_ending("a\r\nb\r\nc\n"), Some(LineEnding::CrLf));
        assert_eq!(detect_line_ending("a\nb\n"), Some(LineEnding::Lf));
        assert_eq!(detect_line_ending("a\rb\r"), Some(LineEnding::Cr));
        assert_eq!(detect_line_ending("abc"), None);
        assert_eq!(
            convert_line_endings("a\r\nb\rc\nd", LineEnding::Lf),
            "a\nb\nc\nd"
        );
        assert_eq!(convert_line_endings("a\nb", LineEnding::CrLf), "a\r\nb");
    }
}
