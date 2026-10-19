//! Archive and single-file compression formats: converting between them, packing any file
//! into one, listing, extracting and creating from folders.
use oc_archive::format::WriteOpts;
use oc_archive::safety::{self, ExtractOptions, ExtractReport};
use oc_archive::{sevenz, zip, Archive, Context, Entry, EntryKind, Format};
use oc_core::options::{OptionKind, OptionSchema, OptionSpec, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Limits, Note, Notes,
    Ownership, Progress, Result, Severity,
};
use std::path::{Path, PathBuf};

/// Any file whose type the engine does not know; it can still be packed into an archive.
pub const FILE: FormatId = FormatId("file");

struct Spec {
    id: &'static str,
    name: &'static str,
    exts: &'static [&'static str],
    mime: &'static [&'static str],
    format: Format,
    spec: &'static str,
}

use oc_archive::Codec::{Bzip2, Gzip, Lz4, Lzma, Xz, Zstd, Z};

const SPECS: [Spec; 21] = [
    Spec { id: "tar", name: "TAR", exts: &["tar"], mime: &["application/x-tar"], format: Format::Tar, spec: "POSIX.1-2001 pax / ustar / GNU tar" },
    Spec { id: "tar.gz", name: "TAR + gzip", exts: &["tar.gz", "tgz"], mime: &["application/gzip"], format: Format::TarCompressed(Gzip), spec: "tar inside RFC 1952 gzip" },
    Spec { id: "tar.bz2", name: "TAR + bzip2", exts: &["tar.bz2", "tbz2", "tbz"], mime: &["application/x-bzip2"], format: Format::TarCompressed(Bzip2), spec: "tar inside bzip2" },
    Spec { id: "tar.xz", name: "TAR + xz", exts: &["tar.xz", "txz"], mime: &["application/x-xz"], format: Format::TarCompressed(Xz), spec: "tar inside the .xz format" },
    Spec { id: "tar.zst", name: "TAR + Zstandard", exts: &["tar.zst", "tzst"], mime: &["application/zstd"], format: Format::TarCompressed(Zstd), spec: "tar inside RFC 8878 Zstandard" },
    Spec { id: "tar.lz4", name: "TAR + LZ4", exts: &["tar.lz4"], mime: &["application/x-lz4"], format: Format::TarCompressed(Lz4), spec: "tar inside the LZ4 frame format" },
    Spec { id: "tar.lzma", name: "TAR + LZMA", exts: &["tar.lzma", "tlz"], mime: &["application/x-lzma"], format: Format::TarCompressed(Lzma), spec: "tar inside .lzma" },
    Spec { id: "tar.z", name: "TAR + compress", exts: &["tar.z", "taz"], mime: &["application/x-compress"], format: Format::TarCompressed(Z), spec: "tar inside Unix compress (read only)" },
    Spec { id: "zip", name: "ZIP", exts: &["zip"], mime: &["application/zip"], format: Format::Zip, spec: "PKWARE APPNOTE 6.3.10 incl. ZIP64, AES-WinZip" },
    Spec { id: "7z", name: "7z", exts: &["7z"], mime: &["application/x-7z-compressed"], format: Format::SevenZ, spec: "7z format (LZMA SDK documentation)" },
    Spec { id: "cpio", name: "CPIO", exts: &["cpio"], mime: &["application/x-cpio"], format: Format::Cpio, spec: "SVR4 newc / crc / POSIX odc" },
    Spec { id: "ar", name: "ar archive", exts: &["ar", "a", "deb"], mime: &["application/x-archive"], format: Format::Ar, spec: "Unix ar, GNU and BSD long names" },
    Spec { id: "iso", name: "ISO 9660 image", exts: &["iso"], mime: &["application/x-iso9660-image"], format: Format::Iso, spec: "ECMA-119 with Joliet and Rock Ridge (read only)" },
    Spec { id: "cab", name: "Cabinet", exts: &["cab"], mime: &["application/vnd.ms-cab-compressed"], format: Format::Cab, spec: "Microsoft Cabinet: stored, MSZIP, LZX (read only)" },
    Spec { id: "gz", name: "gzip", exts: &["gz"], mime: &["application/gzip"], format: Format::Single(Gzip), spec: "RFC 1952" },
    Spec { id: "bz2", name: "bzip2", exts: &["bz2"], mime: &["application/x-bzip2"], format: Format::Single(Bzip2), spec: "bzip2 format" },
    Spec { id: "xz", name: "xz", exts: &["xz"], mime: &["application/x-xz"], format: Format::Single(Xz), spec: "The .xz file format" },
    Spec { id: "zst", name: "Zstandard", exts: &["zst"], mime: &["application/zstd"], format: Format::Single(Zstd), spec: "RFC 8878" },
    Spec { id: "lz4", name: "LZ4", exts: &["lz4"], mime: &["application/x-lz4"], format: Format::Single(Lz4), spec: "LZ4 frame format 1.6" },
    Spec { id: "lzma", name: "LZMA", exts: &["lzma"], mime: &["application/x-lzma"], format: Format::Single(Lzma), spec: "LZMA SDK .lzma format" },
    Spec { id: "z", name: "compress (.Z)", exts: &["z"], mime: &["application/x-compress"], format: Format::Single(Z), spec: "Unix compress, read only" },
];

