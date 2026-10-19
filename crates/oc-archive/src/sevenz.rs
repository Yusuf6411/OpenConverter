//! 7z: reads and writes LZMA, LZMA2, BZip2, Deflate, Copy, Zstandard folders with
//! BCJ/delta filters and AES-256 encryption (including encrypted headers).
use crate::crypto::{self, AesKey};
use crate::entry::{Archive, Budget, Entry, EntryKind};
use crate::format::WriteOpts;
use crate::safety::clean_for_write;
use crate::Context;
use oc_checksum::crc32;
use oc_compress::bcj::{self, Arch};
use oc_compress::{bzip2, inflate, lz4, lzma, ppmd7, zstd};
use oc_core::{Error, ErrorCode, Result};
use sha2::{Digest, Sha256};

const SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];

mod id {
    pub const END: u8 = 0x00;
    pub const HEADER: u8 = 0x01;
    pub const ARCHIVE_PROPS: u8 = 0x02;
    pub const ADDITIONAL: u8 = 0x03;
    pub const MAIN_STREAMS: u8 = 0x04;
    pub const FILES_INFO: u8 = 0x05;
    pub const PACK_INFO: u8 = 0x06;
    pub const UNPACK_INFO: u8 = 0x07;
    pub const SUBSTREAMS: u8 = 0x08;
    pub const SIZE: u8 = 0x09;
    pub const CRC: u8 = 0x0A;
    pub const FOLDER: u8 = 0x0B;
    pub const CODERS_UNPACK_SIZE: u8 = 0x0C;
    pub const NUM_UNPACK_STREAM: u8 = 0x0D;
    pub const EMPTY_STREAM: u8 = 0x0E;
    pub const EMPTY_FILE: u8 = 0x0F;
    pub const NAMES: u8 = 0x11;
    pub const MTIME: u8 = 0x14;
    pub const WIN_ATTRIBUTES: u8 = 0x15;
    pub const ENCODED_HEADER: u8 = 0x17;
}

const M_COPY: u64 = 0x00;
const M_DELTA: u64 = 0x03;
const M_LZMA2: u64 = 0x21;
const M_BCJ2: u64 = 0x0303_011B;
const M_PPMD: u64 = 0x03_04_01;
const M_LZMA: u64 = 0x03_01_01;
const M_BZIP2: u64 = 0x04_02_02;
const M_DEFLATE: u64 = 0x04_01_08;
const M_DEFLATE64: u64 = 0x04_01_09;
const M_AES: u64 = 0x06_F1_07_01;
const M_ZSTD: u64 = 0x04_F7_11_01;
const M_LZ4: u64 = 0x04_F7_11_04;

