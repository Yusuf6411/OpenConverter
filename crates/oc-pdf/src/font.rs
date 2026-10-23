//! Fonts as PDF describes them: encodings, widths, glyph lookup, and the font program (embedded
//! TrueType, CFF or OpenType, or an installed font standing in for a missing one).
use crate::cmap::CMap;
use crate::doc::Pdf;
use crate::object::{Dict, Obj, Stream};
use crate::tables;
use oc_font::db::FontDb;
use oc_font::Font;
use oc_raster::Path;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Simple,
    Composite,
    Type3,
}

pub struct Type3 {
    pub font_matrix: [f64; 6],
    pub procs: HashMap<String, Arc<Stream>>,
    pub resources: Option<Dict>,
    pub names: Vec<Option<String>>,
}

/// One character drawn: how far to move, what to draw, what it means.
#[derive(Debug, Clone)]
pub struct CharInfo {
    pub code: u32,
    pub nbytes: usize,
    /// Advance in thousandths of text space.
    pub width: f64,
    pub gid: Option<u16>,
    pub unicode: String,
    pub is_space: bool,
    /// Glyph name for Type 3 lookups.
    pub name: Option<String>,
    /// Vertical writing: advance and origin shift (thousandths).
    pub vertical: Option<(f64, f64, f64)>,
}

pub struct PdfFont {
    pub kind: Kind,
    pub name: String,
    pub program: Option<Arc<Font>>,
    pub embedded: bool,
    pub substituted: bool,
    pub bold_hint: bool,
    pub italic_hint: bool,
    pub type3: Option<Type3>,
    widths: Vec<f64>,
    first_char: i64,
    missing_width: f64,
    /// Per code (simple fonts): glyph id and Unicode.
    gids: Vec<Option<u16>>,
    names: Vec<Option<String>>,
    unis: Vec<String>,
    // composite
    cmap: Option<CMap>,
    cid_widths: HashMap<u32, f64>,
    dw: f64,
    cid_to_gid: Option<Vec<u16>>,
    cff_cid: bool,
    cid_gid_map: HashMap<u16, u16>,
    to_unicode: Option<CMap>,
    gid_unicode: HashMap<u16, char>,
    vertical: bool,
    dw2: (f64, f64),
    w2: HashMap<u32, (f64, f64, f64)>,
    cache: RefCell<HashMap<u16, Option<Arc<Path>>>>,
    pub unit_scale: f64,
}

/// A glyph name to a Unicode string.
pub fn name_to_unicode(name: &str) -> Option<String> {
    if let Ok(i) = tables::AGL.binary_search_by(|(n, _)| (*n).cmp(name)) {
        return char::from_u32(tables::AGL[i].1).map(String::from);
    }
    let base = name.split('.').next().unwrap_or(name);
    if base != name && !base.is_empty() {
        return name_to_unicode(base);
    }
    if let Some(h) = name.strip_prefix("uni") {
        if h.len() >= 4 && h.len() % 4 == 0 {
            let s: Option<String> = (0..h.len() / 4)
                .map(|i| u32::from_str_radix(h.get(i * 4..i * 4 + 4)?, 16).ok().and_then(char::from_u32))
                .collect();
            return s;
        }
    }
    if let Some(h) = name.strip_prefix('u') {
        if (4..=6).contains(&h.len()) {
            return u32::from_str_radix(h, 16).ok().and_then(char::from_u32).map(String::from);
        }
    }
    if name.contains('_') {
        let s: Option<String> = name.split('_').map(name_to_unicode).collect();
        return s;
    }
    None
}

fn base_encoding(name: &str) -> Option<&'static [&'static str; 256]> {
    match name {
        "WinAnsiEncoding" => Some(&tables::WIN_ANSI),
        "MacRomanEncoding" => Some(&tables::MAC_ROMAN),
        "StandardEncoding" | "MacExpertEncoding" => Some(&tables::STANDARD),
        _ => None,
    }
}

fn strip_subset(name: &str) -> &str {
    if name.len() > 7 && name.as_bytes().get(6) == Some(&b'+') && name[..6].bytes().all(|b| b.is_ascii_uppercase()) {
        &name[7..]
    } else {
        name
    }
}