pub const FORMATS: [FormatId; 21] = [
    FormatId("tar"), FormatId("tar.gz"), FormatId("tar.bz2"), FormatId("tar.xz"), FormatId("tar.zst"),
    FormatId("tar.lz4"), FormatId("tar.lzma"), FormatId("tar.z"), FormatId("zip"), FormatId("7z"),
    FormatId("cpio"), FormatId("ar"), FormatId("iso"), FormatId("cab"), FormatId("gz"), FormatId("bz2"),
    FormatId("xz"), FormatId("zst"), FormatId("lz4"), FormatId("lzma"), FormatId("z"),
];

fn spec_of(id: FormatId) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.id == id.0)
}

pub fn is_archive(id: FormatId) -> bool {
    spec_of(id).is_some()
}

/// The archive-crate format behind an engine format id.
pub fn format_of(id: FormatId) -> Option<Format> {
    spec_of(id).map(|s| s.format)
}

/// Whether an archive holds several named entries (as opposed to one compressed stream).
pub fn is_container(id: FormatId) -> bool {
    format_of(id).is_some_and(Format::is_container)
}

pub fn id_of(format: Format) -> Option<FormatId> {
    SPECS.iter().find(|s| s.format == format).map(|s| FormatId(s.id))
}

fn opt(key: &'static str, label: &'static str, help: &'static str, kind: OptionKind) -> OptionSpec {
    OptionSpec { key, label, help, i18n_key: "", kind }
}

fn leak(s: String) -> &'static str {
    // Option metadata lives for the whole process; leaking a few short strings once is fine.
    Box::leak(s.into_boxed_str())
}