// ---------------- parsing helpers ----------------

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, p: 0 }
    }
    fn trunc(&self) -> Error {
        Error::truncated("7z: header cut short").at(self.p as u64)
    }
    fn u8(&mut self) -> Result<u8> {
        let v = *self.b.get(self.p).ok_or_else(|| self.trunc())?;
        self.p += 1;
        Ok(v)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or_else(|| self.trunc())?;
        let s = self.b.get(self.p..end).ok_or_else(|| self.trunc())?;
        self.p = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes(s.try_into().map_err(|_| self.trunc())?))
    }
    fn u64(&mut self) -> Result<u64> {
        let s = self.take(8)?;
        Ok(u64::from_le_bytes(s.try_into().map_err(|_| self.trunc())?))
    }
    /// 7z variable-length number.
    fn num(&mut self) -> Result<u64> {
        let first = self.u8()?;
        let mut mask = 0x80u8;
        let mut v: u64 = 0;
        for i in 0..8 {
            if first & mask == 0 {
                let high = u64::from(first & mask.wrapping_sub(1));
                return Ok(v | (high << (8 * i)));
            }
            v |= u64::from(self.u8()?) << (8 * i);
            mask >>= 1;
        }
        Ok(v)
    }
    fn count(&mut self, limit: u64) -> Result<usize> {
        let n = self.num()?;
        if n > limit {
            return Err(Error::corrupt("7z: implausible count in header").at(self.p as u64));
        }
        Ok(n as usize)
    }
    fn bits(&mut self, n: usize) -> Result<Vec<bool>> {
        let mut out = Vec::with_capacity(n);
        let mut cur = 0u8;
        let mut mask = 0u8;
        for _ in 0..n {
            if mask == 0 {
                cur = self.u8()?;
                mask = 0x80;
            }
            out.push(cur & mask != 0);
            mask >>= 1;
        }
        Ok(out)
    }
    /// "AllAreDefined" byte followed by an optional bit vector.
    fn defined(&mut self, n: usize) -> Result<Vec<bool>> {
        if self.u8()? != 0 {
            Ok(vec![true; n])
        } else {
            self.bits(n)
        }
    }
    fn digests(&mut self, n: usize) -> Result<Vec<Option<u32>>> {
        let def = self.defined(n)?;
        def.into_iter()
            .map(|d| if d { self.u32().map(Some) } else { Ok(None) })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct Coder {
    method: u64,
    n_in: usize,
    n_out: usize,
    props: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct Folder {
    coders: Vec<Coder>,
    /// (in-stream index, out-stream index)
    bind: Vec<(usize, usize)>,
    /// In-stream indices fed by pack streams, in pack-stream order.
    packed: Vec<usize>,
    unpack_sizes: Vec<u64>,
    crc: Option<u32>,
    first_pack: usize,
}

impl Folder {
    fn total_in(&self) -> usize {
        self.coders.iter().map(|c| c.n_in).sum()
    }
    fn total_out(&self) -> usize {
        self.coders.iter().map(|c| c.n_out).sum()
    }
    fn main_out(&self) -> Result<usize> {
        (0..self.total_out())
            .find(|o| !self.bind.iter().any(|(_, bo)| bo == o))
            .ok_or_else(|| Error::corrupt("7z: folder has no output stream"))
    }
    fn size(&self) -> u64 {
        self.main_out()
            .ok()
            .and_then(|o| self.unpack_sizes.get(o).copied())
            .unwrap_or(0)
    }
    fn uses_aes(&self) -> bool {
        self.coders.iter().any(|c| c.method == M_AES)
    }
}

#[derive(Default)]
struct Streams {
    pack_pos: u64,
    pack_sizes: Vec<u64>,
    folders: Vec<Folder>,
    /// Per folder: (size, crc) of each file stream.
    subs: Vec<Vec<(u64, Option<u32>)>>,
}

fn read_folder(r: &mut Rd<'_>) -> Result<Folder> {
    let n = r.count(64)?;
    let mut f = Folder::default();
    for _ in 0..n {
        let flags = r.u8()?;
        let idlen = usize::from(flags & 0x0F);
        if flags & 0x80 != 0 {
            return Err(Error::new(ErrorCode::UnsupportedVariant, "7z: alternative coder methods"));
        }
        if idlen > 8 {
            return Err(Error::corrupt("7z: method id too long"));
        }
        let method = r.take(idlen)?.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
        let (n_in, n_out) = if flags & 0x10 != 0 {
            (r.count(32)?, r.count(32)?)
        } else {
            (1, 1)
        };
        let props = if flags & 0x20 != 0 {
            let sz = r.count(1 << 16)?;
            r.take(sz)?.to_vec()
        } else {
            Vec::new()
        };
        f.coders.push(Coder { method, n_in, n_out, props });
    }
    let (tin, tout) = (f.total_in(), f.total_out());
    if tout == 0 {
        return Err(Error::corrupt("7z: folder without coders"));
    }
    let nbind = tout - 1;
    for _ in 0..nbind {
        let i = r.count(tin as u64)?;
        let o = r.count(tout as u64)?;
        f.bind.push((i, o));
    }
    let npacked = tin.checked_sub(nbind).ok_or_else(|| Error::corrupt("7z: bad bind pairs"))?;
    if npacked == 1 {
        let p = (0..tin)
            .find(|i| !f.bind.iter().any(|(bi, _)| bi == i))
            .ok_or_else(|| Error::corrupt("7z: no packed input"))?;
        f.packed.push(p);
    } else {
        for _ in 0..npacked {
            f.packed.push(r.count(tin as u64)?);
        }
    }
    Ok(f)
}

fn read_streams(r: &mut Rd<'_>, cx: &Context) -> Result<Streams> {
    let mut s = Streams::default();
    let mut digests_pack: Vec<Option<u32>> = Vec::new();
    let mut have_subs = false;
    loop {
        match r.u8()? {
            id::END => break,
            id::PACK_INFO => {
                s.pack_pos = r.num()?;
                let n = r.count(cx.limits.max_entries)?;
                loop {
                    match r.u8()? {
                        id::END => break,
                        id::SIZE => {
                            s.pack_sizes = (0..n).map(|_| r.num()).collect::<Result<_>>()?;
                        }
                        id::CRC => digests_pack = r.digests(n)?,
                        _ => return Err(Error::corrupt("7z: unexpected property in PackInfo")),
                    }
                }
                let _ = &digests_pack;
            }
            id::UNPACK_INFO => {
                if r.u8()? != id::FOLDER {
                    return Err(Error::corrupt("7z: expected Folder"));
                }
                let n = r.count(cx.limits.max_entries)?;
                if r.u8()? != 0 {
                    return Err(Error::new(ErrorCode::UnsupportedVariant, "7z: external folder data"));
                }
                let mut first = 0usize;
                for _ in 0..n {
                    let mut f = read_folder(r)?;
                    f.first_pack = first;
                    first += f.packed.len();
                    s.folders.push(f);
                }
                if r.u8()? != id::CODERS_UNPACK_SIZE {
                    return Err(Error::corrupt("7z: expected CodersUnpackSize"));
                }
                for f in &mut s.folders {
                    f.unpack_sizes = (0..f.total_out()).map(|_| r.num()).collect::<Result<_>>()?;
                }
                loop {
                    match r.u8()? {
                        id::END => break,
                        id::CRC => {
                            let d = r.digests(s.folders.len())?;
                            for (f, c) in s.folders.iter_mut().zip(d) {
                                f.crc = c;
                            }
                        }
                        _ => return Err(Error::corrupt("7z: unexpected property in UnpackInfo")),
                    }
                }
            }
            id::SUBSTREAMS => {
                have_subs = true;
                let mut counts: Vec<usize> = vec![1; s.folders.len()];
                let mut next = r.u8()?;
                if next == id::NUM_UNPACK_STREAM {
                    for c in &mut counts {
                        *c = r.count(cx.limits.max_entries)?;
                    }
                    next = r.u8()?;
                }
                s.subs = Vec::new();
                let mut sizes: Vec<Vec<u64>> = Vec::new();
                for (f, &c) in s.folders.iter().zip(&counts) {
                    let mut v = Vec::new();
                    if c > 0 {
                        let mut sum = 0u64;
                        if next == id::SIZE {
                            for _ in 0..c - 1 {
                                let x = r.num()?;
                                sum = sum.saturating_add(x);
                                v.push(x);
                            }
                        } else if c > 1 {
                            return Err(Error::corrupt("7z: missing substream sizes"));
                        }
                        v.push(
                            f.size()
                                .checked_sub(sum)
                                .ok_or_else(|| Error::corrupt("7z: substream sizes exceed folder"))?,
                        );
                    }
                    sizes.push(v);
                }
                if next == id::SIZE {
                    next = r.u8()?;
                }
                // CRCs: streams whose folder CRC does not already cover them.
                let mut need = 0usize;
                for (f, &c) in s.folders.iter().zip(&counts) {
                    if !(c == 1 && f.crc.is_some()) {
                        need += c;
                    }
                }
                let mut crcs: Vec<Option<u32>> = vec![None; need];
                while next != id::END {
                    if next == id::CRC {
                        crcs = r.digests(need)?;
                    } else {
                        return Err(Error::corrupt("7z: unexpected property in SubStreamsInfo"));
                    }
                    next = r.u8()?;
                }
                let mut ci = crcs.into_iter();
                for ((f, &c), sz) in s.folders.iter().zip(&counts).zip(sizes) {
                    let mut v = Vec::new();
                    if c == 1 && f.crc.is_some() {
                        v.push((sz.first().copied().unwrap_or(0), f.crc));
                    } else {
                        for x in sz {
                            v.push((x, ci.next().flatten()));
                        }
                    }
                    s.subs.push(v);
                }
            }
            _ => return Err(Error::corrupt("7z: unexpected property in StreamsInfo")),
        }
    }
    if !have_subs {
        s.subs = s.folders.iter().map(|f| vec![(f.size(), f.crc)]).collect();
    }
    Ok(s)
}

fn aes_key(props: &[u8], password: &str) -> Result<(AesKey, [u8; 16])> {
    let b0 = *props.first().ok_or_else(|| Error::corrupt("7z: AES properties missing"))?;
    let cycles = u32::from(b0 & 0x3F);
    let (mut salt, mut iv) = (Vec::new(), [0u8; 16]);
    if b0 & 0xC0 != 0 {
        let b1 = *props.get(1).ok_or_else(|| Error::corrupt("7z: AES properties cut short"))?;
        let ssz = usize::from((b0 >> 7) & 1) + usize::from(b1 >> 4);
        let isz = usize::from((b0 >> 6) & 1) + usize::from(b1 & 0xF);
        if isz > 16 {
            return Err(Error::corrupt("7z: bad AES IV size"));
        }
        salt = props.get(2..2 + ssz).ok_or_else(|| Error::corrupt("7z: AES salt cut short"))?.to_vec();
        let ivb = props.get(2 + ssz..2 + ssz + isz).ok_or_else(|| Error::corrupt("7z: AES IV cut short"))?;
        iv[..isz].copy_from_slice(ivb);
    }
    let pw: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut key = [0u8; 32];
    if cycles == 0x3F {
        let mut k = salt.clone();
        k.extend_from_slice(&pw);
        for (d, s) in key.iter_mut().zip(k.iter()) {
            *d = *s;
        }
    } else {
        let mut h = Sha256::new();
        for i in 0u64..(1u64 << cycles) {
            h.update(&salt);
            h.update(&pw);
            h.update(i.to_le_bytes());
        }
        key.copy_from_slice(&h.finalize());
    }
    Ok((AesKey::new(&key)?, iv))
}

fn run_coder(c: &Coder, mut inputs: Vec<Vec<u8>>, out_size: u64, cx: &Context, budget: &Budget<'_>) -> Result<Vec<u8>> {
    cx.limits.check_alloc(out_size)?;
    let mut lim = budget.member_limits(out_size);
    lim.max_ratio = cx.limits.max_ratio;
    let p = cx.progress;
    if c.method == M_BCJ2 && inputs.len() == 4 {
        let rc = inputs.pop().unwrap_or_default();
        let jump = inputs.pop().unwrap_or_default();
        let call = inputs.pop().unwrap_or_default();
        let main = inputs.pop().unwrap_or_default();
        let out = bcj::bcj2_decode(&main, &call, &jump, &rc, usize::try_from(out_size).unwrap_or(usize::MAX))?;
        return Ok(out);
    }
    if inputs.len() != 1 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!("7z: coder {:#x} with {} inputs is not supported", c.method, inputs.len()),
        ));
    }
    let input = inputs.pop().unwrap_or_default();
    let mut out = match c.method {
        M_COPY => input,
        M_PPMD => ppmd7::decode(&c.props, &input, usize::try_from(out_size).unwrap_or(usize::MAX), &lim, p)?,
        M_LZMA2 => lzma::decode_lzma2(&input, 0, &lim, p)?,
        M_LZMA => {
            let props = *c.props.first().ok_or_else(|| Error::corrupt("7z: LZMA properties missing"))?;
            lzma::decode_raw(props, &input, Some(out_size), &lim, p)?
        }
        M_BZIP2 => bzip2::decompress(&input, &lim, p)?,
        M_DEFLATE => inflate::inflate(&input, &lim, p)?.data,
        M_DEFLATE64 => inflate::inflate64(&input, &lim, p)?.data,
        M_ZSTD => zstd::decompress(&input, &lim, p)?,
        M_LZ4 => lz4::decompress(&input, &lim, p)?,
        M_DELTA => {
            let mut v = input;
            let d = usize::from(c.props.first().copied().unwrap_or(0)) + 1;
            bcj::delta(&mut v, d, false);
            v
        }
        m @ (0x03030103 | 0x04 | 0x03030205 | 0x05 | 0x03030501 | 0x07 | 0x03030701 | 0x08 | 0x03030805 | 0x09 | 0x0A) => {
            let arch = match m {
                0x03030103 | 0x04 => Arch::X86,
                0x03030205 | 0x05 => Arch::PowerPc,
                0x03030501 | 0x07 => Arch::Arm,
                0x03030701 | 0x08 => Arch::ArmThumb,
                0x03030805 | 0x09 => Arch::Sparc,
                _ => Arch::Arm64,
            };
            let mut v = input;
            bcj::convert(arch, &mut v, false);
            v
        }
        M_AES => {
            let pw = cx.password.ok_or_else(|| Error::new(ErrorCode::EncryptedNeedPassword, "archive is encrypted"))?;
            let (key, iv) = aes_key(&c.props, pw)?;
            let mut v = input;
            let whole = v.len() / 16 * 16;
            v.truncate(whole);
            crypto::cbc_decrypt(&key, &iv, &mut v);
            v
        }
        m => {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                format!("7z: compression method {m:#x} is not supported"),
            ))
        }
    };
    if c.method == M_AES {
        out.truncate(usize::try_from(out_size).unwrap_or(usize::MAX));
    }
    if out.len() as u64 != out_size {
        return Err(Error::corrupt("7z: coder output size differs from header"));
    }
    Ok(out)
}

