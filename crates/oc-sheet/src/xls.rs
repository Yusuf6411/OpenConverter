//! Legacy Excel workbooks (BIFF8, `.xls`): sheets of cells with shared strings, numbers, dates
//! (by number format), booleans, errors and the cached results of formulas.
//! Not read: BIFF2-5 files (Excel 5 and older), encrypted workbooks, charts, macros, styles.
#![allow(clippy::indexing_slicing)]

use crate::book::{format_datetime, format_number, serial_to_datetime, Workbook};
use crate::xlsx::{assemble, is_date_format};
use oc_archive::cfb::Cfb;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};
use std::collections::{BTreeMap, HashMap};

fn corrupt(m: &str) -> Error {
    Error::new(ErrorCode::CorruptFile, m.to_string())
}

struct Rec<'a> {
    id: u16,
    data: &'a [u8],
    /// Data of the CONTINUE records that followed (SST strings span them).
    cont: Vec<&'a [u8]>,
}

fn records(d: &[u8]) -> Vec<(usize, Rec<'_>)> {
    let mut out: Vec<(usize, Rec<'_>)> = Vec::new();
    let mut p = 0;
    while p + 4 <= d.len() {
        let id = u16::from_le_bytes([d[p], d[p + 1]]);
        let len = usize::from(u16::from_le_bytes([d[p + 2], d[p + 3]]));
        let Some(data) = d.get(p + 4..p + 4 + len) else {
            break;
        };
        if id == 0x003C {
            if let Some((_, last)) = out.last_mut() {
                last.cont.push(data);
            }
        } else {
            out.push((
                p,
                Rec {
                    id,
                    data,
                    cont: Vec::new(),
                },
            ));
        }
        p += 4 + len;
    }
    out
}

fn cp1252(b: u8) -> char {
    const HI: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&b) {
        char::from_u32(u32::from(HI[usize::from(b - 0x80)])).unwrap_or('?')
    } else {
        char::from(b)
    }
}

/// A cursor over a record and its CONTINUE records.
struct Cur<'a> {
    segs: Vec<&'a [u8]>,
    seg: usize,
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(r: &Rec<'a>) -> Self {
        let mut segs = vec![r.data];
        segs.extend(r.cont.iter().copied());
        Cur {
            segs,
            seg: 0,
            pos: 0,
        }
    }

    fn avail(&self) -> usize {
        self.segs
            .get(self.seg)
            .map_or(0, |s| s.len() - self.pos.min(s.len()))
    }

    fn u8(&mut self) -> Option<u8> {
        while self.avail() == 0 {
            self.seg += 1;
            self.pos = 0;
            if self.seg >= self.segs.len() {
                return None;
            }
        }
        let b = self.segs[self.seg][self.pos];
        self.pos += 1;
        Some(b)
    }

    fn u16(&mut self) -> Option<usize> {
        let a = self.u8()?;
        let b = self.u8()?;
        Some(usize::from(u16::from_le_bytes([a, b])))
    }

    fn u32(&mut self) -> Option<usize> {
        let a = self.u16()?;
        let b = self.u16()?;
        Some(a | (b << 16))
    }

    fn skip(&mut self, mut n: usize) {
        while n > 0 {
            if self.avail() == 0 {
                self.seg += 1;
                self.pos = 0;
                if self.seg >= self.segs.len() {
                    return;
                }
                continue;
            }
            let k = n.min(self.avail());
            self.pos += k;
            n -= k;
        }
    }

    /// `cch` characters; a CONTINUE boundary inside the text restarts with a flag byte.
    fn chars(&mut self, cch: usize, mut wide: bool) -> String {
        let mut s = String::new();
        let mut units: Vec<u16> = Vec::new();
        let mut left = cch;
        while left > 0 {
            if self.avail() == 0 {
                self.seg += 1;
                self.pos = 0;
                if self.seg >= self.segs.len() {
                    break;
                }
                wide = self.u8().unwrap_or(0) & 1 != 0;
                continue;
            }
            if wide {
                let lo = self.u8();
                let hi = self.u8();
                match (lo, hi) {
                    (Some(l), Some(h)) => units.push(u16::from_le_bytes([l, h])),
                    _ => break,
                }
            } else {
                match self.u8() {
                    Some(b) => units.push(cp1252(b) as u32 as u16),
                    None => break,
                }
            }
            left -= 1;
        }
        s.push_str(&String::from_utf16_lossy(&units));
        s
    }

    /// An XLUnicodeRichExtendedString.
    fn rich_string(&mut self) -> Option<String> {
        let cch = self.u16()?;
        let flags = self.u8()?;
        let runs = if flags & 8 != 0 { self.u16()? } else { 0 };
        let ext = if flags & 4 != 0 { self.u32()? } else { 0 };
        let s = self.chars(cch, flags & 1 != 0);
        self.skip(runs * 4 + ext);
        Some(s)
    }
}