fn schema(s: &Spec) -> OptionSchema {
    let mut specs = vec![opt(
        "source_password",
        "Password of the input archive",
        "Needed only when the input is encrypted",
        OptionKind::Text { default: String::new() },
    )];
    let text = |d: &str| OptionKind::Text { default: d.to_string() };
    match s.format {
        Format::Zip => {
            specs.push(opt("level", "Compression level", "1 (fast) to 9 (small)", OptionKind::Int { min: 1, max: 9, default: 6 }));
            specs.push(opt(
                "method",
                "Compression method",
                "auto keeps a member stored when deflate would not shrink it",
                OptionKind::Enum { choices: vec!["auto", "stored", "deflate", "bzip2", "lzma", "zstd", "xz"], default: "auto" },
            ));
            specs.push(opt("password", "Password", "Encrypts every file; leave empty for none", text("")));
            specs.push(opt(
                "encryption",
                "Encryption",
                "AES is strong; zipcrypto is weak and only for old unzip tools",
                OptionKind::Enum { choices: vec!["aes256", "aes192", "aes128", "zipcrypto"], default: "aes256" },
            ));
            specs.push(opt("comment", "Archive comment", "Text stored in the archive", text("")));
        }
        Format::SevenZ => {
            specs.push(opt("level", "Compression level", "0 (fast) to 9 (small)", OptionKind::Int { min: 0, max: 9, default: 5 }));
            specs.push(opt(
                "method",
                "Compression method",
                "LZMA2 is the 7-Zip default",
                OptionKind::Enum { choices: vec!["lzma2", "lzma", "bzip2", "deflate", "zstd", "copy"], default: "lzma2" },
            ));
            specs.push(opt("password", "Password", "Encrypts data and file names (AES-256)", text("")));
        }
        Format::Cpio | Format::Ar | Format::Iso | Format::Cab | Format::Tar => {}
        Format::TarCompressed(c) | Format::Single(c) => {
            if c.can_write() {
                let (lo, hi) = c.level_range();
                specs.push(opt(
                    "level",
                    "Compression level",
                    leak(format!("{lo} (fast) to {hi} (small)")),
                    OptionKind::Int { min: i64::from(lo), max: i64::from(hi), default: i64::from(c.default_level()) },
                ));
            }
        }
    }
    OptionSchema { specs }
}

pub fn register(registry: &mut Registry, others: &[FormatId]) {
    registry.add_format(FormatDescriptor {
        id: FILE,
        name: "Any file",
        category: Category::Archive,
        extensions: &[],
        mime: &["application/octet-stream"],
        sniffers: Vec::new(),
        capabilities: Capabilities { read: true, ..Capabilities::default() },
        writer_options: OptionSchema::default(),
        ownership: Ownership::Own,
        spec: "Any file can be stored in an archive",
    });
    for s in &SPECS {
        let can_write = s.format.can_write();
        registry.add_format(FormatDescriptor {
            id: FormatId(s.id),
            name: s.name,
            category: Category::Archive,
            extensions: s.exts,
            mime: s.mime,
            sniffers: Vec::new(),
            capabilities: Capabilities { read: true, write: can_write, streams: s.format.is_container(), ..Capabilities::default() },
            writer_options: schema(s),
            ownership: Ownership::Own,
            spec: s.spec,
        });
    }
    let edge = |from: FormatId, to: FormatId, description: &'static str| Edge {
        from,
        to,
        kind: EdgeKind::Transcode,
        loss: 0,
        ownership: Ownership::Own,
        time: 2,
        description,
    };
    for a in &SPECS {
        let from = FormatId(a.id);
        for b in SPECS.iter().filter(|b| b.format.can_write()) {
            registry.add_edge(edge(from, FormatId(b.id), "Read archive -> write archive"));
        }
    }
    for from in others.iter().copied().chain(std::iter::once(FILE)) {
        for b in SPECS.iter().filter(|b| b.format.can_write()) {
            registry.add_edge(edge(from, FormatId(b.id), "Store the file in an archive"));
        }
    }
}

fn password(opts: &Options, key: &str) -> Option<String> {
    opts.text(key).filter(|p| !p.is_empty()).map(str::to_string)
}

