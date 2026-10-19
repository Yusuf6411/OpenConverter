//! TAR: V7, ustar, pax (POSIX.1-2001) and GNU long names; writes ustar with pax extensions.
use crate::entry::{Archive, Budget, Entry, EntryKind};
use crate::safety::clean_for_write;
use crate::Context;
use oc_core::{Error, ErrorCode, Result};

const BLOCK: usize = 512;

fn field(h: &[u8], from: usize, to: usize) -> &[u8] {
    h.get(from..to).unwrap_or(&[])
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(b.get(..end).unwrap_or(&[])).into_owned()
}

/// Octal or GNU base-256 number.
fn num(b: &[u8]) -> Result<i64> {
    if let Some(&first) = b.first() {
        if first & 0x80 != 0 {
            let neg = first & 0x40 != 0;
            let mut v: i128 = if neg { -1 } else { 0 };
            for (i, &c) in b.iter().enumerate() {
                let c = if i == 0 { c & 0x7F | if neg { 0x80 } else { 0 } } else { c };
                v = (v << 8) | i128::from(c);
                if v > i128::from(i64::MAX) || v < i128::from(i64::MIN) {
                    return Err(Error::corrupt("tar number out of range"));
                }
            }
            return i64::try_from(v).map_err(|_| Error::corrupt("tar number out of range"));
        }
    }
    let mut v: i64 = 0;
    let mut seen = false;
    for &c in b {
        match c {
            b'0'..=b'7' => {
                v = v
                    .checked_mul(8)
                    .and_then(|x| x.checked_add(i64::from(c - b'0')))
                    .ok_or_else(|| Error::corrupt("tar number overflow"))?;
                seen = true;
            }
            b' ' | 0 if !seen => {}
            b' ' | 0 => break,
            _ => return Err(Error::corrupt("invalid octal digit in tar header")),
        }
    }
    Ok(v)
}

fn checksum_ok(h: &[u8]) -> bool {
    let stored = match num(field(h, 148, 156)) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let mut unsigned: i64 = 0;
    let mut signed: i64 = 0;
    for (i, &c) in h.iter().enumerate() {
        let c = if (148..156).contains(&i) { b' ' } else { c };
        unsigned += i64::from(c);
        signed += i64::from(c as i8);
    }
    stored == unsigned || stored == signed
}

#[derive(Default, Clone)]
struct Pax {
    path: Option<String>,
    linkpath: Option<String>,
    size: Option<u64>,
    mtime: Option<i64>,
}

impl Pax {
    fn merge(&mut self, o: &Pax) {
        if o.path.is_some() {
            self.path = o.path.clone();
        }
        if o.linkpath.is_some() {
            self.linkpath = o.linkpath.clone();
        }
        if o.size.is_some() {
            self.size = o.size;
        }
        if o.mtime.is_some() {
            self.mtime = o.mtime;
        }
    }
}

fn parse_pax(data: &[u8]) -> Result<Pax> {
    let mut p = Pax::default();
    let mut i = 0;
    while i < data.len() {
        let rest = data.get(i..).unwrap_or(&[]);
        let sp = rest
            .iter()
            .position(|&c| c == b' ')
            .ok_or_else(|| Error::corrupt("bad pax record"))?;
        let len: usize = std::str::from_utf8(rest.get(..sp).unwrap_or(&[]))
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::corrupt("bad pax record length"))?;
        if len <= sp + 1 || len > rest.len() {
            return Err(Error::corrupt("pax record length out of range"));
        }
        let body = rest.get(sp + 1..len).unwrap_or(&[]);
        let body = body.strip_suffix(b"\n").unwrap_or(body);
        if let Some(eq) = body.iter().position(|&c| c == b'=') {
            let key = String::from_utf8_lossy(body.get(..eq).unwrap_or(&[])).into_owned();
            let val = String::from_utf8_lossy(body.get(eq + 1..).unwrap_or(&[])).into_owned();
            match key.as_str() {
                "path" => p.path = Some(val),
                "linkpath" => p.linkpath = Some(val),
                "size" => p.size = val.parse().ok(),
                "mtime" => {
                    p.mtime = val
                        .split('.')
                        .next()
                        .and_then(|s| s.parse::<i64>().ok())
                }
                _ => {}
            }
        }
        i += len;
    }
    Ok(p)
}