fn families_for(base: &str, flags: i64) -> Vec<String> {
    let n = base.to_lowercase().replace([' ', '_'], "");
    let mut f: Vec<String> = Vec::new();
    let fam = n.split(['-', ',']).next().unwrap_or(&n).to_string();
    f.push(fam.clone());
    let push = |f: &mut Vec<String>, xs: &[&str]| f.extend(xs.iter().map(|s| (*s).to_string()));
    if fam.contains("arial") || fam.contains("helvetica") || fam.contains("calibri") || fam.contains("verdana") || fam.contains("tahoma") {
        push(&mut f, &["arial", "helvetica", "liberation sans", "dejavu sans"]);
    } else if fam.contains("times") || fam.contains("georgia") || fam.contains("palatino") || fam.contains("garamond") || fam.contains("cambria") || fam.contains("bookman") {
        push(&mut f, &["times new roman", "times", "liberation serif", "dejavu serif"]);
    } else if fam.contains("courier") || fam.contains("consolas") || fam.contains("mono") {
        push(&mut f, &["courier new", "courier", "liberation mono", "dejavu sans mono"]);
    } else if fam == "symbol" {
        push(&mut f, &["symbol", "standard symbols ps", "dejavu sans"]);
    } else if fam.contains("zapf") || fam.contains("dingbats") {
        push(&mut f, &["zapfdingbats", "dingbats", "dejavu sans"]);
    } else if flags & 1 != 0 {
        push(&mut f, &["liberation mono", "dejavu sans mono"]);
    } else if flags & 2 != 0 {
        push(&mut f, &["liberation serif", "dejavu serif"]);
    } else {
        push(&mut f, &["liberation sans", "dejavu sans"]);
    }
    f
}

/// Wrap a bare CFF table in a minimal OpenType file so `oc_font` can read it.
fn wrap_cff(cff: &[u8]) -> Option<Font> {
    let c = oc_font::cff::Cff::parse(cff)?;
    let upem = if c.font_matrix[0].abs() > 1e-9 { (1.0 / c.font_matrix[0].abs()).round().clamp(16.0, 65535.0) as u16 } else { 1000 };
    let n = c.num_glyphs().min(65535) as u16;
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&[0, 1, 0, 0]);
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&upem.to_be_bytes());
    let mut maxp = vec![0u8; 6];
    maxp[0..4].copy_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&n.to_be_bytes());
    let hhea = vec![0u8; 36];
    let mut tables = vec![(*b"CFF ", cff.to_vec()), (*b"head", head), (*b"hhea", hhea), (*b"maxp", maxp), (*b"hmtx", vec![0u8; 4])];
    let bytes = oc_font::build::build_sfnt(*b"OTTO", &mut tables);
    Font::parse(&bytes).ok()
}

fn load_program(pdf: &Pdf, desc: &Dict) -> Option<Font> {
    for key in ["FontFile2", "FontFile3", "FontFile"] {
        let Obj::Stream(s) = pdf.dget(desc, key) else { continue };
        let Ok((data, _)) = crate::filter::decode_stream(pdf, &s, false) else { continue };
        if key == "FontFile" {
            continue; // Type 1: not read
        }
        if oc_font::sfnt::is_sfnt(&data) {
            if let Ok(f) = Font::parse(&data) {
                return Some(f);
            }
        }
        if data.first() == Some(&1) && data.get(1) == Some(&0) {
            if let Some(f) = wrap_cff(&data) {
                return Some(f);
            }
        }
        if oc_font::build::is_woff(&data) {
            continue;
        }
    }
    None
}

