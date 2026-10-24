//! Documents to PDF: a small layout engine (paragraphs, headings, lists, quotes, code, tables,
//! pictures, links, bookmarks) and a writer that embeds TrueType subsets with a text layer.
//!
//! Text runs left to right in one font per character (with fallback faces for coverage); there is
//! no complex-script shaping, no right-to-left reordering and no hyphenation.
#![allow(
    clippy::indexing_slicing,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::while_let_loop
)]
use oc_core::limits::Limits;
use oc_core::options::Options;
use oc_core::{Error, ErrorCode, Note, Notes, Result, Severity};
use oc_document::model::{plain, Align, Block, Cell, Document, Inline, ListItem};
use oc_font::db::FontDb;
use oc_font::Font;
use oc_pdf::object::{Dict, Obj};
use oc_pdf::writer::{self, dict, name, reals, text_string, Builder, Source};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

const MAX_PAGES: usize = 20_000;
const LEADING: f64 = 1.3;
const ASC: f64 = 0.98;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Role {
    Serif,
    Sans,
    Mono,
}

struct Face {
    font: Arc<Font>,
    kerns: HashMap<(u16, u16), i16>,
    used: BTreeMap<u16, char>,
    bold: bool,
    italic: bool,
    mono: bool,
    ps_name: String,
}

struct Fonts {
    db: FontDb,
    faces: Vec<Face>,
    chains: HashMap<(Role, bool, bool), Vec<usize>>,
    custom: Option<String>,
    body: Role,
    missing: u32,
    complex: bool,
}

fn usable(f: &Font) -> bool {
    if f.is_cff || f.table(b"glyf").is_none() || f.table(b"loca").is_none() {
        return false;
    }
    // OS/2 fsType: bit 1 forbids embedding
    match f.table(b"OS/2") {
        Some(t) if t.len() >= 10 => u16::from_be_bytes([t[8], t[9]]) & 0x0002 == 0,
        _ => true,
    }
}

fn families(role: Role) -> &'static [&'static str] {
    match role {
        Role::Serif => &[
            "liberation serif",
            "dejavu serif",
            "times new roman",
            "times",
            "georgia",
            "noto serif",
            "freeserif",
        ],
        Role::Sans => &[
            "liberation sans",
            "dejavu sans",
            "arial",
            "helvetica",
            "noto sans",
            "freesans",
        ],
        Role::Mono => &[
            "liberation mono",
            "dejavu sans mono",
            "courier new",
            "courier",
            "noto sans mono",
            "freemono",
        ],
    }
}

impl Fonts {
    fn intern(&mut self, f: Arc<Font>, bold: bool, italic: bool) -> usize {
        if let Some(i) = self.faces.iter().position(|x| Arc::ptr_eq(&x.font, &f)) {
            return i;
        }
        let kerns = oc_font::render::kern_pairs(&f);
        let ps = f
            .name(6)
            .unwrap_or_else(|| "Font".to_string())
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>();
        let mono = f.is_monospace();
        self.faces.push(Face {
            font: f,
            kerns,
            used: BTreeMap::new(),
            bold,
            italic,
            mono,
            ps_name: if ps.is_empty() { "Font".into() } else { ps },
        });
        self.faces.len() - 1
    }

    fn pick(&mut self, fams: &[String], bold: bool, italic: bool) -> Option<usize> {
        let w = if bold { 700 } else { 400 };
        let found = self.db.find(fams, w, italic)?;
        if usable(&found.font) {
            return Some(self.intern(found.font, bold, italic));
        }
        // the best match cannot be embedded; try the common free families
        for f in [
            "dejavu sans",
            "liberation sans",
            "freesans",
            "noto sans",
            "dejavu serif",
        ] {
            if let Some(found) = self.db.find(&[f.to_string()], w, italic) {
                if usable(&found.font) {
                    return Some(self.intern(found.font, bold, italic));
                }
            }
        }
        None
    }

    fn chain(&mut self, role: Role, bold: bool, italic: bool) -> Result<Vec<usize>> {
        if let Some(c) = self.chains.get(&(role, bold, italic)) {
            return Ok(c.clone());
        }
        let mut fams: Vec<String> = Vec::new();
        if role == self.body {
            if let Some(c) = &self.custom {
                fams.push(c.clone());
            }
        }
        fams.extend(families(role).iter().map(|s| s.to_string()));
        let primary = self.pick(&fams, bold, italic).ok_or_else(|| {
            Error::new(
                ErrorCode::UnsupportedFormat,
                "no TrueType font that may be embedded was found on this computer",
            )
            .with_detail("install a font such as Liberation, DejaVu or Noto and try again")
        })?;
        let mut chain = vec![primary];
        for fb in ["dejavu sans", "noto sans", "freesans", "liberation sans"] {
            if let Some(i) = self.pick(&[fb.to_string()], bold, italic) {
                if !chain.contains(&i) {
                    chain.push(i);
                }
            }
        }
        self.chains.insert((role, bold, italic), chain.clone());
        Ok(chain)
    }

    fn glyph(&mut self, chain: &[usize], c: char) -> (usize, u16) {
        for &f in chain {
            if let Some(g) = self.faces[f].font.glyph_for_char(c) {
                if g != 0 {
                    return (f, g);
                }
            }
        }
        self.missing += 1;
        (chain[0], 0)
    }
}

#[derive(Clone, Default, PartialEq)]
struct St {
    bold: bool,
    italic: bool,
    mono: bool,
    sans: bool,
    ul: bool,
    strike: bool,
    sup: bool,
    sub: bool,
    link: Option<usize>,
}

#[derive(Clone)]
struct Piece {
    face: usize,
    /// glyph, kerning before it (font units), character
    glyphs: Vec<(u16, i32, char)>,
    width: f64,
}

#[derive(Clone)]
enum Atom {
    Text {
        text: String,
        pieces: Vec<Piece>,
        st: St,
        size: f64,
        width: f64,
        space: bool,
    },
    Break,
    Image {
        idx: usize,
        w: f64,
        h: f64,
    },
}

impl Atom {
    fn width(&self) -> f64 {
        match self {
            Atom::Text { width, .. } => *width,
            Atom::Image { w, .. } => *w,
            Atom::Break => 0.0,
        }
    }
    fn is_space(&self) -> bool {
        matches!(self, Atom::Text { space: true, .. })
    }
}

struct Liner {
    lines: Vec<Line>,
    cur: Vec<(Atom, f64)>,
    x: f64,
    avail: f64,
    width: f64,
    base: f64,
}

impl Liner {
    fn push(&mut self, a: Atom) {
        let w = a.width();
        self.cur.push((a, self.x));
        self.x += w;
    }

    fn finish(&mut self) {
        while self.cur.last().is_some_and(|(a, _)| a.is_space()) {
            self.cur.pop();
        }
        let w = self.cur.last().map_or(0.0, |(a, ax)| ax + a.width());
        let items = std::mem::take(&mut self.cur);
        self.lines.push(L::make_line(items, w, self.base));
        self.x = 0.0;
        self.avail = self.width;
    }
}

