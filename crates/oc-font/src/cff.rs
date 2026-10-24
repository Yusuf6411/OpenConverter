//! Compact Font Format (CFF) parsing and Type 2 charstring interpretation.
use crate::reader::R;
use oc_raster::Path;
use std::collections::HashMap;

/// Standard strings for SIDs 0..=228 (the rest of the 391 are rarely used as glyph names).
pub const STD_STRINGS: [&str; 229] = [
    ".notdef",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quoteright",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "quoteleft",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "exclamdown",
    "cent",
    "sterling",
    "fraction",
    "yen",
    "florin",
    "section",
    "currency",
    "quotesingle",
    "quotedblleft",
    "guillemotleft",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "endash",
    "dagger",
    "daggerdbl",
    "periodcentered",
    "paragraph",
    "bullet",
    "quotesinglbase",
    "quotedblbase",
    "quotedblright",
    "guillemotright",
    "ellipsis",
    "perthousand",
    "questiondown",
    "grave",
    "acute",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "dieresis",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "emdash",
    "AE",
    "ordfeminine",
    "Lslash",
    "Oslash",
    "OE",
    "ordmasculine",
    "ae",
    "dotlessi",
    "lslash",
    "oslash",
    "oe",
    "germandbls",
    "onesuperior",
    "logicalnot",
    "mu",
    "trademark",
    "Eth",
    "onehalf",
    "plusminus",
    "Thorn",
    "onequarter",
    "divide",
    "brokenbar",
    "degree",
    "thorn",
    "threequarters",
    "twosuperior",
    "registered",
    "minus",
    "eth",
    "multiply",
    "threesuperior",
    "copyright",
    "Aacute",
    "Acircumflex",
    "Adieresis",
    "Agrave",
    "Aring",
    "Atilde",
    "Ccedilla",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Ntilde",
    "Oacute",
    "Ocircumflex",
    "Odieresis",
    "Ograve",
    "Otilde",
    "Scaron",
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Ugrave",
    "Yacute",
    "Ydieresis",
    "Zcaron",
    "aacute",
    "acircumflex",
    "adieresis",
    "agrave",
    "aring",
    "atilde",
    "ccedilla",
    "eacute",
    "ecircumflex",
    "edieresis",
    "egrave",
    "iacute",
    "icircumflex",
    "idieresis",
    "igrave",
    "ntilde",
    "oacute",
    "ocircumflex",
    "odieresis",
    "ograve",
    "otilde",
    "scaron",
    "uacute",
    "ucircumflex",
    "udieresis",
    "ugrave",
    "yacute",
    "ydieresis",
    "zcaron",
];

/// The SID of the glyph at each character code of the Standard Encoding (0 = unencoded).
pub fn standard_encoding_sid(code: u8) -> u16 {
    let c = u16::from(code);
    match c {
        32..=126 => c - 31,
        161..=175 => c - 161 + 96,
        177..=180 => c - 177 + 111,
        182..=189 => c - 182 + 115,
        191 => 123,
        193..=200 => c - 193 + 124,
        202 | 203 => c - 202 + 132,
        205..=208 => c - 205 + 134,
        225 => 138,
        227 => 139,
        232..=235 => c - 232 + 140,
        241 => 144,
        245 => 145,
        248..=251 => c - 248 + 146,
        _ => 0,
    }
}

type Dict = Vec<(u16, Vec<f64>)>;

