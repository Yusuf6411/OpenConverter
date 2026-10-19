//! Format identifiers, content sniffing and read/write dispatch.
use crate::entry::{Archive, Entry};
use crate::{ar, cpio, tar, zip, Context};
use oc_compress::{bzip2, compress_z, gzip, lz4, lzma, xz, zstd};
use oc_core::{Error, ErrorCode, Result};

/// A single-stream compressor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    Lz4,
    /// Unix `compress` (.Z), read only.
    Z,
    /// Legacy `.lzma`.
    Lzma,
}

impl Codec {
    pub const ALL: [Codec; 7] = [Self::Gzip, Self::Bzip2, Self::Xz, Self::Zstd, Self::Lz4, Self::Z, Self::Lzma];
    pub fn extension(self) -> &'static str {
        match self {
            Self::Gzip => "gz",
            Self::Bzip2 => "bz2",
            Self::Xz => "xz",
            Self::Zstd => "zst",
            Self::Lz4 => "lz4",
            Self::Z => "Z",
            Self::Lzma => "lzma",
        }
    }
    pub fn can_write(self) -> bool {
        self != Self::Z
    }
    pub fn default_level(self) -> u32 {
        match self {
            Self::Gzip | Self::Xz | Self::Lzma => 6,
            Self::Bzip2 => 9,
            Self::Zstd => 3,
            Self::Lz4 => 1,
            Self::Z => 0,
        }
    }
    pub fn level_range(self) -> (u32, u32) {
        match self {
            Self::Gzip | Self::Bzip2 => (1, 9),
            Self::Xz | Self::Lzma => (0, 9),
            Self::Zstd => (1, 19),
            Self::Lz4 => (1, 12),
            Self::Z => (0, 0),
        }
    }

    pub fn decompress(self, data: &[u8], cx: &Context) -> Result<Vec<u8>> {
        let (l, p) = (cx.limits, cx.progress);
        match self {
            Self::Gzip => gzip::decompress(data, l, p),
            Self::Bzip2 => bzip2::decompress(data, l, p),
            Self::Xz => xz::decompress(data, l, p),
            Self::Zstd => zstd::decompress(data, l, p),
            Self::Lz4 => lz4::decompress(data, l, p),
            Self::Z => compress_z::decompress(data, l, p),
            Self::Lzma => lzma::decode_alone(data, l, p),
        }
    }

    pub fn compress(self, data: &[u8], level: Option<u32>) -> Result<Vec<u8>> {
        let (lo, hi) = self.level_range();
        let lv = level.unwrap_or_else(|| self.default_level()).clamp(lo, hi);
        Ok(match self {
            Self::Gzip => gzip::compress(data, lv),
            Self::Bzip2 => bzip2::compress(data, lv),
            Self::Xz => xz::compress(data, lv, xz::Check::Crc64),
            Self::Zstd => zstd::compress(data, lv as i32, true),
            Self::Lz4 => lz4::compress(data, lv),
            Self::Lzma => lzma::encode_alone(data, lzma::Level(lv)),
            Self::Z => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "writing Unix compress (.Z) is not supported",
                ))
            }
        })
    }

    pub fn sniff(data: &[u8]) -> Option<Codec> {
        let h = |n: usize| data.get(..n).unwrap_or(&[]);
        if h(2) == [0x1F, 0x8B] {
            Some(Self::Gzip)
        } else if h(3) == *b"BZh" && data.get(3).is_some_and(|c| (b'1'..=b'9').contains(c)) {
            Some(Self::Bzip2)
        } else if h(6) == [0xFD, b'7', b'z', b'X', b'Z', 0] {
            Some(Self::Xz)
        } else if h(4) == [0x28, 0xB5, 0x2F, 0xFD] {
            Some(Self::Zstd)
        } else if h(4) == [0x04, 0x22, 0x4D, 0x18] {
            Some(Self::Lz4)
        } else if h(2) == [0x1F, 0x9D] {
            Some(Self::Z)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Tar,
    /// A tar wrapped in a single-stream compressor (.tar.gz, .tgz ...).
    TarCompressed(Codec),
    Zip,
    SevenZ,
    Cpio,
    Ar,
    Iso,
    Cab,
    /// One file compressed alone (.gz, .xz ...).
    Single(Codec),
}

impl Format {
    pub fn id(self) -> String {
        match self {
            Self::Tar => "tar".into(),
            Self::TarCompressed(c) => format!("tar.{}", c.extension().to_ascii_lowercase()),
            Self::Zip => "zip".into(),
            Self::SevenZ => "7z".into(),
            Self::Cpio => "cpio".into(),
            Self::Ar => "ar".into(),
            Self::Iso => "iso".into(),
            Self::Cab => "cab".into(),
            Self::Single(c) => c.extension().to_ascii_lowercase(),
        }
    }

    pub fn from_id(id: &str) -> Option<Format> {
        Self::all().into_iter().find(|f| f.id() == id)
    }

    pub fn all() -> Vec<Format> {
        let mut v = vec![Self::Tar];
        v.extend(Codec::ALL.iter().map(|&c| Self::TarCompressed(c)));
        v.extend([Self::Zip, Self::SevenZ, Self::Cpio, Self::Ar, Self::Iso, Self::Cab]);
        v.extend(Codec::ALL.iter().map(|&c| Self::Single(c)));
        v
    }

    /// Whether this format can hold several files and directories.
    pub fn is_container(self) -> bool {
        !matches!(self, Self::Single(_))
    }

    pub fn can_write(self) -> bool {
        match self {
            Self::Iso | Self::Cab => false,
            Self::TarCompressed(c) | Self::Single(c) => c.can_write(),
            _ => true,
        }
    }

    /// File name suffixes without the dot, longest first.
    pub fn extensions(self) -> Vec<String> {
        match self {
            Self::TarCompressed(c) => {
                let e = c.extension();
                let short = match c {
                    Codec::Gzip => Some("tgz"),
                    Codec::Bzip2 => Some("tbz2"),
                    Codec::Xz => Some("txz"),
                    Codec::Zstd => Some("tzst"),
                    Codec::Z => Some("taz"),
                    _ => None,
                };
                let mut v = vec![format!("tar.{e}")];
                v.extend(short.map(String::from));
                v
            }
            f => vec![f.id()],
        }
    }

    /// Detect from content. Compressed streams are reported as `Single`; use
    /// [`Format::from_name`] to tell `.tar.gz` from `.gz`.
    pub fn sniff(data: &[u8]) -> Option<Format> {
        let h = |a: usize, n: usize| data.get(a..a + n).unwrap_or(&[]);
        if h(0, 4) == *b"PK\x03\x04" || h(0, 4) == *b"PK\x05\x06" {
            Some(Self::Zip)
        } else if h(0, 6) == [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C] {
            Some(Self::SevenZ)
        } else if h(0, 8) == *b"!<arch>\n" {
            Some(Self::Ar)
        } else if h(0, 4) == *b"MSCF" {
            Some(Self::Cab)
        } else if h(0x8001, 5) == *b"CD001" {
            Some(Self::Iso)
        } else if matches!(h(0, 6), b"070701" | b"070702" | b"070707") {
            Some(Self::Cpio)
        } else if h(257, 5) == *b"ustar" {
            Some(Self::Tar)
        } else {
            Codec::sniff(data).map(Self::Single)
        }
    }

    /// Resolve a file name's suffix (case-insensitive, longest match).
    pub fn from_name(name: &str) -> Option<Format> {
        let lower = name.to_ascii_lowercase();
        let mut best: Option<(usize, Format)> = None;
        for f in Self::all() {
            for e in f.extensions() {
                if lower.ends_with(&format!(".{}", e.to_ascii_lowercase()))
                    && best.map_or(true, |(l, _)| e.len() > l)
                {
                    best = Some((e.len(), f));
                }
            }
        }
        best.map(|(_, f)| f)
    }
}

/// Settings for [`write`]; irrelevant fields are ignored by formats that lack them.
#[derive(Debug, Clone, Default)]
pub struct WriteOpts {
    /// Compression level for the codec (or the ZIP/7z method).
    pub level: Option<u32>,
    pub zip_method: Option<zip::Method>,
    pub password: Option<String>,
    pub zip_encryption: Option<zip::Encryption>,
    pub sevenz_method: Option<crate::sevenz::Method>,
    pub comment: Option<String>,
    /// File name for a single-file stream (stored in gzip's header).
    pub single_name: Option<String>,
}

fn strip_codec_ext(name: &str, codec: Codec) -> String {
    let lower = name.to_ascii_lowercase();
    let suffix = format!(".{}", codec.extension().to_ascii_lowercase());
    if lower.ends_with(&suffix) && name.len() > suffix.len() {
        name[..name.len() - suffix.len()].to_string()
    } else {
        name.to_string()
    }
}

pub fn read(format: Format, data: &[u8], cx: &Context, name_hint: Option<&str>) -> Result<Archive> {
    match format {
        Format::Tar => tar::read(data, cx),
        Format::TarCompressed(c) => tar::read(&c.decompress(data, cx)?, cx),
        Format::Zip => zip::read(data, cx),
        Format::Cpio => cpio::read(data, cx),
        Format::Ar => ar::read(data, cx),
        Format::SevenZ => crate::sevenz::read(data, cx),
        Format::Iso => crate::iso::read(data, cx),
        Format::Cab => crate::cab::read(data, cx),
        Format::Single(c) => {
            let out = c.decompress(data, cx)?;
            let name = name_hint
                .map(|n| strip_codec_ext(n, c))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "data".to_string());
            // A compressed tar named plainly (`foo.tar` inside `foo.tar.gz` renamed) still opens as a tar.
            if name.to_ascii_lowercase().ends_with(".tar") && out.get(257..262) == Some(b"ustar") {
                return tar::read(&out, cx);
            }
            Ok(Archive {
                entries: vec![Entry::file(name, out)],
                comment: None,
            })
        }
    }
}