fn rk(v: u32) -> f64 {
    let n = if v & 2 != 0 {
        f64::from((v as i32) >> 2)
    } else {
        f64::from_bits(u64::from(v & 0xFFFF_FFFC) << 32)
    };
    if v & 1 != 0 {
        n / 100.0
    } else {
        n
    }
}

fn error_text(c: u8) -> &'static str {
    match c {
        0x00 => "#NULL!",
        0x07 => "#DIV/0!",
        0x0F => "#VALUE!",
        0x17 => "#REF!",
        0x1D => "#NAME?",
        0x24 => "#NUM!",
        0x2A => "#N/A",
        _ => "#ERROR!",
    }
}

pub fn is_xls(d: &[u8]) -> bool {
    d.starts_with(&oc_archive::cfb::MAGIC)
}

pub fn read(data: &[u8], limits: &Limits, has_header: bool, notes: &mut Notes) -> Result<Workbook> {
    let cfb = Cfb::open(data, limits)?;
    if cfb.has_stream("EncryptedPackage") {
        return Err(Error::new(
            ErrorCode::EncryptedNeedPassword,
            "this workbook is password protected",
        ));
    }
    let wb = cfb
        .stream("Workbook")
        .or_else(|| cfb.stream("Book"))
        .ok_or_else(|| corrupt("this compound file holds no Excel workbook"))?;
    let recs = records(&wb);
    let mut date1904 = false;
    let mut formats: HashMap<usize, String> = HashMap::new();
    let mut xf: Vec<usize> = Vec::new();
    let mut sst: Vec<String> = Vec::new();
    let mut sheets: Vec<(String, bool, usize, u8)> = Vec::new();
    let mut first = true;
    for (_, r) in &recs {
        match r.id {
            0x0809 if first => {
                first = false;
                let ver = u16::from_le_bytes([r.data[0], r.data[1]]);
                if ver != 0x0600 {
                    return Err(Error::new(
                        ErrorCode::UnsupportedVariant,
                        "only Excel 97-2003 workbooks (BIFF8) can be read",
                    ));
                }
            }
            0x002F => {
                return Err(Error::new(
                    ErrorCode::EncryptedNeedPassword,
                    "this workbook is password protected",
                ))
            }
            0x0022 => date1904 = r.data.first() == Some(&1),
            0x041E => {
                let mut c = Cur::new(r);
                if let (Some(id), Some(cch), Some(fl)) = (c.u16(), c.u16(), c.u8()) {
                    formats.insert(id, c.chars(cch, fl & 1 != 0));
                }
            }
            0x00E0 => xf.push(
                r.data
                    .get(2..4)
                    .map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]]))),
            ),
            0x00FC => {
                let mut c = Cur::new(r);
                c.skip(8);
                while let Some(s) = c.rich_string() {
                    sst.push(s);
                    if sst.len() > 5_000_000 {
                        break;
                    }
                }
            }
            0x0085 if r.data.len() >= 8 => {
                let off = u32::from_le_bytes([r.data[0], r.data[1], r.data[2], r.data[3]]) as usize;
                let state = r.data[4];
                let kind = r.data[5];
                let cch = usize::from(r.data[6]);
                let wide = r.data[7] & 1 != 0;
                let mut c = Cur {
                    segs: vec![&r.data[8..]],
                    seg: 0,
                    pos: 0,
                };
                sheets.push((c.chars(cch, wide), state != 0, off, kind));
            }
            _ => {}
        }
    }
    if sheets.is_empty() {
        return Err(corrupt("the workbook lists no sheets"));
    }
    let style_of = |ixfe: usize| -> (bool, bool) {
        let id = xf.get(ixfe).copied().unwrap_or(0);
        is_date_format(id as u32, formats.get(&id).map(String::as_str))
    };
    let num = |v: f64, ixfe: usize| -> String {
        let (date, time) = style_of(ixfe);
        if date || time {
            if let Some(p) = serial_to_datetime(v, date1904) {
                return format_datetime(p, date, time);
            }
        }
        format_number(v)
    };
    let mut book = Workbook {
        sheets: Vec::new(),
        date1904,
    };
    let mut skipped = 0;
    for (name, hidden, off, kind) in sheets {
        if kind != 0 {
            skipped += 1;
            continue;
        }
        let Some(tail) = wb.get(off..) else {
            skipped += 1;
            continue;
        };
        let mut cells: BTreeMap<(usize, usize), String> = BTreeMap::new();
        let mut pending_string: Option<(usize, usize)> = None;
        let mut depth = 0;
        for (_, r) in records(tail) {
            let d = r.data;
            let w = |p: usize| {
                d.get(p..p + 2)
                    .map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]])))
            };
            match r.id {
                0x0809 => depth += 1,
                0x000A => {
                    depth -= 1;
                    if depth <= 0 {
                        break;
                    }
                }
                0x00FD if d.len() >= 10 => {
                    let i = u32::from_le_bytes([d[6], d[7], d[8], d[9]]) as usize;
                    cells.insert((w(0), w(2)), sst.get(i).cloned().unwrap_or_default());
                }
                0x0203 if d.len() >= 14 => {
                    let v = f64::from_le_bytes(d[6..14].try_into().unwrap_or([0; 8]));
                    cells.insert((w(0), w(2)), num(v, w(4)));
                }
                0x027E if d.len() >= 10 => {
                    let v = rk(u32::from_le_bytes([d[6], d[7], d[8], d[9]]));
                    cells.insert((w(0), w(2)), num(v, w(4)));
                }
                0x00BD if d.len() >= 6 => {
                    let (row, c0) = (w(0), w(2));
                    let n = (d.len() - 6) / 6;
                    for i in 0..n {
                        let o = 4 + i * 6;
                        let v = rk(u32::from_le_bytes([d[o + 2], d[o + 3], d[o + 4], d[o + 5]]));
                        cells.insert((row, c0 + i), num(v, w(o)));
                    }
                }
                0x0204 if d.len() >= 8 => {
                    let mut c = Cur {
                        segs: vec![&d[6..]],
                        seg: 0,
                        pos: 0,
                    };
                    if let (Some(cch), Some(fl)) = (c.u16(), c.u8()) {
                        cells.insert((w(0), w(2)), c.chars(cch, fl & 1 != 0));
                    }
                }
                0x0205 if d.len() >= 8 => {
                    let t = if d[7] == 0 {
                        if d[6] != 0 { "true" } else { "false" }.to_string()
                    } else {
                        error_text(d[6]).to_string()
                    };
                    cells.insert((w(0), w(2)), t);
                }
                0x0006 if d.len() >= 14 => {
                    let key = (w(0), w(2));
                    if d[12] == 0xFF && d[13] == 0xFF {
                        match d[6] {
                            0 => pending_string = Some(key),
                            1 => {
                                cells.insert(key, if d[8] != 0 { "true" } else { "false" }.into());
                            }
                            2 => {
                                cells.insert(key, error_text(d[8]).into());
                            }
                            _ => {}
                        }
                    } else {
                        let v = f64::from_le_bytes(d[6..14].try_into().unwrap_or([0; 8]));
                        cells.insert(key, num(v, w(4)));
                    }
                }
                0x0207 => {
                    if let Some(key) = pending_string.take() {
                        let mut c = Cur::new(&r);
                        if let (Some(cch), Some(fl)) = (c.u16(), c.u8()) {
                            cells.insert(key, c.chars(cch, fl & 1 != 0));
                        }
                    }
                }
                _ => {}
            }
            if cells.len() as u64 > limits.max_entries {
                return Err(Error::limit(
                    "table",
                    "the sheet holds more cells than the allowed maximum",
                ));
            }
        }
        book.sheets.push(assemble(
            name,
            hidden,
            cells,
            BTreeMap::new(),
            Vec::new(),
            has_header,
            limits,
        )?);
    }
    if skipped > 0 {
        notes.push(Note::new(
            Severity::Info,
            "sheet_skipped",
            format!("{skipped} chart or macro sheet(s) have no cell data and were skipped"),
        ));
    }
    notes.push(Note::new(
        Severity::Info,
        "xls.values_only",
        "formulas were read as their last calculated values; styles, charts and macros are not carried",
    ));
    if book.sheets.is_empty() {
        return Err(corrupt("the workbook has no sheets with cells"));
    }
    Ok(book)
}
