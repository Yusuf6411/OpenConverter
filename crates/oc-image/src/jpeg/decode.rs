//! JPEG decoder: baseline, extended-sequential and progressive, Huffman or arithmetic, 8-bit samples,
//! 1/3/4 components, any integral chroma subsampling, restart intervals.
//!
//! All scans are decoded into per-component coefficient buffers first, so sequential,
//! progressive and multi-scan files share one path. Pixel arithmetic follows libjpeg
//! (integer IDCT, "fancy" chroma upsampling, fixed-point YCbCr), so output normally matches
//! what other software shows, bit for bit.
//!
//! Arithmetic-coded files (sequential and progressive) are read as well.
//!
//! Not supported (typed error): lossless, hierarchical, 12-bit samples,
//! non-integral sampling ratios. EXIF orientation is reported, not applied.
mod arith;
mod lossless;

use super::dct;
use super::tables::ZIGZAG;
use crate::icc;
use crate::image::{Depth, Image, Layout};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(&[0xFF, 0xD8, 0xFF])
}

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorCode::UnsupportedVariant,
        format!("JPEG: {what} is not supported"),
    )
}

// ---------------------------------------------------------------- Huffman tables

struct Huff {
    /// 9-bit lookahead: `len << 8 | symbol`, 0 when the code is longer.
    look: [u16; 512],
    maxcode: [i32; 17],
    valoff: [i32; 17],
    vals: Vec<u8>,
}

impl Huff {
    fn build(bits: &[u8; 16], vals: &[u8]) -> Result<Self> {
        let total: usize = bits.iter().map(|&b| usize::from(b)).sum();
        if total > 256 || total != vals.len() {
            return Err(Error::corrupt("JPEG Huffman table has inconsistent counts"));
        }
        let mut h = Huff {
            look: [0; 512],
            maxcode: [-1; 17],
            valoff: [0; 17],
            vals: vals.to_vec(),
        };
        let (mut code, mut k) = (0i32, 0usize);
        for len in 1..=16usize {
            let n = usize::from(bits.get(len - 1).copied().unwrap_or(0));
            if let Some(v) = h.valoff.get_mut(len) {
                *v = k as i32 - code;
            }
            for _ in 0..n {
                if len <= 9 {
                    let sym = u16::from(vals.get(k).copied().unwrap_or(0));
                    let base = (code as usize) << (9 - len);
                    for suffix in 0..(1usize << (9 - len)) {
                        if let Some(e) = h.look.get_mut(base + suffix) {
                            *e = ((len as u16) << 8) | sym;
                        }
                    }
                }
                code += 1;
                k += 1;
            }
            if code > (1 << len) {
                return Err(Error::corrupt("JPEG Huffman table is over-subscribed"));
            }
            if n > 0 {
                if let Some(m) = h.maxcode.get_mut(len) {
                    *m = code - 1;
                }
            }
            code <<= 1;
        }
        Ok(h)
    }
}

// ---------------------------------------------------------------- entropy-coded data

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    nbits: u32,
    /// Trailing zero bits in `buf` that were invented because the data ran out.
    pad: u32,
    marker: bool,
    starved: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            buf: 0,
            nbits: 0,
            pad: 0,
            marker: false,
            starved: false,
        }
    }

    fn fill(&mut self) {
        while self.nbits <= 48 {
            let byte = if self.marker {
                None
            } else {
                match self.data.get(self.pos) {
                    None => {
                        self.marker = true;
                        None
                    }
                    Some(&0xFF) => match self.data.get(self.pos + 1) {
                        Some(0) => {
                            self.pos += 2;
                            Some(0xFF)
                        }
                        Some(0xFF) => {
                            self.pos += 1; // fill byte before a marker
                            continue;
                        }
                        _ => {
                            self.marker = true;
                            None
                        }
                    },
                    Some(&b) => {
                        self.pos += 1;
                        Some(b)
                    }
                }
            };
            match byte {
                Some(b) => {
                    self.buf = (self.buf << 8) | u64::from(b);
                }
                None => {
                    self.buf <<= 8;
                    self.pad += 8;
                }
            }
            self.nbits += 8;
        }
    }

    fn consume(&mut self, n: u32) {
        self.nbits -= n;
        if self.nbits < self.pad {
            self.starved = true;
            self.pad = self.nbits;
        }
    }

    fn peek(&mut self, n: u32) -> u32 {
        if self.nbits < n {
            self.fill();
        }
        ((self.buf >> (self.nbits - n)) & ((1u64 << n) - 1)) as u32
    }

    fn get(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.consume(n);
        v
    }

    fn bit(&mut self) -> bool {
        self.get(1) != 0
    }

    fn symbol(&mut self, h: &Huff) -> Result<u8> {
        let look = h.look.get(self.peek(9) as usize).copied().unwrap_or(0);
        if look != 0 {
            self.consume(u32::from(look >> 8));
            return Ok((look & 0xFF) as u8);
        }
        let v = self.peek(16) as i32;
        for len in 10..=16usize {
            let code = v >> (16 - len);
            if h.maxcode.get(len).is_some_and(|&m| m >= 0 && code <= m) {
                let idx = code + h.valoff.get(len).copied().unwrap_or(0);
                self.consume(len as u32);
                return h
                    .vals
                    .get(usize::try_from(idx).unwrap_or(usize::MAX))
                    .copied()
                    .ok_or_else(|| Error::corrupt("JPEG Huffman code out of range"));
            }
        }
        Err(Error::corrupt("JPEG has an invalid Huffman code"))
    }

    /// Read `s` bits and sign-extend them as a JPEG magnitude category.
    fn extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.get(s) as i32;
        if v < (1 << (s - 1)) {
            v - (1 << s) + 1
        } else {
            v
        }
    }

    /// Move to just after the next RSTn marker, if there is one.
    fn restart(&mut self) {
        self.buf = 0;
        self.nbits = 0;
        self.pad = 0;
        self.marker = false;
        while self.pos + 1 < self.data.len() {
            let (a, b) = (
                self.data.get(self.pos).copied().unwrap_or(0),
                self.data.get(self.pos + 1).copied().unwrap_or(0),
            );
            if a == 0xFF && (0xD0..=0xD7).contains(&b) {
                self.pos += 2;
                return;
            }
            if a == 0xFF && b != 0 && b != 0xFF {
                // Some other marker: the interval is over, leave it for the caller.
                self.marker = true;
                return;
            }
            self.pos += 1;
        }
        self.marker = true;
    }
}