fn decode_folder(f: &Folder, packs: &[&[u8]], cx: &Context, budget: &Budget<'_>) -> Result<Vec<u8>> {
    fn out_of(
        f: &Folder,
        out_idx: usize,
        packs: &[&[u8]],
        cx: &Context,
        budget: &Budget<'_>,
        depth: u32,
    ) -> Result<Vec<u8>> {
        if depth > 32 {
            return Err(Error::corrupt("7z: coder graph too deep"));
        }
        let (mut in_base, mut out_base) = (0usize, 0usize);
        for c in &f.coders {
            if out_idx >= out_base && out_idx < out_base + c.n_out {
                if c.n_out != 1 {
                    return Err(Error::new(ErrorCode::UnsupportedVariant, "7z: multi-output coder"));
                }
                let mut inputs = Vec::new();
                for j in 0..c.n_in {
                    let gin = in_base + j;
                    if let Some((_, bo)) = f.bind.iter().find(|(bi, _)| *bi == gin) {
                        inputs.push(out_of(f, *bo, packs, cx, budget, depth + 1)?);
                    } else {
                        let pi = f
                            .packed
                            .iter()
                            .position(|&x| x == gin)
                            .ok_or_else(|| Error::corrupt("7z: unconnected coder input"))?;
                        let d = packs
                            .get(pi)
                            .ok_or_else(|| Error::corrupt("7z: missing pack stream"))?;
                        inputs.push(d.to_vec());
                    }
                }
                let size = f.unpack_sizes.get(out_idx).copied().unwrap_or(0);
                return run_coder(c, inputs, size, cx, budget);
            }
            in_base += c.n_in;
            out_base += c.n_out;
        }
        Err(Error::corrupt("7z: bind pair points outside the folder"))
    }
    out_of(f, f.main_out()?, packs, cx, budget, 0)
}

