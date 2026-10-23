//! Finding installed fonts: directories are discovered by our own code (fontconfig's
//! configuration is read as text, libfontconfig is not used) and each file's names and
//! style are read from its header tables.
use crate::reader::R;
use crate::Font;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Face {
    pub path: PathBuf,
    pub index: usize,
    /// Lower-case family name.
    pub family: String,
    pub full_name: String,
    pub weight: u16,
    pub italic: bool,
    pub mono: bool,
}

pub struct FontDb {
    pub faces: Vec<Face>,
    cache: Mutex<HashMap<(PathBuf, usize), Option<Arc<Font>>>>,
}

pub struct Found {
    pub font: Arc<Font>,
    pub family: String,
    /// True when the family asked for is not the family returned.
    pub substituted: bool,
}

fn read_range(f: &mut std::fs::File, off: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut v = vec![0u8; len];
    f.read_exact(&mut v).ok()?;
    Some(v)
}

fn name_of(table: &[u8], id: u16) -> Option<String> {
    let r = R::new(table);
    let count = r.u16(2)? as usize;
    let storage = r.u16(4)? as usize;
    let mut best: Option<(u8, String)> = None;
    for i in 0..count.min(500) {
        let e = 6 + i * 12;
        let (pid, eid, lang, nid, len, off) = (
            r.u16(e)?,
            r.u16(e + 2)?,
            r.u16(e + 4)?,
            r.u16(e + 6)?,
            r.u16(e + 8)? as usize,
            r.u16(e + 10)? as usize,
        );
        if nid != id {
            continue;
        }
        let raw = r.slice(storage + off, len)?;
        let (rank, s) = match (pid, eid) {
            (3, _) | (0, _) => {
                let u: Vec<u16> = raw
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                (
                    if lang == 0x409 { 3 } else { 2 },
                    String::from_utf16_lossy(&u),
                )
            }
            (1, 0) => (1, raw.iter().map(|b| char::from(*b)).collect()),
            _ => continue,
        };
        if best.as_ref().is_none_or(|(r0, _)| rank > *r0) {
            best = Some((rank, s));
        }
    }
    best.map(|(_, s)| s)
}

/// The faces of one font file, read from its table directory without loading it all.
fn faces_of(path: &Path) -> Vec<Face> {
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = f.metadata() else {
        return Vec::new();
    };
    let Some(head) = read_range(&mut f, 0, 16.min(meta.len() as usize)) else {
        return Vec::new();
    };
    let r = R::new(&head);
    let offsets: Vec<u64> = if r.tag(0) == Some(*b"ttcf") {
        let n = r.u32(8).unwrap_or(0).min(64) as usize;
        let Some(list) = read_range(&mut f, 12, n * 4) else {
            return Vec::new();
        };
        (0..n)
            .filter_map(|i| R::new(&list).u32(i * 4).map(u64::from))
            .collect()
    } else {
        vec![0]
    };
    let mut out = Vec::new();
    for (index, off) in offsets.into_iter().enumerate() {
        let Some(h) = read_range(&mut f, off, 12) else {
            continue;
        };
        let hr = R::new(&h);
        let ok = matches!(
            hr.tag(0),
            Some([0, 1, 0, 0] | [b'O', b'T', b'T', b'O'] | [b't', b'r', b'u', b'e'])
        );
        let n = hr.u16(4).unwrap_or(0).min(100) as usize;
        if !ok || n == 0 {
            continue;
        }
        let Some(dir) = read_range(&mut f, off + 12, n * 16) else {
            continue;
        };
        let dr = R::new(&dir);
        let find = |tag: &[u8; 4]| -> Option<(u64, usize)> {
            (0..n).find_map(|i| {
                if dr.tag(i * 16)? == *tag {
                    Some((
                        u64::from(dr.u32(i * 16 + 8)?),
                        dr.u32(i * 16 + 12)? as usize,
                    ))
                } else {
                    None
                }
            })
        };
        let Some((no, nl)) = find(b"name") else {
            continue;
        };
        let Some(name) = read_range(&mut f, no, nl.min(1 << 20)) else {
            continue;
        };
        let family = name_of(&name, 16)
            .or_else(|| name_of(&name, 1))
            .unwrap_or_default();
        let full_name = name_of(&name, 4).unwrap_or_default();
        let (mut weight, mut italic) = (400u16, false);
        if let Some((oo, ol)) = find(b"OS/2") {
            if let Some(os2) = read_range(&mut f, oo, ol.min(80)) {
                let o = R::new(&os2);
                weight = o.u16(4).unwrap_or(400);
                italic = o.u16(62).unwrap_or(0) & 1 != 0;
            }
        }
        let mut mono = false;
        if let Some((po, pl)) = find(b"post") {
            if let Some(post) = read_range(&mut f, po, pl.min(16)) {
                mono = R::new(&post).u32(12).unwrap_or(0) != 0;
            }
        }
        if family.is_empty() {
            continue;
        }
        out.push(Face {
            path: path.to_path_buf(),
            index,
            family: family.to_lowercase(),
            full_name: full_name.to_lowercase(),
            weight,
            italic,
            mono,
        });
    }
    out
}