fn write_opts(to: FormatId, opts: &Options) -> Result<WriteOpts> {
    let mut w = WriteOpts {
        level: opts.int("level").and_then(|l| u32::try_from(l).ok()),
        password: password(opts, "password"),
        comment: opts.text("comment").filter(|c| !c.is_empty()).map(str::to_string),
        ..WriteOpts::default()
    };
    match (format_of(to), opts.text("method")) {
        (Some(Format::Zip), Some(m)) => {
            w.zip_method = Some(match m {
                "stored" => zip::Method::Stored,
                "deflate" => zip::Method::Deflate,
                "bzip2" => zip::Method::Bzip2,
                "lzma" => zip::Method::Lzma,
                "zstd" => zip::Method::Zstd,
                "xz" => zip::Method::Xz,
                _ => zip::Method::Auto,
            });
        }
        (Some(Format::SevenZ), Some(m)) => {
            w.sevenz_method = Some(match m {
                "lzma" => sevenz::Method::Lzma,
                "bzip2" => sevenz::Method::Bzip2,
                "deflate" => sevenz::Method::Deflate,
                "zstd" => sevenz::Method::Zstd,
                "copy" => sevenz::Method::Copy,
                _ => sevenz::Method::Lzma2,
            });
        }
        _ => {}
    }
    w.zip_encryption = match opts.text("encryption") {
        Some("aes192") => Some(zip::Encryption::Aes192),
        Some("aes128") => Some(zip::Encryption::Aes128),
        Some("zipcrypto") => Some(zip::Encryption::ZipCrypto),
        _ => None,
    };
    Ok(w)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}
#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0o644
}

/// Converts `data` (the file at `input`) from `from` to the archive format `to`.
pub fn convert(
    from: FormatId,
    to: FormatId,
    data: &[u8],
    input: &Path,
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
    notes: &mut Notes,
) -> Result<(Vec<u8>, String)> {
    let pw = password(opts, "source_password");
    let cx = Context { limits, progress, password: pw.as_deref(), list_only: false };
    let to_fmt = format_of(to).ok_or_else(|| Error::new(ErrorCode::UnsupportedFormat, "not an archive format"))?;
    let name = file_name(input);
    let (archive, read_text) = if let Some(f) = format_of(from) {
        let a = oc_archive::format::read(f, data, &cx, Some(&name))?;
        (a, format!("Read {}", spec_of(from).map_or("archive", |s| s.name)))
    } else {
        let mut e = Entry::file(name.clone(), data.to_vec());
        if let Ok(meta) = std::fs::metadata(input) {
            e.mode = mode_of(&meta) & 0o777;
            e.mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
        }
        (Archive { entries: vec![e], comment: None }, "Store the file".to_string())
    };
    if archive.entries.iter().any(|e| e.encrypted) && password(opts, "password").is_none() {
        notes.push(Note::new(
            Severity::Warning,
            "archive.decrypted",
            "the input was encrypted; the output is not (set a password to encrypt it)",
        ));
    }
    let n_links = archive.entries.iter().filter(|e| e.kind == EntryKind::Symlink).count();
    if n_links > 0 && matches!(to_fmt, Format::Ar) {
        return Err(Error::new(ErrorCode::UnsupportedVariant, "ar archives cannot hold symbolic links"));
    }
    if to_fmt == Format::Ar && archive.entries.iter().any(|e| e.kind == EntryKind::Dir) {
        return Err(Error::new(ErrorCode::UnsupportedVariant, "ar archives cannot hold directories"));
    }
    let w = write_opts(to, opts)?;
    let out = oc_archive::format::write(to_fmt, &archive, &w, &cx)?;
    let write_text = format!("Write {}", spec_of(to).map_or("archive", |s| s.name));
    Ok((out, format!("{read_text} -> {write_text}")))
}

/// One member of an archive, as shown by listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    pub path: String,
    /// `f`ile, `d`irectory or `l`ink.
    pub kind: char,
    pub size: u64,
    pub mode: u32,
    pub mtime: Option<i64>,
    pub encrypted: bool,
    pub link: Option<String>,
}

fn info(e: &Entry) -> EntryInfo {
    EntryInfo {
        path: e.path.clone(),
        kind: match e.kind {
            EntryKind::File => 'f',
            EntryKind::Dir => 'd',
            EntryKind::Symlink => 'l',
        },
        size: e.size(),
        mode: e.mode,
        mtime: e.mtime,
        encrypted: e.encrypted,
        link: e.link.clone(),
    }
}

