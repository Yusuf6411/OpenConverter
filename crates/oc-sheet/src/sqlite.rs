//! SQLite database files: ordinary rowid tables are read into a workbook (one sheet per table)
//! and a workbook is written as a database with one table per sheet.
//!
//! Reading follows the documented file format: page 1 header, B-tree pages, records, overflow
//! chains, the `sqlite_schema` table. Not read: `WITHOUT ROWID` tables, virtual tables, data
//! still in a write-ahead log, encrypted databases.
#![allow(
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks,
    clippy::needless_range_loop
)]

use crate::book::{format_number, Sheet, Workbook};
use crate::table::{Table, TableLimits};
use oc_core::{Error, ErrorCode, Note, Notes, Result, Severity};
use std::collections::{BTreeMap, HashSet};

fn corrupt(m: &str) -> Error {
    Error::new(ErrorCode::CorruptFile, m.to_string())
}

pub fn is_sqlite(d: &[u8]) -> bool {
    d.starts_with(b"SQLite format 3\0")
}

fn varint(d: &[u8], mut p: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let b = *d.get(p)?;
        p += 1;
        if i == 8 {
            return Some(((v << 8) | u64::from(b), p));
        }
        v = (v << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some((v, p));
        }
    }
    None
}

fn be(d: &[u8], p: usize, n: usize) -> Option<u64> {
    let s = d.get(p..p.checked_add(n)?)?;
    Some(s.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b)))
}

struct Db<'a> {
    d: &'a [u8],
    ps: usize,
    usable: usize,
    enc: u8,
    pages: usize,
}

#[derive(Debug, Clone)]
enum V {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Db<'_> {
    fn page(&self, n: usize) -> Option<&[u8]> {
        if n == 0 || n > self.pages {
            return None;
        }
        self.d.get((n - 1) * self.ps..n * self.ps)
    }

    fn text(&self, b: &[u8]) -> String {
        match self.enc {
            2 | 3 => {
                let u: Vec<u16> = b
                    .chunks_exact(2)
                    .map(|c| {
                        if self.enc == 2 {
                            u16::from_le_bytes([c[0], c[1]])
                        } else {
                            u16::from_be_bytes([c[0], c[1]])
                        }
                    })
                    .collect();
                String::from_utf16_lossy(&u)
            }
            _ => String::from_utf8_lossy(b).into_owned(),
        }
    }

    /// The full payload of a table leaf cell, following overflow pages.
    fn payload(&self, page: &[u8], p: usize, total: usize) -> Option<Vec<u8>> {
        let u = self.usable;
        let x = u.checked_sub(35)?;
        if total <= x {
            return page.get(p..p + total).map(<[u8]>::to_vec);
        }
        let m = ((u - 12) * 32 / 255).checked_sub(23)?;
        let k = m + (total - m) % (u - 4);
        let local = if k <= x { k } else { m };
        let mut out = page.get(p..p + local)?.to_vec();
        let mut next = be(page, p + local, 4)? as usize;
        let mut seen = 0;
        while out.len() < total {
            seen += 1;
            if next == 0 || seen > self.pages {
                return None;
            }
            let pg = self.page(next)?;
            let take = (total - out.len()).min(u - 4);
            out.extend_from_slice(pg.get(4..4 + take)?);
            next = be(pg, 0, 4)? as usize;
        }
        Some(out)
    }