pub fn read(data: &[u8], cx: &Context) -> Result<Archive> {
    let mut budget = Budget::new(cx.limits);
    let mut archive = Archive::default();
    let mut pos = 0usize;
    let mut global = Pax::default();
    let mut local = Pax::default();
    let mut gnu_name: Option<String> = None;
    let mut gnu_link: Option<String> = None;
    loop {
        cx.progress.check_cancelled()?;
        let Some(h) = data.get(pos..pos + BLOCK) else {
            // A tar without end blocks is tolerated once at least the data is exhausted.
            if pos >= data.len() {
                break;
            }
            return Err(Error::truncated("tar header cut short").at(pos as u64));
        };
        if h.iter().all(|&b| b == 0) {
            break;
        }
        if !checksum_ok(h) {
            return Err(Error::corrupt("tar header checksum mismatch").at(pos as u64));
        }
        let typeflag = h.get(156).copied().unwrap_or(0);
        let mut size = u64::try_from(num(field(h, 124, 136))?)
            .map_err(|_| Error::corrupt("negative tar size"))?;
        let mut pax = global.clone();
        pax.merge(&local);
        if typeflag != b'x' && typeflag != b'g' && typeflag != b'L' && typeflag != b'K' {
            if let Some(s) = pax.size {
                size = s;
            }
        }
        let body_start = pos + BLOCK;
        let padded = size
            .checked_add(511)
            .map(|v| v & !511)
            .ok_or_else(|| Error::corrupt("tar size overflow"))?;
        let body_end = body_start as u64 + size;
        let is_data_type = !matches!(typeflag, b'x' | b'g' | b'L' | b'K' | b'1' | b'2' | b'3' | b'4' | b'5' | b'6');
        if is_data_type && body_end > data.len() as u64 {
            return Err(Error::truncated("tar member data cut short").at(pos as u64));
        }
        let next = (body_start as u64).saturating_add(if is_data_type || matches!(typeflag, b'x' | b'g' | b'L' | b'K') { padded } else { 0 });
        let body: &[u8] = if is_data_type || matches!(typeflag, b'x' | b'g' | b'L' | b'K') {
            let end = usize::try_from(body_end).map_err(|_| Error::corrupt("tar size overflow"))?;
            data.get(body_start..end)
                .ok_or_else(|| Error::truncated("tar metadata cut short").at(pos as u64))?
        } else {
            &[]
        };
        pos = usize::try_from(next).map_err(|_| Error::corrupt("tar size overflow"))?;
        match typeflag {
            b'x' => {
                local = parse_pax(body)?;
                continue;
            }
            b'g' => {
                global.merge(&parse_pax(body)?);
                continue;
            }
            b'L' => {
                gnu_name = Some(cstr(body));
                continue;
            }
            b'K' => {
                gnu_link = Some(cstr(body));
                continue;
            }
            b'S' => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "sparse GNU tar members are not supported",
                ))
            }
            _ => {}
        }
        let mut name = cstr(field(h, 0, 100));
        let magic = field(h, 257, 263);
        if magic.starts_with(b"ustar") && magic.get(5) != Some(&b' ') {
            let prefix = cstr(field(h, 345, 500));
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        let mut linkname = cstr(field(h, 157, 257));
        if let Some(n) = gnu_name.take() {
            name = n;
        }
        if let Some(n) = gnu_link.take() {
            linkname = n;
        }
        if let Some(n) = pax.path.clone() {
            name = n;
        }
        if let Some(n) = pax.linkpath.clone() {
            linkname = n;
        }
        let mtime = pax.mtime.or_else(|| num(field(h, 136, 148)).ok());
        local = Pax::default();
        let mode = (num(field(h, 100, 108)).unwrap_or(0o644) & 0o7777) as u32;
        let name = name.trim_end_matches('/').to_string();
        let kind_dir = typeflag == b'5' || (typeflag == 0 && h.first() != Some(&0) && cstr(field(h, 0, 100)).ends_with('/'));
        let mut entry = match typeflag {
            b'0' | 0 | b'7' if !kind_dir => Entry::from_body(name, body, cx.list_only),
            b'5' => Entry::dir(name),
            _ if kind_dir => Entry::dir(name),
            b'2' => Entry::symlink(name, linkname),
            b'1' => {
                let src = linkname.trim_end_matches('/');
                let found = archive
                    .entries
                    .iter()
                    .find(|e| e.path == src && e.kind == EntryKind::File)
                    .ok_or_else(|| Error::corrupt("hard link to unknown member"))?;
                let mut e = if cx.list_only {
                    Entry::listed(name, found.size())
                } else {
                    Entry::file(name, found.data.clone())
                };
                e.mode = found.mode;
                e
            }
            // Devices, FIFOs and unknown types carry no convertible content.
            _ => continue,
        };
        if entry.path.is_empty() {
            return Err(Error::corrupt("empty tar member name"));
        }
        entry.mode = if entry.kind == EntryKind::Symlink { 0o777 } else { mode };
        entry.mtime = mtime;
        budget.entry()?;
        budget.add_output(entry.size())?;
        archive.entries.push(entry);
    }
    Ok(archive)
}

