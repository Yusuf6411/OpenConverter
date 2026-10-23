//! The file structure: header, cross-reference tables and streams, object streams, recovery
//! by scanning, and the page tree.
use crate::crypt::Crypt;
use crate::object::{is_delim, is_ws, Dict, Lexer, Obj, Stream, Token};
use oc_core::{Error, Result};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The permission flags of the standard security handler that conversions care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Print,
    Modify,
    /// Copying text and graphics (also what text extraction needs).
    Copy,
    Annotate,
    /// Inserting, rotating and deleting pages.
    Assemble,
}

#[derive(Debug, Clone, Copy)]
pub enum Entry {
    Offset(usize),
    InStream(u32, u32),
}

pub struct Pdf {
    pub data: Vec<u8>,
    pub xref: HashMap<u32, Entry>,
    pub trailer: Dict,
    pub crypt: Option<Crypt>,
    /// True when the cross-reference data was damaged and rebuilt by scanning.
    pub recovered: bool,
    cache: RefCell<HashMap<u32, Obj>>,
    objstm: RefCell<HashMap<u32, Arc<Vec<(u32, Obj)>>>>,
    busy: RefCell<HashSet<u32>>,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct PageInfo {
    pub num: u32,
    pub dict: Dict,
}

fn find_last(data: &[u8], needle: &[u8]) -> Option<usize> {
    if data.len() < needle.len() {
        return None;
    }
    (0..=data.len() - needle.len()).rev().find(|&i| data.get(i..i + needle.len()) == Some(needle))
}

fn find_from(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= data.len() || data.len() - from < needle.len() {
        return None;
    }
    (from..=data.len() - needle.len()).find(|&i| data.get(i..i + needle.len()) == Some(needle))
}

impl Pdf {
    pub fn open(mut data: Vec<u8>) -> Result<Pdf> {
        let head = data.get(..data.len().min(2048)).unwrap_or_default();
        let hdr = find_from(head, b"%PDF-", 0).ok_or_else(|| Error::corrupt("not a PDF file (no %PDF- header)"))?;
        let version = String::from_utf8_lossy(data.get(hdr + 5..(hdr + 8).min(data.len())).unwrap_or_default()).trim().to_string();
        if hdr > 0 {
            data.drain(..hdr);
        }
        let mut pdf = Pdf {
            data,
            xref: HashMap::new(),
            trailer: Dict::new(),
            crypt: None,
            recovered: false,
            cache: RefCell::new(HashMap::new()),
            objstm: RefCell::new(HashMap::new()),
            busy: RefCell::new(HashSet::new()),
            version,
        };
        let ok = pdf.read_xref_chain().is_ok() && pdf.catalog_dict().is_some();
        if !ok {
            pdf.rebuild();
        }
        if pdf.catalog_dict().is_none() {
            return Err(Error::corrupt("the PDF has no document catalog"));
        }
        pdf.setup_encryption()?;
        Ok(pdf)
    }

    fn setup_encryption(&mut self) -> Result<()> {
        let Some(enc) = self.trailer.get("Encrypt").cloned() else { return Ok(()) };
        let enc_num = if let Obj::Ref(n, _) = enc { Some(n) } else { None };
        let edict = self.resolve(&enc);
        let Some(d) = edict.as_dict().cloned() else { return Ok(()) };
        let id0 = self
            .trailer
            .get("ID")
            .map(|o| self.resolve(o))
            .and_then(|o| o.as_array().and_then(|a| a.first().cloned()))
            .map(|o| self.resolve(&o))
            .and_then(|o| o.as_str().map(<[u8]>::to_vec))
            .unwrap_or_default();
        let c = Crypt::new(self, &d, &id0, enc_num)?;
        self.crypt = Some(c);
        // objects parsed before the key was known (strings) must be read again
        self.cache.borrow_mut().clear();
        self.objstm.borrow_mut().clear();
        Ok(())
    }

    /// Try a password; true when it opens the document. Clears caches so strings are re-read.
    pub fn authenticate(&mut self, password: &[u8]) -> bool {
        let ok = match self.crypt.as_mut() {
            Some(c) => c.authenticate(password),
            None => true,
        };
        self.cache.borrow_mut().clear();
        self.objstm.borrow_mut().clear();
        ok
    }

    /// What the document's permission flags allow the current credentials to do; everything when
    /// there is no encryption or the owner password was used.
    pub fn permission(&self, p: Permission) -> bool {
        match &self.crypt {
            Some(c) if !c.owner => {
                let bit = match p {
                    Permission::Print => 3,
                    Permission::Modify => 4,
                    Permission::Copy => 5,
                    Permission::Annotate => 6,
                    Permission::Assemble => 11,
                };
                c.permissions() & (1 << (bit - 1)) != 0
            }
            _ => true,
        }
    }

