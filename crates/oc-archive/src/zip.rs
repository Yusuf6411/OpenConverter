//! ZIP (PKWARE APPNOTE 6.3.x): reads and writes stored, deflate, bzip2, LZMA, zstd and xz
//! members, ZIP64, Unicode names, timestamps, Unix permissions and symlinks, WinZip AES and
//! traditional encryption; reads Deflate64.
use crate::crypto::{self, AesKey, ZipCrypto};
use crate::entry::{Archive, Budget, Entry, EntryKind};
use crate::safety::clean_for_write;
use crate::Context;
use oc_checksum::crc32;
use oc_compress::{bzip2, deflate, inflate, lzma, xz, zstd};
use oc_core::{Error, ErrorCode, Result};
use oc_io::ByteReader;

const SIG_LOCAL: u32 = 0x0403_4B50;
const SIG_CENTRAL: u32 = 0x0201_4B50;
const SIG_EOCD: u32 = 0x0605_4B50;
const SIG_EOCD64: u32 = 0x0606_4B50;
const SIG_LOC64: u32 = 0x0706_4B50;

const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

fn decode_name(raw: &[u8], utf8_flag: bool) -> String {
    if utf8_flag {
        return String::from_utf8_lossy(raw).into_owned();
    }
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    let high: Vec<char> = CP437_HIGH.chars().collect();
    raw.iter()
        .map(|&b| {
            if b < 0x80 {
                char::from(b)
            } else {
                high.get(usize::from(b - 0x80)).copied().unwrap_or('?')
            }
        })
        .collect()
}

fn dos_to_unix(date: u16, time: u16) -> Option<i64> {
    if date == 0 {
        return None;
    }
    let year = i64::from(date >> 9) + 1980;
    let month = i64::from((date >> 5) & 0xF);
    let day = i64::from(date & 0x1F);
    if !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let (h, m, s) = (
        i64::from(time >> 11),
        i64::from((time >> 5) & 0x3F),
        i64::from(time & 0x1F) * 2,
    );
    Some(days_from_civil(year, month, day) * 86400 + h * 3600 + m * 60 + s)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn unix_to_dos(t: i64) -> (u16, u16) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    if y < 1980 {
        return (0x21, 0); // 1980-01-01
    }
    if y > 2107 {
        return (0xFF9F, 0xBF7D);
    }
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time = (((secs / 3600) as u16) << 11) | ((((secs % 3600) / 60) as u16) << 5) | ((secs % 60 / 2) as u16);
    (date, time)
}

#[derive(Default)]
struct Extra {
    zip64: Option<Vec<u8>>,
    unicode_name: Option<(u32, String)>,
    mtime: Option<i64>,
    aes: Option<(u16, u8, u16)>, // version, strength, real method
}

fn parse_extra(mut b: &[u8]) -> Extra {
    let mut ex = Extra::default();
    while b.len() >= 4 {
        let id = u16::from_le_bytes([b[0], b[1]]);
        let len = usize::from(u16::from_le_bytes([b[2], b[3]]));
        let Some(body) = b.get(4..4 + len) else { break };
        match id {
            0x0001 => ex.zip64 = Some(body.to_vec()),
            0x7075 if body.len() >= 5 => {
                let crc = u32::from_le_bytes([body[1], body[2], body[3], body[4]]);
                ex.unicode_name = Some((crc, String::from_utf8_lossy(&body[5..]).into_owned()));
            }
            0x5455 if body.len() >= 5 && body[0] & 1 != 0 => {
                ex.mtime = Some(i64::from(i32::from_le_bytes([body[1], body[2], body[3], body[4]])));
            }
            0x9901 if body.len() >= 7 => {
                ex.aes = Some((
                    u16::from_le_bytes([body[0], body[1]]),
                    body[4],
                    u16::from_le_bytes([body[5], body[6]]),
                ));
            }
            _ => {}
        }
        b = b.get(4 + len..).unwrap_or(&[]);
    }
    ex
}

struct Eocd {
    entries: u64,
    cd_size: u64,
    cd_offset: u64,
    /// Position of the (64-bit) end record, to detect data prepended before the archive.
    end_pos: u64,
}