impl PdfFont {
    pub fn load(pdf: &Pdf, fd: &Dict, fonts: Option<&FontDb>, notes: &mut Vec<String>) -> PdfFont {
        let subtype = pdf.dget_name(fd, "Subtype").unwrap_or_default();
        let kind = match subtype.as_str() {
            "Type0" => Kind::Composite,
            "Type3" => Kind::Type3,
            _ => Kind::Simple,
        };
        let base_name = pdf.dget_name(fd, "BaseFont").unwrap_or_default();
        let name = strip_subset(&base_name).to_string();
        let to_unicode = match pdf.dget(fd, "ToUnicode") {
            Obj::Stream(s) => crate::filter::decode_stream(pdf, &s, false).ok().map(|d| CMap::parse(&d.0)).filter(CMap::has_unicode),
            _ => None,
        };
        let mut f = PdfFont {
            kind,
            name: name.clone(),
            program: None,
            embedded: false,
            substituted: false,
            bold_hint: false,
            italic_hint: false,
            type3: None,
            widths: Vec::new(),
            first_char: 0,
            missing_width: 0.0,
            gids: vec![None; 256],
            names: vec![None; 256],
            unis: vec![String::new(); 256],
            cmap: None,
            cid_widths: HashMap::new(),
            dw: 1000.0,
            cid_to_gid: None,
            cff_cid: false,
            cid_gid_map: HashMap::new(),
            to_unicode,
            gid_unicode: HashMap::new(),
            vertical: false,
            dw2: (880.0, -1000.0),
            w2: HashMap::new(),
            cache: RefCell::new(HashMap::new()),
            unit_scale: 1.0 / 1000.0,
        };
        match kind {
            Kind::Composite => f.load_composite(pdf, fd, fonts, notes),
            Kind::Type3 => f.load_type3(pdf, fd),
            Kind::Simple => f.load_simple(pdf, fd, fonts, notes),
        }
        f
    }

    fn descriptor_info(&mut self, pdf: &Pdf, desc: &Dict) -> i64 {
        let flags = pdf.dget_i64(desc, "Flags").unwrap_or(0);
        self.missing_width = pdf.dget_f64(desc, "MissingWidth").unwrap_or(0.0);
        let lname = self.name.to_lowercase();
        self.bold_hint = lname.contains("bold") || lname.contains("black") || lname.contains("heavy") || flags & (1 << 18) != 0 || pdf.dget_f64(desc, "FontWeight").is_some_and(|w| w >= 700.0);
        self.italic_hint = lname.contains("italic") || lname.contains("oblique") || flags & (1 << 6) != 0 || pdf.dget_f64(desc, "ItalicAngle").is_some_and(|a| a != 0.0);
        flags
    }

    fn substitute(&mut self, fonts: Option<&FontDb>, flags: i64, notes: &mut Vec<String>) {
        let Some(db) = fonts else { return };
        let families = families_for(&self.name, flags);
        let weight = if self.bold_hint { 700 } else { 400 };
        if let Some(found) = db.find(&families, weight, self.italic_hint) {
            self.unit_scale = 1.0 / f64::from(found.font.units_per_em);
            self.program = Some(found.font);
            self.substituted = true;
            let msg = format!("font {} is not embedded; {} was used instead", self.name, found.family);
            if !notes.contains(&msg) {
                notes.push(msg);
            }
        }
    }