pub fn write(format: Format, archive: &Archive, opts: &WriteOpts, cx: &Context) -> Result<Vec<u8>> {
    match format {
        Format::Tar => tar::write(archive, cx),
        Format::TarCompressed(c) => c.compress(&tar::write(archive, cx)?, opts.level),
        Format::Zip => {
            let mut z = zip::WriteOptions::default();
            if let Some(m) = opts.zip_method {
                z.method = m;
            }
            if let Some(l) = opts.level {
                z.level = l;
            }
            z.password.clone_from(&opts.password);
            if let Some(e) = opts.zip_encryption {
                z.encryption = e;
            }
            z.comment.clone_from(&opts.comment);
            zip::write(archive, &z, cx)
        }
        Format::Cpio => cpio::write(archive, cx),
        Format::Ar => ar::write(archive, cx),
        Format::SevenZ => crate::sevenz::write(archive, opts, cx),
        Format::Iso | Format::Cab => Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!("writing {} archives is not supported", format.id()),
        )),
        Format::Single(c) => {
            let files: Vec<&Entry> = archive
                .entries
                .iter()
                .filter(|e| e.kind == crate::EntryKind::File)
                .collect();
            let [only] = files.as_slice() else {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    format!(".{} holds exactly one file; use .tar.{} for several", c.extension(), c.extension()),
                ));
            };
            c.compress(&only.data, opts.level)
        }
    }
}
