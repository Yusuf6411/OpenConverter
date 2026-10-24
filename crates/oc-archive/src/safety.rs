//! Path safety and extraction to disk (MASTER_PROMPT §16): zip-slip, absolute paths,
//! symlink escapes, device names and permission bits are all handled here.
use crate::entry::{Archive, Entry, EntryKind};
use oc_core::{Error, ErrorCode, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Why a stored path is unsafe to extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsafe {
    Absolute,
    ParentTraversal,
    Empty,
    NulByte,
    DriveLetter,
}

/// Split a stored name into clean components, or say why it is unsafe.
/// Backslashes count as separators (Windows archivers), `.` components are dropped.
pub fn components(name: &str) -> std::result::Result<Vec<String>, Unsafe> {
    if name.contains('\0') {
        return Err(Unsafe::NulByte);
    }
    let norm = name.replace('\\', "/");
    if norm.starts_with('/') {
        return Err(Unsafe::Absolute);
    }
    let b = norm.as_bytes();
    if matches!(b, [a, b':', ..] if a.is_ascii_alphabetic()) {
        return Err(Unsafe::DriveLetter);
    }
    let mut out = Vec::new();
    for c in norm.split('/') {
        match c {
            "" | "." => {}
            ".." => return Err(Unsafe::ParentTraversal),
            c => out.push(c.to_string()),
        }
    }
    if out.is_empty() {
        return Err(Unsafe::Empty);
    }
    Ok(out)
}

/// Normalise a name for writing into an archive: forward slashes, no leading `/` or `./`.
/// Fails on `..` so converting never produces a hostile archive.
pub fn clean_for_write(name: &str) -> Result<String> {
    let norm = name.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for c in norm.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                return Err(Error::new(
                    ErrorCode::InvalidOptions,
                    format!("refusing to store path with '..': {name}"),
                ))
            }
            c => out.push(c),
        }
    }
    if out.is_empty() {
        return Err(Error::new(ErrorCode::InvalidOptions, "empty entry name"));
    }
    Ok(out.join("/"))
}

/// Lexically resolve a symlink target relative to the link's directory; `None` if it
/// escapes the extraction root.
fn link_stays_inside(link_path: &[String], target: &str) -> bool {
    let t = target.replace('\\', "/");
    if t.starts_with('/') {
        return false;
    }
    let mut depth: i64 = link_path.len() as i64 - 1;
    for c in t.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    true
}

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub overwrite: bool,
    /// Drop this many leading path components (like `tar --strip-components`).
    pub strip_components: usize,
    /// Restrict extraction to these stored paths (and anything beneath them); empty = all.
    pub only: Vec<String>,
    /// Apply stored permission bits (setuid/setgid/sticky are always dropped).
    pub preserve_mode: bool,
    pub preserve_mtime: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            overwrite: false,
            strip_components: 0,
            only: Vec::new(),
            preserve_mode: true,
            preserve_mtime: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct ExtractReport {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
    /// Entries refused for safety reasons, with the reason.
    pub skipped: Vec<(String, String)>,
}

fn selected(entry: &str, only: &[String]) -> bool {
    if only.is_empty() {
        return true;
    }
    let e = entry.trim_end_matches('/');
    only.iter().any(|o| {
        let o = o.trim_matches('/');
        e == o || e.starts_with(&format!("{o}/"))
    })
}

fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Create every missing directory of `rel` under `root`, refusing to traverse a symlink.
fn make_dirs(root: &Path, comps: &[String]) -> Result<PathBuf> {
    let mut cur = root.to_path_buf();
    for c in comps {
        cur.push(c);
        if is_symlink(&cur) {
            return Err(Error::new(
                ErrorCode::InvalidOptions,
                format!("refusing to write through symlink {}", cur.display()),
            ));
        }
        if !cur.exists() {
            fs::create_dir(&cur)?;
        } else if !cur.is_dir() {
            return Err(Error::new(
                ErrorCode::OutputExists,
                format!("{} exists and is not a directory", cur.display()),
            ));
        }
    }
    Ok(cur)
}

#[cfg(windows)]
fn reserved_name(c: &str) -> bool {
    let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes().get(3).is_some_and(|d| d.is_ascii_digit()))
}
#[cfg(not(windows))]
fn reserved_name(_c: &str) -> bool {
    false
}

/// Extract `archive` below `dest`. Unsafe entries are skipped and reported, never written.
pub fn extract(archive: &Archive, dest: &Path, opts: &ExtractOptions) -> Result<ExtractReport> {
    fs::create_dir_all(dest)?;
    let root = dest.canonicalize()?;
    let mut rep = ExtractReport::default();
    for e in &archive.entries {
        if !selected(&e.path, &opts.only) {
            continue;
        }
        match extract_one(e, &root, opts, &mut rep) {
            Ok(()) => {}
            Err(err) if err.code == ErrorCode::InvalidOptions => {
                rep.skipped.push((e.path.clone(), err.message.clone()));
            }
            Err(err) => return Err(err),
        }
    }
    Ok(rep)
}