    pub fn is_encrypted(&self) -> bool {
        self.crypt.is_some()
    }
    pub fn needs_password(&self) -> bool {
        self.crypt.as_ref().is_some_and(|c| !c.authenticated)
    }

    // ---- xref ----

    fn read_xref_chain(&mut self) -> Result<()> {
        let tail_start = self.data.len().saturating_sub(2048);
        let tail = self.data.get(tail_start..).unwrap_or_default();
        let sx = find_last(tail, b"startxref").ok_or_else(|| Error::corrupt("no startxref"))?;
        let mut lx = Lexer::new(tail, sx + 9);
        let Token::Obj(Obj::Int(off)) = lx.next() else {
            return Err(Error::corrupt("bad startxref"));
        };
        let mut next = Some(off.max(0) as usize);
        let mut seen = HashSet::new();
        let mut first = true;
        while let Some(off) = next {
            if !seen.insert(off) || seen.len() > 256 {
                break;
            }
            let trailer = self.read_xref_section(off)?;
            if first {
                self.trailer = trailer.clone();
                first = false;
            } else {
                for (k, v) in &trailer {
                    self.trailer.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            if let Some(Obj::Int(x)) = trailer.get("XRefStm") {
                let _ = self.read_xref_section(*x as usize);
            }
            next = trailer.get("Prev").and_then(Obj::as_i64).map(|p| p.max(0) as usize);
        }
        if first {
            return Err(Error::corrupt("no cross-reference data"));
        }
        Ok(())
    }

    fn read_xref_section(&mut self, off: usize) -> Result<Dict> {
        if off >= self.data.len() {
            return Err(Error::corrupt("xref offset past the end"));
        }
        let mut lx = Lexer::new(&self.data, off);
        lx.skip_ws();
        let save = lx.pos;
        if let Token::Keyword(k) = lx.next() {
            if k == b"xref" {
                return self.read_xref_table(lx.pos);
            }
        }
        // an xref stream
        let (_, _, obj) = self.parse_indirect_at(save, true).ok_or_else(|| Error::corrupt("bad xref stream"))?;
        let Obj::Stream(s) = obj else { return Err(Error::corrupt("xref is not a stream")) };
        let data = crate::filter::decode_stream(self, &s, false)?.0;
        let dict = s.dict.clone();
        let w: Vec<usize> = dict
            .get("W")
            .and_then(Obj::as_array)
            .map(|a| a.iter().map(|o| o.as_i64().unwrap_or(0).clamp(0, 8) as usize).collect())
            .unwrap_or_default();
        if w.len() < 3 {
            return Err(Error::corrupt("bad xref stream /W"));
        }
        let size = dict.get("Size").and_then(Obj::as_i64).unwrap_or(0);
        let index: Vec<i64> = dict
            .get("Index")
            .and_then(Obj::as_array)
            .map(|a| a.iter().filter_map(Obj::as_i64).collect())
            .unwrap_or_else(|| vec![0, size]);
        let rec = w[0] + w[1] + w[2];
        if rec == 0 {
            return Err(Error::corrupt("empty xref records"));
        }
        let mut pos = 0usize;
        for pair in index.chunks(2) {
            let (start, count) = (pair[0], pair.get(1).copied().unwrap_or(0));
            for k in 0..count.max(0) {
                let Some(r) = data.get(pos..pos + rec) else { break };
                pos += rec;
                let field = |a: usize, b: usize, default: u64| -> u64 {
                    if b == a {
                        return default;
                    }
                    r[a..b].iter().fold(0u64, |acc, x| (acc << 8) | u64::from(*x))
                };
                let t = field(0, w[0], 1);
                let f2 = field(w[0], w[0] + w[1], 0);
                let f3 = field(w[0] + w[1], rec, 0);
                let num = (start + k) as u32;
                match t {
                    1 => {
                        self.xref.entry(num).or_insert(Entry::Offset(f2 as usize));
                    }
                    2 => {
                        self.xref.entry(num).or_insert(Entry::InStream(f2 as u32, f3 as u32));
                    }
                    _ => {}
                }
            }
        }
        Ok(dict)
    }

    fn read_xref_table(&mut self, pos: usize) -> Result<Dict> {
        let mut lx = Lexer::new(&self.data, pos);
        loop {
            match lx.next() {
                Token::Obj(Obj::Int(start)) => {
                    let Token::Obj(Obj::Int(count)) = lx.next() else {
                        return Err(Error::corrupt("bad xref subsection"));
                    };
                    for k in 0..count.max(0) {
                        let a = lx.next();
                        let b = lx.next();
                        let c = lx.next();
                        if let (Token::Obj(Obj::Int(off)), Token::Obj(Obj::Int(_gen)), Token::Keyword(t)) = (a, b, c) {
                            if t == b"n" && off > 0 {
                                self.xref.entry((start + k) as u32).or_insert(Entry::Offset(off as usize));
                            }
                        } else {
                            return Err(Error::corrupt("bad xref entry"));
                        }
                    }
                }
                Token::Keyword(k) if k == b"trailer" => {
                    return match lx.object() {
                        Some(Obj::Dict(d)) => Ok(d),
                        _ => Err(Error::corrupt("bad trailer")),
                    };
                }
                _ => {
                    return Err(Error::corrupt("bad xref table"));
                }
            }
        }
    }

    /// Rebuild the cross-reference by scanning the file for `n g obj`.
    fn rebuild(&mut self) {
        self.recovered = true;
        self.xref.clear();
        self.cache.borrow_mut().clear();
        self.objstm.borrow_mut().clear();
        let mut trailers: Vec<Dict> = Vec::new();
        let mut objstms: Vec<u32> = Vec::new();
        let data = std::mem::take(&mut self.data);
        let n = data.len();
        let mut i = 0usize;
        while i < n {
            let Some(p) = find_from(&data, b"obj", i) else { break };
            i = p + 3;
            if p >= 3 && data.get(p - 3..p) == Some(b"end") {
                continue;
            }
            if data.get(p + 3).is_some_and(|b| !(is_ws(*b) || is_delim(*b))) {
                continue;
            }
            // walk back over "gen" and "num"
            let mut q = p;
            while q > 0 && data.get(q - 1).is_some_and(|b| is_ws(*b)) {
                q -= 1;
            }
            let ge = q;
            while q > 0 && data.get(q - 1).is_some_and(u8::is_ascii_digit) {
                q -= 1;
            }
            let gs = q;
            if gs == ge {
                continue;
            }
            while q > 0 && data.get(q - 1).is_some_and(|b| is_ws(*b)) {
                q -= 1;
            }
            let ne = q;
            while q > 0 && data.get(q - 1).is_some_and(u8::is_ascii_digit) {
                q -= 1;
            }
            let ns = q;
            if ns == ne {
                continue;
            }
            let num: u32 = std::str::from_utf8(data.get(ns..ne).unwrap_or_default()).ok().and_then(|s| s.parse().ok()).unwrap_or(u32::MAX);
            if num == u32::MAX {
                continue;
            }
            self.xref.insert(num, Entry::Offset(ns));
            // peek at the dictionary for /Type /ObjStm or /XRef
            let mut lx = Lexer::new(&data, p + 3);
            if let Some(Obj::Dict(d)) = lx.object() {
                match d.get("Type").and_then(Obj::as_name) {
                    Some("ObjStm") => objstms.push(num),
                    Some("XRef") => trailers.push(d),
                    _ => {}
                }
            }
        }
        let mut j = 0usize;
        while let Some(p) = find_from(&data, b"trailer", j) {
            j = p + 7;
            let mut lx = Lexer::new(&data, j);
            if let Some(Obj::Dict(d)) = lx.object() {
                trailers.push(d);
            }
        }
        self.data = data;
        // trailers later in the file win
        let mut merged = Dict::new();
        for t in trailers {
            for (k, v) in t {
                if matches!(k.as_str(), "Root" | "Info" | "ID" | "Encrypt" | "Size") {
                    merged.insert(k, v);
                }
            }
        }
        self.trailer = merged;
        // objects inside object streams
        for stm in objstms {
            if let Some(list) = self.load_objstm(stm) {
                for (idx, (num, _)) in list.iter().enumerate() {
                    self.xref.entry(*num).or_insert(Entry::InStream(stm, idx as u32));
                }
            }
        }
        if self.catalog_dict().is_none() {
            // find a catalog by scanning every object
            let nums: Vec<u32> = self.xref.keys().copied().collect();
            for num in nums {
                if let Obj::Dict(d) = self.get(num) {
                    if d.get("Type").and_then(Obj::as_name) == Some("Catalog") {
                        self.trailer.insert("Root".into(), Obj::Ref(num, 0));
                    }
                }
            }
        }
    }

    // ---- objects ----

    /// Parse `n g obj ... endobj` at `off`; `skip_decrypt` leaves strings as they are.
    pub fn parse_indirect_at(&self, off: usize, skip_decrypt: bool) -> Option<(u32, u16, Obj)> {
        let mut lx = Lexer::new(&self.data, off);
        let Token::Obj(Obj::Int(num)) = lx.next() else { return None };
        let Token::Obj(Obj::Int(gen)) = lx.next() else { return None };
        let Token::Keyword(k) = lx.next() else { return None };
        if k != b"obj" {
            return None;
        }
        let (num, gen) = (num as u32, gen.clamp(0, 65535) as u16);
        let mut obj = lx.object()?;
        if let Obj::Dict(d) = &obj {
            if let Token::Keyword(k) = lx.next() {
                if k == b"stream" {
                    let mut p = lx.pos;
                    if self.data.get(p) == Some(&b'\r') {
                        p += 1;
                    }
                    if self.data.get(p) == Some(&b'\n') {
                        p += 1;
                    }
                    let len = d.get("Length").map(|l| self.resolve_busy(l, num)).and_then(|l| l.as_i64());
                    let mut end = None;
                    if let Some(l) = len.filter(|l| *l >= 0) {
                        let e = p + l as usize;
                        if e <= self.data.len() {
                            let mut t = Lexer::new(&self.data, e);
                            if matches!(t.next(), Token::Keyword(k) if k == b"endstream") {
                                end = Some(e);
                            }
                        }
                    }
                    let end = end.unwrap_or_else(|| {
                        let e = find_from(&self.data, b"endstream", p).unwrap_or(self.data.len());
                        // drop the EOL before endstream
                        let mut e2 = e;
                        if e2 > p && self.data.get(e2 - 1) == Some(&b'\n') {
                            e2 -= 1;
                        }
                        if e2 > p && self.data.get(e2 - 1) == Some(&b'\r') {
                            e2 -= 1;
                        }
                        e2
                    });
                    let raw = self.data.get(p..end).unwrap_or_default().to_vec();
                    obj = Obj::Stream(Arc::new(Stream { dict: d.clone(), raw, id: (num, gen) }));
                }
            }
        }
        if !skip_decrypt {
            if let Some(c) = &self.crypt {
                if Some(num) != c.encrypt_obj {
                    obj = c.decrypt_strings(obj, num, gen);
                }
            }
        }
        Some((num, gen, obj))
    }

    /// Resolve a reference found while parsing object `owner` (a /Length).
    fn resolve_busy(&self, o: &Obj, owner: u32) -> Obj {
        match o {
            Obj::Ref(n, _) if *n != owner => self.get(*n),
            Obj::Ref(..) => Obj::Null,
            o => o.clone(),
        }
    }

    pub fn load_objstm(&self, stm: u32) -> Option<Arc<Vec<(u32, Obj)>>> {
        if let Some(v) = self.objstm.borrow().get(&stm) {
            return Some(v.clone());
        }
        let Obj::Stream(s) = self.get(stm) else { return None };
        let data = crate::filter::decode_stream(self, &s, false).ok()?.0;
        let n = s.dict.get("N").map(|o| self.resolve(o)).and_then(|o| o.as_i64())?.clamp(0, 1_000_000) as usize;
        let first = s.dict.get("First").map(|o| self.resolve(o)).and_then(|o| o.as_i64())?.max(0) as usize;
        let mut lx = Lexer::new(&data, 0);
        let mut pairs = Vec::new();
        for _ in 0..n {
            let (Token::Obj(Obj::Int(num)), Token::Obj(Obj::Int(off))) = (lx.next(), lx.next()) else { break };
            pairs.push((num as u32, off.max(0) as usize));
        }
        let mut out = Vec::with_capacity(pairs.len());
        for (num, off) in pairs {
            let mut l2 = Lexer::new(&data, first + off);
            let obj = l2.object().unwrap_or(Obj::Null);
            out.push((num, obj));
        }
        let rc = Arc::new(out);
        self.objstm.borrow_mut().insert(stm, rc.clone());
        Some(rc)
    }

    /// The object with this number, or Null.
    pub fn get(&self, num: u32) -> Obj {
        if let Some(o) = self.cache.borrow().get(&num) {
            return o.clone();
        }
        if !self.busy.borrow_mut().insert(num) {
            return Obj::Null;
        }
        let obj = match self.xref.get(&num).copied() {
            Some(Entry::Offset(off)) => match self.parse_indirect_at(off, false) {
                Some((n, _, o)) if n == num => o,
                _ => Obj::Null,
            },
            Some(Entry::InStream(stm, idx)) => self
                .load_objstm(stm)
                .and_then(|l| {
                    l.get(idx as usize)
                        .filter(|(n, _)| *n == num)
                        .or_else(|| l.iter().find(|(n, _)| *n == num))
                        .map(|(_, o)| o.clone())
                })
                .unwrap_or(Obj::Null),
            None => Obj::Null,
        };
        self.busy.borrow_mut().remove(&num);
        self.cache.borrow_mut().insert(num, obj.clone());
        obj
    }

    /// Follow references until a direct object.
    pub fn resolve(&self, o: &Obj) -> Obj {
        let mut cur = o.clone();
        for _ in 0..32 {
            match cur {
                Obj::Ref(n, _) => cur = self.get(n),
                other => return other,
            }
        }
        Obj::Null
    }

    pub fn dget(&self, d: &Dict, key: &str) -> Obj {
        d.get(key).map_or(Obj::Null, |o| self.resolve(o))
    }
    pub fn dget_i64(&self, d: &Dict, key: &str) -> Option<i64> {
        self.dget(d, key).as_i64()
    }
    pub fn dget_f64(&self, d: &Dict, key: &str) -> Option<f64> {
        self.dget(d, key).as_f64()
    }
    pub fn dget_name(&self, d: &Dict, key: &str) -> Option<String> {
        self.dget(d, key).as_name().map(str::to_string)
    }
    pub fn dget_dict(&self, d: &Dict, key: &str) -> Option<Dict> {
        self.dget(d, key).as_dict().cloned()
    }
    pub fn dget_array(&self, d: &Dict, key: &str) -> Option<Vec<Obj>> {
        self.dget(d, key).as_array().map(<[Obj]>::to_vec)
    }
    pub fn dget_numbers(&self, d: &Dict, key: &str) -> Option<Vec<f64>> {
        self.dget_array(d, key).map(|a| a.iter().filter_map(|o| self.resolve(o).as_f64()).collect())
    }

    pub fn catalog_dict(&self) -> Option<Dict> {
        let root = self.trailer.get("Root")?;
        let d = self.resolve(root);
        let d = d.as_dict()?.clone();
        Some(d)
    }

    pub fn info_dict(&self) -> Dict {
        self.trailer
            .get("Info")
            .map(|o| self.resolve(o))
            .and_then(|o| o.as_dict().cloned())
            .unwrap_or_default()
    }

    // ---- pages ----

    pub fn pages(&self) -> Vec<PageInfo> {
        let mut out = Vec::new();
        let Some(cat) = self.catalog_dict() else { return out };
        let mut seen = HashSet::new();
        if let Some(Obj::Ref(root, _)) = cat.get("Pages") {
            self.walk_pages(*root, &Dict::new(), &mut out, &mut seen, 0);
        }
        if out.is_empty() {
            // a broken tree: take every object that says it is a page
            let mut nums: Vec<u32> = self.xref.keys().copied().collect();
            nums.sort_unstable();
            for n in nums {
                if let Obj::Dict(d) = self.get(n) {
                    if d.get("Type").and_then(Obj::as_name) == Some("Page") {
                        out.push(PageInfo { num: n, dict: d });
                    }
                }
            }
        }
        out
    }

    fn walk_pages(&self, num: u32, inherited: &Dict, out: &mut Vec<PageInfo>, seen: &mut HashSet<u32>, depth: u32) {
        if depth > 64 || !seen.insert(num) || out.len() > 200_000 {
            return;
        }
        let Obj::Dict(d) = self.get(num) else { return };
        let mut inh = inherited.clone();
        for k in ["Resources", "MediaBox", "CropBox", "Rotate"] {
            if let Some(v) = d.get(k) {
                inh.insert(k.to_string(), v.clone());
            }
        }
        let kids = d.get("Kids").map(|k| self.resolve(k));
        let is_pages = d.get("Type").and_then(Obj::as_name) == Some("Pages") || (kids.is_some() && d.get("Type").is_none());
        if is_pages {
            if let Some(Obj::Array(kids)) = kids {
                for k in kids {
                    if let Obj::Ref(n, _) = k {
                        self.walk_pages(n, &inh, out, seen, depth + 1);
                    }
                }
            }
        } else {
            let mut page = d.clone();
            for (k, v) in inh {
                page.entry(k).or_insert(v);
            }
            out.push(PageInfo { num, dict: page });
        }
    }
}