fn decode_folder_at(s: &Streams, fi: usize, data: &[u8], cx: &Context, budget: &Budget<'_>) -> Result<Vec<u8>> {
    let f = s.folders.get(fi).ok_or_else(|| Error::corrupt("7z: folder index"))?;
    let mut off: u64 = 32 + s.pack_pos;
    for sz in s.pack_sizes.iter().take(f.first_pack) {
        off = off.saturating_add(*sz);
    }
    let mut packs: Vec<&[u8]> = Vec::new();
    for k in 0..f.packed.len() {
        let sz = *s
            .pack_sizes
            .get(f.first_pack + k)
            .ok_or_else(|| Error::corrupt("7z: missing pack size"))?;
        let a = usize::try_from(off).map_err(|_| Error::corrupt("7z: offset overflow"))?;
        let b = usize::try_from(off.saturating_add(sz)).map_err(|_| Error::corrupt("7z: offset overflow"))?;
        packs.push(
            data.get(a..b)
                .ok_or_else(|| Error::truncated("7z: pack stream cut short").at(a as u64))?,
        );
        off = off.saturating_add(sz);
    }
    let out = decode_folder(f, &packs, cx, budget).map_err(|e| {
        if f.uses_aes() && matches!(e.code, ErrorCode::CorruptFile | ErrorCode::TruncatedFile) {
            Error::new(ErrorCode::WrongPassword, "wrong password or damaged data")
        } else {
            e
        }
    })?;
    if let Some(c) = f.crc {
        if crc32(&out) != c {
            return Err(if f.uses_aes() {
                Error::new(ErrorCode::WrongPassword, "wrong password or damaged data")
            } else {
                Error::corrupt("7z: folder CRC mismatch")
            });
        }
    }
    Ok(out)
}