struct Line {
    items: Vec<(Atom, f64)>,
    width: f64,
    asc: f64,
    desc: f64,
}

impl Line {
    fn height(&self) -> f64 {
        self.asc + self.desc
    }
}

#[derive(Default)]
struct Page {
    ops: String,
    fonts: BTreeSet<usize>,
    images: BTreeSet<usize>,
    links: Vec<([f64; 4], usize)>,
    color: u8,
}

struct Node {
    title: String,
    page: usize,
    y: f64,
    parent: Option<usize>,
}

struct L<'a> {
    fonts: Fonts,
    doc: &'a Document,
    limits: &'a Limits,
    b: Builder,
    pages: Vec<Page>,
    pw: f64,
    ph: f64,
    m: f64,
    x0: f64,
    width: f64,
    y: f64,
    size: f64,
    links: Vec<String>,
    outline: Vec<Node>,
    heading_stack: Vec<(u8, usize)>,
    image_cache: HashMap<String, Option<(usize, f64, f64)>>,
    image_objs: Vec<Obj>,
    bars: Vec<(f64, f64)>,
    marker: Option<(String, f64)>,
    notes: Notes,
    errors: Option<Error>,
}

fn is_complex(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x0900..=0x0DFF)
}

impl<'a> L<'a> {
    fn top(&self) -> f64 {
        self.ph - self.m
    }
    fn bottom(&self) -> f64 {
        self.m
    }
    fn cur(&mut self) -> &mut Page {
        if self.pages.is_empty() {
            self.pages.push(Page::default());
            self.y = self.top();
        }
        let n = self.pages.len() - 1;
        &mut self.pages[n]
    }

    fn flush_bars(&mut self, to_y: f64) {
        let bars = self.bars.clone();
        for (x, start) in bars {
            if start > to_y {
                let (w, h) = (2.0, start - to_y);
                let s = format!("0.78 g {:.2} {:.2} {w:.2} {h:.2} re f\n", x, to_y);
                let p = self.cur();
                p.ops.push_str(&s);
                p.color = 255;
            }
        }
    }

    fn new_page(&mut self) {
        if self.pages.is_empty() {
            self.cur();
            return;
        }
        let y = self.y;
        self.flush_bars(y);
        if self.pages.len() >= MAX_PAGES {
            self.errors.get_or_insert_with(|| {
                Error::new(
                    ErrorCode::LimitExceeded,
                    "the document is too long for one PDF",
                )
            });
            return;
        }
        self.pages.push(Page::default());
        self.y = self.top();
        let top = self.top();
        for b in &mut self.bars {
            b.1 = top;
        }
    }

    fn ensure(&mut self, h: f64) {
        self.cur();
        if self.y - h < self.bottom() - 0.01 && self.y < self.top() - 0.01 {
            self.new_page();
        }
    }

    fn gap(&mut self, h: f64) {
        if self.pages.is_empty() || self.y >= self.top() - 0.01 {
            return;
        }
        self.y -= h;
    }

    fn set_color(&mut self, c: u8) {
        let p = self.cur();
        if p.color != c {
            p.ops.push_str(match c {
                1 => "0.02 0.39 0.76 rg\n",
                255 => "0 g\n",
                _ => "0 g\n",
            });
            p.color = if c == 255 { 0 } else { c };
        }
    }

    // ---- shaping ----

    fn role(&self, st: &St) -> Role {
        if st.mono {
            Role::Mono
        } else if st.sans {
            Role::Sans
        } else {
            self.fonts.body
        }
    }

    fn shape(&mut self, text: &str, st: &St, size: f64) -> Result<(Vec<Piece>, f64)> {
        let chain = self.fonts.chain(self.role(st), st.bold, st.italic)?;
        let mut pieces: Vec<Piece> = Vec::new();
        let mut total = 0.0;
        let mut prev: Option<(usize, u16)> = None;
        for c in text.chars() {
            if matches!(c as u32, 0x200B..=0x200F | 0xFEFF | 0xAD | 0x202A..=0x202E | 0x2066..=0x2069)
            {
                continue;
            }
            if is_complex(c) {
                self.fonts.complex = true;
            }
            let c = if c == '\t'
                || c == '\n'
                || c == '\r'
                || c == '\u{a0}'
                || c == '\u{2007}'
                || c == '\u{202f}'
            {
                ' '
            } else {
                c
            };
            let (f, g) = self.fonts.glyph(&chain, c);
            let face = &self.fonts.faces[f];
            let upem = f64::from(face.font.units_per_em.max(1));
            let kern = match prev {
                Some((pf, pg)) if pf == f => {
                    i32::from(face.kerns.get(&(pg, g)).copied().unwrap_or(0))
                }
                _ => 0,
            };
            let adv = f64::from(face.font.advance(g)) + f64::from(kern);
            let w = adv * size / upem;
            total += w;
            match pieces.last_mut() {
                Some(p) if p.face == f => {
                    p.glyphs.push((g, kern, c));
                    p.width += w;
                }
                _ => pieces.push(Piece {
                    face: f,
                    glyphs: vec![(g, 0, c)],
                    width: w,
                }),
            }
            prev = Some((f, g));
        }
        Ok((pieces, total))
    }

    fn text_atom(&mut self, text: &str, st: &St, size: f64, space: bool) -> Result<Atom> {
        let size = if st.sup || st.sub { size * 0.7 } else { size };
        let (pieces, width) = self.shape(text, st, size)?;
        Ok(Atom::Text {
            text: text.to_string(),
            pieces,
            st: st.clone(),
            size,
            width,
            space,
        })
    }

    // ---- inline flattening ----

    fn flatten(&mut self, inl: &[Inline], st: &St, size: f64, out: &mut Vec<Atom>) -> Result<()> {
        for i in inl {
            match i {
                Inline::Text(t) => self.words(t, st, size, out)?,
                Inline::Emph(c) => self.flatten(
                    c,
                    &St {
                        italic: true,
                        ..st.clone()
                    },
                    size,
                    out,
                )?,
                Inline::Strong(c) => self.flatten(
                    c,
                    &St {
                        bold: true,
                        ..st.clone()
                    },
                    size,
                    out,
                )?,
                Inline::Strike(c) => self.flatten(
                    c,
                    &St {
                        strike: true,
                        ..st.clone()
                    },
                    size,
                    out,
                )?,
                Inline::Underline(c) => self.flatten(
                    c,
                    &St {
                        ul: true,
                        ..st.clone()
                    },
                    size,
                    out,
                )?,
                Inline::Sup(c) => self.flatten(
                    c,
                    &St {
                        sup: true,
                        sub: false,
                        ..st.clone()
                    },
                    size,
                    out,
                )?,
                Inline::Sub(c) => self.flatten(
                    c,
                    &St {
                        sub: true,
                        sup: false,
                        ..st.clone()
                    },
                    size,
                    out,
                )?,
                Inline::Code(t) => {
                    let s = St {
                        mono: true,
                        ..st.clone()
                    };
                    self.words(t, &s, size * 0.92, out)?
                }
                Inline::Link { url, content, .. } => {
                    self.links.push(url.clone());
                    let s = St {
                        link: Some(self.links.len() - 1),
                        ul: true,
                        ..st.clone()
                    };
                    self.flatten(content, &s, size, out)?;
                }
                Inline::Image { src, alt, .. } => match self.image(src) {
                    Some((idx, w, h)) => out.push(Atom::Image { idx, w, h }),
                    None => {
                        let a = plain(alt);
                        if !a.is_empty() {
                            self.words(
                                &a,
                                &St {
                                    italic: true,
                                    ..st.clone()
                                },
                                size,
                                out,
                            )?;
                        }
                    }
                },
                Inline::SoftBreak => self.words(" ", st, size, out)?,
                Inline::LineBreak => out.push(Atom::Break),
                Inline::Html(_) => {}
            }
        }
        Ok(())
    }

