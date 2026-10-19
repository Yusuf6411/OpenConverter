//! Unix `ar` archives (GNU and BSD long-name conventions); writes the GNU variant.
use crate::entry::{Archive, Budget, Entry};
use crate::safety::clean_for_write;
use crate::Context;
use oc_core::{Error, Result};

const MAGIC: &[u8] = b"!<arch>\n";

fn txt(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap_or("").trim()
}

pub fn read(data: &[u8], cx: &Context) -> Result<Archive> {
    if data.get(..8) != Some(MAGIC) {
        return Err(Error::corrupt("ar: bad magic"));
    }
    let mut budget = Budget::new(cx.limits);
    let mut archive = Archive::default();
    let mut names: Vec<u8> = Vec::new();
    let mut pos = 8usize;
    while pos < data.len() {
        cx.progress.check_cancelled()?;
        let h = data
            .get(pos..pos + 60)
            .ok_or_else(|| Error::truncated("ar: header cut short").at(pos as u64))?;
        if h.get(58..60) != Some(b"`\n") {
            return Err(Error::corrupt("ar: bad header terminator").at(pos as u64));
        }
        let raw_name = txt(h.get(..16).unwrap_or(&[])).to_string();
        let mtime = txt(h.get(16..28).unwrap_or(&[])).parse::<i64>().ok();
        let mode = u32::from_str_radix(txt(h.get(40..48).unwrap_or(&[])), 8).unwrap_or(0o644);
        let mut size: usize = txt(h.get(48..58).unwrap_or(&[]))
            .parse()
            .map_err(|_| Error::corrupt("ar: bad member size"))?;
        let mut start = pos + 60;
        let mut name = raw_name.clone();
        if let Some(n) = raw_name.strip_prefix("#1/") {
            // BSD: the name precedes the data and counts toward the size.
            let nl: usize = n.parse().map_err(|_| Error::corrupt("ar: bad BSD name length"))?;
            let nb = data
                .get(start..start + nl)
                .ok_or_else(|| Error::truncated("ar: name cut short"))?;
            name = String::from_utf8_lossy(nb.split(|&c| c == 0).next().unwrap_or(&[])).into_owned();
            start += nl;
            size = size.checked_sub(nl).ok_or_else(|| Error::corrupt("ar: name longer than member"))?;
        }
        let end = start
            .checked_add(size)
            .ok_or_else(|| Error::corrupt("ar: size overflow"))?;
        let body = data
            .get(start..end)
            .ok_or_else(|| Error::truncated("ar: member data cut short").at(start as u64))?;
        pos = end + (size & 1);
        if raw_name == "//" {
            names = body.to_vec();
            continue;
        }
        if raw_name == "/" || raw_name == "/SYM64/" || raw_name.starts_with("__.SYMDEF") {
            continue; // symbol tables carry no files
        }
        if let Some(idx) = raw_name.strip_prefix('/').and_then(|d| d.parse::<usize>().ok()) {
            let tail = names.get(idx..).unwrap_or(&[]);
            let end = tail.iter().position(|&c| c == b'\n' || c == 0).unwrap_or(tail.len());
            name = String::from_utf8_lossy(tail.get(..end).unwrap_or(&[])).into_owned();
        }
        let name = name.trim_end_matches('/').to_string();
        if name.is_empty() {
            continue;
        }
        budget.entry()?;
        budget.add_output(body.len() as u64)?;
        let mut e = Entry::file(name, body.to_vec());
        e.mode = mode & 0o7777;
        e.mtime = mtime;
        archive.entries.push(e);
    }
    Ok(archive)
}

pub fn write(archive: &Archive, cx: &Context) -> Result<Vec<u8>> {
    let mut table: Vec<u8> = Vec::new();
    let mut slots: Vec<String> = Vec::new();
    for e in &archive.entries {
        if e.kind != crate::EntryKind::File {
            return Err(Error::new(
                oc_core::ErrorCode::UnsupportedVariant,
                "ar archives hold only regular files",
            ));
        }
        let n = clean_for_write(&e.path)?;
        if n.contains('/') {
            return Err(Error::new(
                oc_core::ErrorCode::UnsupportedVariant,
                format!("ar members cannot have directories in their names: {n}"),
            ));
        }
        if n.len() > 15 {
            let off = table.len();
            table.extend_from_slice(n.as_bytes());
            table.extend_from_slice(b"/\n");
            slots.push(format!("/{off}"));
        } else {
            slots.push(format!("{n}/"));
        }
    }
    let mut out = MAGIC.to_vec();
    let hdr = |out: &mut Vec<u8>, name: &str, mtime: i64, mode: u32, size: usize| {
        out.extend_from_slice(
            format!("{name:<16}{mtime:<12}{:<6}{:<6}{mode:<8o}{size:<10}`\n", 0, 0).as_bytes(),
        );
    };
    if !table.is_empty() {
        hdr(&mut out, "//", 0, 0, table.len());
        out.extend_from_slice(&table);
        if table.len() & 1 == 1 {
            out.push(b'\n');
        }
    }
    for (e, slot) in archive.entries.iter().zip(slots.iter()) {
        cx.progress.check_cancelled()?;
        hdr(&mut out, slot, e.mtime.unwrap_or(0).max(0), 0o100_000 | (e.mode & 0o7777), e.data.len());
        out.extend_from_slice(&e.data);
        if e.data.len() & 1 == 1 {
            out.push(b'\n');
        }
    }
    Ok(out)
}