pub fn read(data: &[u8], cx: &Context) -> Result<Archive> {
    if data.get(..6) != Some(&SIGNATURE[..]) {
        return Err(Error::corrupt("7z: bad signature"));
    }
    let mut r = Rd::new(data.get(6..32).ok_or_else(|| Error::truncated("7z: start header cut short"))?);
    let (_maj, _min) = (r.u8()?, r.u8()?);
    let start_crc = r.u32()?;
    let hdr_off = r.u64()?;
    let hdr_size = r.u64()?;
    let hdr_crc = r.u32()?;
    if crc32(data.get(12..32).unwrap_or(&[])) != start_crc {
        return Err(Error::corrupt("7z: start header CRC mismatch"));
    }
    let mut archive = Archive::default();
    if hdr_size == 0 {
        return Ok(archive);
    }
    let a = usize::try_from(32 + hdr_off).map_err(|_| Error::corrupt("7z: header offset"))?;
    let b = usize::try_from(32 + hdr_off + hdr_size).map_err(|_| Error::corrupt("7z: header size"))?;
    let mut header: Vec<u8> = data
        .get(a..b)
        .ok_or_else(|| Error::truncated("7z: header lies beyond the end of the file").at(a as u64))?
        .to_vec();
    if crc32(&header) != hdr_crc {
        return Err(Error::corrupt("7z: header CRC mismatch"));
    }
    let mut budget = Budget::new(cx.limits);
    // Unwrap (possibly nested) encoded headers.
    for _ in 0..4 {
        let mut hr = Rd::new(&header);
        match hr.u8()? {
            id::ENCODED_HEADER => {
                let s = read_streams(&mut hr, cx)?;
                let out = decode_folder_at(&s, 0, data, cx, &budget).map_err(|e| {
                    if e.code == ErrorCode::EncryptedNeedPassword {
                        Error::new(ErrorCode::EncryptedNeedPassword, "the archive's file list is encrypted")
                    } else {
                        e
                    }
                })?;
                header = out;
            }
            id::HEADER => break,
            _ => return Err(Error::corrupt("7z: unknown header type")),
        }
    }
    let mut hr = Rd::new(&header);
    if hr.u8()? != id::HEADER {
        return Err(Error::corrupt("7z: header did not decode to a plain header"));
    }
    let mut streams = Streams::default();
    let mut names: Vec<String> = Vec::new();
    let mut empty_stream: Vec<bool> = Vec::new();
    let mut empty_file: Vec<bool> = Vec::new();
    let mut mtimes: Vec<Option<i64>> = Vec::new();
    let mut attrs: Vec<Option<u32>> = Vec::new();
    loop {
        match hr.u8()? {
            id::END => break,
            id::ARCHIVE_PROPS => loop {
                if hr.u8()? == id::END {
                    break;
                }
                let n = hr.count(1 << 30)?;
                hr.take(n)?;
            },
            id::ADDITIONAL => {
                return Err(Error::new(ErrorCode::UnsupportedVariant, "7z: additional streams"));
            }
            id::MAIN_STREAMS => streams = read_streams(&mut hr, cx)?,
            id::FILES_INFO => {
                let n = hr.count(cx.limits.max_entries)?;
                loop {
                    let t = hr.u8()?;
                    if t == id::END {
                        break;
                    }
                    let size = hr.count(1 << 40)?;
                    let body = hr.take(size)?;
                    let mut br = Rd::new(body);
                    match t {
                        id::EMPTY_STREAM => empty_stream = br.bits(n)?,
                        id::EMPTY_FILE => {
                            let k = empty_stream.iter().filter(|&&x| x).count();
                            empty_file = br.bits(k)?;
                        }
                        id::NAMES => {
                            if br.u8()? != 0 {
                                return Err(Error::new(ErrorCode::UnsupportedVariant, "7z: external names"));
                            }
                            let rest = br.b.get(br.p..).unwrap_or(&[]);
                            let units: Vec<u16> = rest
                                .chunks_exact(2)
                                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                                .collect();
                            names = units
                                .split(|&u| u == 0)
                                .take(n)
                                .map(String::from_utf16_lossy)
                                .collect();
                        }
                        id::MTIME => {
                            let def = br.defined(n)?;
                            if br.u8()? != 0 {
                                return Err(Error::new(ErrorCode::UnsupportedVariant, "7z: external times"));
                            }
                            mtimes = def
                                .into_iter()
                                .map(|d| {
                                    if d {
                                        br.u64().map(|ft| Some((ft / 10_000_000) as i64 - 11_644_473_600))
                                    } else {
                                        Ok(None)
                                    }
                                })
                                .collect::<Result<_>>()?;
                        }
                        id::WIN_ATTRIBUTES => {
                            let def = br.defined(n)?;
                            if br.u8()? != 0 {
                                return Err(Error::new(ErrorCode::UnsupportedVariant, "7z: external attributes"));
                            }
                            attrs = def
                                .into_iter()
                                .map(|d| if d { br.u32().map(Some) } else { Ok(None) })
                                .collect::<Result<_>>()?;
                        }
                        _ => {}
                    }
                }
                if names.len() != n {
                    return Err(Error::corrupt("7z: file name count differs from file count"));
                }
                if empty_stream.is_empty() {
                    empty_stream = vec![false; n];
                }
            }
            _ => return Err(Error::corrupt("7z: unexpected property in header")),
        }
    }
    // Decode folders on demand while walking the files.
    let mut folder_data: Option<(usize, Vec<u8>)> = None;
    let mut sub_cursor: (usize, usize, usize) = (0, 0, 0); // folder, substream, byte offset
    let mut empty_idx = 0usize;
    for (i, name) in names.iter().enumerate() {
        cx.progress.check_cancelled()?;
        cx.progress.report(i as u64, names.len() as u64, "reading 7z members");
        budget.entry()?;
        let attr = attrs.get(i).copied().flatten();
        let name = name.replace('\\', "/").trim_end_matches('/').to_string();
        if name.is_empty() {
            return Err(Error::corrupt("7z: empty member name"));
        }
        let unix_mode = attr.filter(|a| a & 0x8000 != 0).map(|a| a >> 16);
        let is_empty = empty_stream.get(i).copied().unwrap_or(false);
        let mut e;
        if is_empty {
            let is_file = empty_file.get(empty_idx).copied().unwrap_or(false);
            empty_idx += 1;
            let dir_attr = attr.is_some_and(|a| a & 0x10 != 0);
            e = if is_file && !dir_attr { Entry::file(name, Vec::new()) } else { Entry::dir(name) };
        } else {
            // Advance to the next non-empty substream.
            loop {
                let (fi, si, _) = sub_cursor;
                match streams.subs.get(fi) {
                    None => return Err(Error::corrupt("7z: more files than data streams")),
                    Some(v) if si >= v.len() => sub_cursor = (fi + 1, 0, 0),
                    Some(_) => break,
                }
            }
            let (fi, si, off) = sub_cursor;
            let (size, crc) = streams.subs.get(fi).and_then(|v| v.get(si)).copied().unwrap_or((0, None));
            let end = off.checked_add(size as usize).ok_or_else(|| Error::corrupt("7z: size overflow"))?;
            let is_link = unix_mode.is_some_and(|m| m & 0o170_000 == 0o120_000);
            if cx.list_only {
                budget.add_output(size)?;
                sub_cursor = (fi, si + 1, end);
                e = if is_link { Entry::symlink(name, String::new()) } else { Entry::listed(name, size) };
            } else {
                if folder_data.as_ref().map(|(k, _)| *k) != Some(fi) {
                    let d = decode_folder_at(&streams, fi, data, cx, &budget)?;
                    budget.add_output(d.len() as u64)?;
                    folder_data = Some((fi, d));
                }
                let fd = folder_data.as_ref().map(|(_, d)| d.as_slice()).unwrap_or(&[]);
                let body = fd.get(off..end).ok_or_else(|| Error::corrupt("7z: stream beyond folder"))?;
                if let Some(c) = crc {
                    if crc32(body) != c {
                        return Err(Error::corrupt(format!("7z: CRC mismatch in '{name}'")));
                    }
                }
                sub_cursor = (fi, si + 1, end);
                e = if is_link {
                    Entry::symlink(name, String::from_utf8_lossy(body).into_owned())
                } else {
                    Entry::file(name, body.to_vec())
                };
            }
            e.encrypted = streams.folders.get(fi).is_some_and(Folder::uses_aes);
        }
        e.mtime = mtimes.get(i).copied().flatten();
        if let Some(m) = unix_mode {
            if m & 0o7777 != 0 && e.kind != EntryKind::Symlink {
                e.mode = m & 0o7777;
            }
        }
        archive.entries.push(e);
    }
    Ok(archive)
}