fn read_archive(path: &Path, password: Option<&str>, list_only: bool, limits: &Limits, progress: &dyn Progress) -> Result<(FormatId, Archive)> {
    let meta = std::fs::metadata(path)?;
    limits.check_alloc(meta.len())?;
    let data = std::fs::read(path)?;
    let name = file_name(path);
    let by_name = Format::from_name(&name);
    let sniffed = Format::sniff(&data);
    let fmt = match (by_name, sniffed) {
        (Some(n), Some(s)) if compatible(n, s) => n,
        (_, Some(s)) if s.is_container() || by_name.is_none() => s,
        (Some(n), _) => n,
        (None, None) => return Err(Error::new(ErrorCode::UnsupportedFormat, "not a recognised archive").with_detail(name)),
        (None, Some(s)) => s,
    };
    let cx = Context { limits, progress, password, list_only };
    let a = oc_archive::format::read(fmt, &data, &cx, Some(&name))?;
    Ok((id_of(fmt).unwrap_or(FILE), a))
}

/// If the content of an archive file is a different archive format than its extension
/// says, the format to use instead.
pub fn sniff_conflict(by_ext: FormatId, data: &[u8]) -> Option<FormatId> {
    let named = format_of(by_ext)?;
    let found = Format::sniff(data)?;
    if compatible(named, found) {
        return None;
    }
    // Plain tar has no magic in old variants and gz/xz payloads are tars: trust names then.
    id_of(found).filter(|_| found.is_container() || !named.is_container())
}

fn compatible(by_name: Format, sniffed: Format) -> bool {
    use oc_archive::Format::{Single, Tar, TarCompressed};
    by_name == sniffed
        || matches!((by_name, sniffed), (TarCompressed(a), Single(b)) if a == b)
        || matches!((by_name, sniffed), (Single(a), Single(b)) if a == b)
        || (by_name == Tar && sniffed == Tar)
}

pub fn list(path: &Path, password: Option<&str>, limits: &Limits, progress: &dyn Progress) -> Result<(FormatId, Vec<EntryInfo>)> {
    let (f, a) = read_archive(path, password, true, limits, progress)?;
    Ok((f, a.entries.iter().map(info).collect()))
}

pub fn extract(
    path: &Path,
    dest: &Path,
    opts: &ExtractOptions,
    password: Option<&str>,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<ExtractReport> {
    let (_, a) = read_archive(path, password, false, limits, progress)?;
    safety::extract(&a, dest, opts)
}

/// Build an archive at `output` from files and folders. Names inside are relative to each
/// source's parent, so `create(["photos"], x.zip)` stores `photos/...`.
pub fn create(
    sources: &[PathBuf],
    output_format: FormatId,
    opts: &Options,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<(Vec<u8>, usize)> {
    let to_fmt = format_of(output_format).ok_or_else(|| Error::new(ErrorCode::UnsupportedFormat, "not an archive format"))?;
    let mut archive = Archive::default();
    for src in sources {
        progress.check_cancelled()?;
        let meta = std::fs::symlink_metadata(src)?;
        let base = file_name(src);
        if meta.is_dir() {
            let mut top = Entry::dir(base.clone());
            top.mode = mode_of(&meta) & 0o777;
            top.mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
            archive.entries.push(top);
            for mut e in safety::collect_dir(src)?.entries {
                e.path = format!("{base}/{}", e.path);
                limits.check_entries(archive.entries.len() as u64 + 1)?;
                archive.entries.push(e);
            }
        } else if meta.file_type().is_symlink() {
            let t = std::fs::read_link(src)?.to_string_lossy().into_owned();
            archive.entries.push(Entry::symlink(base, t));
        } else {
            limits.check_alloc(meta.len())?;
            let mut e = Entry::file(base, std::fs::read(src)?);
            e.mode = mode_of(&meta) & 0o777;
            e.mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
            archive.entries.push(e);
        }
    }
    let cx = Context { limits, progress, password: None, list_only: false };
    let w = write_opts(output_format, opts)?;
    let n = archive.entries.len();
    Ok((oc_archive::format::write(to_fmt, &archive, &w, &cx)?, n))
}