fn extract_one(
    e: &Entry,
    root: &Path,
    opts: &ExtractOptions,
    rep: &mut ExtractReport,
) -> Result<()> {
    let bad = |why: &str| Error::new(ErrorCode::InvalidOptions, why.to_string());
    let comps = components(&e.path).map_err(|u| bad(&format!("unsafe path ({u:?})")))?;
    if comps.len() <= opts.strip_components {
        return Ok(());
    }
    let comps: Vec<String> = comps.into_iter().skip(opts.strip_components).collect();
    if comps.iter().any(|c| reserved_name(c)) {
        return Err(bad("reserved device name"));
    }
    match e.kind {
        EntryKind::Dir => {
            let p = make_dirs(root, &comps)?;
            rep.dirs += 1;
            apply_meta(&p, e, opts, true);
        }
        EntryKind::File => {
            let (name, parents) = comps.split_last().ok_or_else(|| bad("empty path"))?;
            let dir = make_dirs(root, parents)?;
            let target = dir.join(name);
            if is_symlink(&target) {
                return Err(bad("refusing to write through symlink"));
            }
            if target.exists() && !opts.overwrite {
                return Err(bad("already exists (use overwrite)"));
            }
            let tmp = dir.join(format!(".{name}.oc-partial"));
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&e.data)?;
            drop(f);
            fs::rename(&tmp, &target)?;
            rep.files += 1;
            rep.bytes += e.size();
            apply_meta(&target, e, opts, false);
        }
        EntryKind::Symlink => {
            let link = e.link.as_deref().unwrap_or("");
            if link.is_empty() || link.contains('\0') || !link_stays_inside(&comps, link) {
                return Err(bad("symlink target escapes the extraction directory"));
            }
            let (name, parents) = comps.split_last().ok_or_else(|| bad("empty path"))?;
            let dir = make_dirs(root, parents)?;
            let target = dir.join(name);
            if fs::symlink_metadata(&target).is_ok() {
                if !opts.overwrite {
                    return Err(bad("already exists (use overwrite)"));
                }
                fs::remove_file(&target)?;
            }
            make_symlink(link, &target)?;
            rep.symlinks += 1;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_symlink(link: &str, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(link, target)?;
    Ok(())
}
#[cfg(windows)]
fn make_symlink(link: &str, target: &Path) -> Result<()> {
    std::os::windows::fs::symlink_file(link, target)?;
    Ok(())
}
#[cfg(not(any(unix, windows)))]
fn make_symlink(_link: &str, _target: &Path) -> Result<()> {
    Err(Error::new(
        ErrorCode::InvalidOptions,
        "symlinks unsupported here",
    ))
}

#[cfg(unix)]
fn apply_meta(p: &Path, e: &Entry, opts: &ExtractOptions, dir: bool) {
    use std::os::unix::fs::PermissionsExt;
    if opts.preserve_mode {
        // Never restore setuid/setgid/sticky; directories stay owner-writable so later
        // entries can still be created below them.
        let mut mode = e.mode & 0o777;
        if dir {
            mode |= 0o700;
        }
        if mode != 0 {
            let _ = fs::set_permissions(p, fs::Permissions::from_mode(mode));
        }
    }
    set_mtime(p, e, opts);
}
#[cfg(not(unix))]
fn apply_meta(p: &Path, e: &Entry, opts: &ExtractOptions, _dir: bool) {
    set_mtime(p, e, opts);
}

fn set_mtime(p: &Path, e: &Entry, opts: &ExtractOptions) {
    if !opts.preserve_mtime {
        return;
    }
    if let Some(t) = e.mtime.filter(|t| *t >= 0) {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(t as u64);
        if let Ok(f) = fs::File::options().write(true).open(p) {
            let _ = f.set_modified(when);
        } else if let Ok(f) = fs::File::open(p) {
            let _ = f.set_modified(when);
        }
    }
}

/// Build an archive from a directory tree (files, directories and symlinks, sorted by path).
pub fn collect_dir(dir: &Path) -> Result<Archive> {
    let mut archive = Archive::default();
    walk(dir, dir, &mut archive)?;
    archive.entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(archive)
}

fn walk(root: &Path, cur: &Path, out: &mut Archive) -> Result<()> {
    let mut items: Vec<_> = fs::read_dir(cur)?.collect::<std::io::Result<Vec<_>>>()?;
    items.sort_by_key(|d| d.file_name());
    for it in items {
        let p = it.path();
        let rel = p
            .strip_prefix(root)
            .map_err(|e| Error::new(ErrorCode::InternalError, e.to_string()))?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let meta = fs::symlink_metadata(&p)?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = if meta.is_dir() { 0o755 } else { 0o644 };
        let ft = meta.file_type();
        let mut e = if ft.is_symlink() {
            let t = fs::read_link(&p)?.to_string_lossy().into_owned();
            Entry::symlink(rel, t)
        } else if ft.is_dir() {
            Entry::dir(rel)
        } else if ft.is_file() {
            Entry::file(rel, fs::read(&p)?)
        } else {
            continue;
        };
        e.mode = mode;
        e.mtime = mtime;
        let is_dir = e.kind == EntryKind::Dir;
        out.entries.push(e);
        if is_dir {
            walk(root, &p, out)?;
        }
    }
    Ok(())
}