// ---------------- writer ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Lzma2,
    Lzma,
    Bzip2,
    Deflate,
    Zstd,
    Copy,
}

fn put_num(v: &mut Vec<u8>, mut x: u64) {
    // Smallest encoding: leading 1-bits of the first byte count extra bytes.
    let mut extra = 0usize;
    while extra < 8 && x >= 1u64 << (7 * (extra + 1)) {
        extra += 1;
    }
    if extra == 8 {
        v.push(0xFF);
        v.extend_from_slice(&x.to_le_bytes());
        return;
    }
    let mask = !(0xFFu16 >> extra) as u8;
    let high = (x >> (8 * extra)) as u8;
    v.push(mask | high);
    for _ in 0..extra {
        v.push(x as u8);
        x >>= 8;
    }
}

fn put_bits(v: &mut Vec<u8>, bits: &[bool]) {
    let mut cur = 0u8;
    let mut mask = 0x80u8;
    for &b in bits {
        if b {
            cur |= mask;
        }
        mask >>= 1;
        if mask == 0 {
            v.push(cur);
            cur = 0;
            mask = 0x80;
        }
    }
    if mask != 0x80 {
        v.push(cur);
    }
}

fn put_defined(v: &mut Vec<u8>, def: &[bool]) {
    if def.iter().all(|&d| d) {
        v.push(1);
    } else {
        v.push(0);
        put_bits(v, def);
    }
}