fn parse_dict(d: &[u8]) -> Dict {
    let mut out = Vec::new();
    let mut ops: Vec<f64> = Vec::new();
    let mut i = 0usize;
    while i < d.len() {
        let b = d[i];
        match b {
            0..=21 => {
                let op = if b == 12 {
                    i += 1;
                    0x0C00 | u16::from(d.get(i).copied().unwrap_or(0))
                } else {
                    u16::from(b)
                };
                out.push((op, std::mem::take(&mut ops)));
                i += 1;
            }
            28 => {
                let v = i16::from_be_bytes([
                    d.get(i + 1).copied().unwrap_or(0),
                    d.get(i + 2).copied().unwrap_or(0),
                ]);
                ops.push(f64::from(v));
                i += 3;
            }
            29 => {
                let v = i32::from_be_bytes([
                    d.get(i + 1).copied().unwrap_or(0),
                    d.get(i + 2).copied().unwrap_or(0),
                    d.get(i + 3).copied().unwrap_or(0),
                    d.get(i + 4).copied().unwrap_or(0),
                ]);
                ops.push(f64::from(v));
                i += 5;
            }
            30 => {
                let mut s = String::new();
                i += 1;
                'outer: while i < d.len() {
                    let byte = d[i];
                    i += 1;
                    for nib in [byte >> 4, byte & 15] {
                        match nib {
                            0..=9 => s.push(char::from(b'0' + nib)),
                            10 => s.push('.'),
                            11 => s.push('E'),
                            12 => s.push_str("E-"),
                            14 => s.push('-'),
                            15 => break 'outer,
                            _ => {}
                        }
                    }
                }
                ops.push(s.parse::<f64>().unwrap_or(0.0));
            }
            32..=246 => {
                ops.push(f64::from(b) - 139.0);
                i += 1;
            }
            247..=250 => {
                let b1 = f64::from(d.get(i + 1).copied().unwrap_or(0));
                ops.push((f64::from(b) - 247.0) * 256.0 + b1 + 108.0);
                i += 2;
            }
            251..=254 => {
                let b1 = f64::from(d.get(i + 1).copied().unwrap_or(0));
                ops.push(-(f64::from(b) - 251.0) * 256.0 - b1 - 108.0);
                i += 2;
            }
            _ => i += 1,
        }
        if ops.len() > 64 {
            ops.clear();
        }
    }
    out
}

fn dict_get(d: &Dict, op: u16) -> Option<&Vec<f64>> {
    d.iter().find(|(o, _)| *o == op).map(|(_, v)| v)
}

/// Offsets of the items of an INDEX at `off`: `(items, end_offset)`.
fn index(data: &[u8], off: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let r = R::new(data);
    let count = r.u16(off)? as usize;
    if count == 0 {
        return Some((Vec::new(), off + 2));
    }
    let osz = r.u8(off + 2)? as usize;
    if !(1..=4).contains(&osz) {
        return None;
    }
    let rd = |i: usize| -> Option<usize> {
        let o = off + 3 + i * osz;
        let b = r.slice(o, osz)?;
        Some(b.iter().fold(0usize, |a, v| (a << 8) | usize::from(*v)))
    };
    let base = off + 3 + (count + 1) * osz - 1;
    let mut items = Vec::with_capacity(count.min(70_000));
    let mut prev = rd(0)?;
    for i in 1..=count {
        let cur = rd(i)?;
        if cur < prev || base.checked_add(cur)? > data.len() {
            return None;
        }
        items.push((base + prev, cur - prev));
        prev = cur;
    }
    Some((items, base + prev))
}

#[derive(Debug, Clone, Default)]
struct Private {
    subrs: Vec<(usize, usize)>,
    default_width: f64,
    nominal_width: f64,
}

#[derive(Debug, Clone)]
pub struct Cff<'a> {
    data: &'a [u8],
    strings: Vec<(usize, usize)>,
    gsubrs: Vec<(usize, usize)>,
    charstrings: Vec<(usize, usize)>,
    charset: Vec<u16>,
    privates: Vec<Private>,
    fd_select: Vec<u8>,
    pub is_cid: bool,
    pub font_matrix: [f64; 6],
    encoding_off: usize,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Glyph {
    pub path: Path,
    /// Advance width in font units.
    pub width: f64,
}

