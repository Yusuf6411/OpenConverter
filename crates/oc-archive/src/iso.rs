//! ISO 9660 images: primary and Joliet volume descriptors, Rock Ridge names, permissions,
//! symlinks and timestamps. Read only.
use crate::entry::{Archive, Budget, Entry};
use crate::Context;
use oc_core::{Error, ErrorCode, Result};

const SECTOR: usize = 2048;

#[derive(Clone, Copy)]
struct Vd {
    root_lba: u32,
    root_len: u32,
    joliet: bool,
}

struct Record {
    lba: u32,
    len: u32,
    flags: u8,
    name: Vec<u8>,
    mtime: Option<i64>,
    sys: Vec<u8>,
}

fn le32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .unwrap_or(0)
}

fn dir_date(b: &[u8]) -> Option<i64> {
    let d = b.get(..7)?;
    if d[1] == 0 {
        return None;
    }
    let days = crate::zip::days_from_civil(1900 + i64::from(d[0]), i64::from(d[1]), i64::from(d[2]));
    let tz = i64::from(d[6] as i8) * 900;
    Some(days * 86400 + i64::from(d[3]) * 3600 + i64::from(d[4]) * 60 + i64::from(d[5]) - tz)
}

fn parse_record(b: &[u8]) -> Option<Record> {
    let len = usize::from(*b.first()?);
    if len < 34 || len > b.len() {
        return None;
    }
    let nlen = usize::from(*b.get(32)?);
    let name = b.get(33..33 + nlen)?.to_vec();
    let mut su = 33 + nlen;
    if nlen % 2 == 0 {
        su += 1; // padding byte
    }
    let sys = b.get(su.min(len)..len).unwrap_or(&[]).to_vec();
    Some(Record {
        lba: le32(b, 2),
        len: le32(b, 10),
        flags: b.get(25).copied().unwrap_or(0),
        name,
        mtime: dir_date(b.get(18..25)?),
        sys,
    })
}

#[derive(Default)]
struct Rr {
    name: Option<String>,
    mode: Option<u32>,
    link: Option<String>,
    mtime: Option<i64>,
    child_link: Option<u32>,
    relocated: bool,
    present: bool,
}

fn parse_rr(data: &[u8], sys: &[u8], depth: u32) -> Rr {
    let mut rr = Rr::default();
    let mut buf = sys.to_vec();
    let mut link_parts: Vec<String> = Vec::new();
    let mut name = String::new();
    let mut hops = 0;
    'outer: loop {
        let mut i = 0usize;
        let mut next: Option<Vec<u8>> = None;
        while i + 4 <= buf.len() {
            let (sig, len) = (&buf[i..i + 2], usize::from(buf[i + 2]));
            if len < 4 || i + len > buf.len() {
                break;
            }
            let body = &buf[i + 4..i + len];
            match sig {
                b"SP" | b"RR" => rr.present = true,
                b"NM" if !body.is_empty() => {
                    if body[0] & 0x06 == 0 {
                        name.push_str(&String::from_utf8_lossy(&body[1..]));
                    }
                    rr.present = true;
                }
                b"PX" if body.len() >= 4 => {
                    rr.mode = Some(le32(body, 0));
                    rr.present = true;
                }
                b"SL" if !body.is_empty() => {
                    let mut j = 1usize;
                    while j + 2 <= body.len() {
                        let (fl, l) = (body[j], usize::from(body[j + 1]));
                        let c = body.get(j + 2..j + 2 + l).unwrap_or(&[]);
                        link_parts.push(if fl & 0x08 != 0 {
                            String::new()
                        } else if fl & 0x04 != 0 {
                            "..".into()
                        } else if fl & 0x02 != 0 {
                            ".".into()
                        } else {
                            String::from_utf8_lossy(c).into_owned()
                        });
                        j += 2 + l;
                    }
                }
                b"TF" if !body.is_empty() => {
                    let fl = body[0];
                    let long = fl & 0x80 != 0;
                    let sz = if long { 17 } else { 7 };
                    let mut j = 1usize;
                    for bit in 0..4 {
                        if fl & (1 << bit) != 0 {
                            if bit == 1 && !long {
                                rr.mtime = body.get(j..j + 7).and_then(dir_date);
                            }
                            j += sz;
                        }
                    }
                }
                b"CL" if body.len() >= 4 => rr.child_link = Some(le32(body, 0)),
                b"RE" => rr.relocated = true,
                b"CE" if body.len() >= 24 && hops < 8 && depth < 8 => {
                    let (lba, off, l) = (le32(body, 0) as usize, le32(body, 8) as usize, le32(body, 16) as usize);
                    let start = lba * SECTOR + off;
                    if let Some(c) = data.get(start..start + l) {
                        next = Some(c.to_vec());
                    }
                }
                _ => {}
            }
            i += len;
        }
        match next {
            Some(n) => {
                buf = n;
                hops += 1;
            }
            None => break 'outer,
        }
    }
    if !name.is_empty() {
        rr.name = Some(name);
    }
    if !link_parts.is_empty() {
        rr.link = Some(if link_parts.first().is_some_and(String::is_empty) && link_parts.len() > 1 {
            format!("/{}", link_parts[1..].join("/"))
        } else {
            link_parts.join("/")
        });
    }
    rr
}