    fn record(&self, r: &[u8]) -> Option<Vec<V>> {
        let (hsize, mut p) = varint(r, 0)?;
        let hsize = hsize as usize;
        let mut types = Vec::new();
        while p < hsize {
            let (t, np) = varint(r, p)?;
            types.push(t);
            p = np;
        }
        let mut body = hsize;
        let mut out = Vec::with_capacity(types.len());
        for t in types {
            let size = match t {
                0 | 8 | 9 => 0,
                1 => 1,
                2 => 2,
                3 => 3,
                4 => 4,
                5 => 6,
                6 | 7 => 8,
                n if n >= 12 => ((n - 12) / 2) as usize,
                _ => return None,
            };
            let raw = r.get(body..body.checked_add(size)?)?;
            body += size;
            out.push(match t {
                0 => V::Null,
                8 => V::Int(0),
                9 => V::Int(1),
                1..=6 => {
                    let mut v = raw.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b));
                    let bits = size * 8;
                    if bits < 64 && v & (1 << (bits - 1)) != 0 {
                        v |= !0u64 << bits;
                    }
                    V::Int(v as i64)
                }
                7 => V::Real(f64::from_bits(u64::from_be_bytes(raw.try_into().ok()?))),
                n if n % 2 == 0 => V::Blob(raw.to_vec()),
                _ => V::Text(self.text(raw)),
            });
        }
        Some(out)
    }

    /// Every (rowid, record) of the table whose root is `root`.
    fn rows(&self, root: usize, max_rows: u64, notes: &mut Notes) -> Vec<(i64, Vec<V>)> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![(root, 0usize)];
        let mut damaged = false;
        while let Some((n, depth)) = stack.pop() {
            if depth > 40 || !seen.insert(n) || out.len() as u64 >= max_rows {
                damaged |= depth > 40;
                continue;
            }
            let Some(pg) = self.page(n) else {
                damaged = true;
                continue;
            };
            let h = if n == 1 { 100 } else { 0 };
            let kind = pg[h];
            let ncells = be(pg, h + 3, 2).unwrap_or(0) as usize;
            match kind {
                0x0d => {
                    for i in 0..ncells {
                        let Some(cp) = be(pg, h + 8 + 2 * i, 2) else {
                            continue;
                        };
                        let cp = cp as usize;
                        let rec = (|| {
                            let (len, p) = varint(pg, cp)?;
                            let (rowid, p) = varint(pg, p)?;
                            let payload = self.payload(pg, p, len as usize)?;
                            Some((rowid as i64, self.record(&payload)?))
                        })();
                        match rec {
                            Some(r) => out.push(r),
                            None => damaged = true,
                        }
                    }
                }
                0x05 => {
                    let mut kids = Vec::new();
                    for i in 0..ncells {
                        if let Some(cp) = be(pg, h + 12 + 2 * i, 2) {
                            if let Some(c) = be(pg, cp as usize, 4) {
                                kids.push(c as usize);
                            }
                        }
                    }
                    if let Some(r) = be(pg, h + 8, 4) {
                        kids.push(r as usize);
                    }
                    // pushed in reverse so rows come out in order
                    for k in kids.into_iter().rev() {
                        stack.push((k, depth + 1));
                    }
                }
                _ => damaged = true,
            }
        }
        if damaged {
            notes.push(Note::new(
                Severity::Warning,
                "sqlite.damaged_pages",
                "some pages of the database could not be read; rows from them are missing",
            ));
        }
        out
    }
}