impl<'a> Cff<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Cff<'a>> {
        let r = R::new(data);
        let hdr = r.u8(2)? as usize;
        let (names, e1) = index(data, hdr)?;
        let (tops, e2) = index(data, e1)?;
        let (strings, e3) = index(data, e2)?;
        let (gsubrs, _) = index(data, e3)?;
        let top = parse_dict(data.get(tops.first()?.0..tops.first()?.0 + tops.first()?.1)?);
        let cs_off = dict_get(&top, 17)?.first().copied()? as usize;
        let (charstrings, _) = index(data, cs_off)?;
        let n = charstrings.len();
        let is_cid = dict_get(&top, 0x0C1E).is_some();
        let mut privates = Vec::new();
        let mut fd_select = Vec::new();
        let read_private = |dict: &Dict| -> Private {
            let mut p = Private::default();
            if let Some(v) = dict_get(dict, 18) {
                if let (Some(&size), Some(&off)) = (v.first(), v.get(1)) {
                    let (size, off) = (size as usize, off as usize);
                    if let Some(pd) = data.get(off..off.saturating_add(size)) {
                        let pdict = parse_dict(pd);
                        p.default_width = dict_get(&pdict, 20)
                            .and_then(|v| v.first().copied())
                            .unwrap_or(0.0);
                        p.nominal_width = dict_get(&pdict, 21)
                            .and_then(|v| v.first().copied())
                            .unwrap_or(0.0);
                        if let Some(so) = dict_get(&pdict, 19).and_then(|v| v.first().copied()) {
                            p.subrs = index(data, off + so as usize)
                                .map(|(i, _)| i)
                                .unwrap_or_default();
                        }
                    }
                }
            }
            p
        };
        if is_cid {
            let fda = dict_get(&top, 0x0C24)?.first().copied()? as usize;
            let (fds, _) = index(data, fda)?;
            for (o, l) in fds.iter().take(256) {
                let d = parse_dict(data.get(*o..o + l)?);
                privates.push(read_private(&d));
            }
            let fs = dict_get(&top, 0x0C25)?.first().copied()? as usize;
            fd_select = vec![0u8; n];
            match r.u8(fs)? {
                0 => {
                    for (g, slot) in fd_select.iter_mut().enumerate() {
                        *slot = r.u8(fs + 1 + g)?;
                    }
                }
                3 => {
                    let nr = r.u16(fs + 1)? as usize;
                    for i in 0..nr {
                        let first = r.u16(fs + 3 + i * 3)? as usize;
                        let fd = r.u8(fs + 5 + i * 3)?;
                        let next = r.u16(fs + 6 + i * 3)? as usize;
                        for g in first..next.min(n) {
                            fd_select[g] = fd;
                        }
                    }
                }
                _ => {}
            }
        } else {
            privates.push(read_private(&top));
        }
        // Charset
        let mut charset = vec![0u16; n];
        let cso = dict_get(&top, 15)
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0) as usize;
        if cso > 2 {
            let fmt = r.u8(cso)?;
            match fmt {
                0 => {
                    for g in 1..n {
                        charset[g] = r.u16(cso + 1 + (g - 1) * 2)?;
                    }
                }
                1 | 2 => {
                    let mut g = 1usize;
                    let mut o = cso + 1;
                    while g < n {
                        let first = r.u16(o)?;
                        let left = if fmt == 1 {
                            let v = r.u8(o + 2)? as usize;
                            o += 3;
                            v
                        } else {
                            let v = r.u16(o + 2)? as usize;
                            o += 4;
                            v
                        };
                        for k in 0..=left {
                            if g >= n {
                                break;
                            }
                            charset[g] = first.wrapping_add(k as u16);
                            g += 1;
                        }
                    }
                }
                _ => {}
            }
        } else if !is_cid {
            // Predefined ISOAdobe charset: SID == GID for the first 229 glyphs.
            for (g, slot) in charset.iter_mut().enumerate().take(n.min(229)) {
                *slot = g as u16;
            }
        } else {
            for (g, slot) in charset.iter_mut().enumerate() {
                *slot = g as u16;
            }
        }
        let mut fm = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
        if let Some(v) = dict_get(&top, 0x0C07) {
            if v.len() == 6 {
                fm.copy_from_slice(v);
            }
        }
        let name = names
            .first()
            .and_then(|(o, l)| data.get(*o..o + l))
            .map(|b| b.iter().map(|c| char::from(*c)).collect())
            .unwrap_or_default();
        Some(Cff {
            data,
            strings,
            gsubrs,
            charstrings,
            charset,
            privates,
            fd_select,
            is_cid,
            font_matrix: fm,
            encoding_off: dict_get(&top, 16)
                .and_then(|v| v.first().copied())
                .unwrap_or(0.0) as usize,
            name,
        })
    }

    pub fn num_glyphs(&self) -> usize {
        self.charstrings.len()
    }

    fn string(&self, sid: u16) -> Option<String> {
        let sid = sid as usize;
        if sid < STD_STRINGS.len() {
            return STD_STRINGS.get(sid).map(|s| (*s).to_string());
        }
        if sid < 391 {
            return None;
        }
        let (o, l) = *self.strings.get(sid - 391)?;
        self.data
            .get(o..o + l)
            .map(|b| b.iter().map(|c| char::from(*c)).collect())
    }

    /// The CID (for CID-keyed fonts) or SID of a glyph.
    pub fn charset_id(&self, gid: usize) -> u16 {
        self.charset.get(gid).copied().unwrap_or(0)
    }

    pub fn glyph_name(&self, gid: usize) -> Option<String> {
        if self.is_cid {
            return None;
        }
        self.string(self.charset_id(gid))
    }

    pub fn names(&self) -> HashMap<String, u16> {
        let mut m = HashMap::new();
        if !self.is_cid {
            for g in 0..self.num_glyphs() {
                if let Some(n) = self.glyph_name(g) {
                    m.entry(n).or_insert(g as u16);
                }
            }
        }
        m
    }

    /// Glyph for a CID of a CID-keyed font.
    pub fn gid_for_cid(&self, cid: u16) -> Option<u16> {
        self.charset
            .iter()
            .position(|c| *c == cid)
            .map(|g| g as u16)
    }

    /// The font's own encoding: character code to glyph id.
    pub fn builtin_encoding(&self) -> HashMap<u8, u16> {
        let mut m = HashMap::new();
        if self.is_cid {
            return m;
        }
        let r = R::new(self.data);
        let off = self.encoding_off;
        if off <= 1 {
            // Standard (0) and Expert (1, treated alike): through the SID of each code.
            let sid_to_gid: HashMap<u16, u16> = self
                .charset
                .iter()
                .enumerate()
                .map(|(g, s)| (*s, g as u16))
                .collect();
            for code in 0..=255u8 {
                let sid = standard_encoding_sid(code);
                if sid != 0 {
                    if let Some(g) = sid_to_gid.get(&sid) {
                        m.insert(code, *g);
                    }
                }
            }
            return m;
        }
        let Some(fmt) = r.u8(off) else { return m };
        match fmt & 0x7f {
            0 => {
                let n = r.u8(off + 1).unwrap_or(0) as usize;
                for i in 0..n {
                    if let Some(c) = r.u8(off + 2 + i) {
                        m.insert(c, (i + 1) as u16);
                    }
                }
            }
            1 => {
                let n = r.u8(off + 1).unwrap_or(0) as usize;
                let mut gid = 1u16;
                for i in 0..n {
                    let (Some(first), Some(left)) = (r.u8(off + 2 + i * 2), r.u8(off + 3 + i * 2))
                    else {
                        break;
                    };
                    for k in 0..=u16::from(left) {
                        let code = u16::from(first) + k;
                        if code < 256 {
                            m.insert(code as u8, gid);
                        }
                        gid += 1;
                    }
                }
            }
            _ => {}
        }
        m
    }

    pub fn glyph(&self, gid: usize) -> Option<Glyph> {
        let (o, l) = *self.charstrings.get(gid)?;
        let code = self.data.get(o..o + l)?;
        let fd = self.fd_select.get(gid).copied().unwrap_or(0) as usize;
        let private = self.privates.get(fd).or_else(|| self.privates.first())?;
        let mut interp = Interp {
            cff: self,
            private,
            stack: Vec::new(),
            x: 0.0,
            y: 0.0,
            nhints: 0,
            width: None,
            open: false,
            path: Path::new(),
            transient: [0.0; 32],
            steps: 0,
            done: false,
        };
        interp.run(code, 0);
        if interp.open {
            interp.path.close();
        }
        let width = interp
            .width
            .map_or(private.default_width, |w| private.nominal_width + w);
        Some(Glyph {
            path: interp.path,
            width,
        })
    }
}