fn scan_dir(dir: &Path, depth: u32, out: &mut Vec<Face>, seen: &mut usize) {
    if depth > 6 || *seen > 20_000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            scan_dir(&p, depth + 1, out, seen);
        } else if ft.is_file() || ft.is_symlink() {
            let ext = p
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") {
                *seen += 1;
                out.extend(faces_of(&p));
            }
        }
    }
}

/// The font directories named by fontconfig's configuration files, plus the usual places.
pub fn system_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(windows) {
        if let Some(w) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(w).join("Fonts"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(l).join("Microsoft/Windows/Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(
            ["/System/Library/Fonts", "/Library/Fonts"]
                .into_iter()
                .map(PathBuf::from),
        );
        if let Some(h) = &home {
            dirs.push(h.join("Library/Fonts"));
        }
    } else {
        let mut conf_files = vec![PathBuf::from("/etc/fonts/fonts.conf")];
        if let Ok(rd) = std::fs::read_dir("/etc/fonts/conf.d") {
            let mut v: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            v.sort();
            conf_files.extend(v);
        }
        for f in conf_files.iter().take(200) {
            let Ok(text) = std::fs::read_to_string(f) else {
                continue;
            };
            let mut rest = text.as_str();
            while let Some(i) = rest.find("<dir") {
                let tail = rest.get(i + 4..).unwrap_or("");
                let Some(gt) = tail.find('>') else { break };
                let attrs = tail.get(..gt).unwrap_or("");
                let body = tail.get(gt + 1..).unwrap_or("");
                let Some(end) = body.find("</dir>") else {
                    break;
                };
                let d = body.get(..end).unwrap_or("").trim();
                rest = body.get(end + 6..).unwrap_or("");
                if attrs.contains("xdg") {
                    let base = std::env::var_os("XDG_DATA_HOME")
                        .map(PathBuf::from)
                        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
                    if let Some(b) = base {
                        dirs.push(b.join(d));
                    }
                } else if let Some(stripped) = d.strip_prefix("~/") {
                    if let Some(h) = &home {
                        dirs.push(h.join(stripped));
                    }
                } else if !d.is_empty() {
                    dirs.push(PathBuf::from(d));
                }
            }
        }
        for d in ["/usr/share/fonts", "/usr/local/share/fonts"] {
            dirs.push(PathBuf::from(d));
        }
        if let Some(h) = &home {
            dirs.push(h.join(".fonts"));
            dirs.push(h.join(".local/share/fonts"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()) && d.is_dir());
    dirs
}

fn generic(name: &str) -> &'static [&'static str] {
    match name {
        "serif" => &[
            "liberation serif",
            "dejavu serif",
            "noto serif",
            "times new roman",
            "freeserif",
            "tinos",
            "nimbus roman",
            "caladea",
        ],
        "sans-serif" | "system-ui" | "ui-sans-serif" => &[
            "liberation sans",
            "dejavu sans",
            "noto sans",
            "arial",
            "freesans",
            "arimo",
            "nimbus sans",
            "carlito",
            "inter",
        ],
        "monospace" | "ui-monospace" => &[
            "liberation mono",
            "dejavu sans mono",
            "noto sans mono",
            "courier new",
            "freemono",
            "cousine",
            "nimbus mono",
        ],
        "cursive" | "fantasy" => &["dejavu sans", "liberation sans", "noto sans"],
        _ => &[],
    }
}

/// Metric-compatible and look-alike stand-ins for common fonts.
fn substitutes(name: &str) -> &'static [&'static str] {
    match name {
        "arial" | "helvetica" | "helvetica neue" | "arial unicode ms" => &[
            "liberation sans",
            "arimo",
            "nimbus sans",
            "dejavu sans",
            "freesans",
        ],
        "times new roman" | "times" | "georgia" | "palatino" | "palatino linotype"
        | "book antiqua" => &[
            "liberation serif",
            "tinos",
            "nimbus roman",
            "dejavu serif",
            "freeserif",
        ],
        "courier new" | "courier" | "consolas" | "lucida console" | "menlo" | "monaco" => &[
            "liberation mono",
            "cousine",
            "nimbus mono",
            "dejavu sans mono",
            "freemono",
        ],
        "calibri" => &["carlito", "liberation sans", "dejavu sans"],
        "cambria" => &["caladea", "liberation serif", "dejavu serif"],
        "verdana" | "tahoma" | "segoe ui" | "trebuchet ms" | "lucida sans" | "lucida grande"
        | "san francisco" | "-apple-system" | "blinkmacsystemfont" | "roboto" => {
            &["dejavu sans", "liberation sans", "noto sans"]
        }
        _ => &[],
    }
}