fn iso_name(raw: &[u8], joliet: bool) -> String {
    let s = if joliet {
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    let s = match s.rsplit_once(';') {
        Some((a, v)) if v.chars().all(|c| c.is_ascii_digit()) => a.to_string(),
        _ => s,
    };
    s.strip_suffix('.').map(str::to_string).unwrap_or(s)
}

struct Walker<'a, 'c> {
    data: &'a [u8],
    cx: &'a Context<'c>,
    budget: Budget<'a>,
    out: Archive,
    visited: std::collections::HashSet<u32>,
    joliet: bool,
    use_rr: bool,
}

impl Walker<'_, '_> {
    fn extent(&self, lba: u32, len: u32) -> Result<&[u8]> {
        let a = lba as usize * SECTOR;
        self.data
            .get(a..a + len as usize)
            .ok_or_else(|| Error::truncated("ISO: extent lies beyond the end of the image").at(a as u64))
    }

    fn walk(&mut self, lba: u32, len: u32, prefix: &str, depth: u32) -> Result<()> {
        self.cx.limits.check_depth(depth)?;
        if !self.visited.insert(lba) {
            return Err(Error::corrupt("ISO: directory loop"));
        }
        let dir = self.extent(lba, len)?.to_vec();
        let mut pos = 0usize;
        // Multi-extent files: consecutive records with the same name.
        let mut pending: Option<(String, Vec<u8>, Option<i64>, u32, bool)> = None;
        while pos < dir.len() {
            self.cx.progress.check_cancelled()?;
            let l = usize::from(dir[pos]);
            if l == 0 {
                pos = (pos / SECTOR + 1) * SECTOR;
                continue;
            }
            let Some(rec) = dir.get(pos..pos + l).and_then(parse_record) else {
                return Err(Error::corrupt("ISO: bad directory record").at((lba as usize * SECTOR + pos) as u64));
            };
            pos += l;
            if rec.name == [0] || rec.name == [1] {
                continue;
            }
            let rr = if self.use_rr { parse_rr(self.data, &rec.sys, 0) } else { Rr::default() };
            if rr.relocated {
                continue;
            }
            let name = rr.name.clone().unwrap_or_else(|| iso_name(&rec.name, self.joliet));
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let is_dir = rec.flags & 2 != 0 || rr.child_link.is_some();
            self.budget.entry()?;
            if is_dir {
                let (dl, dlen) = match rr.child_link {
                    Some(cl) => {
                        let hdr = self.extent(cl, 34)?;
                        (cl, le32(hdr, 10))
                    }
                    None => (rec.lba, rec.len),
                };
                let mut e = Entry::dir(path.clone());
                e.mtime = rr.mtime.or(rec.mtime);
                if let Some(m) = rr.mode {
                    e.mode = m & 0o7777;
                }
                self.out.entries.push(e);
                self.walk(dl, dlen, &path, depth + 1)?;
                continue;
            }
            if let Some(link) = rr.link.clone().filter(|_| rr.mode.is_some_and(|m| m & 0o170_000 == 0o120_000)) {
                let mut e = Entry::symlink(path, link);
                e.mtime = rr.mtime.or(rec.mtime);
                self.out.entries.push(e);
                continue;
            }
            self.budget.add_output(u64::from(rec.len))?;
            let body = self.extent(rec.lba, rec.len)?;
            let multi = rec.flags & 0x80 != 0;
            let mut acc = match pending.take() {
                Some((n, d, _, _, _)) if n == path => d,
                _ => Vec::new(),
            };
            acc.extend_from_slice(body);
            if multi {
                pending = Some((path, acc, rec.mtime, rr.mode.unwrap_or(0o644), true));
                continue;
            }
            let mut e = Entry::file(path, acc);
            e.mtime = rr.mtime.or(rec.mtime);
            if let Some(m) = rr.mode {
                e.mode = m & 0o7777;
            }
            self.out.entries.push(e);
        }
        if let Some((path, acc, mtime, mode, _)) = pending {
            let mut e = Entry::file(path, acc);
            e.mtime = mtime;
            e.mode = mode & 0o7777;
            self.out.entries.push(e);
        }
        Ok(())
    }
}

pub fn read(data: &[u8], cx: &Context) -> Result<Archive> {
    let mut primary: Option<Vd> = None;
    let mut joliet: Option<Vd> = None;
    let mut sec = 16usize;
    loop {
        let Some(d) = data.get(sec * SECTOR..(sec + 1) * SECTOR) else {
            return Err(Error::truncated("ISO: volume descriptors cut short"));
        };
        if d.get(1..6) != Some(b"CD001") {
            return Err(Error::corrupt("ISO: not an ISO 9660 volume").at((sec * SECTOR) as u64));
        }
        match d[0] {
            1 => {
                primary = Some(Vd {
                    root_lba: le32(d, 158),
                    root_len: le32(d, 166),
                    joliet: false,
                })
            }
            2 if d.get(88..91).is_some_and(|e| matches!(e, b"%/@" | b"%/C" | b"%/E")) => {
                joliet = Some(Vd {
                    root_lba: le32(d, 158),
                    root_len: le32(d, 166),
                    joliet: true,
                })
            }
            255 => break,
            _ => {}
        }
        sec += 1;
        if sec > 64 {
            break;
        }
    }
    let pvd = primary.ok_or_else(|| Error::new(ErrorCode::UnsupportedVariant, "ISO: no primary volume descriptor (UDF-only images are not supported)"))?;
    // Rock Ridge lives in the primary tree; check the root's "." record for SUSP.
    let root = data
        .get(pvd.root_lba as usize * SECTOR..)
        .and_then(|b| b.get(..255).or(Some(b)))
        .and_then(parse_record);
    let has_rr = root.as_ref().is_some_and(|r| {
        let rr = parse_rr(data, &r.sys, 0);
        rr.present
    });
    let (vd, use_rr) = match (has_rr, joliet) {
        (true, _) => (pvd, true),
        (false, Some(j)) => (j, false),
        (false, None) => (pvd, false),
    };
    let mut w = Walker {
        data,
        cx,
        budget: Budget::new(cx.limits),
        out: Archive::default(),
        visited: std::collections::HashSet::new(),
        joliet: vd.joliet,
        use_rr,
    };
    w.walk(vd.root_lba, vd.root_len, "", 0)?;
    Ok(w.out)
}