struct PackedFolder {
    coders: Vec<(u64, Vec<u8>)>, // (method, props) in encode order
    unpack_sizes: Vec<u64>,      // per coder output (decode direction)
    packed: Vec<u8>,
}

fn encode_stream(data: &[u8], method: Method, level: u32, password: Option<&str>) -> Result<PackedFolder> {
    let (mid, props, mut packed): (u64, Vec<u8>, Vec<u8>) = match method {
        Method::Copy => (M_COPY, vec![], data.to_vec()),
        Method::Lzma2 => {
            let (dict, body) = lzma::encode_lzma2(data, lzma::Level(level.clamp(0, 9)))?;
            (M_LZMA2, vec![dict], body)
        }
        Method::Lzma => {
            let (p, dict, body) = lzma::encode_lzma(data, lzma::Level(level.clamp(0, 9)), false);
            let mut props = vec![p];
            props.extend_from_slice(&dict.to_le_bytes());
            (M_LZMA, props, body)
        }
        Method::Bzip2 => (M_BZIP2, vec![], bzip2::compress(data, level.clamp(1, 9))),
        Method::Deflate => (M_DEFLATE, vec![], oc_compress::deflate::deflate(data, level.clamp(1, 9))),
        Method::Zstd => (M_ZSTD, vec![1, 5, 0, 0, 0], zstd::compress(data, level.clamp(1, 19) as i32, true)),
    };
    let mut coders = vec![(mid, props)];
    let mut sizes = vec![data.len() as u64];
    if let Some(pw) = password {
        let cycles = 19u8;
        let (mut salt, mut iv) = ([0u8; 16], [0u8; 16]);
        crypto::random_bytes(&mut salt);
        crypto::random_bytes(&mut iv);
        let mut props = vec![0xC0 | cycles, 0xFF];
        props.extend_from_slice(&salt);
        props.extend_from_slice(&iv);
        let (key, _) = aes_key(&props, pw)?;
        sizes.insert(0, packed.len() as u64);
        packed.resize(packed.len().div_ceil(16) * 16, 0);
        crypto::cbc_encrypt(&key, &iv, &mut packed);
        coders.insert(0, (M_AES, props));
    }
    Ok(PackedFolder { coders, unpack_sizes: sizes, packed })
}

fn put_folder(v: &mut Vec<u8>, f: &PackedFolder) {
    put_num(v, f.coders.len() as u64);
    for (m, props) in &f.coders {
        let idb: Vec<u8> = m.to_be_bytes().iter().copied().skip_while(|&b| b == 0).collect();
        let idb = if idb.is_empty() { vec![0] } else { idb };
        let mut flags = idb.len() as u8;
        if !props.is_empty() {
            flags |= 0x20;
        }
        v.push(flags);
        v.extend_from_slice(&idb);
        if !props.is_empty() {
            put_num(v, props.len() as u64);
            v.extend_from_slice(props);
        }
    }
    // Coders are listed in decoding order: coder 0 reads the pack stream and each later
    // coder's input is bound to the previous coder's output.
    for k in 0..f.coders.len().saturating_sub(1) {
        put_num(v, k as u64 + 1); // in-stream index of coder k+1
        put_num(v, k as u64); // out-stream index of coder k
    }
}