struct Interp<'a, 'b> {
    cff: &'b Cff<'a>,
    private: &'b Private,
    stack: Vec<f64>,
    x: f64,
    y: f64,
    nhints: usize,
    width: Option<f64>,
    open: bool,
    path: Path,
    transient: [f64; 32],
    steps: u32,
    done: bool,
}

fn bias(n: usize) -> i64 {
    if n < 1240 {
        107
    } else if n < 33900 {
        1131
    } else {
        32768
    }
}

impl Interp<'_, '_> {
    fn take_width(&mut self, expected_even: bool, min_args: usize) {
        // The first stack-clearing operator may carry the width as an extra first operand.
        if self.width.is_none() {
            let extra = if expected_even {
                self.stack.len() % 2 == 1
            } else {
                self.stack.len() > min_args
            };
            if extra && !self.stack.is_empty() {
                self.width = Some(self.stack.remove(0));
            } else {
                self.width = Some(f64::NAN); // NaN marks "default width"
            }
        }
    }

    fn move_to(&mut self, dx: f64, dy: f64) {
        if self.open {
            self.path.close();
        }
        self.x += dx;
        self.y += dy;
        self.path.move_to(self.x, self.y);
        self.open = true;
    }
    fn line(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
        self.path.line_to(self.x, self.y);
    }
    fn curve(&mut self, d: [f64; 6]) {
        let (x1, y1) = (self.x + d[0], self.y + d[1]);
        let (x2, y2) = (x1 + d[2], y1 + d[3]);
        self.x = x2 + d[4];
        self.y = y2 + d[5];
        self.path.cubic_to(x1, y1, x2, y2, self.x, self.y);
    }