fn find_eocd(data: &[u8]) -> Result<Eocd> {
    if data.len() < 22 {
        return Err(Error::truncated("ZIP: file too small"));
    }
    let lo = data.len().saturating_sub(22 + 65535);
    let mut p = data.len() - 22;
    loop {
        if data.get(p..p + 4) == Some(&SIG_EOCD.to_le_bytes()[..]) {
            let mut r = ByteReader::new(data.get(p + 4..).unwrap_or(&[]));
            let _disk = r.u16_le()?;
            let _cd_disk = r.u16_le()?;
            let _n_disk = r.u16_le()?;
            let n = u64::from(r.u16_le()?);
            let size = u64::from(r.u32_le()?);
            let off = u64::from(r.u32_le()?);
            let clen = usize::from(r.u16_le()?);
            if p + 22 + clen <= data.len() {
                let mut e = Eocd {
                    entries: n,
                    cd_size: size,
                    cd_offset: off,
                    end_pos: p as u64,
                };
                if (n == 0xFFFF || size == 0xFFFF_FFFF || off == 0xFFFF_FFFF) && p >= 20 {
                    if let Some(loc) = data.get(p - 20..p) {
                        if loc.get(..4) == Some(&SIG_LOC64.to_le_bytes()[..]) {
                            let o64 = u64::from_le_bytes(loc[8..16].try_into().unwrap_or([0; 8]));
                            read_eocd64(data, o64, &mut e)?;
                        }
                    }
                }
                return Ok(e);
            }
        }
        if p == lo {
            break;
        }
        p -= 1;
    }
    Err(Error::corrupt("ZIP: end of central directory not found"))
}

fn read_eocd64(data: &[u8], off: u64, e: &mut Eocd) -> Result<()> {
    let off = usize::try_from(off).map_err(|_| Error::corrupt("ZIP64: bad offset"))?;
    let rec = data.get(off..).unwrap_or(&[]);
    let mut r = ByteReader::new(rec);
    if r.u32_le()? != SIG_EOCD64 {
        return Err(Error::corrupt("ZIP64: end record signature mismatch"));
    }
    let _size = r.u64_le()?;
    r.skip(2 + 2 + 4 + 4)?;
    let _n_disk = r.u64_le()?;
    e.entries = r.u64_le()?;
    e.cd_size = r.u64_le()?;
    e.cd_offset = r.u64_le()?;
    e.end_pos = off as u64;
    Ok(())
}

struct Central {
    name_raw: Vec<u8>,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    lho: u64,
    made_by: u16,
    ext_attr: u32,
    extra: Extra,
}

fn read_central(r: &mut ByteReader<'_>) -> Result<Central> {
    if r.u32_le()? != SIG_CENTRAL {
        return Err(Error::corrupt("ZIP: bad central directory signature"));
    }
    let made_by = r.u16_le()?;
    let _need = r.u16_le()?;
    let flags = r.u16_le()?;
    let method = r.u16_le()?;
    let dos_time = r.u16_le()?;
    let dos_date = r.u16_le()?;
    let crc = r.u32_le()?;
    let csize32 = r.u32_le()?;
    let usize32 = r.u32_le()?;
    let nlen = usize::from(r.u16_le()?);
    let elen = usize::from(r.u16_le()?);
    let clen = usize::from(r.u16_le()?);
    let _disk = r.u16_le()?;
    let _int = r.u16_le()?;
    let ext_attr = r.u32_le()?;
    let lho32 = r.u32_le()?;
    let name_raw = r.take(nlen)?.to_vec();
    let extra = parse_extra(r.take(elen)?);
    r.skip(clen)?;
    let (mut csize, mut usz, mut lho) = (u64::from(csize32), u64::from(usize32), u64::from(lho32));
    if let Some(z) = &extra.zip64 {
        let mut zr = ByteReader::new(z);
        if usize32 == u32::MAX {
            usz = zr.u64_le()?;
        }
        if csize32 == u32::MAX {
            csize = zr.u64_le()?;
        }
        if lho32 == u32::MAX {
            lho = zr.u64_le()?;
        }
    }
    Ok(Central {
        name_raw,
        flags,
        method,
        dos_time,
        dos_date,
        crc,
        csize,
        usize: usz,
        lho,
        made_by,
        ext_attr,
        extra,
    })
}