pub fn write(archive: &Archive, opts: &WriteOpts, cx: &Context) -> Result<Vec<u8>> {
    let method = opts.sevenz_method.unwrap_or(Method::Lzma2);
    let level = opts.level.unwrap_or(5);
    let pw = opts.password.as_deref();
    let mut names: Vec<String> = Vec::new();
    let mut solid: Vec<u8> = Vec::new();
    let mut stream_sizes: Vec<u64> = Vec::new();
    let mut crcs: Vec<u32> = Vec::new();
    let mut empty_stream: Vec<bool> = Vec::new();
    let mut empty_file: Vec<bool> = Vec::new();
    let mut mtimes: Vec<Option<i64>> = Vec::new();
    let mut attrs: Vec<u32> = Vec::new();
    for e in &archive.entries {
        cx.progress.check_cancelled()?;
        let name = clean_for_write(&e.path)?;
        names.push(name);
        mtimes.push(e.mtime);
        let (payload, unix): (&[u8], u32) = match e.kind {
            EntryKind::File => (&e.data, 0o100_000 | (e.mode & 0o7777)),
            EntryKind::Dir => (&[], 0o040_000 | (e.mode & 0o7777)),
            EntryKind::Symlink => (e.link.as_deref().unwrap_or("").as_bytes(), 0o120_777),
        };
        let win = if e.kind == EntryKind::Dir { 0x10 } else { 0x20 };
        attrs.push(win | 0x8000 | (unix << 16));
        if payload.is_empty() {
            empty_stream.push(true);
            empty_file.push(e.kind != EntryKind::Dir);
        } else {
            empty_stream.push(false);
            stream_sizes.push(payload.len() as u64);
            crcs.push(crc32(payload));
            solid.extend_from_slice(payload);
        }
    }
    let n_files = names.len();
    let mut out = vec![0u8; 32];
    let mut h: Vec<u8> = vec![id::HEADER];
    if !solid.is_empty() {
        let f = encode_stream(&solid, method, level, pw)?;
        out.extend_from_slice(&f.packed);
        h.push(id::MAIN_STREAMS);
        h.push(id::PACK_INFO);
        put_num(&mut h, 0);
        put_num(&mut h, 1);
        h.push(id::SIZE);
        put_num(&mut h, f.packed.len() as u64);
        h.push(id::END);
        h.push(id::UNPACK_INFO);
        h.push(id::FOLDER);
        put_num(&mut h, 1);
        h.push(0);
        put_folder(&mut h, &f);
        h.push(id::CODERS_UNPACK_SIZE);
        // Sizes are listed per out-stream in coder order (decode direction of each coder).
        for s in &f.unpack_sizes {
            put_num(&mut h, *s);
        }
        h.push(id::END);
        h.push(id::SUBSTREAMS);
        h.push(id::NUM_UNPACK_STREAM);
        put_num(&mut h, stream_sizes.len() as u64);
        h.push(id::SIZE);
        for s in stream_sizes.iter().take(stream_sizes.len().saturating_sub(1)) {
            put_num(&mut h, *s);
        }
        h.push(id::CRC);
        h.push(1);
        for c in &crcs {
            h.extend_from_slice(&c.to_le_bytes());
        }
        h.push(id::END);
        h.push(id::END);
    }
    h.push(id::FILES_INFO);
    put_num(&mut h, n_files as u64);
    let prop = |h: &mut Vec<u8>, t: u8, body: &[u8]| {
        h.push(t);
        put_num(h, body.len() as u64);
        h.extend_from_slice(body);
    };
    if empty_stream.iter().any(|&b| b) {
        let mut b = Vec::new();
        put_bits(&mut b, &empty_stream);
        prop(&mut h, id::EMPTY_STREAM, &b);
        if empty_file.iter().any(|&x| x) {
            let mut b = Vec::new();
            put_bits(&mut b, &empty_file);
            prop(&mut h, id::EMPTY_FILE, &b);
        }
    }
    {
        let mut b = vec![0u8];
        for n in &names {
            for u in n.encode_utf16() {
                b.extend_from_slice(&u.to_le_bytes());
            }
            b.extend_from_slice(&[0, 0]);
        }
        prop(&mut h, id::NAMES, &b);
    }
    if mtimes.iter().any(Option::is_some) {
        let def: Vec<bool> = mtimes.iter().map(Option::is_some).collect();
        let mut b = Vec::new();
        put_defined(&mut b, &def);
        b.push(0);
        for t in mtimes.iter().flatten() {
            let ft = ((*t + 11_644_473_600).max(0) as u64) * 10_000_000;
            b.extend_from_slice(&ft.to_le_bytes());
        }
        prop(&mut h, id::MTIME, &b);
    }
    {
        let mut b = vec![1u8, 0u8];
        for a in &attrs {
            b.extend_from_slice(&a.to_le_bytes());
        }
        prop(&mut h, id::WIN_ATTRIBUTES, &b);
    }
    h.push(id::END);
    h.push(id::END);
    // Compress the header itself (and encrypt it when a password is set).
    let hf = encode_stream(&h, Method::Lzma, 5, pw)?;
    let pack_pos = (out.len() - 32) as u64;
    out.extend_from_slice(&hf.packed);
    let mut eh: Vec<u8> = vec![id::ENCODED_HEADER, id::PACK_INFO];
    put_num(&mut eh, pack_pos);
    put_num(&mut eh, 1);
    eh.push(id::SIZE);
    put_num(&mut eh, hf.packed.len() as u64);
    eh.push(id::END);
    eh.push(id::UNPACK_INFO);
    eh.push(id::FOLDER);
    put_num(&mut eh, 1);
    eh.push(0);
    put_folder(&mut eh, &hf);
    eh.push(id::CODERS_UNPACK_SIZE);
    for s in &hf.unpack_sizes {
        put_num(&mut eh, *s);
    }
    eh.push(id::CRC);
    eh.push(1);
    eh.extend_from_slice(&crc32(&h).to_le_bytes());
    eh.push(id::END);
    eh.push(id::END);
    let next_off = (out.len() - 32) as u64;
    out.extend_from_slice(&eh);
    let mut start = Vec::new();
    start.extend_from_slice(&next_off.to_le_bytes());
    start.extend_from_slice(&(eh.len() as u64).to_le_bytes());
    start.extend_from_slice(&crc32(&eh).to_le_bytes());
    out[0..6].copy_from_slice(&SIGNATURE);
    out[6] = 0;
    out[7] = 4;
    out[8..12].copy_from_slice(&crc32(&start).to_le_bytes());
    out[12..32].copy_from_slice(&start);
    Ok(out)
}