    fn run(&mut self, code: &[u8], depth: u32) {
        if depth > 10 {
            return;
        }
        let mut i = 0usize;
        while i < code.len() && !self.done {
            self.steps += 1;
            if self.steps > 200_000 {
                self.done = true;
                return;
            }
            let b = code[i];
            i += 1;
            match b {
                32..=246 => self.stack.push(f64::from(b) - 139.0),
                247..=250 => {
                    let b1 = f64::from(code.get(i).copied().unwrap_or(0));
                    i += 1;
                    self.stack.push((f64::from(b) - 247.0) * 256.0 + b1 + 108.0);
                }
                251..=254 => {
                    let b1 = f64::from(code.get(i).copied().unwrap_or(0));
                    i += 1;
                    self.stack
                        .push(-(f64::from(b) - 251.0) * 256.0 - b1 - 108.0);
                }
                255 => {
                    let v = i32::from_be_bytes([
                        code.get(i).copied().unwrap_or(0),
                        code.get(i + 1).copied().unwrap_or(0),
                        code.get(i + 2).copied().unwrap_or(0),
                        code.get(i + 3).copied().unwrap_or(0),
                    ]);
                    i += 4;
                    self.stack.push(f64::from(v) / 65536.0);
                }
                28 => {
                    let v = i16::from_be_bytes([
                        code.get(i).copied().unwrap_or(0),
                        code.get(i + 1).copied().unwrap_or(0),
                    ]);
                    i += 2;
                    self.stack.push(f64::from(v));
                }
                1 | 3 | 18 | 23 => {
                    self.take_width(true, 0);
                    self.nhints += self.stack.len() / 2;
                    self.stack.clear();
                }
                19 | 20 => {
                    self.take_width(true, 0);
                    self.nhints += self.stack.len() / 2;
                    self.stack.clear();
                    i += self.nhints.div_ceil(8);
                }
                21 => {
                    self.take_width(false, 2);
                    let s = std::mem::take(&mut self.stack);
                    if s.len() >= 2 {
                        self.move_to(s[s.len() - 2], s[s.len() - 1]);
                    }
                }
                22 => {
                    self.take_width(false, 1);
                    let s = std::mem::take(&mut self.stack);
                    if let Some(dx) = s.last() {
                        self.move_to(*dx, 0.0);
                    }
                }
                4 => {
                    self.take_width(false, 1);
                    let s = std::mem::take(&mut self.stack);
                    if let Some(dy) = s.last() {
                        self.move_to(0.0, *dy);
                    }
                }
                5 => {
                    let s = std::mem::take(&mut self.stack);
                    for c in s.chunks_exact(2) {
                        self.line(c[0], c[1]);
                    }
                }
                6 | 7 => {
                    let s = std::mem::take(&mut self.stack);
                    let mut horiz = b == 6;
                    for v in s {
                        if horiz {
                            self.line(v, 0.0);
                        } else {
                            self.line(0.0, v);
                        }
                        horiz = !horiz;
                    }
                }
                8 => {
                    let s = std::mem::take(&mut self.stack);
                    for c in s.chunks_exact(6) {
                        self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                }
                24 => {
                    let s = std::mem::take(&mut self.stack);
                    let n = s.len();
                    if n >= 2 {
                        let curves = (n - 2) / 6;
                        for k in 0..curves {
                            let c = &s[k * 6..k * 6 + 6];
                            self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]);
                        }
                        self.line(s[curves * 6], s[curves * 6 + 1]);
                    }
                }
                25 => {
                    let s = std::mem::take(&mut self.stack);
                    let n = s.len();
                    if n >= 6 {
                        let lines = (n - 6) / 2;
                        for k in 0..lines {
                            self.line(s[k * 2], s[k * 2 + 1]);
                        }
                        let c = &s[lines * 2..lines * 2 + 6];
                        self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                }
                26 => {
                    let s = std::mem::take(&mut self.stack);
                    let (mut dx1, mut k) = (0.0, 0usize);
                    if s.len() % 4 == 1 {
                        dx1 = s[0];
                        k = 1;
                    }
                    while k + 4 <= s.len() {
                        self.curve([dx1, s[k], s[k + 1], s[k + 2], 0.0, s[k + 3]]);
                        dx1 = 0.0;
                        k += 4;
                    }
                }
                27 => {
                    let s = std::mem::take(&mut self.stack);
                    let (mut dy1, mut k) = (0.0, 0usize);
                    if s.len() % 4 == 1 {
                        dy1 = s[0];
                        k = 1;
                    }
                    while k + 4 <= s.len() {
                        self.curve([s[k], dy1, s[k + 1], s[k + 2], s[k + 3], 0.0]);
                        dy1 = 0.0;
                        k += 4;
                    }
                }
                30 | 31 => {
                    let s = std::mem::take(&mut self.stack);
                    let mut horiz = b == 31;
                    let mut k = 0usize;
                    while k + 4 <= s.len() {
                        let last = s.len() - k == 5;
                        let extra = if last { s[k + 4] } else { 0.0 };
                        if horiz {
                            self.curve([s[k], 0.0, s[k + 1], s[k + 2], extra, s[k + 3]]);
                        } else {
                            self.curve([0.0, s[k], s[k + 1], s[k + 2], s[k + 3], extra]);
                        }
                        horiz = !horiz;
                        k += 4;
                    }
                }
                10 | 29 => {
                    let idx = self.stack.pop().unwrap_or(0.0) as i64;
                    let subrs = if b == 10 {
                        &self.private.subrs
                    } else {
                        &self.cff.gsubrs
                    };
                    let n = idx + bias(subrs.len());
                    if n >= 0 {
                        if let Some((o, l)) = subrs.get(n as usize).copied() {
                            if let Some(sub) = self.cff.data.get(o..o + l) {
                                self.run(sub, depth + 1);
                            }
                        }
                    }
                }
                11 => return,
                14 => {
                    // endchar, possibly with a width and an accented-character composition
                    if self.width.is_none() {
                        if self.stack.len() == 1 || self.stack.len() == 5 {
                            self.width = Some(self.stack.remove(0));
                        } else {
                            self.width = Some(f64::NAN);
                        }
                    }
                    if self.stack.len() >= 4 {
                        let n = self.stack.len();
                        let (adx, ady, bchar, achar) = (
                            self.stack[n - 4],
                            self.stack[n - 3],
                            self.stack[n - 2] as u8,
                            self.stack[n - 1] as u8,
                        );
                        self.seac(adx, ady, bchar, achar);
                    }
                    if self.open {
                        self.path.close();
                        self.open = false;
                    }
                    self.done = true;
                    return;
                }
                12 => {
                    let op = code.get(i).copied().unwrap_or(0);
                    i += 1;
                    self.escape(op);
                }
                _ => self.stack.clear(),
            }
            if self.stack.len() > 96 {
                self.stack.clear();
            }
        }
    }