fn decode_member(method: u16, data: &[u8], size: u64, flags: u16, b: &Budget<'_>, cx: &Context) -> Result<Vec<u8>> {
    let lim = b.member_limits(size);
    let p = cx.progress;
    let out = match method {
        0 => data.to_vec(),
        8 => inflate::inflate(data, &lim, p)?.data,
        9 => inflate::inflate64(data, &lim, p)?.data,
        12 => bzip2::decompress(data, &lim, p)?,
        14 => {
            let psize = usize::from(u16::from_le_bytes([
                data.get(2).copied().unwrap_or(0),
                data.get(3).copied().unwrap_or(0),
            ]));
            let props = data.get(4).copied().ok_or_else(|| Error::truncated("ZIP: LZMA header"))?;
            let body = data.get(4 + psize..).ok_or_else(|| Error::truncated("ZIP: LZMA header"))?;
            let _ = flags;
            lzma::decode_raw(props, body, Some(size), &lim, p)?
        }
        93 => zstd::decompress(data, &lim, p)?,
        95 => xz::decompress(data, &lim, p)?,
        m => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!("ZIP compression method {m} is not supported"),
            ))
        }
    };
    if out.len() as u64 != size {
        return Err(Error::corrupt("ZIP: member size differs from header"));
    }
    Ok(out)
}

fn aes_params(strength: u8) -> Result<(usize, usize)> {
    match strength {
        1 => Ok((8, 16)),
        2 => Ok((12, 24)),
        3 => Ok((16, 32)),
        _ => Err(Error::new(ErrorCode::UnsupportedVariant, "ZIP: unknown AES strength")),
    }
}