    fn words(&mut self, t: &str, st: &St, size: f64, out: &mut Vec<Atom>) -> Result<()> {
        let mut word = String::new();
        let mut spaces = String::new();
        for c in t.chars() {
            if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                if !word.is_empty() {
                    let w = std::mem::take(&mut word);
                    out.push(self.text_atom(&w, st, size, false)?);
                }
                spaces.push(' ');
            } else {
                if !spaces.is_empty() {
                    // a run of spaces is one breakable space
                    spaces.clear();
                    out.push(self.text_atom(" ", st, size, true)?);
                }
                word.push(c);
            }
        }
        if !word.is_empty() {
            out.push(self.text_atom(&word, st, size, false)?);
        }
        if !spaces.is_empty() {
            out.push(self.text_atom(" ", st, size, true)?);
        }
        Ok(())
    }

    // ---- pictures ----

    fn image(&mut self, src: &str) -> Option<(usize, f64, f64)> {
        if let Some(c) = self.image_cache.get(src) {
            return *c;
        }
        let res = self.load_image(src);
        self.image_cache.insert(src.to_string(), res);
        res
    }

    fn load_image(&mut self, src: &str) -> Option<(usize, f64, f64)> {
        let img = self.doc.image(src)?;
        let source = match img.mime.as_str() {
            "image/jpeg" => {
                let probe = Builder::new();
                drop(probe);
                Source::Jpeg(img.data.clone(), None)
            }
            "image/png" | "image/gif" | "image/webp" | "image/bmp" => {
                let fmt = match img.mime.as_str() {
                    "image/png" => crate::image::PNG,
                    "image/gif" => crate::image::GIF,
                    "image/webp" => crate::image::WEBP,
                    _ => crate::image::BMP,
                };
                match crate::image::decode(fmt, &img.data, self.limits, &oc_core::NoProgress) {
                    Ok((im, _)) => Source::Raster(im),
                    Err(_) => {
                        self.notes.push(Note::new(
                            Severity::Loss,
                            "pdf.image_unreadable",
                            "a picture could not be decoded and was left out",
                        ));
                        return None;
                    }
                }
            }
            _ => {
                self.notes.push(Note::new(
                    Severity::Loss,
                    "pdf.image_format",
                    format!("a picture in {} format was left out", img.mime),
                ));
                return None;
            }
        };
        let (obj, w, h, dpi) = match writer::add_image_source(&mut self.b, source) {
            Ok(r) => r,
            Err(_) => {
                // 12-bit or CMYK-less oddities: decode and store as pixels
                let fmt = crate::image::JPEG;
                match crate::image::decode(fmt, &img.data, self.limits, &oc_core::NoProgress)
                    .ok()
                    .and_then(|(im, _)| {
                        writer::add_image_source(&mut self.b, Source::Raster(im)).ok()
                    }) {
                    Some(r) => r,
                    None => {
                        self.notes.push(Note::new(
                            Severity::Loss,
                            "pdf.image_unreadable",
                            "a picture could not be stored and was left out",
                        ));
                        return None;
                    }
                }
            }
        };
        let dpi = dpi.filter(|d| (10.0..=2400.0).contains(d)).unwrap_or(96.0);
        let (nw, nh) = (f64::from(w) * 72.0 / dpi, f64::from(h) * 72.0 / dpi);
        self.image_objs.push(obj);
        Some((self.image_objs.len() - 1, nw, nh))
    }

    // ---- line breaking ----

    fn split_word(&mut self, a: &Atom, avail: f64) -> Result<(Atom, Option<Atom>)> {
        let Atom::Text { text, st, size, .. } = a else {
            return Ok((a.clone(), None));
        };
        let chars: Vec<char> = text.chars().collect();
        let base = if st.sup || st.sub { *size / 0.7 } else { *size };
        let (mut lo, mut hi) = (1usize, chars.len());
        // largest prefix that fits, at least one character
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let s: String = chars[..mid].iter().collect();
            let (_, w) = self.shape(&s, st, base * if st.sup || st.sub { 0.7 } else { 1.0 })?;
            if w <= avail {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let head: String = chars[..lo].iter().collect();
        let rest: String = chars[lo..].iter().collect();
        let h = self.text_atom(&head, st, base, false)?;
        let r = if rest.is_empty() {
            None
        } else {
            Some(self.text_atom(&rest, st, base, false)?)
        };
        Ok((h, r))
    }

    fn make_line(items: Vec<(Atom, f64)>, width: f64, base: f64) -> Line {
        let mut asc = base * ASC;
        let mut desc = base * (LEADING - ASC);
        let mut any_text = false;
        for (a, _) in &items {
            match a {
                Atom::Text { size, st, .. } => {
                    any_text = true;
                    let lift = if st.sup { *size * 0.5 } else { 0.0 };
                    asc = asc.max(*size * ASC + lift);
                    desc = desc.max(*size * (LEADING - ASC));
                }
                Atom::Image { h, .. } => asc = asc.max(*h),
                Atom::Break => {}
            }
        }
        if !any_text && items.is_empty() {
            asc = base * ASC;
        }
        Line {
            items,
            width,
            asc,
            desc,
        }
    }

    /// Break atoms into lines no wider than `width` (the first one `first_width`).
    fn flow(
        &mut self,
        atoms: Vec<Atom>,
        width: f64,
        first_width: f64,
        base: f64,
    ) -> Result<Vec<Line>> {
        let mut lw = Liner {
            lines: Vec::new(),
            cur: Vec::new(),
            x: 0.0,
            avail: first_width,
            width,
            base,
        };
        let mut pending: Vec<Atom> = Vec::new();
        let mut space: Option<Atom> = None;
        for a in atoms {
            match &a {
                Atom::Break => {
                    self.cluster(&mut lw, std::mem::take(&mut pending), space.take())?;
                    lw.finish();
                }
                _ if a.is_space() => {
                    self.cluster(&mut lw, std::mem::take(&mut pending), space.take())?;
                    space = Some(a);
                }
                _ => pending.push(a),
            }
        }
        self.cluster(&mut lw, pending, space.take())?;
        if !lw.cur.is_empty() || lw.lines.is_empty() {
            lw.finish();
        }
        Ok(lw.lines)
    }

    fn cluster(&mut self, lw: &mut Liner, cluster: Vec<Atom>, space: Option<Atom>) -> Result<()> {
        if cluster.is_empty() {
            return Ok(());
        }
        let cw: f64 = cluster.iter().map(Atom::width).sum();
        let sw = space.as_ref().map_or(0.0, Atom::width);
        if !lw.cur.is_empty() {
            if lw.x + sw + cw > lw.avail + 0.01 {
                lw.finish();
            } else if let Some(s) = space {
                lw.push(s);
            }
        }
        let mut rest: Vec<Atom> = cluster;
        rest.reverse();
        while let Some(a) = rest.pop() {
            let w = a.width();
            if lw.x + w <= lw.avail + 0.01 {
                lw.push(a);
            } else if matches!(a, Atom::Text { .. }) && w > lw.width {
                // wider than any line: cut the word where it fits
                let room = lw.avail - lw.x;
                if room < 1.0 && !lw.cur.is_empty() {
                    lw.finish();
                    rest.push(a);
                    continue;
                }
                let (h, r) = self.split_word(&a, room.max(1.0))?;
                lw.push(h);
                if let Some(r) = r {
                    lw.finish();
                    rest.push(r);
                }
            } else if !lw.cur.is_empty() {
                lw.finish();
                rest.push(a);
            } else {
                lw.push(a);
            }
        }
        Ok(())
    }

    // ---- drawing ----

    fn draw_line(&mut self, line: &Line, x0: f64, y_top: f64, avail: f64, align: &Align) {
        let off = match align {
            Align::Center => ((avail - line.width) / 2.0).max(0.0),
            Align::Right => (avail - line.width).max(0.0),
            _ => 0.0,
        };
        let base_y = y_top - line.asc;
        for (a, ax) in &line.items {
            let x = x0 + off + ax;
            match a {
                Atom::Text {
                    pieces,
                    st,
                    size,
                    width,
                    space,
                    ..
                } => {
                    let (dy, size) = if st.sup {
                        (*size / 0.7 * 0.35, *size)
                    } else if st.sub {
                        (-(*size / 0.7) * 0.12, *size)
                    } else {
                        (0.0, *size)
                    };
                    let y = base_y + dy;
                    self.set_color(u8::from(st.link.is_some()));
                    let mut px = x;
                    for p in pieces {
                        self.draw_piece(p, px, y, size);
                        px += p.width;
                    }
                    if !*space || st.ul || st.strike {
                        let th = (size * 0.06).max(0.4);
                        if st.ul {
                            let s =
                                format!("{x:.2} {:.2} {width:.2} {th:.2} re f\n", y - size * 0.12);
                            self.cur().ops.push_str(&s);
                        }
                        if st.strike {
                            let s =
                                format!("{x:.2} {:.2} {width:.2} {th:.2} re f\n", y + size * 0.3);
                            self.cur().ops.push_str(&s);
                        }
                    }
                    if let Some(l) = st.link {
                        let r = [x, y - size * 0.25, x + width, y + size * 0.85];
                        self.cur().links.push((r, l));
                    }
                }
                Atom::Image { idx, w, h } => {
                    let s = format!("q {w:.2} 0 0 {h:.2} {x:.2} {base_y:.2} cm /Im{idx} Do Q\n");
                    let p = self.cur();
                    p.ops.push_str(&s);
                    p.images.insert(*idx);
                }
                Atom::Break => {}
            }
        }
        self.set_color(255);
    }

    fn draw_piece(&mut self, p: &Piece, x: f64, y: f64, size: f64) {
        let upem = f64::from(self.fonts.faces[p.face].font.units_per_em.max(1));
        let mut s = format!("BT /F{} {size:.2} Tf {x:.2} {y:.2} Td [", p.face);
        let mut open = false;
        for (g, kern, ch) in &p.glyphs {
            if *kern != 0 {
                if open {
                    s.push('>');
                    open = false;
                }
                s.push_str(&format!(" {:.1} ", -f64::from(*kern) * 1000.0 / upem));
            }
            if !open {
                s.push('<');
                open = true;
            }
            s.push_str(&format!("{g:04x}"));
            if *g != 0 {
                self.fonts.faces[p.face].used.entry(*g).or_insert(*ch);
            }
        }
        if open {
            s.push('>');
        }
        s.push_str("] TJ ET\n");
        let pg = self.cur();
        pg.fonts.insert(p.face);
        pg.ops.push_str(&s);
    }

    fn take_marker_line(&mut self, line: &Line, y_top: f64) {
        if let Some((m, _)) = self.marker.take() {
            let st = St::default();
            if let Ok(a) = self.text_atom(&m, &st, self.size, false) {
                let w = a.width();
                let l = Line {
                    items: vec![(a, 0.0)],
                    width: w,
                    asc: line.asc,
                    desc: line.desc,
                };
                let x = self.x0 - 5.0 - w;
                self.draw_line(&l, x, y_top, w, &Align::Left);
            }
        }
    }

    /// Place lines one by one, starting new pages as needed.
    fn place(&mut self, lines: Vec<Line>, align: &Align, keep_next: f64) {
        let n = lines.len();
        for (i, line) in lines.iter().enumerate() {
            let h = line.height();
            // keep a heading with what follows; avoid a lone first line at the bottom
            let extra = if i == 0 && n > 1 {
                lines[1].height()
            } else {
                0.0
            } + if i == 0 { keep_next } else { 0.0 };
            self.ensure(h + extra);
            if self.errors.is_some() {
                return;
            }
            let y_top = self.y;
            if self.marker.is_some() {
                self.take_marker_line(line, y_top);
            }
            let (x0, w) = (self.x0, self.width);
            self.draw_line(line, x0, y_top, w, align);
            self.y -= h;
        }
    }

    // ---- blocks ----

    fn paragraph(
        &mut self,
        inl: &[Inline],
        st: &St,
        size: f64,
        align: &Align,
        keep_next: f64,
    ) -> Result<()> {
        let mut atoms = Vec::new();
        self.flatten(inl, st, size, &mut atoms)?;
        // a picture alone on its line is centred and fitted to the page
        let only_image = atoms.iter().all(|a| matches!(a, Atom::Image { .. })) && !atoms.is_empty();
        let max_h = (self.top() - self.bottom()) * 0.9;
        let width = self.width;
        for a in &mut atoms {
            if let Atom::Image { w, h, .. } = a {
                let s = (width / *w).min(max_h / *h).min(1.0);
                *w *= s;
                *h *= s;
            }
        }
        let lines = self.flow(atoms, width, width, size)?;
        if lines.iter().all(|l| l.items.is_empty()) && self.marker.is_none() {
            return Ok(());
        }
        let al = if only_image {
            Align::Center
        } else {
            align.clone()
        };
        self.place(lines, &al, keep_next);
        Ok(())
    }

    fn flush_marker(&mut self) {
        if self.marker.is_some() {
            let lh = self.size * LEADING;
            self.ensure(lh);
            let y_top = self.y;
            let line = Line {
                items: vec![],
                width: 0.0,
                asc: self.size * ASC,
                desc: self.size * (LEADING - ASC),
            };
            self.take_marker_line(&line, y_top);
            self.y -= lh;
        }
    }

    fn blocks(&mut self, bs: &[Block], tight: bool) -> Result<()> {
        for (i, b) in bs.iter().enumerate() {
            if i > 0 {
                self.gap(if tight {
                    self.size * 0.25
                } else {
                    self.size * 0.7
                });
            }
            self.block(b)?;
            if let Some(e) = &self.errors {
                return Err(e.clone());
            }
        }
        Ok(())
    }

    fn block(&mut self, b: &Block) -> Result<()> {
        let size = self.size;
        match b {
            Block::Paragraph(c) => self.paragraph(c, &St::default(), size, &Align::Left, 0.0)?,
            Block::Heading { level, content } => {
                self.flush_marker();
                let l = (*level).clamp(1, 6);
                let f = [2.0, 1.55, 1.3, 1.15, 1.05, 1.0][usize::from(l) - 1];
                if self.y < self.top() - 0.01 {
                    self.gap(size * f * 0.55);
                }
                let st = St {
                    bold: true,
                    sans: true,
                    ..St::default()
                };
                let title = plain(content);
                // the bookmark points at the top of the heading, wherever it lands
                self.ensure(size * f * LEADING + size * LEADING * 2.0);
                let (pg, y) = (self.pages.len().saturating_sub(1), self.y);
                if !title.trim().is_empty() {
                    let mut parent = None;
                    while self.heading_stack.last().is_some_and(|(lv, _)| *lv >= l) {
                        self.heading_stack.pop();
                    }
                    if let Some((_, p)) = self.heading_stack.last() {
                        parent = Some(*p);
                    }
                    self.outline.push(Node {
                        title: title.trim().to_string(),
                        page: pg,
                        y,
                        parent,
                    });
                    self.heading_stack.push((l, self.outline.len() - 1));
                }
                self.paragraph(content, &st, size * f, &Align::Left, size * LEADING * 2.0)?;
                self.gap(size * f * 0.25);
            }
            Block::List {
                ordered,
                start,
                items,
                tight,
            } => self.list(*ordered, *start, items, *tight)?,
            Block::Quote(c) => {
                self.flush_marker();
                let old = (self.x0, self.width);
                let ind = 16.0f64.min((self.width - 120.0).max(0.0));
                let bar_x = self.x0 + 3.0;
                self.x0 += ind;
                self.width -= ind;
                self.cur();
                self.bars.push((bar_x, self.y));
                let r = self.blocks(c, false);
                let y = self.y;
                if let Some((x, start)) = self.bars.pop() {
                    if start > y {
                        let s = format!("0.78 g {x:.2} {y:.2} 2 {:.2} re f\n", start - y);
                        let p = self.cur();
                        p.ops.push_str(&s);
                        p.color = 0;
                        p.ops.push_str("0 g\n");
                    }
                }
                self.x0 = old.0;
                self.width = old.1;
                r?;
            }
            Block::Code { text, .. } => {
                self.flush_marker();
                self.code(text)?;
            }
            Block::Rule => {
                self.flush_marker();
                self.ensure(size);
                self.y -= size * 0.5;
                let s = format!(
                    "0.6 g {:.2} {:.2} {:.2} 0.6 re f\n",
                    self.x0, self.y, self.width
                );
                let p = self.cur();
                p.ops.push_str(&s);
                p.ops.push_str("0 g\n");
                self.y -= size * 0.5;
            }
            Block::PageBreak => {
                if self.pages.last().is_some_and(|p| !p.ops.is_empty())
                    && self.y < self.top() - 0.01
                {
                    self.new_page();
                }
            }
            Block::Html(_) => {}
            Block::Table { rows } => {
                self.flush_marker();
                self.table(rows)?;
            }
        }
        Ok(())
    }

    fn list(&mut self, ordered: bool, start: u32, items: &[ListItem], tight: bool) -> Result<()> {
        let size = self.size;
        let (ox, ow) = (self.x0, self.width);
        for (n, it) in items.iter().enumerate() {
            if n > 0 {
                self.gap(if tight { size * 0.2 } else { size * 0.5 });
            }
            let marker = match it.checked {
                Some(true) => "\u{2611}".to_string(),
                Some(false) => "\u{2610}".to_string(),
                None if ordered => format!("{}.", u64::from(start) + n as u64),
                None => "\u{2022}".to_string(),
            };
            let st = St::default();
            let mw = self.shape(&marker, &st, size)?.1;
            let indent = (mw + 10.0).max(20.0).min((ow - 120.0).max(0.0));
            self.marker = Some((marker, mw));
            self.x0 = ox + indent;
            self.width = ow - indent;
            let r = self.blocks(&it.blocks, tight);
            self.flush_marker();
            self.x0 = ox;
            self.width = ow;
            self.marker = None;
            r?;
        }
        Ok(())
    }

    fn code(&mut self, text: &str) -> Result<()> {
        let size = self.size * 0.88;
        let st = St {
            mono: true,
            ..St::default()
        };
        let (_, cw) = self.shape("M", &st, size)?;
        let pad = 5.0;
        let cols = (((self.width - 2.0 * pad) / cw.max(1.0)).floor() as usize).max(8);
        let mut rows: Vec<String> = Vec::new();
        for l in text.trim_end_matches('\n').split('\n') {
            let l = l.replace('\t', "    ");
            let chars: Vec<char> = l.chars().collect();
            if chars.is_empty() {
                rows.push(String::new());
            }
            for ch in chars.chunks(cols) {
                rows.push(ch.iter().collect());
            }
        }
        let lh = size * 1.25;
        let mut i = 0;
        while i < rows.len() {
            self.ensure(lh + 2.0 * pad);
            if self.errors.is_some() {
                return Ok(());
            }
            let room = ((self.y - self.bottom() - 2.0 * pad) / lh).floor().max(1.0) as usize;
            let take = room.min(rows.len() - i);
            let h = take as f64 * lh + 2.0 * pad;
            let s = format!(
                "0.95 g {:.2} {:.2} {:.2} {h:.2} re f\n0 g\n",
                self.x0,
                self.y - h,
                self.width
            );
            self.cur().ops.push_str(&s);
            let mut y = self.y - pad;
            for r in &rows[i..i + take] {
                if !r.is_empty() {
                    let a = self.text_atom(r, &st, size, false)?;
                    let line = Line {
                        items: vec![(a, 0.0)],
                        width: 0.0,
                        asc: size * 0.95,
                        desc: lh - size * 0.95,
                    };
                    let x = self.x0 + pad;
                    self.draw_line(&line, x, y, self.width, &Align::Left);
                }
                y -= lh;
            }
            self.y -= h;
            i += take;
        }
        Ok(())
    }

    fn table(&mut self, rows: &[Vec<Cell>]) -> Result<()> {
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return Ok(());
        }
        let size = self.size * 0.95;
        let pad = 4.0;
        // measure
        let mut desired = vec![0.0f64; cols];
        let mut minimum = vec![0.0f64; cols];
        let mut cells: Vec<Vec<Vec<Atom>>> = Vec::new();
        for r in rows {
            let mut row = Vec::new();
            for (c, cell) in r.iter().enumerate() {
                let st = St {
                    bold: cell.header,
                    ..St::default()
                };
                let mut atoms = Vec::new();
                self.flatten(&cell.content, &st, size, &mut atoms)?;
                let total: f64 = atoms.iter().map(Atom::width).sum();
                let longest = atoms
                    .iter()
                    .filter(|a| !a.is_space())
                    .map(Atom::width)
                    .fold(0.0, f64::max);
                desired[c] = desired[c].max(total);
                minimum[c] = minimum[c].max(longest.min(120.0));
                row.push(atoms);
            }
            cells.push(row);
        }
        let avail = self.width - 2.0 * pad * cols as f64;
        let sum_d: f64 = desired.iter().sum();
        let mut widths: Vec<f64> = if sum_d <= avail {
            desired.clone()
        } else {
            let sum_m: f64 = minimum.iter().sum();
            if sum_m >= avail {
                vec![avail / cols as f64; cols]
            } else {
                let extra = avail - sum_m;
                let want: f64 = desired
                    .iter()
                    .zip(&minimum)
                    .map(|(d, m)| (d - m).max(0.0))
                    .sum();
                desired
                    .iter()
                    .zip(&minimum)
                    .map(|(d, m)| m + extra * (d - m).max(0.0) / want.max(1.0))
                    .collect()
            }
        };
        for w in &mut widths {
            *w = w.max(8.0) + 2.0 * pad;
        }
        let total: f64 = widths.iter().sum();
        if total > self.width {
            let s = self.width / total;
            for w in &mut widths {
                *w *= s;
            }
        }
        // lay out every cell
        let mut laid: Vec<(Vec<Vec<Line>>, f64, bool)> = Vec::new();
        for (ri, r) in cells.into_iter().enumerate() {
            let mut row_lines = Vec::new();
            let mut hmax = size * LEADING;
            for (c, atoms) in r.into_iter().enumerate() {
                let w = (widths[c] - 2.0 * pad).max(4.0);
                let lines = self.flow(atoms, w, w, size)?;
                let h: f64 = lines.iter().map(Line::height).sum();
                hmax = hmax.max(h);
                row_lines.push(lines);
            }
            let header = rows[ri].iter().any(|c| c.header);
            laid.push((row_lines, hmax + 2.0 * pad, header));
        }
        let full = self.top() - self.bottom();
        let header_row = laid.first().filter(|r| r.2).map(|r| r.1);
        let mut first_on_page = true;
        let mut idx = 0;
        while idx < laid.len() {
            let rh = laid[idx].1.min(full);
            let need = rh
                + if idx == 0 {
                    laid.get(1).map_or(0.0, |r| r.1.min(full))
                } else {
                    0.0
                };
            if self.y - need < self.bottom() - 0.01 && self.y < self.top() - 0.01 {
                self.new_page();
                first_on_page = true;
                // repeat the header row on the new page
                if idx > 0 {
                    if let Some(hh) = header_row {
                        self.table_row(
                            &laid[0].0,
                            hh.min(full),
                            &widths,
                            rows.first().map(|r| r.as_slice()),
                            true,
                            pad,
                        )?;
                    }
                }
            }
            let _ = first_on_page;
            if laid[idx].1 > full + 0.01 {
                self.notes.push(Note::new(
                    Severity::Loss,
                    "pdf.table_row_clipped",
                    "a table row is taller than a page; the part that does not fit was cut",
                ));
            }
            let rh = laid[idx].1.min(full);
            self.table_row(
                &laid[idx].0,
                rh,
                &widths,
                rows.get(idx).map(|r| r.as_slice()),
                laid[idx].2,
                pad,
            )?;
            first_on_page = false;
            idx += 1;
        }
        self.gap(size * 0.2);
        Ok(())
    }

    fn table_row(
        &mut self,
        cells: &[Vec<Line>],
        rh: f64,
        widths: &[f64],
        src: Option<&[Cell]>,
        header: bool,
        pad: f64,
    ) -> Result<()> {
        let y_top = self.y;
        let mut x = self.x0;
        for (c, lines) in cells.iter().enumerate() {
            let w = widths[c];
            let bg = if header { "0.92 g" } else { "1 g" };
            let s = format!(
                "{bg} {x:.2} {:.2} {w:.2} {rh:.2} re f\n0.55 G 0.5 w {x:.2} {:.2} {w:.2} {rh:.2} re S\n0 g\n",
                y_top - rh,
                y_top - rh
            );
            self.cur().ops.push_str(&s);
            let align = src
                .and_then(|r| r.get(c))
                .map_or(Align::None, |c| c.align.clone());
            let mut y = y_top - pad;
            for l in lines {
                if y - l.height() < y_top - rh {
                    break;
                }
                self.draw_line(l, x + pad, y, w - 2.0 * pad, &align);
                y -= l.height();
            }
            x += w;
        }
        self.y -= rh;
        Ok(())
    }
}