    fn seac(&mut self, adx: f64, ady: f64, bchar: u8, achar: u8) {
        let sid_to_gid: HashMap<u16, usize> = self
            .cff
            .charset
            .iter()
            .enumerate()
            .map(|(g, s)| (*s, g))
            .collect();
        let find = |c: u8| sid_to_gid.get(&standard_encoding_sid(c)).copied();
        if self.open {
            self.path.close();
            self.open = false;
        }
        if let Some(g) = find(bchar).and_then(|g| self.cff.glyph(g)) {
            self.path.append(&g.path);
        }
        if let Some(g) = find(achar).and_then(|g| self.cff.glyph(g)) {
            self.path
                .append(&g.path.transform(&oc_raster::Transform::translate(adx, ady)));
        }
    }

    fn escape(&mut self, op: u8) {
        let st = &mut self.stack;
        match op {
            3 => {
                let (b, a) = (st.pop().unwrap_or(0.0), st.pop().unwrap_or(0.0));
                st.push(f64::from(u8::from(a != 0.0 && b != 0.0)));
            }
            4 => {
                let (b, a) = (st.pop().unwrap_or(0.0), st.pop().unwrap_or(0.0));
                st.push(f64::from(u8::from(a != 0.0 || b != 0.0)));
            }
            5 => {
                let a = st.pop().unwrap_or(0.0);
                st.push(f64::from(u8::from(a == 0.0)));
            }
            9 => {
                let a = st.pop().unwrap_or(0.0);
                st.push(a.abs());
            }
            10 => {
                let (b, a) = (st.pop().unwrap_or(0.0), st.pop().unwrap_or(0.0));
                st.push(a + b);
            }
            11 => {
                let (b, a) = (st.pop().unwrap_or(0.0), st.pop().unwrap_or(0.0));
                st.push(a - b);
            }
            12 => {
                let (b, a) = (st.pop().unwrap_or(1.0), st.pop().unwrap_or(0.0));
                st.push(if b == 0.0 { 0.0 } else { a / b });
            }
            14 => {
                let a = st.pop().unwrap_or(0.0);
                st.push(-a);
            }
            15 => {
                let (b, a) = (st.pop().unwrap_or(0.0), st.pop().unwrap_or(0.0));
                st.push(f64::from(u8::from(a == b)));
            }
            18 => {
                st.pop();
            }
            20 => {
                let (i, v) = (st.pop().unwrap_or(0.0), st.pop().unwrap_or(0.0));
                if let Some(slot) = self.transient.get_mut(i as usize) {
                    *slot = v;
                }
            }
            21 => {
                let i = st.pop().unwrap_or(0.0);
                st.push(self.transient.get(i as usize).copied().unwrap_or(0.0));
            }
            22 => {
                let (v2, v1, s2, s1) = (
                    st.pop().unwrap_or(0.0),
                    st.pop().unwrap_or(0.0),
                    st.pop().unwrap_or(0.0),
                    st.pop().unwrap_or(0.0),
                );
                st.push(if v1 <= v2 { s1 } else { s2 });
            }
            23 => st.push(0.5),
            24 => {
                let (b, a) = (st.pop().unwrap_or(0.0), st.pop().unwrap_or(0.0));
                st.push(a * b);
            }
            26 => {
                let a = st.pop().unwrap_or(0.0);
                st.push(a.max(0.0).sqrt());
            }
            27 => {
                let a = st.last().copied().unwrap_or(0.0);
                st.push(a);
            }
            28 => {
                let n = st.len();
                if n >= 2 {
                    st.swap(n - 1, n - 2);
                }
            }
            29 => {
                let i = st.pop().unwrap_or(0.0);
                let n = st.len();
                if n > 0 {
                    let k = if i < 0.0 { 0 } else { (i as usize).min(n - 1) };
                    st.push(st[n - 1 - k]);
                }
            }
            30 => {
                let (j, n) = (
                    st.pop().unwrap_or(0.0) as i64,
                    st.pop().unwrap_or(0.0) as i64,
                );
                let len = st.len();
                if n > 0 && (n as usize) <= len {
                    let start = len - n as usize;
                    let j = j.rem_euclid(n) as usize;
                    st[start..].rotate_right(j);
                }
            }
            34 => {
                // hflex
                let s = std::mem::take(&mut self.stack);
                if s.len() >= 7 {
                    self.curve([s[0], 0.0, s[1], s[2], s[3], 0.0]);
                    self.curve([s[4], 0.0, s[5], -s[2], s[6], 0.0]);
                }
            }
            35 => {
                let s = std::mem::take(&mut self.stack);
                if s.len() >= 12 {
                    self.curve([s[0], s[1], s[2], s[3], s[4], s[5]]);
                    self.curve([s[6], s[7], s[8], s[9], s[10], s[11]]);
                }
            }
            36 => {
                // hflex1
                let s = std::mem::take(&mut self.stack);
                if s.len() >= 9 {
                    self.curve([s[0], s[1], s[2], s[3], s[4], 0.0]);
                    self.curve([s[5], 0.0, s[6], s[7], s[8], -(s[1] + s[3] + s[7])]);
                }
            }
            37 => {
                // flex1
                let s = std::mem::take(&mut self.stack);
                if s.len() >= 11 {
                    let dx = s[0] + s[2] + s[4] + s[6] + s[8];
                    let dy = s[1] + s[3] + s[5] + s[7] + s[9];
                    self.curve([s[0], s[1], s[2], s[3], s[4], s[5]]);
                    if dx.abs() > dy.abs() {
                        self.curve([s[6], s[7], s[8], s[9], s[10], -dy]);
                    } else {
                        self.curve([s[6], s[7], s[8], s[9], -dx, s[10]]);
                    }
                }
            }
            _ => st.clear(),
        }
    }
}