pub fn read(data: &[u8], cx: &Context) -> Result<Archive> {
    let eocd = find_eocd(data)?;
    // Data prepended to the archive (self-extractors) shifts every stored offset.
    let mut shift: u64 = 0;
    let expect_end = eocd.cd_offset.saturating_add(eocd.cd_size);
    if expect_end <= eocd.end_pos {
        let d = eocd.end_pos - expect_end;
        let at = usize::try_from(eocd.cd_offset + d).unwrap_or(usize::MAX);
        if d > 0 && data.get(at..at + 4) == Some(&SIG_CENTRAL.to_le_bytes()[..]) {
            shift = d;
        }
    }
    cx.limits.check_entries(eocd.entries)?;
    let cd_start = usize::try_from(eocd.cd_offset + shift).map_err(|_| Error::corrupt("ZIP: bad directory offset"))?;
    let cd = data
        .get(cd_start..)
        .ok_or_else(|| Error::truncated("ZIP: central directory out of range"))?;
    let mut r = ByteReader::new(cd);
    let mut budget = Budget::new(cx.limits);
    let mut archive = Archive::default();
    let mut total_in: u64 = 0;
    for i in 0..eocd.entries {
        cx.progress.check_cancelled()?;
        cx.progress.report(i, eocd.entries, "reading ZIP members");
        let c = read_central(&mut r).map_err(|e| e.at(cd_start as u64 + r.position() as u64))?;
        budget.entry()?;
        let lho = usize::try_from(c.lho + shift).map_err(|_| Error::corrupt("ZIP: bad member offset"))?;
        let mut lr = ByteReader::new(data.get(lho..).unwrap_or(&[]));
        if lr.u32_le().ok() != Some(SIG_LOCAL) {
            return Err(Error::corrupt("ZIP: bad local header signature").at(lho as u64));
        }
        lr.skip(22)?;
        let lnlen = usize::from(lr.u16_le()?);
        let lelen = usize::from(lr.u16_le()?);
        let body_at = lho + 30 + lnlen + lelen;
        let body_end = body_at
            .checked_add(usize::try_from(c.csize).map_err(|_| Error::corrupt("ZIP: size overflow"))?)
            .ok_or_else(|| Error::corrupt("ZIP: size overflow"))?;
        let raw = data
            .get(body_at..body_end)
            .ok_or_else(|| Error::truncated("ZIP: member data cut short").at(body_at as u64))?;
        total_in += raw.len() as u64;
        // Names.
        let mut name = decode_name(&c.name_raw, c.flags & 0x800 != 0);
        if let Some((ncrc, n)) = &c.extra.unicode_name {
            if *ncrc == crc32(&c.name_raw) {
                name = n.clone();
            }
        }
        name = name.replace('\\', "/");
        let unix = c.made_by >> 8 == 3;
        let mode_full = if unix { c.ext_attr >> 16 } else { 0 };
        let fmt = mode_full & 0o170_000;
        let is_dir = name.ends_with('/') || fmt == 0o040_000 || (!unix && c.ext_attr & 0x10 != 0);
        let is_link = fmt == 0o120_000;
        let name = name.trim_end_matches('/').to_string();
        if name.is_empty() {
            return Err(Error::corrupt("ZIP: empty member name"));
        }
        let mtime = c.extra.mtime.or_else(|| dos_to_unix(c.dos_date, c.dos_time));
        let mut e;
        if is_dir {
            e = Entry::dir(name);
        } else {
            let encrypted = c.flags & 1 != 0;
            if cx.list_only && is_link && encrypted && cx.password.is_none() {
                archive.entries.push(Entry::symlink(name, String::new()));
                continue;
            }
            if cx.list_only && !is_link {
                budget.add_output(c.usize)?;
                let mut le = Entry::listed(name, c.usize);
                le.encrypted = encrypted;
                le.mtime = mtime;
                if unix && mode_full & 0o7777 != 0 {
                    le.mode = mode_full & 0o7777;
                }
                archive.entries.push(le);
                continue;
            }
            // Encryption.
            let mut method = c.method;
            let mut body: Vec<u8> = Vec::new();
            let mut check_crc = true;
            if encrypted {
                let pw = cx.password.ok_or_else(|| {
                    Error::new(ErrorCode::EncryptedNeedPassword, format!("'{name}' is encrypted"))
                })?;
                if c.method == 99 {
                    let (_ver, strength, real) = c
                        .extra
                        .aes
                        .ok_or_else(|| Error::corrupt("ZIP: AES member without AES extra field"))?;
                    let (salt_len, key_len) = aes_params(strength)?;
                    if raw.len() < salt_len + 2 + 10 {
                        return Err(Error::truncated("ZIP: AES member too short"));
                    }
                    let (salt, rest) = raw.split_at(salt_len);
                    let (pwv, rest) = rest.split_at(2);
                    let (ct, mac) = rest.split_at(rest.len() - 10);
                    let mut km = vec![0u8; 2 * key_len + 2];
                    crypto::pbkdf2_sha1(pw.as_bytes(), salt, 1000, &mut km);
                    if km.get(2 * key_len..) != Some(pwv) {
                        return Err(Error::new(ErrorCode::WrongPassword, "wrong password"));
                    }
                    let want = crypto::hmac_sha1(km.get(key_len..2 * key_len).unwrap_or(&[]), ct);
                    if want.get(..10) != Some(mac) {
                        return Err(Error::corrupt("ZIP: AES authentication code mismatch"));
                    }
                    let key = AesKey::new(km.get(..key_len).unwrap_or(&[]))?;
                    body = ct.to_vec();
                    crypto::aes_ctr_le(&key, &mut body);
                    method = real;
                    // AE-2 leaves the CRC at zero; the HMAC already authenticates.
                    check_crc = c.extra.aes.map(|a| a.0) == Some(1);
                } else {
                    if raw.len() < 12 {
                        return Err(Error::truncated("ZIP: encrypted member too short"));
                    }
                    let mut z = ZipCrypto::new(pw.as_bytes());
                    let mut buf = raw.to_vec();
                    z.decrypt(&mut buf);
                    let check = if c.flags & 8 != 0 { (c.dos_time >> 8) as u8 } else { (c.crc >> 24) as u8 };
                    if buf.get(11).copied() != Some(check) {
                        return Err(Error::new(ErrorCode::WrongPassword, "wrong password"));
                    }
                    body = buf.split_off(12);
                }
            }
            let src: &[u8] = if encrypted { &body } else { raw };
            if is_link {
                let t = decode_member(method, src, c.usize, c.flags, &budget, cx)?;
                let target = String::from_utf8_lossy(&t).into_owned();
                e = Entry::symlink(name, target);
            } else {
                budget.add_output(c.usize)?;
                cx.limits.check_alloc(c.usize)?;
                let out = decode_member(method, src, c.usize, c.flags, &budget, cx)?;
                if check_crc && crc32(&out) != c.crc {
                    return Err(Error::corrupt(format!("ZIP: CRC mismatch in '{name}'")));
                }
                e = Entry::file(name, out);
            }
            e.encrypted = encrypted;
        }
        e.mtime = mtime;
        if unix && mode_full & 0o7777 != 0 {
            e.mode = if e.kind == EntryKind::Symlink { 0o777 } else { mode_full & 0o7777 };
        }
        archive.entries.push(e);
    }
    let _ = total_in;
    // Archive comment.
    let ce = eocd.end_pos as usize;
    if let Some(cl) = data.get(ce + 20..ce + 22) {
        let n = usize::from(u16::from_le_bytes([cl[0], cl[1]]));
        if let Some(c) = data.get(ce + 22..ce + 22 + n) {
            if !c.is_empty() {
                archive.comment = Some(String::from_utf8_lossy(c).into_owned());
            }
        }
    }
    Ok(archive)
}