// ---------------------------------------------------------------- frame structures

struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks per row/column including MCU padding (storage size).
    bw: usize,
    bh: usize,
    /// Blocks that cover the real component dimensions (non-interleaved scan order).
    real_bw: usize,
    real_bh: usize,
    coefs: Vec<i16>,
    /// Decoded samples of a lossless component (there is no transform to undo).
    samples: Vec<u16>,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    arithmetic: bool,
    lossless: bool,
    /// Bits per sample: 8 or 12 for DCT files, 2 to 16 for lossless ones.
    precision: u32,
    comps: Vec<Comp>,
    hmax: usize,
    vmax: usize,
    mcus_x: usize,
    mcus_y: usize,
}

struct ScanComp {
    index: usize,
    td: usize,
    ta: usize,
}

struct Scan {
    comps: Vec<ScanComp>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

fn clamp16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

struct Tables {
    dc: [Option<Huff>; 4],
    ac: [Option<Huff>; 4],
    q: [Option<[u16; 64]>; 4],
}

fn decode_scan(
    frame: &mut Frame,
    scan: &Scan,
    bits: &mut Bits<'_>,
    tables: &Tables,
    restart_interval: usize,
    progress: &dyn Progress,
) -> Result<()> {
    let interleaved = scan.comps.len() > 1;
    let total = if interleaved {
        frame.mcus_x * frame.mcus_y
    } else {
        let c = scan
            .comps
            .first()
            .and_then(|s| frame.comps.get(s.index))
            .ok_or_else(|| Error::corrupt("JPEG scan has no components"))?;
        c.real_bw * c.real_bh
    };
    let mut pred = vec![0i32; frame.comps.len()];
    let mut eobrun = 0u32;
    let (mcus_x, progressive) = (frame.mcus_x, frame.progressive);
    for mcu in 0..total {
        if restart_interval > 0 && mcu > 0 && mcu % restart_interval == 0 {
            bits.restart();
            pred.iter_mut().for_each(|p| *p = 0);
            eobrun = 0;
        }
        if mcu % 256 == 0 {
            progress.check_cancelled()?;
        }
        if interleaved {
            let (mx, my) = (mcu % mcus_x, mcu / mcus_x);
            for sc in &scan.comps {
                let (h, v) = {
                    let c = frame
                        .comps
                        .get(sc.index)
                        .ok_or_else(|| Error::corrupt("JPEG scan names a missing component"))?;
                    (c.h, c.v)
                };
                for by in 0..v {
                    for bx in 0..h {
                        decode_block(
                            frame,
                            scan,
                            sc,
                            my * v + by,
                            mx * h + bx,
                            bits,
                            tables,
                            &mut pred,
                            &mut eobrun,
                            progressive,
                        )?;
                    }
                }
            }
        } else if let Some(sc) = scan.comps.first() {
            let real_bw = frame.comps.get(sc.index).map_or(1, |c| c.real_bw.max(1));
            decode_block(
                frame,
                scan,
                sc,
                mcu / real_bw,
                mcu % real_bw,
                bits,
                tables,
                &mut pred,
                &mut eobrun,
                progressive,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // one hot path; a struct would only rename the arguments
fn decode_block(
    frame: &mut Frame,
    scan: &Scan,
    sc: &ScanComp,
    by: usize,
    bx: usize,
    bits: &mut Bits<'_>,
    tables: &Tables,
    pred: &mut [i32],
    eobrun: &mut u32,
    progressive: bool,
) -> Result<()> {
    let comp = frame
        .comps
        .get_mut(sc.index)
        .ok_or_else(|| Error::corrupt("JPEG scan names a missing component"))?;
    let off = (by * comp.bw + bx) * 64;
    let block = comp
        .coefs
        .get_mut(off..off + 64)
        .ok_or_else(|| Error::corrupt("JPEG block outside the image"))?;
    let dc = || {
        tables
            .dc
            .get(sc.td)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::corrupt("JPEG scan uses an undefined DC table"))
    };
    let ac = || {
        tables
            .ac
            .get(sc.ta)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::corrupt("JPEG scan uses an undefined AC table"))
    };
    let pred = pred
        .get_mut(sc.index)
        .ok_or_else(|| Error::corrupt("JPEG scan names a missing component"))?;
    let zz = |k: usize| ZIGZAG.get(k).copied().unwrap_or(0);
    let set = |block: &mut [i16], k: usize, v: i32| {
        if let Some(c) = block.get_mut(zz(k)) {
            *c = clamp16(v);
        }
    };

    if !progressive {
        let s = u32::from(bits.symbol(dc()?)?);
        if s > 15 {
            return Err(Error::corrupt("JPEG DC category out of range"));
        }
        *pred += bits.extend(s);
        set(block, 0, *pred);
        let ac = ac()?;
        let mut k = 1;
        while k < 64 {
            let rs = bits.symbol(ac)?;
            let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
            if s == 0 {
                if r == 15 {
                    k += 16;
                    continue;
                }
                break;
            }
            k += r;
            if k > 63 {
                break;
            }
            let v = bits.extend(s);
            set(block, k, v);
            k += 1;
        }
        return Ok(());
    }

    let (ss, se, ah, al) = (scan.ss, scan.se, scan.ah, scan.al);
    if ss == 0 {
        if ah == 0 {
            let s = u32::from(bits.symbol(dc()?)?);
            if s > 15 {
                return Err(Error::corrupt("JPEG DC category out of range"));
            }
            *pred += bits.extend(s);
            set(block, 0, *pred << al);
        } else if bits.bit() {
            if let Some(c) = block.first_mut() {
                *c |= 1 << al;
            }
        }
        return Ok(());
    }

    let ac = ac()?;
    if ah == 0 {
        // AC first pass.
        if *eobrun > 0 {
            *eobrun -= 1;
            return Ok(());
        }
        let mut k = ss;
        while k <= se {
            let rs = bits.symbol(ac)?;
            let (r, s) = (u32::from(rs >> 4), u32::from(rs & 15));
            if s != 0 {
                k += r as usize;
                if k > 63 {
                    break;
                }
                let v = bits.extend(s);
                set(block, k, v << al);
            } else if r == 15 {
                k += 15;
            } else {
                *eobrun = 1 << r;
                if r > 0 {
                    *eobrun += bits.get(r);
                }
                *eobrun -= 1;
                break;
            }
            k += 1;
        }
        return Ok(());
    }

    // AC refinement pass.
    let p1 = 1i16 << al;
    let m1 = -1i16 << al;
    let refine = |bits: &mut Bits<'_>, c: &mut i16| {
        if bits.bit() && (*c & p1) == 0 {
            *c = c.saturating_add(if *c >= 0 { p1 } else { m1 });
        }
    };
    let mut k = ss;
    if *eobrun == 0 {
        while k <= se {
            let rs = bits.symbol(ac)?;
            let (mut r, s) = (i32::from(rs >> 4), u32::from(rs & 15));
            let mut value = 0i16;
            if s != 0 {
                value = if bits.bit() { p1 } else { m1 };
            } else if r != 15 {
                *eobrun = 1 << r;
                if r > 0 {
                    *eobrun += bits.get(r as u32);
                }
                break;
            }
            while k <= se {
                let Some(c) = block.get_mut(zz(k)) else { break };
                if *c != 0 {
                    refine(bits, c);
                } else {
                    r -= 1;
                    if r < 0 {
                        break;
                    }
                }
                k += 1;
            }
            if value != 0 && k <= se {
                if let Some(c) = block.get_mut(zz(k)) {
                    *c = value;
                }
            }
            k += 1;
        }
    }
    if *eobrun > 0 {
        while k <= se {
            if let Some(c) = block.get_mut(zz(k)) {
                if *c != 0 {
                    refine(bits, c);
                }
            }
            k += 1;
        }
        *eobrun -= 1;
    }
    Ok(())
}

// ---------------------------------------------------------------- marker parsing

fn u16_at(d: &[u8], i: usize) -> Option<usize> {
    Some(usize::from(*d.get(i)?) << 8 | usize::from(*d.get(i + 1)?))
}

/// Find the next marker at or after `*pos`, returning its code and leaving `*pos` after it.
fn next_marker(data: &[u8], pos: &mut usize) -> Option<u8> {
    loop {
        while *data.get(*pos)? != 0xFF {
            *pos += 1;
        }
        while *data.get(*pos)? == 0xFF {
            *pos += 1;
        }
        let m = *data.get(*pos)?;
        *pos += 1;
        if m != 0 {
            return Some(m);
        }
    }
}

#[derive(Default)]
struct Meta {
    jfif: Option<(u8, usize, usize)>,
    adobe_transform: Option<u8>,
    icc: Vec<(u8, u8, Vec<u8>)>,
    orientation: Option<u16>,
    /// Set by TIFF: three components are RGB (true) or YCbCr (false), whatever the stream says.
    rgb_override: Option<bool>,
}

fn parse_exif_orientation(seg: &[u8]) -> Option<u16> {
    let t = seg.strip_prefix(b"Exif\0\0")?;
    let le = match t.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16r = |i: usize| -> Option<u16> {
        let b = [*t.get(i)?, *t.get(i + 1)?];
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32r = |i: usize| -> Option<u32> {
        let b = [*t.get(i)?, *t.get(i + 1)?, *t.get(i + 2)?, *t.get(i + 3)?];
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let ifd = u32r(4)? as usize;
    let n = usize::from(u16r(ifd)?);
    for i in 0..n.min(256) {
        let e = ifd + 2 + i * 12;
        if u16r(e)? == 0x0112 {
            return u16r(e + 8);
        }
    }
    None
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    decode_with(data, None, limits, progress)
}

/// A JPEG stream from inside a TIFF (compression 7): the tables may live in the file's
/// `JPEGTables` tag, and the TIFF's photometric tag, not the stream, says whether three
/// components are RGB (`rgb_direct`) or YCbCr.
pub fn decode_tiff(
    data: &[u8],
    tables: Option<&[u8]>,
    rgb_direct: bool,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<(Image, Notes)> {
    if let Some(t) = tables.filter(|t| t.starts_with(&[0xFF, 0xD8]) && sniff(data)) {
        // Splice the shared tables in after the stream's SOI.
        let body = t.get(2..).unwrap_or(&[]);
        let body = body.strip_suffix(&[0xFF, 0xD9]).unwrap_or(body);
        let mut joined = Vec::with_capacity(data.len() + body.len());
        joined.extend_from_slice(&[0xFF, 0xD8]);
        joined.extend_from_slice(body);
        joined.extend_from_slice(data.get(2..).unwrap_or(&[]));
        return decode_with(&joined, Some(rgb_direct), limits, progress);
    }
    decode_with(data, Some(rgb_direct), limits, progress)
}

/// Everything read from the marker stream before pixels are built.
struct Parsed {
    frame: Frame,
    tables: Tables,
    meta: Meta,
    /// The scans ran out of data.
    starved: bool,
    /// APPn and COM segments in file order, as (marker, body).
    kept: Vec<(u8, Vec<u8>)>,
}

fn parse_stream(
    data: &[u8],
    rgb_direct: Option<bool>,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Parsed> {
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a JPEG file"));
    }
    let mut pos = 2usize;
    let mut tables = Tables {
        dc: [None, None, None, None],
        ac: [None, None, None, None],
        q: [None; 4],
    };
    let mut frame: Option<Frame> = None;
    let mut restart_interval = 0usize;
    let mut cond = arith::Cond::default();
    let mut meta = Meta {
        rgb_override: rgb_direct,
        ..Meta::default()
    };
    let mut scans = 0usize;
    let mut kept: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut starved = false;

    while let Some(m) = next_marker(data, &mut pos) {
        match m {
            0xD9 => break, // EOI
            0x01 | 0xD0..=0xD8 => continue,
            _ => {}
        }
        let len = u16_at(data, pos).ok_or_else(|| Error::truncated("JPEG marker"))?;
        if len < 2 {
            return Err(Error::corrupt("JPEG marker segment is too short"));
        }
        let seg = data
            .get(pos + 2..pos + len)
            .ok_or_else(|| Error::truncated("JPEG marker segment"))?;
        pos += len;
        if matches!(m, 0xE0..=0xEF | 0xFE) {
            kept.push((m, seg.to_vec()));
        }
        match m {
            0xC0..=0xC3 | 0xC9 | 0xCA => {
                if frame.is_some() {
                    return Err(Error::corrupt("JPEG has more than one frame header"));
                }
                let kind = Kind {
                    progressive: m == 0xC2 || m == 0xCA,
                    arithmetic: m >= 0xC9,
                    lossless: m == 0xC3,
                };
                frame = Some(parse_sof(seg, kind, limits)?);
            }
            0xCC => arith::parse_dac(seg, &mut cond)?,
            0xC5..=0xC7 => return Err(unsupported("hierarchical coding")),
            0xCB => return Err(unsupported("lossless coding")),
            0xCD..=0xCF => return Err(unsupported("hierarchical coding")),
            0xC4 => parse_dht(seg, &mut tables)?,
            0xDB => parse_dqt(seg, &mut tables)?,
            0xDD => {
                restart_interval = u16_at(seg, 0).ok_or_else(|| Error::corrupt("JPEG DRI"))?;
            }
            0xE0 => {
                if let Some(rest) = seg.strip_prefix(b"JFIF\0") {
                    if let (Some(&u), Some(x), Some(y)) =
                        (rest.get(2), u16_at(rest, 3), u16_at(rest, 5))
                    {
                        meta.jfif = Some((u, x, y));
                    }
                }
            }
            0xE1 => {
                if meta.orientation.is_none() {
                    meta.orientation = parse_exif_orientation(seg);
                }
            }
            0xE2 => {
                if let Some(rest) = seg.strip_prefix(b"ICC_PROFILE\0") {
                    if let (Some(&seq), Some(&count)) = (rest.first(), rest.get(1)) {
                        meta.icc
                            .push((seq, count, rest.get(2..).unwrap_or(&[]).to_vec()));
                    }
                }
            }
            0xEE => {
                if let Some(rest) = seg.strip_prefix(b"Adobe") {
                    meta.adobe_transform = rest.get(6).copied();
                }
            }
            0xDA => {
                let f = frame
                    .as_mut()
                    .ok_or_else(|| Error::corrupt("JPEG scan before frame header"))?;
                let scan = parse_sos(seg, f)?;
                if f.arithmetic {
                    let (end, bad) =
                        arith::decode_scan(f, &scan, data, pos, &cond, restart_interval, progress)?;
                    starved |= bad;
                    pos = end;
                } else if f.lossless {
                    let mut bits = Bits::new(data, pos);
                    lossless::decode_scan(
                        f,
                        &scan,
                        &mut bits,
                        &tables,
                        restart_interval,
                        progress,
                    )?;
                    starved |= bits.starved;
                    pos = bits.pos;
                } else {
                    let mut bits = Bits::new(data, pos);
                    decode_scan(f, &scan, &mut bits, &tables, restart_interval, progress)?;
                    starved |= bits.starved;
                    pos = bits.pos;
                }
                scans += 1;
                progress.check_cancelled()?;
            }
            _ => {} // DNL and anything else carry nothing we need
        }
    }
    let frame = frame.ok_or_else(|| Error::corrupt("JPEG has no frame header"))?;
    if scans == 0 {
        return Err(Error::truncated("JPEG image data"));
    }
    Ok(Parsed {
        frame,
        tables,
        meta,
        starved,
        kept,
    })
}

/// Quantised coefficients of a DCT file, for editing without decoding to pixels.
pub(super) struct Coefs {
    pub width: usize,
    pub height: usize,
    pub progressive: bool,
    pub hmax: usize,
    pub vmax: usize,
    pub mcus_x: usize,
    pub mcus_y: usize,
    pub comps: Vec<CoefComp>,
    /// Quantisation tables by id, in natural order.
    pub q: [Option<[u16; 64]>; 4],
    /// APPn and COM segments in file order, as (marker, body).
    pub kept: Vec<(u8, Vec<u8>)>,
    pub truncated: bool,
}

pub(super) struct CoefComp {
    pub id: u8,
    pub h: usize,
    pub v: usize,
    pub tq: usize,
    /// Blocks per row and column of the MCU-padded grid.
    pub bw: usize,
    pub bh: usize,
    /// Natural-order coefficients, rows of `bw` blocks.
    pub blocks: Vec<[i16; 64]>,
}

/// Reads the coefficients of an 8-bit Huffman-coded DCT file (baseline or progressive).
pub(super) fn read_coefficients(
    data: &[u8],
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Coefs> {
    let Parsed {
        frame,
        tables,
        starved,
        kept,
        meta: _,
    } = parse_stream(data, None, limits, progress)?;
    if frame.arithmetic || frame.lossless || frame.precision != 8 {
        return Err(unsupported(
            "arithmetic, lossless or 12-bit coding in a lossless transform",
        ));
    }
    if frame
        .comps
        .iter()
        .any(|c| tables.q.get(c.tq).is_none_or(Option::is_none))
    {
        return Err(Error::corrupt("JPEG uses an undefined quantisation table"));
    }
    let single = frame.comps.len() == 1;
    let (hmax, vmax) = if single {
        (1, 1)
    } else {
        (frame.hmax, frame.vmax)
    };
    let mcus_x = frame.width.div_ceil(8 * hmax);
    let mcus_y = frame.height.div_ceil(8 * vmax);
    let mut comps = Vec::new();
    for c in &frame.comps {
        let (h, v) = if single { (1, 1) } else { (c.h, c.v) };
        if hmax % h != 0 || vmax % v != 0 {
            return Err(unsupported(
                "sampling factors that do not divide each other",
            ));
        }
        let (bw, bh) = (mcus_x * h, mcus_y * v);
        let mut blocks = vec![[0i16; 64]; bw * bh];
        for by in 0..bh.min(c.bh) {
            for bx in 0..bw.min(c.bw) {
                let at = (by * c.bw + bx) * 64;
                if let (Some(src), Some(dst)) =
                    (c.coefs.get(at..at + 64), blocks.get_mut(by * bw + bx))
                {
                    dst.copy_from_slice(src);
                }
            }
        }
        comps.push(CoefComp {
            id: c.id,
            h,
            v,
            tq: c.tq,
            bw,
            bh,
            blocks,
        });
    }
    Ok(Coefs {
        width: frame.width,
        height: frame.height,
        progressive: frame.progressive,
        hmax,
        vmax,
        mcus_x,
        mcus_y,
        comps,
        q: tables.q,
        kept,
        truncated: starved,
    })
}

fn decode_with(
    data: &[u8],
    rgb_direct: Option<bool>,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let Parsed {
        frame,
        tables,
        mut meta,
        starved,
        kept: _,
    } = parse_stream(data, rgb_direct, limits, progress)?;
    if starved {
        notes.push(Note::new(
            Severity::Warning,
            "jpeg.truncated",
            "the image data ended early; the rest of the picture is gray",
        ));
    }
    if let Some(o) = meta.orientation.filter(|&o| o > 1) {
        notes.push(
            Note::new(
                Severity::Info,
                "jpeg.orientation_ignored",
                "the file asks to be rotated or mirrored when shown; the pixels are stored unrotated",
            )
            .with_detail(o.to_string()),
        );
    }
    let icc = assemble_icc(&mut meta.icc);
    let (mut img, managed) =
        build_image(&frame, &tables, &meta, icc.as_deref(), &mut notes, limits)?;
    if let Some((unit, x, y)) = meta.jfif {
        let scale = match unit {
            1 => Some(1.0),
            2 => Some(2.54),
            _ => None,
        };
        if let (Some(s), true) = (scale, x > 0 && y > 0) {
            img.metadata.dpi = Some((x as f64 * s, y as f64 * s));
        }
    }
    if !managed {
        img.metadata.icc_profile = icc;
    }
    Ok((img, notes))
}

/// The profile from its numbered APP2 chunks, when they are all there.
fn assemble_icc(chunks: &mut [(u8, u8, Vec<u8>)]) -> Option<Vec<u8>> {
    chunks.sort_by_key(|c| c.0);
    let count = usize::from(chunks.first()?.1);
    (count == chunks.len()
        && chunks
            .iter()
            .enumerate()
            .all(|(i, c)| usize::from(c.0) == i + 1 && usize::from(c.1) == count))
    .then(|| chunks.iter().flat_map(|c| c.2.iter().copied()).collect())
}

#[derive(Clone, Copy)]
struct Kind {
    progressive: bool,
    arithmetic: bool,
    lossless: bool,
}

fn parse_sof(seg: &[u8], kind: Kind, limits: &Limits) -> Result<Frame> {
    let Kind {
        progressive,
        arithmetic,
        lossless,
    } = kind;
    let precision = *seg.first().ok_or_else(|| Error::corrupt("JPEG SOF"))?;
    let ok = if lossless {
        (2..=16).contains(&precision)
    } else {
        precision == 8 || precision == 12
    };
    if !ok {
        return Err(unsupported("this sample precision"));
    }
    let height = u16_at(seg, 1).ok_or_else(|| Error::corrupt("JPEG SOF"))?;
    let width = u16_at(seg, 3).ok_or_else(|| Error::corrupt("JPEG SOF"))?;
    let n = usize::from(*seg.get(5).ok_or_else(|| Error::corrupt("JPEG SOF"))?);
    if width == 0 || height == 0 {
        return Err(Error::corrupt(
            "JPEG has zero width or height (DNL is not supported)",
        ));
    }
    if !matches!(n, 1 | 3 | 4) {
        return Err(unsupported("this number of components"));
    }
    limits.check_pixels(width as u32, height as u32)?;
    let mut comps = Vec::new();
    for i in 0..n {
        let b = seg
            .get(6 + i * 3..9 + i * 3)
            .ok_or_else(|| Error::truncated("JPEG SOF components"))?;
        let &[id, hv, tq] = b else {
            return Err(Error::truncated("JPEG SOF components"));
        };
        let tq = usize::from(tq);
        let (h, v) = (usize::from(hv >> 4), usize::from(hv & 15));
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 {
            return Err(Error::corrupt(
                "JPEG component has invalid sampling or table",
            ));
        }
        comps.push(Comp {
            id,
            h,
            v,
            tq,
            bw: 0,
            bh: 0,
            real_bw: 0,
            real_bh: 0,
            coefs: Vec::new(),
            samples: Vec::new(),
        });
    }
    let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
    let (w, h) = (width, height);
    if lossless {
        if hmax != 1 || vmax != 1 {
            return Err(unsupported("lossless coding with chroma subsampling"));
        }
        // One sample per component per "MCU"; no blocks.
        limits.check_alloc((w * h * (n * 2 + 4)) as u64)?;
        for c in &mut comps {
            c.bw = w;
            c.bh = h;
            c.real_bw = w;
            c.real_bh = h;
            c.samples = vec![0u16; w * h];
        }
        return Ok(Frame {
            width: w,
            height: h,
            progressive: false,
            arithmetic,
            lossless,
            precision: u32::from(precision),
            comps,
            hmax,
            vmax,
            mcus_x: w,
            mcus_y: h,
        });
    }
    let mcus_x = w.div_ceil(8 * hmax);
    let mcus_y = h.div_ceil(8 * vmax);
    let mut bytes = 0u64;
    for c in &mut comps {
        c.bw = mcus_x * c.h;
        c.bh = mcus_y * c.v;
        c.real_bw = (w * c.h).div_ceil(hmax).div_ceil(8);
        c.real_bh = (h * c.v).div_ceil(vmax).div_ceil(8);
        bytes += (c.bw * c.bh * 128) as u64;
    }
    // Coefficients (2 bytes) plus the decoded planes and the output.
    limits.check_alloc(bytes + (w * h * 4) as u64)?;
    for c in &mut comps {
        c.coefs = vec![0i16; c.bw * c.bh * 64];
    }
    Ok(Frame {
        width: w,
        height: h,
        progressive,
        arithmetic,
        lossless,
        precision: u32::from(precision),
        comps,
        hmax,
        vmax,
        mcus_x,
        mcus_y,
    })
}

fn parse_dht(mut seg: &[u8], t: &mut Tables) -> Result<()> {
    while !seg.is_empty() {
        let tc_th = *seg.first().unwrap_or(&0);
        let bits: [u8; 16] = seg
            .get(1..17)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| Error::truncated("JPEG DHT"))?;
        let n: usize = bits.iter().map(|&b| usize::from(b)).sum();
        let vals = seg
            .get(17..17 + n)
            .ok_or_else(|| Error::truncated("JPEG DHT"))?;
        let th = usize::from(tc_th & 15);
        let slot = if tc_th >> 4 == 0 {
            t.dc.get_mut(th)
        } else {
            t.ac.get_mut(th)
        }
        .ok_or_else(|| Error::corrupt("JPEG DHT table id out of range"))?;
        *slot = Some(Huff::build(&bits, vals)?);
        seg = seg.get(17 + n..).unwrap_or(&[]);
    }
    Ok(())
}

fn parse_dqt(mut seg: &[u8], t: &mut Tables) -> Result<()> {
    while !seg.is_empty() {
        let pq_tq = *seg.first().unwrap_or(&0);
        let wide = pq_tq >> 4 != 0;
        let n = if wide { 128 } else { 64 };
        let body = seg
            .get(1..1 + n)
            .ok_or_else(|| Error::truncated("JPEG DQT"))?;
        let mut q = [0u16; 64];
        for (k, zz) in ZIGZAG.iter().enumerate() {
            let v = if wide {
                match body.get(2 * k..2 * k + 2) {
                    Some(&[a, b]) => u16::from_be_bytes([a, b]),
                    _ => 0,
                }
            } else {
                u16::from(body.get(k).copied().unwrap_or(0))
            };
            if let Some(slot) = q.get_mut(*zz) {
                *slot = v;
            }
        }
        *t.q.get_mut(usize::from(pq_tq & 15))
            .ok_or_else(|| Error::corrupt("JPEG DQT table id out of range"))? = Some(q);
        seg = seg.get(1 + n..).unwrap_or(&[]);
    }
    Ok(())
}

fn parse_sos(seg: &[u8], frame: &Frame) -> Result<Scan> {
    let ns = usize::from(*seg.first().ok_or_else(|| Error::corrupt("JPEG SOS"))?);
    if !(1..=4).contains(&ns) {
        return Err(Error::corrupt("JPEG scan has an invalid component count"));
    }
    let mut comps = Vec::new();
    for i in 0..ns {
        let b = seg
            .get(1 + i * 2..3 + i * 2)
            .ok_or_else(|| Error::truncated("JPEG SOS"))?;
        let &[cid, tables] = b else {
            return Err(Error::truncated("JPEG SOS"));
        };
        let index = frame
            .comps
            .iter()
            .position(|c| c.id == cid)
            .ok_or_else(|| Error::corrupt("JPEG scan names an unknown component"))?;
        if comps.iter().any(|c: &ScanComp| c.index == index) {
            return Err(Error::corrupt("JPEG scan repeats a component"));
        }
        comps.push(ScanComp {
            index,
            td: usize::from(tables >> 4),
            ta: usize::from(tables & 15),
        });
    }
    let tail = seg
        .get(1 + ns * 2..4 + ns * 2)
        .ok_or_else(|| Error::truncated("JPEG SOS"))?;
    let &[ss, se, a] = tail else {
        return Err(Error::truncated("JPEG SOS"));
    };
    let (ss, se) = (usize::from(ss), usize::from(se));
    let scan = Scan {
        comps,
        ss,
        se,
        ah: u32::from(a >> 4),
        al: u32::from(a & 15),
    };
    if frame.lossless {
        // Ss is the predictor selection, Al the point transform.
        if !(1..=7).contains(&ss) || scan.al >= frame.precision {
            return Err(Error::corrupt("JPEG lossless scan parameters are invalid"));
        }
    } else if frame.progressive {
        let ok = ss <= se && se <= 63 && scan.al <= 13 && if ss == 0 { se == 0 } else { ns == 1 };
        if !ok {
            return Err(Error::corrupt(
                "JPEG progressive scan parameters are invalid",
            ));
        }
    }
    Ok(scan)
}

// ---------------------------------------------------------------- pixels

/// Edge-clamped sample access into a component plane.
struct Plane {
    data: Vec<u16>,
    stride: usize,
    w: usize,
    h: usize,
}

impl Plane {
    fn at(&self, x: usize, y: usize) -> i32 {
        let (x, y) = (x.min(self.w - 1), y.min(self.h - 1));
        i32::from(self.data.get(y * self.stride + x).copied().unwrap_or(0))
    }
}

fn upsample(p: &Plane, hf: usize, vf: usize, out_w: usize, out_h: usize) -> Vec<u16> {
    let mut out = vec![0u16; out_w * out_h];
    let fancy_ok = p.w > 2;
    for y in 0..out_h {
        let Some(row) = out.get_mut(y * out_w..(y + 1) * out_w) else {
            continue;
        };
        match (hf, vf) {
            (1, 1) => {
                for (x, o) in row.iter_mut().enumerate() {
                    *o = p.at(x, y) as u16;
                }
            }
            (2, 1) if fancy_ok => {
                for (x, o) in row.iter_mut().enumerate() {
                    let i = x / 2;
                    let c = p.at(i, y);
                    *o = if x == 0 || x == 2 * p.w - 1 {
                        c
                    } else if x % 2 == 0 {
                        (3 * c + p.at(i - 1, y) + 1) >> 2
                    } else {
                        (3 * c + p.at(i + 1, y) + 2) >> 2
                    } as u16;
                }
            }
            (2, 2) if fancy_ok => {
                let near = y / 2;
                let far = if y % 2 == 0 {
                    near.saturating_sub(1)
                } else {
                    near + 1
                };
                let col = |i: usize| 3 * p.at(i, near) + p.at(i, far);
                for (x, o) in row.iter_mut().enumerate() {
                    let i = x / 2;
                    let this = col(i);
                    *o = if x == 0 {
                        (this * 4 + 8) >> 4
                    } else if x == 2 * p.w - 1 {
                        (this * 4 + 7) >> 4
                    } else if x % 2 == 0 {
                        (this * 3 + col(i - 1) + 8) >> 4
                    } else {
                        (this * 3 + col(i + 1) + 7) >> 4
                    } as u16;
                }
            }
            (1, 2) => {
                let near = y / 2;
                let (far, bias) = if y % 2 == 0 {
                    (near.saturating_sub(1), 1)
                } else {
                    (near + 1, 2)
                };
                for (x, o) in row.iter_mut().enumerate() {
                    *o = ((3 * p.at(x, near) + p.at(x, far) + bias) >> 2) as u16;
                }
            }
            _ => {
                for (x, o) in row.iter_mut().enumerate() {
                    *o = p.at(x / hf, y / vf) as u16;
                }
            }
        }
    }
    out
}

/// YCbCr to RGB with libjpeg's fixed-point tables; `bits` is the sample precision.
fn ycc_to_rgb(y: i32, cb: i32, cr: i32, bits: u32) -> [i32; 3] {
    // FIX(1.40200), FIX(1.77200), FIX(0.71414), FIX(0.34414) at 16 fractional bits.
    const HALF: i32 = 1 << 15;
    let centre = 1i32 << (bits - 1);
    let max = (1i32 << bits) - 1;
    let (cb, cr) = (cb - centre, cr - centre);
    let r = y + ((91881 * cr + HALF) >> 16);
    let g = y + ((-22554 * cb - 46802 * cr + HALF) >> 16);
    let b = y + ((116130 * cb + HALF) >> 16);
    [r.clamp(0, max), g.clamp(0, max), b.clamp(0, max)]
}

/// Widens (or narrows) a `from`-bit sample to `to` bits by repeating its bit pattern, so
/// full scale stays full scale.
fn rescale(v: u32, from: u32, to: u32) -> u32 {
    if from >= to {
        return v >> (from - to);
    }
    let mut out = v << (to - from);
    let mut shift = to - from;
    while shift > 0 {
        let s = shift.min(from);
        out |= v >> (from - s) << (shift - s);
        shift -= s;
    }
    out
}

/// Packs samples of `bits` precision as 8-bit, or as big-endian 16-bit above that.
fn pack_samples(values: impl Iterator<Item = u16>, bits: u32) -> (Depth, Vec<u8>) {
    if bits <= 8 {
        (
            Depth::Eight,
            values
                .map(|v| rescale(u32::from(v), bits, 8) as u8)
                .collect(),
        )
    } else {
        let mut out = Vec::new();
        for v in values {
            out.extend_from_slice(&(rescale(u32::from(v), bits, 16) as u16).to_be_bytes());
        }
        (Depth::Sixteen, out)
    }
}

fn build_image(
    f: &Frame,
    t: &Tables,
    meta: &Meta,
    profile: Option<&[u8]>,
    notes: &mut Notes,
    limits: &Limits,
) -> Result<(Image, bool)> {
    let (w, h) = (f.width, f.height);
    let bits = f.precision;
    let mut planes: Vec<Vec<u16>> = Vec::new();
    if f.lossless {
        planes.extend(f.comps.iter().map(|c| c.samples.clone()));
    }
    for c in f.comps.iter().filter(|_| !f.lossless) {
        let q =
            t.q.get(c.tq)
                .and_then(Option::as_ref)
                .ok_or_else(|| Error::corrupt("JPEG uses an undefined quantisation table"))?;
        let (cw, ch) = ((w * c.h).div_ceil(f.hmax), (h * c.v).div_ceil(f.vmax));
        if !f.hmax.is_multiple_of(c.h) || !f.vmax.is_multiple_of(c.v) {
            return Err(unsupported("non-integral chroma subsampling"));
        }
        let stride = c.bw * 8;
        let mut plane = Plane {
            data: vec![0u16; stride * c.bh * 8],
            stride,
            w: cw.max(1),
            h: ch.max(1),
        };
        let mut deq = [0i32; 64];
        let mut px = [0u16; 64];
        for by in 0..c.bh {
            for bx in 0..c.bw {
                let off = (by * c.bw + bx) * 64;
                let Some(block) = c.coefs.get(off..off + 64) else {
                    continue;
                };
                for ((d, &co), &qq) in deq.iter_mut().zip(block).zip(q) {
                    *d = i32::from(co) * i32::from(qq);
                }
                if bits == 12 {
                    dct::idct12(&deq, &mut px);
                } else {
                    let mut narrow = [0u8; 64];
                    dct::idct(&deq, &mut narrow);
                    for (o, &v) in px.iter_mut().zip(&narrow) {
                        *o = u16::from(v);
                    }
                }
                for (row, src) in px.as_chunks::<8>().0.iter().enumerate() {
                    let o = (by * 8 + row) * stride + bx * 8;
                    if let Some(dst) = plane.data.get_mut(o..o + 8) {
                        dst.copy_from_slice(src);
                    }
                }
            }
        }
        planes.push(upsample(&plane, f.hmax / c.h, f.vmax / c.v, w, h));
    }
    limits.check_alloc((w * h * f.comps.len() * 3) as u64)?;
    let n = w * h;
    let get =
        |p: usize, i: usize| i32::from(planes.get(p).and_then(|v| v.get(i)).copied().unwrap_or(0));
    let ids: Vec<u8> = f.comps.iter().map(|c| c.id).collect();
    match f.comps.len() {
        1 => {
            let (depth, data) = pack_samples(
                planes.into_iter().next().unwrap_or_default().into_iter(),
                bits,
            );
            Image::new(w as u32, h as u32, Layout::Gray, depth, data).map(|i| (i, false))
        }
        3 => {
            let rgb_direct = match (meta.rgb_override, meta.adobe_transform) {
                (Some(v), _) => v,
                (None, Some(0)) => true,
                (None, Some(_)) => false,
                // Lossless files are overwhelmingly RGB; only a JFIF header says otherwise.
                (None, None) if f.lossless => meta.jfif.is_none(),
                (None, None) => meta.jfif.is_none() && ids.as_slice() == b"RGB",
            };
            let mut rgb = Vec::with_capacity(n * 3);
            for i in 0..n {
                let px = if rgb_direct {
                    [get(0, i), get(1, i), get(2, i)]
                } else {
                    ycc_to_rgb(get(0, i), get(1, i), get(2, i), bits)
                };
                rgb.extend(px.map(|v| v as u16));
            }
            let (depth, out) = pack_samples(rgb.into_iter(), bits);
            Image::new(w as u32, h as u32, Layout::Rgb, depth, out).map(|i| (i, false))
        }
        _ => {
            // Four components: CMYK, or YCCK when Adobe's transform flag is 2.
            if bits != 8 {
                return Err(unsupported("CMYK with this sample precision"));
            }
            let adobe = meta.adobe_transform.is_some();
            let ycck = meta.adobe_transform == Some(2);
            // Ink amounts, 255 = full ink, which is what an ICC profile expects.
            let mut ink = Vec::with_capacity(n * 4);
            for i in 0..n {
                let (c, m, y) = if ycck {
                    let [r, g, b] = ycc_to_rgb(get(0, i), get(1, i), get(2, i), 8);
                    (255 - r, 255 - g, 255 - b)
                } else {
                    (get(0, i), get(1, i), get(2, i))
                };
                let k = get(3, i);
                // Adobe stores inverted values (255 = no ink); plain CMYK does not.
                let (c, m, y, k) = if adobe {
                    (255 - c, 255 - m, 255 - y, 255 - k)
                } else {
                    (c, m, y, k)
                };
                ink.extend([c, m, y, k].map(|v| v.clamp(0, 255) as u8));
            }
            let managed = profile.and_then(|p| {
                let t = icc::Transform::new(p, icc::Intent::RelativeColorimetric, true).ok()?;
                (t.channels() == 4).then(|| icc::convert_samples(&t, &ink, Depth::Eight))
            });
            let (out, is_managed) = if let Some(rgb) = managed {
                notes.push(Note::new(
                    Severity::Info,
                    "jpeg.cmyk_profile",
                    "the file is CMYK; it was converted to sRGB with its embedded colour profile",
                ));
                (rgb, true)
            } else {
                notes.push(Note::new(
                    Severity::Warning,
                    "jpeg.cmyk_converted",
                    "the file is CMYK; it was converted to RGB without colour management",
                ));
                let mut out = Vec::with_capacity(n * 3);
                for &[c, m, y, k] in ink.as_chunks::<4>().0 {
                    let k = 255 - i32::from(k);
                    for v in [c, m, y] {
                        out.push((((255 - i32::from(v)) * k + 127) / 255).clamp(0, 255) as u8);
                    }
                }
                (out, false)
            };
            Image::new(w as u32, h as u32, Layout::Rgb, Depth::Eight, out).map(|i| (i, is_managed))
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn dir() -> String {
        format!("{}/tests/data/jpeg", env!("CARGO_MANIFEST_DIR"))
    }

    fn run(name: &str) -> (Image, Notes) {
        decode(
            &std::fs::read(format!("{}/{name}.jpg", dir())).unwrap(),
            &Limits::default(),
            &NoProgress,
        )
        .unwrap()
    }

    /// Largest per-sample difference between our output and libjpeg's.
    fn max_diff(name: &str) -> u8 {
        let (img, _) = run(name);
        let want = ["rgb", "gray", "rgb16", "gray16"]
            .iter()
            .find_map(|ext| std::fs::read(format!("{}/{name}.{ext}", dir())).ok())
            .unwrap();
        assert_eq!(img.data.len(), want.len(), "{name}: size");
        img.data
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn matches_libjpeg_exactly_on_every_fixture() {
        let mut names: Vec<String> = std::fs::read_dir(dir())
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                (p.extension()? == "jpg").then(|| p.file_stem().unwrap().to_string_lossy().into())
            })
            .collect();
        names.sort();
        assert!(names.len() >= 60, "expected the generated fixtures");
        let mut bad = Vec::new();
        for n in &names {
            let d = max_diff(n);
            if d != 0 {
                bad.push(format!("{n}: max diff {d}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn metadata_dpi_and_orientation() {
        let (img, _) = run("icc_dpi");
        assert_eq!(img.metadata.dpi, Some((300.0, 150.0)));
    }

    #[test]
    fn corruption_is_an_error_or_a_warning_never_a_panic() {
        for name in ["big_420_prog", "arith_prog_420", "arith_restart"] {
            let good = std::fs::read(format!("{}/{name}.jpg", dir())).unwrap();
            for cut in [0, 1, 2, 3, 10, 100, 300, good.len() / 2, good.len() - 5] {
                let r = decode(&good[..cut], &Limits::default(), &NoProgress);
                if let Ok((_, notes)) = r {
                    // Arithmetic data may end silently: zeros are supplied after the end.
                    assert!(
                        name.starts_with("arith")
                            || notes.iter().any(|n| n.code == "jpeg.truncated"),
                        "{name} cut {cut}"
                    );
                }
            }
            // Flip bytes throughout; the decoder may fail but must not panic.
            let mut s = 12345u32;
            for _ in 0..300 {
                let mut bad = good.clone();
                for _ in 0..4 {
                    s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                    let i = (s >> 8) as usize % bad.len();
                    bad[i] ^= (s >> 3) as u8 | 1;
                }
                let _ = decode(&bad, &Limits::default(), &NoProgress);
            }
        }
    }

    #[test]
    fn unsupported_variants_are_typed_errors() {
        let mut d = std::fs::read(format!("{}/big_444.jpg", dir())).unwrap();
        let sof = d.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        d[sof + 1] = 0xC5; // hierarchical coding
        assert_eq!(
            decode(&d, &Limits::default(), &NoProgress)
                .err()
                .map(|e| e.code),
            Some(ErrorCode::UnsupportedVariant)
        );
        d[sof + 1] = 0xC0;
        d[sof + 4] = 5; // 5-bit samples in a DCT file
        assert_eq!(
            decode(&d, &Limits::default(), &NoProgress)
                .err()
                .map(|e| e.code),
            Some(ErrorCode::UnsupportedVariant)
        );
    }

    #[test]
    fn limits_apply_before_allocation() {
        let d = std::fs::read(format!("{}/big_444.jpg", dir())).unwrap();
        let tight = Limits {
            max_pixels: 100,
            ..Limits::default()
        };
        assert!(decode(&d, &tight, &NoProgress).is_err());
    }
}