    fn load_simple(&mut self, pdf: &Pdf, fd: &Dict, fonts: Option<&FontDb>, notes: &mut Vec<String>) {
        let desc = pdf.dget_dict(fd, "FontDescriptor").unwrap_or_default();
        let flags = self.descriptor_info(pdf, &desc);
        let symbolic = flags & 4 != 0 && flags & 32 == 0;
        self.first_char = pdf.dget_i64(fd, "FirstChar").unwrap_or(0);
        self.widths = pdf.dget_numbers(fd, "Widths").unwrap_or_default();
        if let Some(p) = load_program(pdf, &desc) {
            self.unit_scale = 1.0 / f64::from(p.units_per_em);
            self.program = Some(Arc::new(p));
            self.embedded = true;
        } else {
            self.substitute(fonts, flags, notes);
            if self.program.is_none() {
                notes.push(format!("font {} could not be found; its text is not drawn", self.name));
            }
        }
        // encoding: base + differences
        let enc = pdf.dget(fd, "Encoding");
        let mut base: Option<&'static [&'static str; 256]> = None;
        let mut diffs: Vec<(usize, String)> = Vec::new();
        match &enc {
            Obj::Name(n) => base = base_encoding(n),
            Obj::Dict(d) => {
                if let Some(n) = pdf.dget_name(d, "BaseEncoding") {
                    base = base_encoding(&n);
                }
                let mut code = 0usize;
                for o in pdf.dget_array(d, "Differences").unwrap_or_default() {
                    match pdf.resolve(&o) {
                        Obj::Int(i) => code = i.max(0) as usize,
                        Obj::Name(n) => {
                            diffs.push((code, n));
                            code += 1;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let has_encoding = !matches!(enc, Obj::Null);
        let builtin: HashMap<u8, u16> = match &self.program {
            Some(p) if self.embedded && p.is_cff => p.cff_table().map(|c| c.builtin_encoding()).unwrap_or_default(),
            _ => HashMap::new(),
        };
        let std14 = !self.embedded;
        for code in 0..256usize {
            let mut name: Option<String> = None;
            if let Some(b) = base {
                let n = b[code];
                if n != ".notdef" {
                    name = Some(n.to_string());
                }
            } else if std14 && !symbolic && !has_encoding || (std14 && !symbolic) {
                let n = tables::STANDARD[code];
                if n != ".notdef" {
                    name = Some(n.to_string());
                }
            }
            if let Some((_, n)) = diffs.iter().rev().find(|(c, _)| *c == code) {
                name = Some(n.clone());
            }
            self.names[code] = name;
        }
        // glyph ids and Unicode per code
        let prog = self.program.clone();
        let cff_names: HashMap<String, u16> = match &prog {
            Some(p) if p.is_cff => p.cff_table().map(|c| c.names()).unwrap_or_default(),
            _ => HashMap::new(),
        };
        for code in 0..256usize {
            let name = self.names[code].clone();
            let uni = name.as_deref().and_then(name_to_unicode);
            let mut gid: Option<u16> = None;
            if let Some(p) = &prog {
                if p.is_cff && self.embedded {
                    gid = name
                        .as_ref()
                        .and_then(|n| cff_names.get(n).copied())
                        .or_else(|| if name.is_none() || !has_encoding { builtin.get(&(code as u8)).copied() } else { None });
                    if gid.is_none() && p.glyph_for_char('A').is_some() {
                        if let Some(u) = uni.as_ref().and_then(|u| u.chars().next()) {
                            gid = p.glyph_for_char(u);
                        }
                    }
                } else if self.embedded {
                    // TrueType
                    let mut try_cmaps: Vec<Option<u16>> = Vec::new();
                    let uc = uni.as_ref().and_then(|u| u.chars().next());
                    if !symbolic || has_encoding && name.is_some() {
                        if let Some(c) = uc {
                            try_cmaps.push(p.glyph_for_code(3, 1, c as u32));
                            try_cmaps.push(p.glyph_for_char(c));
                        }
                        if let Some(n) = &name {
                            if let Some(mc) = tables::MAC_ROMAN.iter().position(|m| m == n) {
                                try_cmaps.push(p.glyph_for_code(1, 0, mc as u32));
                            }
                        }
                    }
                    try_cmaps.push(p.glyph_for_code(3, 0, code as u32));
                    try_cmaps.push(p.glyph_for_code(3, 0, 0xF000 + code as u32));
                    try_cmaps.push(p.glyph_for_code(1, 0, code as u32));
                    if let Some(c) = uc {
                        try_cmaps.push(p.glyph_for_code(0, 3, c as u32));
                    }
                    gid = try_cmaps.into_iter().flatten().find(|g| *g != 0);
                    if gid.is_none() && !p.has_cmap(3, 1) && !p.has_cmap(3, 0) && !p.has_cmap(1, 0) {
                        gid = Some(code as u16).filter(|g| *g < p.num_glyphs);
                    }
                } else {
                    // substitute font: by Unicode
                    let c = uni.as_ref().and_then(|u| u.chars().next()).or_else(|| if code >= 32 && code != 127 { char::from_u32(code as u32) } else { None });
                    gid = c.and_then(|c| p.glyph_for_char(c));
                }
            }
            self.gids[code] = gid;
            self.unis[code] = uni.unwrap_or_else(|| if (32..127).contains(&code) && (name.is_none() || self.kind == Kind::Type3) { char::from(code as u8).to_string() } else { String::new() });
        }
        if !self.embedded && (self.name.starts_with("Symbol") || self.name.starts_with("ZapfDingbats")) && !has_encoding {
            // the built-in encodings of these two fonts are not available
            self.gids.iter_mut().for_each(|g| *g = None);
            notes.push(format!("font {} uses a built-in symbol encoding that is not available; its text is not drawn", self.name));
        }
        if let Some(p) = &self.program {
            for (g, c) in p.unicode_map().iter().map(|(u, g)| (*g, *u)) {
                if let Some(ch) = char::from_u32(c) {
                    self.gid_unicode.entry(g).or_insert(ch);
                }
            }
        }
        // symbolic TrueType without names: Unicode from the program's cmap
        for code in 0..256usize {
            if self.unis[code].is_empty() {
                if let Some(g) = self.gids[code] {
                    if let Some(c) = self.gid_unicode.get(&g) {
                        self.unis[code] = c.to_string();
                    }
                }
            }
        }
    }

    fn load_composite(&mut self, pdf: &Pdf, fd: &Dict, fonts: Option<&FontDb>, notes: &mut Vec<String>) {
        let enc = pdf.dget(fd, "Encoding");
        let cmap = match &enc {
            Obj::Name(n) if n == "Identity-H" || n == "Identity-V" => CMap::identity(n.ends_with('V')),
            Obj::Name(n) => {
                notes.push(format!("the predefined CMap {n} is not available; codes are read as two-byte CIDs"));
                CMap::identity(n.ends_with('V'))
            }
            Obj::Stream(s) => {
                let mut c = crate::filter::decode_stream(pdf, s, false).map(|d| CMap::parse(&d.0)).unwrap_or_default();
                if c.codespaces.is_empty() {
                    c.codespaces.push((2, 0, 0xFFFF));
                }
                if pdf.dget_i64(&s.dict, "WMode") == Some(1) {
                    c.vertical = true;
                }
                c
            }
            _ => CMap::identity(false),
        };
        self.vertical = cmap.vertical;
        self.cmap = Some(cmap);
        let desc_fonts = pdf.dget_array(fd, "DescendantFonts").unwrap_or_default();
        let cid = desc_fonts.first().map(|o| pdf.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
        self.dw = pdf.dget_f64(&cid, "DW").unwrap_or(1000.0);
        if let Some(w) = pdf.dget_array(&cid, "W") {
            let mut i = 0;
            while i < w.len() {
                let c1 = pdf.resolve(&w[i]).as_i64().unwrap_or(0).max(0) as u32;
                match w.get(i + 1).map(|o| pdf.resolve(o)) {
                    Some(Obj::Array(a)) => {
                        for (k, v) in a.iter().enumerate() {
                            if let Some(x) = pdf.resolve(v).as_f64() {
                                self.cid_widths.insert(c1 + k as u32, x);
                            }
                        }
                        i += 2;
                    }
                    Some(o2) => {
                        let c2 = o2.as_i64().unwrap_or(0).max(0) as u32;
                        let x = w.get(i + 2).map(|o| pdf.resolve(o)).and_then(|o| o.as_f64()).unwrap_or(0.0);
                        for c in c1..=c2.min(c1.saturating_add(70000)) {
                            self.cid_widths.insert(c, x);
                        }
                        i += 3;
                    }
                    None => break,
                }
            }
        }
        if let Some(d) = pdf.dget_numbers(&cid, "DW2") {
            if d.len() >= 2 {
                self.dw2 = (d[0], d[1]);
            }
        }
        if let Some(w) = pdf.dget_array(&cid, "W2") {
            let mut i = 0;
            while i < w.len() {
                let c1 = pdf.resolve(&w[i]).as_i64().unwrap_or(0).max(0) as u32;
                match w.get(i + 1).map(|o| pdf.resolve(o)) {
                    Some(Obj::Array(a)) => {
                        let v: Vec<f64> = a.iter().filter_map(|o| pdf.resolve(o).as_f64()).collect();
                        for (k, t) in v.chunks_exact(3).enumerate() {
                            self.w2.insert(c1 + k as u32, (t[0], t[1], t[2]));
                        }
                        i += 2;
                    }
                    Some(o2) => {
                        let c2 = o2.as_i64().unwrap_or(0).max(0) as u32;
                        let g = |k: usize| w.get(i + k).map(|o| pdf.resolve(o)).and_then(|o| o.as_f64()).unwrap_or(0.0);
                        for c in c1..=c2.min(c1.saturating_add(70000)) {
                            self.w2.insert(c, (g(2), g(3), g(4)));
                        }
                        i += 5;
                    }
                    None => break,
                }
            }
        }
        let desc = pdf.dget_dict(&cid, "FontDescriptor").unwrap_or_default();
        let flags = self.descriptor_info(pdf, &desc);
        if let Some(n) = pdf.dget_name(&cid, "BaseFont") {
            if self.name.is_empty() {
                self.name = strip_subset(&n).to_string();
            }
        }
        match pdf.dget(&cid, "CIDToGIDMap") {
            Obj::Stream(s) => {
                if let Ok((d, _)) = crate::filter::decode_stream(pdf, &s, false) {
                    self.cid_to_gid = Some(d.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect());
                }
            }
            _ => {}
        }
        if let Some(p) = load_program(pdf, &desc) {
            self.unit_scale = 1.0 / f64::from(p.units_per_em);
            if p.is_cff {
                if let Some(c) = p.cff_table() {
                    self.cff_cid = c.is_cid;
                    if c.is_cid {
                        for g in 0..c.num_glyphs() {
                            self.cid_gid_map.entry(c.charset_id(g)).or_insert(g as u16);
                        }
                    }
                }
            }
            for (u, g) in p.unicode_map() {
                if let Some(ch) = char::from_u32(u) {
                    self.gid_unicode.entry(g).or_insert(ch);
                }
            }
            self.program = Some(Arc::new(p));
            self.embedded = true;
        } else {
            self.substitute(fonts, flags, notes);
            // without an embedded program, characters can only be drawn through Unicode
            if self.program.is_none() {
                notes.push(format!("font {} could not be found; its text is not drawn", self.name));
            }
        }
    }

    fn load_type3(&mut self, pdf: &Pdf, fd: &Dict) {
        let fm = pdf.dget_numbers(fd, "FontMatrix").unwrap_or_else(|| vec![0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);
        let mut m = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
        for (i, v) in fm.iter().take(6).enumerate() {
            m[i] = *v;
        }
        let mut procs = HashMap::new();
        if let Some(cp) = pdf.dget_dict(fd, "CharProcs") {
            for (k, v) in &cp {
                if let Obj::Stream(s) = pdf.resolve(v) {
                    procs.insert(k.clone(), s);
                }
            }
        }
        self.first_char = pdf.dget_i64(fd, "FirstChar").unwrap_or(0);
        self.widths = pdf.dget_numbers(fd, "Widths").unwrap_or_default();
        let mut names: Vec<Option<String>> = vec![None; 256];
        let enc = pdf.dget(fd, "Encoding");
        if let Obj::Dict(d) = &enc {
            if let Some(n) = pdf.dget_name(d, "BaseEncoding").and_then(|n| base_encoding(&n)) {
                for (i, nm) in n.iter().enumerate() {
                    if *nm != ".notdef" {
                        names[i] = Some((*nm).to_string());
                    }
                }
            }
            let mut code = 0usize;
            for o in pdf.dget_array(d, "Differences").unwrap_or_default() {
                match pdf.resolve(&o) {
                    Obj::Int(i) => code = i.max(0) as usize,
                    Obj::Name(n) => {
                        if code < 256 {
                            names[code] = Some(n);
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }
        for code in 0..256 {
            if let Some(n) = &names[code] {
                self.unis[code] = name_to_unicode(n).unwrap_or_default();
            }
            if self.unis[code].is_empty() && (33..127).contains(&code) {
                // glyph names of a Type 3 font are arbitrary; the code is the best guess
                self.unis[code] = char::from(code as u8).to_string();
            }
        }
        self.type3 = Some(Type3 { font_matrix: m, procs, resources: pdf.dget_dict(fd, "Resources"), names });
    }

    /// The characters in a string shown with this font.
    pub fn decode(&self, bytes: &[u8]) -> Vec<CharInfo> {
        match self.kind {
            Kind::Simple | Kind::Type3 => bytes
                .iter()
                .map(|&b| {
                    let code = usize::from(b);
                    let mut w = if code as i64 >= self.first_char { self.widths.get((code as i64 - self.first_char) as usize).copied() } else { None };
                    if w.is_none() && self.kind == Kind::Simple {
                        w = if self.missing_width > 0.0 || self.embedded {
                            Some(self.missing_width)
                        } else {
                            self.program.as_ref().and_then(|p| self.gids[code].map(|g| f64::from(p.advance(g)) * 1000.0 / f64::from(p.units_per_em)))
                        };
                    }
                    let uni = self
                        .to_unicode
                        .as_ref()
                        .and_then(|m| m.unicode(u32::from(b)))
                        .unwrap_or_else(|| self.unis[code].clone());
                    CharInfo {
                        code: u32::from(b),
                        nbytes: 1,
                        width: w.unwrap_or(0.0),
                        gid: self.gids[code],
                        unicode: uni,
                        is_space: b == 32,
                        name: self.type3.as_ref().and_then(|t| t.names[code].clone()),
                        vertical: None,
                    }
                })
                .collect(),
            Kind::Composite => {
                let Some(cm) = &self.cmap else { return Vec::new() };
                cm.codes(bytes)
                    .into_iter()
                    .map(|(code, len)| {
                        let cid = cm.cid(code, len);
                        let w = self.cid_widths.get(&cid).copied().unwrap_or(self.dw);
                        let gid = self.gid_for_cid(cid, code);
                        let uni = self
                            .to_unicode
                            .as_ref()
                            .and_then(|m| m.unicode(code))
                            .or_else(|| gid.and_then(|g| self.gid_unicode.get(&g)).map(|c| c.to_string()))
                            .unwrap_or_default();
                        let vertical = self.vertical.then(|| {
                            let (w1, vx, vy) = self.w2.get(&cid).copied().unwrap_or((self.dw2.1, w / 2.0, self.dw2.0));
                            (w1, vx, vy)
                        });
                        CharInfo { code, nbytes: len, width: w, gid, unicode: uni, is_space: len == 1 && code == 32, name: None, vertical }
                    })
                    .collect()
            }
        }
    }

    fn gid_for_cid(&self, cid: u32, code: u32) -> Option<u16> {
        let p = self.program.as_ref()?;
        if !self.embedded {
            // a stand-in font: find the character through the code read as Unicode when the
            // font offers a ToUnicode map
            let u = self.to_unicode.as_ref().and_then(|m| m.unicode(code)).and_then(|s| s.chars().next())?;
            return p.glyph_for_char(u);
        }
        if let Some(map) = &self.cid_to_gid {
            return map.get(cid as usize).copied();
        }
        if self.cff_cid {
            return self.cid_gid_map.get(&(cid as u16)).copied();
        }
        Some(cid as u16).filter(|g| *g < p.num_glyphs.max(1) || p.num_glyphs == 0)
    }

    pub fn is_vertical(&self) -> bool {
        self.vertical
    }

    /// The outline of a glyph in font units (y up), cached.
    pub fn outline(&self, gid: u16) -> Option<Arc<Path>> {
        if let Some(c) = self.cache.borrow().get(&gid) {
            return c.clone();
        }
        let p = self.program.as_ref()?;
        let path = oc_font::outline(p, gid).map(Arc::new);
        self.cache.borrow_mut().insert(gid, path.clone());
        path
    }
}