// ---------------- writer ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Deflate, falling back to stored when it does not shrink the member.
    Auto,
    Stored,
    Deflate,
    Bzip2,
    Lzma,
    Zstd,
    Xz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    Aes256,
    Aes192,
    Aes128,
    /// Traditional PKWARE encryption: weak, offered for compatibility only.
    ZipCrypto,
}

#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub method: Method,
    pub level: u32,
    pub password: Option<String>,
    pub encryption: Encryption,
    pub comment: Option<String>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            method: Method::Auto,
            level: 6,
            password: None,
            encryption: Encryption::Aes256,
            comment: None,
        }
    }
}

fn compress_member(method: Method, level: u32, data: &[u8]) -> (u16, Vec<u8>) {
    match method {
        Method::Stored => (0, data.to_vec()),
        Method::Deflate | Method::Auto => {
            let d = deflate::deflate(data, level.clamp(1, 9));
            if method == Method::Auto && d.len() >= data.len() {
                (0, data.to_vec())
            } else {
                (8, d)
            }
        }
        Method::Bzip2 => (12, bzip2::compress(data, level.clamp(1, 9))),
        Method::Lzma => {
            let (props, dict, stream) = lzma::encode_lzma(data, lzma::Level(level.clamp(0, 9)), false);
            let mut out = vec![9, 20, 5, 0, props];
            out.extend_from_slice(&dict.to_le_bytes());
            out.extend(stream);
            (14, out)
        }
        Method::Zstd => (93, zstd::compress(data, level.clamp(1, 19) as i32, true)),
        Method::Xz => (95, xz::compress(data, level.clamp(0, 9), xz::Check::Crc32)),
    }
}

struct Pending {
    name: Vec<u8>,
    utf8: bool,
    method: u16,
    flags: u16,
    dos: (u16, u16),
    crc: u32,
    csize: u64,
    usize: u64,
    offset: u64,
    ext_attr: u32,
    extra_central: Vec<u8>,
    extra_local: Vec<u8>,
    version: u16,
}

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn extra_field(id: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    put16(&mut v, id);
    put16(&mut v, body.len() as u16);
    v.extend_from_slice(body);
    v
}