// ---- writer ----

fn put_octal(buf: &mut [u8], v: u64) {
    let w = buf.len() - 1;
    let s = format!("{v:0w$o}");
    let s = s.as_bytes();
    let s = s.get(s.len().saturating_sub(w)..).unwrap_or(s);
    for (d, c) in buf.iter_mut().zip(s.iter().chain(std::iter::once(&0))) {
        *d = *c;
    }
}

fn pax_record(key: &str, val: &str) -> Vec<u8> {
    let base = key.len() + val.len() + 3; // ' ' '=' '\n'
    let mut len = base;
    loop {
        let total = base + len.to_string().len();
        if total == len {
            break;
        }
        len = total;
    }
    format!("{len} {key}={val}\n").into_bytes()
}

fn header(name: &str, prefix: &str, size: u64, mode: u32, mtime: u64, typeflag: u8, link: &str) -> [u8; BLOCK] {
    let mut h = [0u8; BLOCK];
    let put = |h: &mut [u8; BLOCK], at: usize, s: &str, max: usize| {
        for (i, b) in s.bytes().take(max).enumerate() {
            h[at + i] = b;
        }
    };
    put(&mut h, 0, name, 100);
    put_octal(&mut h[100..108], u64::from(mode & 0o7777));
    put_octal(&mut h[108..116], 0);
    put_octal(&mut h[116..124], 0);
    put_octal(&mut h[124..136], size);
    put_octal(&mut h[136..148], mtime);
    h[156] = typeflag;
    put(&mut h, 157, link, 100);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    put(&mut h, 345, prefix, 155);
    for b in &mut h[148..156] {
        *b = b' ';
    }
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
    h
}

/// Split a long path at a '/' into (prefix, name) when both fit ustar fields.
fn split_path(path: &str) -> Option<(&str, &str)> {
    if path.len() <= 100 {
        return Some(("", path));
    }
    for (i, _) in path.match_indices('/') {
        let (p, n) = (&path[..i], &path[i + 1..]);
        if p.len() <= 155 && n.len() <= 100 && !n.is_empty() {
            return Some((p, n));
        }
    }
    None
}

pub fn write(archive: &Archive, cx: &Context) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for e in &archive.entries {
        cx.progress.check_cancelled()?;
        let clean = clean_for_write(&e.path)?;
        let is_dir = e.kind == EntryKind::Dir;
        let path = if is_dir { format!("{clean}/") } else { clean };
        let link = e.link.clone().unwrap_or_default();
        let mtime = e.mtime;
        let size = if e.kind == EntryKind::File { e.size() } else { 0 };
        let mut pax: Vec<u8> = Vec::new();
        let split = if path.is_ascii() { split_path(&path) } else { None };
        let (prefix, name) = match split {
            Some(x) => x,
            None => {
                pax.extend(pax_record("path", &path));
                ("", "")
            }
        };
        let (mut hname, mut hprefix) = (name.to_string(), prefix.to_string());
        if hname.is_empty() {
            // Header still needs a plausible name; truncate on a char boundary.
            let mut cut = path.len().min(99);
            while !path.is_char_boundary(cut) {
                cut -= 1;
            }
            hname = path[..cut].to_string();
            hprefix.clear();
        }
        if !link.is_empty() && (link.len() > 100 || !link.is_ascii()) {
            pax.extend(pax_record("linkpath", &link));
        }
        if size > 0o77777777777 {
            pax.extend(pax_record("size", &size.to_string()));
        }
        let hmtime = match mtime {
            Some(t) if (0..=0o77777777777).contains(&t) => t as u64,
            Some(t) => {
                pax.extend(pax_record("mtime", &t.to_string()));
                0
            }
            None => 0,
        };
        if !pax.is_empty() {
            let h = header(
                &format!("PaxHeader/{}", hname.chars().take(80).collect::<String>()),
                "",
                pax.len() as u64,
                0o644,
                hmtime,
                b'x',
                "",
            );
            out.extend_from_slice(&h);
            out.extend_from_slice(&pax);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
        let typeflag = match e.kind {
            EntryKind::File => b'0',
            EntryKind::Dir => b'5',
            EntryKind::Symlink => b'2',
        };
        let hsize = size.min(0o77777777777);
        let h = header(&hname, &hprefix, hsize, e.mode, hmtime, typeflag, &link);
        out.extend_from_slice(&h);
        if e.kind == EntryKind::File {
            out.extend_from_slice(&e.data);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
    }
    out.resize(out.len() + 2 * BLOCK, 0);
    Ok(out)
}