fn unquote(t: &str) -> String {
    let t = t.trim();
    let b = t.as_bytes();
    if b.len() >= 2 {
        let (f, l) = (b[0], b[b.len() - 1]);
        if (f == b'"' && l == b'"') || (f == b'`' && l == b'`') || (f == b'\'' && l == b'\'') {
            return t[1..t.len() - 1]
                .replace(&format!("{0}{0}", f as char), &(f as char).to_string());
        }
        if f == b'[' && l == b']' {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// Column names (and whether the first is an integer primary key, i.e. the rowid) from the
/// text of a CREATE TABLE statement.
fn columns(sql: &str) -> (Vec<String>, Option<usize>) {
    let Some(open) = sql.find('(') else {
        return (vec![], None);
    };
    let Some(close) = sql.rfind(')') else {
        return (vec![], None);
    };
    if close <= open {
        return (vec![], None);
    }
    let body = &sql[open + 1..close];
    let mut defs = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, None::<char>, 0usize);
    for (i, c) in body.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'' | '`') => quote = Some(c),
            (None, '[') => quote = Some(']'),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                defs.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    defs.push(&body[start..]);
    let mut names = Vec::new();
    let mut rowid = None;
    for d in defs {
        let d = d.trim();
        let first = d
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if matches!(
            first.as_str(),
            "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN" | "CONSTRAINT" | ""
        ) {
            continue;
        }
        let name = if d.starts_with(['"', '`', '[', '\'']) {
            let end = match d.as_bytes()[0] {
                b'[' => d.find(']'),
                q => d[1..].find(q as char).map(|i| i + 1),
            };
            end.map_or(d.to_string(), |e| d[..=e].to_string())
        } else {
            d.split_whitespace().next().unwrap_or("").to_string()
        };
        let upper = d.to_ascii_uppercase();
        if rowid.is_none()
            && upper.contains("INTEGER")
            && upper.contains("PRIMARY KEY")
            && !upper.contains("DESC")
        {
            rowid = Some(names.len());
        }
        names.push(unquote(&name));
    }
    (names, rowid)
}

pub fn read(
    data: &[u8],
    limits: &TableLimits,
    has_header: bool,
    notes: &mut Notes,
) -> Result<Workbook> {
    if !is_sqlite(data) || data.len() < 100 {
        return Err(corrupt("this is not a SQLite database"));
    }
    let ps_raw = be(data, 16, 2).unwrap_or(0) as usize;
    let ps = if ps_raw == 1 { 65536 } else { ps_raw };
    if !(512..=65536).contains(&ps) || !ps.is_power_of_two() {
        return Err(corrupt("the SQLite page size is not valid"));
    }
    let reserved = data[20] as usize;
    let enc = be(data, 56, 4).unwrap_or(1) as u8;
    let db = Db {
        d: data,
        ps,
        usable: ps - reserved,
        enc,
        pages: data.len() / ps,
    };
    if db.pages == 0 {
        return Err(corrupt("the database has no complete page"));
    }
    if data[18] == 2 || data[19] == 2 {
        notes.push(Note::new(
            Severity::Warning,
            "sqlite.wal",
            "the database uses a write-ahead log; changes not yet merged into the file are not seen",
        ));
    }
    let schema = db.rows(1, 1_000_000, notes);
    let mut book = Workbook::default();
    let mut cells: u64 = 0;
    let mut skipped = 0;
    for (_, rec) in schema {
        let (V::Text(kind), V::Text(name)) = (
            rec.first().cloned().unwrap_or(V::Null),
            rec.get(1).cloned().unwrap_or(V::Null),
        ) else {
            continue;
        };
        if kind != "table" || name.starts_with("sqlite_") {
            continue;
        }
        let root = match rec.get(3) {
            Some(V::Int(r)) if *r > 0 => *r as usize,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let sql = match rec.get(4) {
            Some(V::Text(s)) => s.clone(),
            _ => String::new(),
        };
        if sql.to_ascii_uppercase().contains("WITHOUT ROWID")
            || sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL")
        {
            skipped += 1;
            continue;
        }
        let (names, rowid_col) = columns(&sql);
        let rows = db.rows(root, limits.max_cells, notes);
        let width = rows
            .iter()
            .map(|r| r.1.len())
            .max()
            .unwrap_or(0)
            .max(names.len());
        let mut out_rows: Vec<Vec<String>> = Vec::with_capacity(rows.len());
        let mut blobs = false;
        for (rowid, rec) in rows {
            cells += width as u64;
            if cells > limits.max_cells {
                return Err(Error::limit(
                    "table",
                    "the database holds more cells than the allowed maximum",
                ));
            }
            let mut r: Vec<String> = rec
                .into_iter()
                .enumerate()
                .map(|(i, v)| match v {
                    V::Null if rowid_col == Some(i) => rowid.to_string(),
                    V::Null => String::new(),
                    V::Int(n) => n.to_string(),
                    V::Real(f) => format_number(f),
                    V::Text(t) => t,
                    V::Blob(b) => {
                        blobs = true;
                        let mut s = String::from("x'");
                        for x in b.iter().take(64) {
                            s.push_str(&format!("{x:02x}"));
                        }
                        if b.len() > 64 {
                            s.push_str("...");
                        }
                        s.push('\'');
                        s
                    }
                })
                .collect();
            r.resize(width, String::new());
            out_rows.push(r);
        }
        if blobs {
            notes.push(Note::new(
                Severity::Loss,
                "sqlite.blobs",
                "binary values were written as hexadecimal text (long ones cut at 64 bytes)",
            ));
        }
        let header: Vec<String> = (0..width)
            .map(|i| {
                names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("column{}", i + 1))
            })
            .collect();
        let table = if has_header {
            Table::new(Some(header), out_rows)
        } else {
            let mut all = vec![header];
            all.extend(out_rows);
            Table::new(None, all)
        };
        book.sheets.push(Sheet {
            name,
            table,
            ..Sheet::default()
        });
    }
    if skipped > 0 {
        notes.push(Note::new(
            Severity::Loss,
            "sqlite.tables_skipped",
            format!("{skipped} table(s) were skipped (WITHOUT ROWID or virtual tables)"),
        ));
    }
    if book.sheets.is_empty() {
        return Err(Error::new(
            ErrorCode::CorruptFile,
            "the database holds no readable tables",
        ));
    }
    Ok(book)
}

// ---- writing ----

const PS: usize = 4096;

fn put_varint(out: &mut Vec<u8>, v: u64) {
    if v >> 56 != 0 {
        // nine bytes: eight groups of 7 bits then 8 bits
        let mut b = [0u8; 9];
        b[8] = (v & 0xff) as u8;
        let mut x = v >> 8;
        for i in (0..8).rev() {
            b[i] = (x & 0x7f) as u8 | 0x80;
            x >>= 7;
        }
        out.extend_from_slice(&b);
        return;
    }
    let mut tmp = [0u8; 9];
    let mut n = 0;
    let mut x = v;
    loop {
        tmp[n] = (x & 0x7f) as u8;
        n += 1;
        x >>= 7;
        if x == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        out.push(tmp[i] | if i > 0 { 0x80 } else { 0 });
    }
}

fn record(vals: &[V]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut body = Vec::new();
    for v in vals {
        match v {
            V::Null => put_varint(&mut types, 0),
            V::Int(0) => put_varint(&mut types, 8),
            V::Int(1) => put_varint(&mut types, 9),
            V::Int(n) => {
                put_varint(&mut types, 6);
                body.extend_from_slice(&n.to_be_bytes());
            }
            V::Real(f) => {
                put_varint(&mut types, 7);
                body.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            V::Text(t) => {
                put_varint(&mut types, 13 + 2 * t.len() as u64);
                body.extend_from_slice(t.as_bytes());
            }
            V::Blob(b) => {
                put_varint(&mut types, 12 + 2 * b.len() as u64);
                body.extend_from_slice(b);
            }
        }
    }
    // the header size counts its own varint (always one byte here for small headers, else two)
    let mut hs = types.len() + 1;
    if hs > 127 {
        hs += 1;
        if hs > 16383 {
            hs += 1;
        }
    }
    let mut out = Vec::new();
    put_varint(&mut out, hs as u64);
    out.extend_from_slice(&types);
    out.extend_from_slice(&body);
    out
}

struct Writer {
    pages: BTreeMap<u32, Vec<u8>>,
    next: u32,
}

impl Writer {
    fn alloc(&mut self) -> u32 {
        let n = self.next;
        self.next += 1;
        n
    }

    /// One leaf cell: payload (with overflow pages when large).
    fn leaf_cell(&mut self, rowid: u64, payload: &[u8]) -> Vec<u8> {
        let u = PS;
        let x = u - 35;
        let mut cell = Vec::new();
        put_varint(&mut cell, payload.len() as u64);
        put_varint(&mut cell, rowid);
        if payload.len() <= x {
            cell.extend_from_slice(payload);
            return cell;
        }
        let m = (u - 12) * 32 / 255 - 23;
        let k = m + (payload.len() - m) % (u - 4);
        let local = if k <= x { k } else { m };
        cell.extend_from_slice(&payload[..local]);
        let mut rest = &payload[local..];
        let first = self.next;
        while !rest.is_empty() {
            let take = rest.len().min(u - 4);
            let no = self.alloc();
            let mut pg = vec![0u8; PS];
            let after = if rest.len() > take { no + 1 } else { 0 };
            pg[..4].copy_from_slice(&after.to_be_bytes());
            pg[4..4 + take].copy_from_slice(&rest[..take]);
            self.pages.insert(no, pg);
            rest = &rest[take..];
        }
        cell.extend_from_slice(&first.to_be_bytes());
        cell
    }

    fn page_of(&mut self, no: u32, kind: u8, cells: &[Vec<u8>], right: Option<u32>) {
        let h = if no == 1 { 100 } else { 0 };
        let mut pg = vec![0u8; PS];
        let hdr = if kind == 0x05 { 12 } else { 8 };
        pg[h] = kind;
        pg[h + 3..h + 5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
        let mut top = PS;
        for (i, c) in cells.iter().enumerate() {
            top -= c.len();
            pg[top..top + c.len()].copy_from_slice(c);
            pg[h + hdr + 2 * i..h + hdr + 2 * i + 2].copy_from_slice(&(top as u16).to_be_bytes());
        }
        let start = if top == 65536 { 0 } else { top };
        pg[h + 5..h + 7].copy_from_slice(&(start as u16).to_be_bytes());
        if let Some(r) = right {
            pg[h + 8..h + 12].copy_from_slice(&r.to_be_bytes());
        }
        self.pages.insert(no, pg);
    }

    /// Build a table B-tree from rows; returns the root page number.
    fn tree(&mut self, rows: Vec<(u64, Vec<u8>)>, forced_root: Option<u32>) -> u32 {
        let h = if forced_root == Some(1) { 100 } else { 0 };
        let room = PS - h - 12;
        // leaves
        let mut groups: Vec<(Vec<Vec<u8>>, u64)> = Vec::new();
        let mut cur: Vec<Vec<u8>> = Vec::new();
        let mut used = 0usize;
        let mut last = 0u64;
        for (rowid, payload) in rows {
            let cell = self.leaf_cell(rowid, &payload);
            if !cur.is_empty() && used + cell.len() + 2 > room {
                groups.push((std::mem::take(&mut cur), last));
                used = 0;
            }
            used += cell.len() + 2;
            cur.push(cell);
            last = rowid;
        }
        groups.push((cur, last));
        let top = groups.len() == 1;
        let mut level: Vec<(u32, u64)> = Vec::new();
        for (cells, max) in groups {
            let no = match (top, forced_root) {
                (true, Some(r)) => r,
                _ => self.alloc(),
            };
            self.page_of(no, 0x0d, &cells, None);
            level.push((no, max));
        }
        while level.len() > 1 {
            // interior pages: each holds cells (child, max rowid) and a right-most child
            let mut next_level = Vec::new();
            let per = (PS - 112) / 15;
            let chunks: Vec<&[(u32, u64)]> = level.chunks(per.max(2)).collect();
            let is_top = chunks.len() == 1;
            for ch in chunks {
                let no = match (is_top, forced_root) {
                    (true, Some(r)) => r,
                    _ => self.alloc(),
                };
                let (last_child, max) = ch[ch.len() - 1];
                let cells: Vec<Vec<u8>> = ch[..ch.len() - 1]
                    .iter()
                    .map(|(c, m)| {
                        let mut v = c.to_be_bytes().to_vec();
                        put_varint(&mut v, *m);
                        v
                    })
                    .collect();
                self.page_of(no, 0x05, &cells, Some(last_child));
                next_level.push((no, max));
            }
            level = next_level;
        }
        level[0].0
    }
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

pub fn write(book: &Workbook, detect_types: bool, notes: &mut Notes) -> Result<Vec<u8>> {
    let mut w = Writer {
        pages: BTreeMap::new(),
        next: 2,
    };
    let mut schema: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut used_names: Vec<String> = Vec::new();
    for (si, sheet) in book.sheets.iter().enumerate() {
        let mut name = if sheet.name.trim().is_empty() {
            format!("sheet{}", si + 1)
        } else {
            sheet.name.clone()
        };
        while used_names.iter().any(|n| n.eq_ignore_ascii_case(&name))
            || name.to_ascii_lowercase().starts_with("sqlite_")
        {
            name.push('_');
        }
        used_names.push(name.clone());
        let t = &sheet.table;
        let width = t.width().max(1);
        let mut cols: Vec<String> = Vec::new();
        for i in 0..width {
            let mut c = t
                .header
                .as_ref()
                .and_then(|h| h.get(i))
                .filter(|s| !s.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| format!("column{}", i + 1));
            while cols.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
                c.push('_');
            }
            cols.push(c);
        }
        // column types from the data
        let mut kinds = vec![0u8; width]; // 1 int, 2 real, 3 text
        if detect_types {
            for i in 0..width {
                let mut k = 1u8;
                let mut any = false;
                for r in &t.rows {
                    let v = r.get(i).map_or("", String::as_str);
                    if v.is_empty() {
                        continue;
                    }
                    any = true;
                    let int = v.parse::<i64>().is_ok()
                        && !(v.len() > 1 && v.starts_with('0'))
                        && !v.starts_with('+');
                    let real = v.parse::<f64>().is_ok_and(f64::is_finite)
                        && !(v.len() > 1 && v.starts_with('0') && !v.starts_with("0."))
                        && !v.starts_with('+');
                    if int {
                        continue;
                    }
                    if real {
                        k = k.max(2);
                    } else {
                        k = 3;
                        break;
                    }
                }
                kinds[i] = if any { k } else { 3 };
            }
        } else {
            kinds.fill(3);
        }
        let decl = |k: u8| match k {
            1 => "INTEGER",
            2 => "REAL",
            _ => "TEXT",
        };
        let sql = format!(
            "CREATE TABLE {} ({})",
            ident(&name),
            cols.iter()
                .zip(&kinds)
                .map(|(c, k)| format!("{} {}", ident(c), decl(*k)))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut rows = Vec::with_capacity(t.rows.len());
        for (ri, r) in t.rows.iter().enumerate() {
            let vals: Vec<V> = (0..width)
                .map(|i| {
                    let v = r.get(i).map_or("", String::as_str);
                    if v.is_empty() && kinds[i] != 3 {
                        return V::Null;
                    }
                    match kinds[i] {
                        1 => v.parse().map_or(V::Text(v.to_string()), V::Int),
                        2 => v.parse().map_or(V::Text(v.to_string()), V::Real),
                        _ => V::Text(v.to_string()),
                    }
                })
                .collect();
            rows.push((ri as u64 + 1, record(&vals)));
        }
        let root = if rows.is_empty() {
            let no = w.alloc();
            w.page_of(no, 0x0d, &[], None);
            no
        } else {
            w.tree(rows, None)
        };
        let rec = record(&[
            V::Text("table".into()),
            V::Text(name.clone()),
            V::Text(name),
            V::Int(i64::from(root)),
            V::Text(sql),
        ]);
        schema.push((si as u64 + 1, rec));
    }
    let _ = w.tree(schema, Some(1));
    let count = w.next - 1;
    let mut out = Vec::with_capacity(count as usize * PS);
    for n in 1..=count {
        match w.pages.remove(&n) {
            Some(p) => out.extend_from_slice(&p),
            None => out.extend_from_slice(&[0u8; PS]),
        }
    }
    // the file header on page 1
    let mut h = [0u8; 100];
    h[..16].copy_from_slice(b"SQLite format 3\0");
    h[16..18].copy_from_slice(&(PS as u16).to_be_bytes());
    h[18] = 1;
    h[19] = 1;
    h[21] = 64;
    h[22] = 32;
    h[23] = 32;
    h[24..28].copy_from_slice(&1u32.to_be_bytes());
    h[28..32].copy_from_slice(&count.to_be_bytes());
    h[40..44].copy_from_slice(&1u32.to_be_bytes());
    h[44..48].copy_from_slice(&4u32.to_be_bytes());
    h[56..60].copy_from_slice(&1u32.to_be_bytes());
    h[92..96].copy_from_slice(&1u32.to_be_bytes());
    h[96..100].copy_from_slice(&3_045_000u32.to_be_bytes());
    out[..100].copy_from_slice(&h);
    if book.sheets.iter().any(|s| s.table.header.is_none()) {
        notes.push(Note::new(
            Severity::Info,
            "sqlite.column_names",
            "columns without a header were named column1, column2, ...",
        ));
    }
    notes.push(Note::new(
        Severity::Info,
        "sqlite.types",
        "columns were typed INTEGER, REAL or TEXT from their values; empty numeric cells became NULL",
    ));
    Ok(out)
}
