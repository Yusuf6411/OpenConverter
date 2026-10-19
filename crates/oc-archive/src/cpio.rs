//! CPIO: reads "newc"/"crc" (SVR4) and "odc" (POSIX.1) archives, writes newc.
use crate::entry::{Archive, Budget, Entry, EntryKind};
use crate::safety::clean_for_write;
use crate::Context;
use oc_core::{Error, ErrorCode, Result};

fn parse(b: &[u8], radix: u32) -> Result<u64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::corrupt("cpio: non-ASCII header field"))?;
    u64::from_str_radix(s, radix).map_err(|_| Error::corrupt("cpio: bad header number"))
}

pub fn read(data: &[u8], cx: &Context) -> Result<Archive> {
    let mut budget = Budget::new(cx.limits);
    let mut archive = Archive::default();
    let mut pos = 0usize;
    // (inode, index) of zero-size hard-link members awaiting their data.
    let mut links: Vec<(u64, usize)> = Vec::new();
    loop {
        cx.progress.check_cancelled()?;
        let magic = data
            .get(pos..pos + 6)
            .ok_or_else(|| Error::truncated("cpio: header cut short").at(pos as u64))?;
        let (fields, hdr_len, radix, name_pad4) = match magic {
            b"070701" | b"070702" => (13usize, 110usize, 16u32, true),
            b"070707" => (0, 76, 8, false),
            _ => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "cpio: only newc, crc and odc formats are supported",
                ))
            }
        };
        let h = data
            .get(pos..pos + hdr_len)
            .ok_or_else(|| Error::truncated("cpio: header cut short").at(pos as u64))?;
        let (ino, mode, mtime, size, namesize) = if fields == 13 {
            let f = |i: usize| parse(h.get(6 + i * 8..14 + i * 8).unwrap_or(&[]), radix);
            (f(0)?, f(1)?, f(5)?, f(6)?, f(11)?)
        } else {
            let f = |a: usize, b: usize| parse(h.get(a..b).unwrap_or(&[]), radix);
            (f(12, 18)?, f(18, 24)?, f(48, 59)?, f(65, 76)?, f(59, 65)?)
        };
        let nstart = pos + hdr_len;
        let nend = nstart + usize::try_from(namesize).map_err(|_| Error::corrupt("cpio: name size"))?;
        let name_raw = data
            .get(nstart..nend)
            .ok_or_else(|| Error::truncated("cpio: name cut short"))?;
        let name = String::from_utf8_lossy(name_raw.split(|&c| c == 0).next().unwrap_or(&[])).into_owned();
        let dstart = if name_pad4 { (nend + 3) & !3 } else { nend };
        if name == "TRAILER!!!" {
            break;
        }
        let dend = dstart
            .checked_add(usize::try_from(size).map_err(|_| Error::corrupt("cpio: size overflow"))?)
            .ok_or_else(|| Error::corrupt("cpio: size overflow"))?;
        let body = data
            .get(dstart..dend)
            .ok_or_else(|| Error::truncated("cpio: member data cut short").at(dstart as u64))?;
        pos = if name_pad4 { (dend + 3) & !3 } else { dend };
        let name = name.trim_start_matches("./").trim_end_matches('/').to_string();
        if name.is_empty() || name == "." {
            continue;
        }
        let mut e = match mode & 0o170_000 {
            0o040_000 => Entry::dir(name),
            0o120_000 => Entry::symlink(name, String::from_utf8_lossy(body).into_owned()),
            0o100_000 => Entry::from_body(name, body, cx.list_only),
            _ => continue,
        };
        e.mode = (mode & 0o7777) as u32;
        e.mtime = i64::try_from(mtime).ok();
        budget.entry()?;
        budget.add_output(size)?;
        if e.kind == EntryKind::File && body.is_empty() && fields == 13 && !cx.list_only {
            links.push((ino, archive.entries.len()));
        }
        archive.entries.push(e);
    }
    // newc stores hard-linked data once, on the last link: copy it to the empty ones.
    let mut inos: Vec<(u64, Vec<u8>)> = Vec::new();
    {
        // Re-scan is unnecessary: gather data of non-empty files by inode through a second pass.
        let mut p = 0usize;
        while let Some(m) = data.get(p..p + 6) {
            if m != b"070701" && m != b"070702" {
                break;
            }
            let h = data.get(p..p + 110).unwrap_or(&[]);
            let f = |i: usize| parse(h.get(6 + i * 8..14 + i * 8).unwrap_or(&[]), 16).unwrap_or(0);
            let (ino, size, ns) = (f(0), f(6) as usize, f(11) as usize);
            let ds = (p + 110 + ns + 3) & !3;
            if size > 0 {
                if let Some(b) = data.get(ds..ds + size) {
                    inos.push((ino, b.to_vec()));
                }
            }
            p = (ds + size + 3) & !3;
        }
    }
    for (ino, idx) in links {
        if let Some((_, d)) = inos.iter().find(|(i, _)| *i == ino) {
            if let Some(e) = archive.entries.get_mut(idx) {
                e.data = d.clone();
            }
        }
    }
    Ok(archive)
}

pub fn write(archive: &Archive, cx: &Context) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let pad4 = |v: &mut Vec<u8>| v.resize(v.len().div_ceil(4) * 4, 0);
    let emit = |out: &mut Vec<u8>, ino: u32, mode: u32, mtime: u32, nlink: u32, name: &str, body: &[u8]| {
        let mut nm = name.as_bytes().to_vec();
        nm.push(0);
        out.extend_from_slice(b"070701");
        for v in [
            ino,
            mode,
            0,
            0,
            nlink,
            mtime,
            body.len() as u32,
            0,
            0,
            0,
            0,
            nm.len() as u32,
            0,
        ] {
            out.extend_from_slice(format!("{v:08X}").as_bytes());
        }
        out.extend_from_slice(&nm);
        pad4(out);
        out.extend_from_slice(body);
        pad4(out);
    };
    for (i, e) in archive.entries.iter().enumerate() {
        cx.progress.check_cancelled()?;
        let name = clean_for_write(&e.path)?;
        let mtime = e.mtime.unwrap_or(0).clamp(0, i64::from(u32::MAX)) as u32;
        if e.size() > u64::from(u32::MAX) {
            return Err(Error::limit("max_output", "cpio newc cannot hold members over 4 GiB"));
        }
        let (mode, body, nlink): (u32, Vec<u8>, u32) = match e.kind {
            EntryKind::File => (0o100_000 | (e.mode & 0o7777), e.data.clone(), 1),
            EntryKind::Dir => (0o040_000 | (e.mode & 0o7777), Vec::new(), 2),
            EntryKind::Symlink => (0o120_777, e.link.clone().unwrap_or_default().into_bytes(), 1),
        };
        emit(&mut out, i as u32 + 1, mode, mtime, nlink, &name, &body);
    }
    emit(&mut out, 0, 0, 0, 1, "TRAILER!!!", &[]);
    // Conventional 512-byte block padding.
    out.resize(out.len().div_ceil(512) * 512, 0);
    Ok(out)
}