impl FontDb {
    pub fn system() -> FontDb {
        Self::from_dirs(&system_dirs())
    }

    pub fn from_dirs(dirs: &[PathBuf]) -> FontDb {
        let mut faces = Vec::new();
        let mut seen = 0usize;
        for d in dirs {
            scan_dir(d, 0, &mut faces, &mut seen);
        }
        FontDb {
            faces,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn empty() -> FontDb {
        FontDb {
            faces: Vec::new(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn load(&self, face: &Face) -> Option<Arc<Font>> {
        let key = (face.path.clone(), face.index);
        let mut cache = self.cache.lock().ok()?;
        if let Some(v) = cache.get(&key) {
            return v.clone();
        }
        let loaded = std::fs::read(&face.path)
            .ok()
            .and_then(|d| Font::parse_index(&d, face.index).ok())
            .map(Arc::new);
        cache.insert(key, loaded.clone());
        loaded
    }

    fn best_in_family(&self, family: &str, weight: u16, italic: bool) -> Option<&Face> {
        self.faces
            .iter()
            .filter(|f| f.family == family || f.full_name == family)
            .min_by_key(|f| {
                let w = i32::from(f.weight) - i32::from(weight);
                (
                    u32::from(f.italic != italic) * 1000 + w.unsigned_abs(),
                    f.path.clone(),
                )
            })
    }

    /// The best installed face for a CSS-style family list, weight and slant.
    pub fn find(&self, families: &[String], weight: u16, italic: bool) -> Option<Found> {
        let wanted: Vec<String> = families
            .iter()
            .map(|f| f.trim().trim_matches(['"', '\'']).to_lowercase())
            .filter(|f| !f.is_empty())
            .collect();
        let mut first = wanted.first().cloned();
        for fam in &wanted {
            let mut candidates: Vec<(&str, bool)> = vec![(fam.as_str(), false)];
            for s in substitutes(fam) {
                candidates.push((s, true));
            }
            for g in generic(fam) {
                candidates.push((g, true));
            }
            for (c, subst) in candidates {
                if let Some(face) = self.best_in_family(c, weight, italic) {
                    if let Some(font) = self.load(face) {
                        return Some(Found {
                            font,
                            family: face.family.clone(),
                            substituted: subst || first.as_deref() != Some(c),
                        });
                    }
                }
            }
            first = first.or(Some(fam.clone()));
        }
        // Anything at all, preferring a sans face.
        for g in generic("sans-serif") {
            if let Some(face) = self.best_in_family(g, weight, italic) {
                if let Some(font) = self.load(face) {
                    return Some(Found {
                        font,
                        family: face.family.clone(),
                        substituted: true,
                    });
                }
            }
        }
        let face = self.faces.first()?;
        Some(Found {
            font: self.load(face)?,
            family: face.family.clone(),
            substituted: true,
        })
    }
}