pub fn write(archive: &Archive, opts: &WriteOptions, cx: &Context) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut list: Vec<Pending> = Vec::new();
    let total = archive.entries.len() as u64;
    for (i, e) in archive.entries.iter().enumerate() {
        cx.progress.check_cancelled()?;
        cx.progress.report(i as u64, total, "writing ZIP members");
        let clean = clean_for_write(&e.path)?;
        let name = if e.kind == EntryKind::Dir { format!("{clean}/") } else { clean };
        let payload: Vec<u8> = match e.kind {
            EntryKind::File => e.data.clone(),
            EntryKind::Dir => Vec::new(),
            EntryKind::Symlink => e.link.clone().unwrap_or_default().into_bytes(),
        };
        let crc = crc32(&payload);
        let (mut method, mut body) = if e.kind == EntryKind::Dir || payload.is_empty() {
            (0, payload.clone())
        } else {
            compress_member(opts.method, opts.level, &payload)
        };
        let mut flags: u16 = 0;
        let mut extra_local: Vec<u8> = Vec::new();
        let mut extra_central: Vec<u8> = Vec::new();
        let mut header_crc = crc;
        let mut version: u16 = match method {
            0 => 10,
            8 => 20,
            12 => 46,
            _ => 63,
        };
        if !name.is_ascii() {
            flags |= 0x800;
        }
        if let (Some(pw), true) = (&opts.password, e.kind != EntryKind::Dir) {
            flags |= 1;
            match opts.encryption {
                Encryption::ZipCrypto => {
                    let mut hdr = [0u8; 12];
                    crypto::random_bytes(&mut hdr);
                    hdr[11] = (crc >> 24) as u8;
                    let mut buf = hdr.to_vec();
                    buf.extend_from_slice(&body);
                    ZipCrypto::new(pw.as_bytes()).encrypt(&mut buf);
                    body = buf;
                }
                enc => {
                    let (strength, salt_len, key_len) = match enc {
                        Encryption::Aes128 => (1u8, 8usize, 16usize),
                        Encryption::Aes192 => (2, 12, 24),
                        _ => (3, 16, 32),
                    };
                    let mut salt = vec![0u8; salt_len];
                    crypto::random_bytes(&mut salt);
                    let mut km = vec![0u8; 2 * key_len + 2];
                    crypto::pbkdf2_sha1(pw.as_bytes(), &salt, 1000, &mut km);
                    let key = AesKey::new(km.get(..key_len).unwrap_or(&[]))?;
                    crypto::aes_ctr_le(&key, &mut body);
                    let mac = crypto::hmac_sha1(km.get(key_len..2 * key_len).unwrap_or(&[]), &body);
                    let mut enc_body = salt;
                    enc_body.extend_from_slice(km.get(2 * key_len..).unwrap_or(&[]));
                    enc_body.extend_from_slice(&body);
                    enc_body.extend_from_slice(mac.get(..10).unwrap_or(&[]));
                    body = enc_body;
                    let mut ae = Vec::new();
                    put16(&mut ae, 2); // AE-2
                    ae.extend_from_slice(b"AE");
                    ae.push(strength);
                    put16(&mut ae, method);
                    let f = extra_field(0x9901, &ae);
                    extra_local.extend_from_slice(&f);
                    extra_central.extend_from_slice(&f);
                    method = 99;
                    header_crc = 0;
                    version = 51;
                }
            }
        }
        let mtime = e.mtime;
        if let Some(t) = mtime.filter(|t| i32::try_from(*t).is_ok()) {
            let mut b = vec![1u8];
            b.extend_from_slice(&(t as i32).to_le_bytes());
            let f = extra_field(0x5455, &b);
            extra_local.extend_from_slice(&f);
            extra_central.extend_from_slice(&f);
        }
        let usize_ = payload.len() as u64;
        let csize = body.len() as u64;
        let offset = out.len() as u64;
        let big_sizes = usize_ >= 0xFFFF_FFFF || csize >= 0xFFFF_FFFF;
        let big_off = offset >= 0xFFFF_FFFF;
        if big_sizes || big_off {
            version = version.max(45);
        }
        // Local header.
        let dos = unix_to_dos(mtime.unwrap_or(0));
        let dos = if mtime.is_none() { (0x21, 0) } else { dos };
        let mut loc_extra = extra_local.clone();
        if big_sizes {
            let mut z = Vec::new();
            put64(&mut z, usize_);
            put64(&mut z, csize);
            loc_extra.extend(extra_field(0x0001, &z));
        }
        put32(&mut out, SIG_LOCAL);
        put16(&mut out, version);
        put16(&mut out, flags);
        put16(&mut out, method);
        put16(&mut out, dos.1);
        put16(&mut out, dos.0);
        put32(&mut out, header_crc);
        put32(&mut out, if big_sizes { 0xFFFF_FFFF } else { csize as u32 });
        put32(&mut out, if big_sizes { 0xFFFF_FFFF } else { usize_ as u32 });
        put16(&mut out, name.len() as u16);
        put16(&mut out, loc_extra.len() as u16);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&loc_extra);
        out.extend_from_slice(&body);
        let unix_mode = match e.kind {
            EntryKind::File => 0o100_000 | (e.mode & 0o7777),
            EntryKind::Dir => 0o040_000 | (e.mode & 0o7777),
            EntryKind::Symlink => 0o120_000 | 0o777,
        };
        let ext_attr = (unix_mode << 16) | if e.kind == EntryKind::Dir { 0x10 } else { 0 };
        let mut cen_extra = extra_central;
        if big_sizes || big_off {
            let mut z = Vec::new();
            if big_sizes {
                put64(&mut z, usize_);
                put64(&mut z, csize);
            }
            if big_off {
                put64(&mut z, offset);
            }
            cen_extra.extend(extra_field(0x0001, &z));
        }
        list.push(Pending {
            name: name.into_bytes(),
            utf8: flags & 0x800 != 0,
            method,
            flags,
            dos,
            crc: header_crc,
            csize: if big_sizes { 0xFFFF_FFFF } else { csize },
            usize: if big_sizes { 0xFFFF_FFFF } else { usize_ },
            offset: if big_off { 0xFFFF_FFFF } else { offset },
            ext_attr,
            extra_central: cen_extra,
            extra_local: Vec::new(),
            version,
        });
    }
    let cd_start = out.len() as u64;
    for p in &list {
        let _ = (&p.extra_local, p.utf8);
        put32(&mut out, SIG_CENTRAL);
        put16(&mut out, (3 << 8) | 63);
        put16(&mut out, p.version);
        put16(&mut out, p.flags);
        put16(&mut out, p.method);
        put16(&mut out, p.dos.1);
        put16(&mut out, p.dos.0);
        put32(&mut out, p.crc);
        put32(&mut out, p.csize as u32);
        put32(&mut out, p.usize as u32);
        put16(&mut out, p.name.len() as u16);
        put16(&mut out, p.extra_central.len() as u16);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put32(&mut out, p.ext_attr);
        put32(&mut out, p.offset as u32);
        out.extend_from_slice(&p.name);
        out.extend_from_slice(&p.extra_central);
    }
    let cd_size = out.len() as u64 - cd_start;
    let n = list.len() as u64;
    let need64 = n >= 0xFFFF || cd_start >= 0xFFFF_FFFF || cd_size >= 0xFFFF_FFFF;
    if need64 {
        let rec_pos = out.len() as u64;
        put32(&mut out, SIG_EOCD64);
        put64(&mut out, 44);
        put16(&mut out, (3 << 8) | 63);
        put16(&mut out, 45);
        put32(&mut out, 0);
        put32(&mut out, 0);
        put64(&mut out, n);
        put64(&mut out, n);
        put64(&mut out, cd_size);
        put64(&mut out, cd_start);
        put32(&mut out, SIG_LOC64);
        put32(&mut out, 0);
        put64(&mut out, rec_pos);
        put32(&mut out, 1);
    }
    let comment = opts.comment.as_deref().or(archive.comment.as_deref()).unwrap_or("");
    let comment = comment.as_bytes().get(..comment.len().min(65535)).unwrap_or(&[]);
    put32(&mut out, SIG_EOCD);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, if need64 { 0xFFFF } else { n as u16 });
    put16(&mut out, if need64 { 0xFFFF } else { n as u16 });
    put32(&mut out, if need64 { 0xFFFF_FFFF } else { cd_size as u32 });
    put32(&mut out, if need64 { 0xFFFF_FFFF } else { cd_start as u32 });
    put16(&mut out, comment.len() as u16);
    out.extend_from_slice(comment);
    Ok(out)
}