// ---- fonts into the file ----

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
}

fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(o)?,
        *d.get(o + 1)?,
        *d.get(o + 2)?,
        *d.get(o + 3)?,
    ]))
}

/// A TrueType font with the outlines of unused glyphs emptied (glyph numbers stay the same).
fn subset(font: &Font, keep: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let glyf = font.table(b"glyf")?;
    let loca = font.table(b"loca")?;
    let n = usize::from(font.num_glyphs);
    let off = |i: usize| -> Option<usize> {
        if font.loca_long {
            be32(loca, i * 4).map(|v| v as usize)
        } else {
            be16(loca, i * 2).map(|v| usize::from(v) * 2)
        }
    };
    let range = |g: usize| -> Option<&[u8]> {
        let (a, b) = (off(g)?, off(g + 1)?);
        if b < a {
            return None;
        }
        glyf.get(a..b)
    };
    let mut set: BTreeSet<u16> = keep.clone();
    set.insert(0);
    let mut work: Vec<u16> = set.iter().copied().collect();
    while let Some(g) = work.pop() {
        let Some(d) = range(usize::from(g)) else {
            continue;
        };
        if d.len() >= 10 && i16::from_be_bytes([d[0], d[1]]) < 0 {
            let mut p = 10;
            loop {
                let (Some(flags), Some(comp)) = (be16(d, p), be16(d, p + 2)) else {
                    break;
                };
                if usize::from(comp) < n && set.insert(comp) {
                    work.push(comp);
                }
                p += 4 + if flags & 1 != 0 { 4 } else { 2 };
                if flags & 0x8 != 0 {
                    p += 2;
                } else if flags & 0x40 != 0 {
                    p += 4;
                } else if flags & 0x80 != 0 {
                    p += 8;
                }
                if flags & 0x20 == 0 {
                    break;
                }
            }
        }
    }
    let mut new_glyf: Vec<u8> = Vec::new();
    let mut new_loca: Vec<u8> = Vec::with_capacity((n + 1) * 4);
    for g in 0..n {
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        if set.contains(&(g as u16)) {
            if let Some(d) = range(g) {
                new_glyf.extend_from_slice(d);
                while !new_glyf.len().is_multiple_of(4) {
                    new_glyf.push(0);
                }
            }
        }
    }
    new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
    let mut tables: Vec<(oc_font::sfnt::Tag, Vec<u8>)> = Vec::new();
    for tag in [
        b"head", b"hhea", b"maxp", b"hmtx", b"cvt ", b"fpgm", b"prep",
    ] {
        if let Some(t) = font.table(tag) {
            let mut t = t.to_vec();
            if tag == b"head" && t.len() >= 52 {
                t[50] = 0;
                t[51] = 1; // long loca
            }
            tables.push((*tag, t));
        }
    }
    tables.push((*b"glyf", new_glyf));
    tables.push((*b"loca", new_loca));
    Some(oc_font::build::build_sfnt([0, 1, 0, 0], &mut tables))
}

fn to_unicode(used: &BTreeMap<u16, char>) -> String {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let items: Vec<_> = used.iter().collect();
    for chunk in items.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (g, c) in chunk {
            let mut b = [0u16; 2];
            let u: String = c
                .encode_utf16(&mut b)
                .iter()
                .map(|u| format!("{u:04X}"))
                .collect();
            s.push_str(&format!("<{g:04X}> <{u}>\n"));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

fn font_objects(b: &mut Builder, face: &Face, tag: usize) -> Option<Obj> {
    let f = &face.font;
    let keep: BTreeSet<u16> = face.used.keys().copied().collect();
    let data = subset(f, &keep)?;
    let upem = f64::from(f.units_per_em.max(1));
    let s = |v: f64| (v * 1000.0 / upem).round();
    let head = f.table(b"head")?;
    let bbox = [
        s(f64::from(be16(head, 36)? as i16)),
        s(f64::from(be16(head, 38)? as i16)),
        s(f64::from(be16(head, 40)? as i16)),
        s(f64::from(be16(head, 42)? as i16)),
    ];
    let len = data.len() as i64;
    let ff = b.add_stream(dict(vec![("Length1", Obj::Int(len))]), data, true);
    let mut flags = 32;
    if face.mono {
        flags |= 1;
    }
    if face.italic {
        flags |= 64;
    }
    let tagname: String = {
        let mut n = (tag as u64 + 1).wrapping_mul(2_654_435_761);
        (0..6)
            .map(|_| {
                let c = (b'A' + (n % 26) as u8) as char;
                n /= 26;
                c
            })
            .collect()
    };
    let base = format!("{tagname}+{}", face.ps_name);
    let desc = b.add(Obj::Dict(dict(vec![
        ("Type", name("FontDescriptor")),
        ("FontName", name(&base)),
        ("Flags", Obj::Int(flags)),
        ("FontBBox", reals(&bbox)),
        ("ItalicAngle", Obj::Int(if face.italic { -12 } else { 0 })),
        ("Ascent", Obj::Real(s(f64::from(f.ascent)))),
        ("Descent", Obj::Real(s(f64::from(f.descent)).min(0.0))),
        ("CapHeight", Obj::Real(s(f64::from(f.ascent) * 0.72))),
        ("StemV", Obj::Int(if face.bold { 150 } else { 80 })),
        ("FontFile2", ff),
    ])));
    let mut w = Vec::new();
    for g in face.used.keys() {
        w.push(Obj::Int(i64::from(*g)));
        w.push(Obj::Array(vec![Obj::Real(s(f64::from(f.advance(*g))))]));
    }
    let cid = b.add(Obj::Dict(dict(vec![
        ("Type", name("Font")),
        ("Subtype", name("CIDFontType2")),
        ("BaseFont", name(&base)),
        (
            "CIDSystemInfo",
            Obj::Dict(dict(vec![
                ("Registry", Obj::Str(b"Adobe".to_vec())),
                ("Ordering", Obj::Str(b"Identity".to_vec())),
                ("Supplement", Obj::Int(0)),
            ])),
        ),
        ("FontDescriptor", desc),
        ("DW", Obj::Int(1000)),
        ("W", Obj::Array(w)),
        ("CIDToGIDMap", name("Identity")),
    ])));
    let tu = b.add_stream(Dict::new(), to_unicode(&face.used).into_bytes(), true);
    Some(b.add(Obj::Dict(dict(vec![
        ("Type", name("Font")),
        ("Subtype", name("Type0")),
        ("BaseFont", name(&base)),
        ("Encoding", name("Identity-H")),
        ("DescendantFonts", Obj::Array(vec![cid])),
        ("ToUnicode", tu),
    ]))))
}

pub fn write(
    doc: &Document,
    opts: &Options,
    limits: &Limits,
    notes: &mut Notes,
) -> Result<Vec<u8>> {
    let letter = opts.text("page_size") == Some("letter");
    let (pw, ph) = if letter {
        (612.0, 792.0)
    } else {
        (595.276, 841.89)
    };
    let margin = match opts.float("margin") {
        Some(m) if m > 0.0 => m.min(pw / 3.0),
        _ => 56.7,
    };
    let size = opts.float("font_size").unwrap_or(11.0).clamp(6.0, 48.0);
    let (body, custom) = match opts.text("font").map(str::trim) {
        None | Some("") | Some("serif") => (Role::Serif, None),
        Some("sans") | Some("sans-serif") => (Role::Sans, None),
        Some("mono") | Some("monospace") => (Role::Mono, None),
        Some(other) => (Role::Serif, Some(other.to_lowercase())),
    };
    let mut l = L {
        fonts: Fonts {
            db: FontDb::system(),
            faces: Vec::new(),
            chains: HashMap::new(),
            custom,
            body,
            missing: 0,
            complex: false,
        },
        doc,
        limits,
        b: Builder::new(),
        pages: Vec::new(),
        pw,
        ph,
        m: margin,
        x0: margin,
        width: pw - 2.0 * margin,
        y: ph - margin,
        size,
        links: Vec::new(),
        outline: Vec::new(),
        heading_stack: Vec::new(),
        image_cache: HashMap::new(),
        image_objs: Vec::new(),
        bars: Vec::new(),
        marker: None,
        notes: Notes::new(),
        errors: None,
    };
    // the title is a heading of its own only when the document has no first heading
    l.cur();
    l.blocks(&doc.blocks, false)?;
    if let Some(e) = l.errors.take() {
        return Err(e);
    }
    let end_y = l.y;
    l.flush_bars(end_y);
    notes.extend(std::mem::take(&mut l.notes));
    if l.fonts.missing > 0 {
        notes.push(Note::new(
            Severity::Loss,
            "pdf.missing_glyphs",
            format!(
                "{} character(s) have no glyph in the available fonts and show as blanks",
                l.fonts.missing
            ),
        ));
    }
    if l.fonts.complex {
        notes.push(Note::new(
            Severity::Warning,
            "pdf.complex_script",
            "text in right-to-left or joining scripts (Arabic, Hebrew, Indic) is laid out left to right without shaping",
        ));
    }
    finish(l, doc)
}

fn finish(mut l: L, doc: &Document) -> Result<Vec<u8>> {
    let catalog = l.b.reserve();
    let pages_node = l.b.reserve();
    // fonts
    let mut font_refs: HashMap<usize, Obj> = HashMap::new();
    for i in 0..l.fonts.faces.len() {
        if l.fonts.faces[i].used.is_empty() {
            continue;
        }
        let face = &l.fonts.faces[i];
        let r = font_objects(&mut l.b, face, i)
            .ok_or_else(|| Error::new(ErrorCode::CorruptFile, "a font could not be embedded"))?;
        font_refs.insert(i, r);
    }
    let mut page_refs = Vec::new();
    let mut page_nums: Vec<u32> = Vec::new();
    let pages = std::mem::take(&mut l.pages);
    for p in &pages {
        let cs =
            l.b.add_stream(Dict::new(), p.ops.clone().into_bytes(), true);
        let mut res = Dict::new();
        let mut fd = Dict::new();
        for f in &p.fonts {
            if let Some(r) = font_refs.get(f) {
                fd.insert(format!("F{f}"), r.clone());
            }
        }
        res.insert("Font".into(), Obj::Dict(fd));
        if !p.images.is_empty() {
            let mut xd = Dict::new();
            for i in &p.images {
                xd.insert(format!("Im{i}"), l.image_objs[*i].clone());
            }
            res.insert("XObject".into(), Obj::Dict(xd));
        }
        let mut annots = Vec::new();
        for (r, li) in &p.links {
            let url = &l.links[*li];
            let a = l.b.add(Obj::Dict(dict(vec![
                ("Type", name("Annot")),
                ("Subtype", name("Link")),
                ("Rect", reals(r)),
                (
                    "Border",
                    Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(0)]),
                ),
                (
                    "A",
                    Obj::Dict(dict(vec![
                        ("S", name("URI")),
                        (
                            "URI",
                            Obj::Str(
                                url.as_bytes()
                                    .iter()
                                    .copied()
                                    .filter(|b| (0x20..0x7f).contains(b))
                                    .collect(),
                            ),
                        ),
                    ])),
                ),
            ])));
            annots.push(a);
        }
        let mut pd = dict(vec![
            ("Type", name("Page")),
            ("Parent", Obj::Ref(pages_node, 0)),
            ("MediaBox", reals(&[0.0, 0.0, l.pw, l.ph])),
            ("Resources", Obj::Dict(res)),
            ("Contents", cs),
        ]);
        if !annots.is_empty() {
            pd.insert("Annots".into(), Obj::Array(annots));
        }
        let r = l.b.add(Obj::Dict(pd));
        if let Obj::Ref(n, _) = r {
            page_nums.push(n);
        }
        page_refs.push(r);
    }
    let count = page_refs.len() as i64;
    l.b.set(
        pages_node,
        Obj::Dict(dict(vec![
            ("Type", name("Pages")),
            ("Kids", Obj::Array(page_refs)),
            ("Count", Obj::Int(count)),
        ])),
    );
    let mut cat = dict(vec![
        ("Type", name("Catalog")),
        ("Pages", Obj::Ref(pages_node, 0)),
    ]);
    // bookmarks
    if !l.outline.is_empty() {
        let root = l.b.reserve();
        let nums: Vec<u32> = (0..l.outline.len()).map(|_| l.b.reserve()).collect();
        let mut kids: Vec<Vec<usize>> = vec![Vec::new(); l.outline.len()];
        let mut top: Vec<usize> = Vec::new();
        for (i, n) in l.outline.iter().enumerate() {
            match n.parent {
                Some(p) => kids[p].push(i),
                None => top.push(i),
            }
        }
        fn desc_count(kids: &[Vec<usize>], i: usize) -> i64 {
            kids[i].iter().map(|k| 1 + desc_count(kids, *k)).sum()
        }
        for (i, n) in l.outline.iter().enumerate() {
            let parent = n.parent.map_or(root, |p| nums[p]);
            let siblings: &Vec<usize> = match n.parent {
                Some(p) => &kids[p],
                None => &top,
            };
            let pos = siblings.iter().position(|s| *s == i).unwrap_or(0);
            let page = page_nums
                .get(n.page)
                .copied()
                .unwrap_or(page_nums.first().copied().unwrap_or(0));
            let mut d = dict(vec![
                ("Title", text_string(&n.title)),
                ("Parent", Obj::Ref(parent, 0)),
                (
                    "Dest",
                    Obj::Array(vec![
                        Obj::Ref(page, 0),
                        name("XYZ"),
                        Obj::Int(0),
                        Obj::Real(n.y),
                        Obj::Null,
                    ]),
                ),
            ]);
            if pos > 0 {
                d.insert("Prev".into(), Obj::Ref(nums[siblings[pos - 1]], 0));
            }
            if pos + 1 < siblings.len() {
                d.insert("Next".into(), Obj::Ref(nums[siblings[pos + 1]], 0));
            }
            if let (Some(f), Some(la)) = (kids[i].first(), kids[i].last()) {
                d.insert("First".into(), Obj::Ref(nums[*f], 0));
                d.insert("Last".into(), Obj::Ref(nums[*la], 0));
                d.insert("Count".into(), Obj::Int(desc_count(&kids, i)));
            }
            l.b.set(nums[i], Obj::Dict(d));
        }
        let total: i64 = top.iter().map(|t| 1 + desc_count(&kids, *t)).sum();
        l.b.set(
            root,
            Obj::Dict(dict(vec![
                ("Type", name("Outlines")),
                ("First", Obj::Ref(nums[top[0]], 0)),
                ("Last", Obj::Ref(nums[top[top.len() - 1]], 0)),
                ("Count", Obj::Int(total)),
            ])),
        );
        cat.insert("Outlines".into(), Obj::Ref(root, 0));
        cat.insert("PageMode".into(), name("UseOutlines"));
    }
    if let Some(lang) = &doc.meta.language {
        if lang.is_ascii() && lang.len() < 20 {
            cat.insert("Lang".into(), Obj::Str(lang.as_bytes().to_vec()));
        }
    }
    cat.insert(
        "ViewerPreferences".into(),
        Obj::Dict(dict(vec![("DisplayDocTitle", Obj::Bool(true))])),
    );
    l.b.set(catalog, Obj::Dict(cat));
    let mut info = dict(vec![
        ("Producer", text_string("OpenConverter")),
        (
            "CreationDate",
            text_string(&writer::pdf_date(writer::now_secs())),
        ),
    ]);
    for (k, v) in [
        ("Title", &doc.meta.title),
        ("Author", &doc.meta.author),
        ("Subject", &doc.meta.subject),
    ] {
        if let Some(v) = v {
            info.insert(k.into(), text_string(v));
        }
    }
    let info = l.b.add(Obj::Dict(info));
    let Obj::Ref(info_num, _) = info else {
        return Err(Error::new(ErrorCode::InternalError, "info object"));
    };
    let b = std::mem::take(&mut l.b);
    Ok(b.finish(catalog, Some(info_num), None))
}
