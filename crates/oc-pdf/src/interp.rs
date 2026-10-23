//! The content-stream interpreter: runs a page's operators and records a display list.
use crate::color::ColorSpace;
use crate::doc::{PageInfo, Pdf};
use crate::filter::decode_stream;
use crate::font::{Kind, PdfFont};
use crate::function::Function;
use crate::image::decode_image;
use crate::object::{Dict, Lexer, Obj, Stream, Token};
use crate::ops::{Fx, ImageData, Op, Page, PaintSrc, PatternKind, PatternPaint, PlacedGlyph, SoftMask, StrokeStyle, TextRun};
use crate::shading;
use oc_font::db::FontDb;
use oc_raster::{Blend, FillRule, Path, Pt, Transform};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const MAX_DEPTH: u32 = 20;
const MAX_OPERATORS: u64 = 6_000_000;
const MAX_OPERANDS: usize = 8192;

pub struct Settings<'a> {
    pub fonts: Option<&'a FontDb>,
    /// Draw annotation appearance streams.
    pub annotations: bool,
}

#[derive(Clone)]
struct TextState {
    font: Option<Arc<PdfFont>>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
    mode: u8,
}

#[derive(Clone)]
struct GState {
    ctm: Transform,
    fill_cs: ColorSpace,
    stroke_cs: ColorSpace,
    fill: PaintSrc,
    stroke: PaintSrc,
    style: StrokeStyle,
    fill_alpha: f32,
    stroke_alpha: f32,
    blend: Blend,
    smask: Option<Arc<SoftMask>>,
    text: TextState,
}

impl GState {
    fn new(ctm: Transform) -> GState {
        GState {
            ctm,
            fill_cs: ColorSpace::Gray,
            stroke_cs: ColorSpace::Gray,
            fill: PaintSrc::Rgb([0.0; 3]),
            stroke: PaintSrc::Rgb([0.0; 3]),
            style: StrokeStyle { width: 1.0, cap: 0, join: 0, miter: 10.0, dash: Vec::new(), phase: 0.0 },
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            blend: Blend::Normal,
            smask: None,
            text: TextState { font: None, size: 0.0, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, rise: 0.0, mode: 0 },
        }
    }
    fn fill_fx(&self) -> Fx {
        Fx { alpha: self.fill_alpha, blend: self.blend, smask: self.smask.clone() }
    }
    fn stroke_fx(&self) -> Fx {
        Fx { alpha: self.stroke_alpha, blend: self.blend, smask: self.smask.clone() }
    }
}

struct Interp<'a> {
    pdf: &'a Pdf,
    fonts: Option<&'a FontDb>,
    notes: Vec<String>,
    hidden_ocgs: HashSet<u32>,
    font_cache: HashMap<u32, Arc<PdfFont>>,
    image_cache: HashMap<u32, Option<Arc<ImageData>>>,
    budget: u64,
    depth: u32,
    /// Pattern space of the content stream being run, to page space.
    pat_base: Transform,
    base: Transform,
}

/// The page box and the matrix from default user space to page space.
pub fn page_geometry(pdf: &Pdf, page: &PageInfo) -> (f64, f64, Transform) {
    let boxv = |k: &str| pdf.dget_numbers(&page.dict, k).filter(|v| v.len() >= 4).map(|v| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])]);
    let media = boxv("MediaBox").filter(|b| b[2] - b[0] > 0.0 && b[3] - b[1] > 0.0).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let mut b = media;
    if let Some(c) = boxv("CropBox") {
        let i = [c[0].max(media[0]), c[1].max(media[1]), c[2].min(media[2]), c[3].min(media[3])];
        if i[2] - i[0] > 0.5 && i[3] - i[1] > 0.5 {
            b = i;
        }
    }
    // keep absurd sizes in check
    let (bw, bh) = ((b[2] - b[0]).min(14400.0), (b[3] - b[1]).min(14400.0));
    let b = [b[0], b[1], b[0] + bw, b[1] + bh];
    let rot = pdf.dget_i64(&page.dict, "Rotate").unwrap_or(0).rem_euclid(360) / 90 * 90;
    let (x0, y0, x1, y1) = (b[0], b[1], b[2], b[3]);
    match rot {
        90 => (y1 - y0, x1 - x0, Transform::new(0.0, 1.0, 1.0, 0.0, -y0, -x0)),
        180 => (x1 - x0, y1 - y0, Transform::new(-1.0, 0.0, 0.0, 1.0, x1, -y0)),
        270 => (y1 - y0, x1 - x0, Transform::new(0.0, -1.0, -1.0, 0.0, y1, x1)),
        _ => (x1 - x0, y1 - y0, Transform::new(1.0, 0.0, 0.0, -1.0, -x0, y1)),
    }
}

fn contents(pdf: &Pdf, page: &Dict) -> Vec<u8> {
    let mut out = Vec::new();
    let c = pdf.dget(page, "Contents");
    let list: Vec<Obj> = match c {
        Obj::Array(a) => a.iter().map(|o| pdf.resolve(o)).collect(),
        o => vec![o],
    };
    for o in list {
        if let Obj::Stream(s) = o {
            if let Ok((d, _)) = decode_stream(pdf, &s, false) {
                out.extend_from_slice(&d);
                out.push(b'\n');
            }
        }
        if out.len() > 256 << 20 {
            break;
        }
    }
    out
}

pub fn interpret_page(pdf: &Pdf, page: &PageInfo, set: &Settings) -> Page {
    let (width, height, base) = page_geometry(pdf, page);
    let mut it = Interp {
        pdf,
        fonts: set.fonts,
        notes: Vec::new(),
        hidden_ocgs: hidden_groups(pdf),
        font_cache: HashMap::new(),
        image_cache: HashMap::new(),
        budget: MAX_OPERATORS,
        depth: 0,
        pat_base: base,
        base,
    };
    let res = pdf.dget_dict(&page.dict, "Resources").unwrap_or_default();
    let data = contents(pdf, &page.dict);
    let mut ops = Vec::new();
    ops.push(Op::Save);
    it.run(&data, &res, GState::new(base), false, &mut ops);
    ops.push(Op::Restore);
    if set.annotations {
        it.annotations(page, &mut ops);
    }
    let mut notes = it.notes;
    notes.dedup();
    Page { width, height, ops, notes }
}

fn hidden_groups(pdf: &Pdf) -> HashSet<u32> {
    let mut hidden = HashSet::new();
    let Some(cat) = pdf.catalog_dict() else { return hidden };
    let Some(ocp) = pdf.dget_dict(&cat, "OCProperties") else { return hidden };
    let Some(d) = pdf.dget_dict(&ocp, "D") else { return hidden };
    let refs = |key: &str, from: &Dict| -> Vec<u32> {
        pdf.dget_array(from, key).map(|a| a.iter().filter_map(|o| if let Obj::Ref(n, _) = o { Some(*n) } else { None }).collect()).unwrap_or_default()
    };
    if pdf.dget_name(&d, "BaseState").as_deref() == Some("OFF") {
        hidden.extend(refs("OCGs", &ocp));
        for n in refs("ON", &d) {
            hidden.remove(&n);
        }
    }
    hidden.extend(refs("OFF", &d));
    hidden
}

fn num(o: &Obj) -> f64 {
    o.as_f64().filter(|v| v.is_finite()).unwrap_or(0.0)
}

/// The last `n` operands as numbers, if there are that many.
fn last_nums(st: &[Obj], n: usize) -> Option<Vec<f64>> {
    if st.len() < n {
        return None;
    }
    let v: Vec<f64> = st[st.len() - n..].iter().map(num).collect();
    Some(v)
}

fn blend_of(pdf: &Pdf, o: &Obj) -> Blend {
    let o = pdf.resolve(o);
    let name = match &o {
        Obj::Name(n) => Some(n.clone()),
        Obj::Array(a) => a.iter().filter_map(|x| pdf.resolve(x).as_name().map(str::to_string)).next(),
        _ => None,
    };
    name.and_then(|n| Blend::from_name(&n)).filter(|b| !matches!(b, Blend::Clear | Blend::Source | Blend::DestOut | Blend::SourceIn | Blend::SourceOut | Blend::SourceAtop | Blend::DestOver | Blend::DestIn | Blend::DestAtop | Blend::Xor | Blend::Plus)).unwrap_or(Blend::Normal)
}

struct Cats {
    font: Dict,
    xobject: Dict,
    extg: Dict,
    pattern: Dict,
    shading: Dict,
    props: Dict,
}

impl<'a> Interp<'a> {
    fn note(&mut self, s: &str) {
        if self.notes.len() < 64 && !self.notes.iter().any(|n| n == s) {
            self.notes.push(s.to_string());
        }
    }

    fn oc_visible(&self, o: &Obj) -> bool {
        let pdf = self.pdf;
        let one = |o: &Obj| match o {
            Obj::Ref(n, _) => !self.hidden_ocgs.contains(n),
            _ => true,
        };
        let d = match pdf.resolve(o) {
            Obj::Dict(d) => d,
            _ => return true,
        };
        if d.get("Type").and_then(Obj::as_name) == Some("OCMD") {
            let groups: Vec<Obj> = match d.get("OCGs") {
                Some(Obj::Array(a)) => a.clone(),
                Some(o @ Obj::Ref(..)) => match pdf.resolve(o) {
                    Obj::Array(a) => a,
                    _ => vec![o.clone()],
                },
                _ => return true,
            };
            if groups.is_empty() {
                return true;
            }
            let on: Vec<bool> = groups.iter().map(one).collect();
            return match pdf.dget_name(&d, "P").as_deref() {
                Some("AllOn") => on.iter().all(|b| *b),
                Some("AnyOff") => on.iter().any(|b| !*b),
                Some("AllOff") => on.iter().all(|b| !*b),
                _ => on.iter().any(|b| *b),
            };
        }
        one(o)
    }

    fn color_from(&self, cs: &ColorSpace, st: &[Obj]) -> [f32; 3] {
        let n = cs.n_components();
        let vals: Vec<f64> = st.iter().filter(|o| o.as_f64().is_some()).map(num).collect();
        let take = if vals.len() >= n { &vals[vals.len() - n..] } else { &vals[..] };
        if take.len() < n {
            return cs.to_rgb(&cs.initial());
        }
        cs.to_rgb(take)
    }

    fn make_pattern(&mut self, name: &str, cats: &Cats, under: [f32; 3], res: &Dict, gs: &GState) -> Option<PaintSrc> {
        let obj = self.pdf.resolve(cats.pattern.get(name)?);
        let d = obj.as_dict()?.clone();
        let m = self.pdf.dget_numbers(&d, "Matrix").filter(|v| v.len() >= 6).map_or(Transform::IDENTITY, |v| Transform::new(v[0], v[1], v[2], v[3], v[4], v[5]));
        let matrix = m.then(&self.pat_base);
        match self.pdf.dget_i64(&d, "PatternType")? {
            2 => {
                let sh = shading::parse(self.pdf, d.get("Shading")?, res)?;
                Some(PaintSrc::Pattern(Arc::new(PatternPaint { matrix, kind: PatternKind::Shading(Arc::new(sh)) }), under))
            }
            1 => {
                let Obj::Stream(s) = obj else { return None };
                let bbox = self.pdf.dget_numbers(&d, "BBox").filter(|v| v.len() >= 4)?;
                let bbox = [bbox[0].min(bbox[2]), bbox[1].min(bbox[3]), bbox[0].max(bbox[2]), bbox[1].max(bbox[3])];
                let xstep = self.pdf.dget_f64(&d, "XStep").filter(|v| v.abs() > 1e-6).unwrap_or(bbox[2] - bbox[0]);
                let ystep = self.pdf.dget_f64(&d, "YStep").filter(|v| v.abs() > 1e-6).unwrap_or(bbox[3] - bbox[1]);
                let uncolored = self.pdf.dget_i64(&d, "PaintType") == Some(2);
                if self.depth >= MAX_DEPTH {
                    return None;
                }
                let data = decode_stream(self.pdf, &s, false).ok()?.0;
                let pres = self.pdf.dget_dict(&d, "Resources").unwrap_or_else(|| res.clone());
                let mut tile = GState::new(Transform::IDENTITY);
                if uncolored {
                    tile.fill = PaintSrc::Rgb(under);
                    tile.stroke = PaintSrc::Rgb(under);
                }
                let _ = gs;
                let saved = self.pat_base;
                self.pat_base = Transform::IDENTITY;
                self.depth += 1;
                let mut ops = Vec::new();
                self.run(&data, &pres, tile, uncolored, &mut ops);
                self.depth -= 1;
                self.pat_base = saved;
                Some(PaintSrc::Pattern(Arc::new(PatternPaint { matrix, kind: PatternKind::Tiling { ops, bbox, xstep, ystep, uncolored } }), under))
            }
            _ => None,
        }
    }

    fn load_font(&mut self, cats: &Cats, name: &str) -> Option<Arc<PdfFont>> {
        let entry = cats.font.get(name)?;
        let key = if let Obj::Ref(n, _) = entry { Some(*n) } else { None };
        if let Some(f) = key.and_then(|k| self.font_cache.get(&k)) {
            return Some(f.clone());
        }
        let fd = self.pdf.resolve(entry);
        let fd = fd.as_dict()?.clone();
        let mut notes = Vec::new();
        let f = Arc::new(PdfFont::load(self.pdf, &fd, self.fonts, &mut notes));
        for n in notes {
            self.note(&n);
        }
        if let Some(k) = key {
            self.font_cache.insert(k, f.clone());
        }
        Some(f)
    }

    fn apply_extg(&mut self, cats: &Cats, name: &str, gs: &mut GState, res: &Dict) {
        let Some(d) = cats.extg.get(name).map(|o| self.pdf.resolve(o)).and_then(|o| o.as_dict().cloned()) else { return };
        let pdf = self.pdf;
        for (k, v) in &d {
            let v = pdf.resolve(v);
            match k.as_str() {
                "LW" => gs.style.width = num(&v),
                "LC" => gs.style.cap = v.as_i64().unwrap_or(0).clamp(0, 2) as u8,
                "LJ" => gs.style.join = v.as_i64().unwrap_or(0).clamp(0, 2) as u8,
                "ML" => gs.style.miter = num(&v).max(1.0),
                "D" => {
                    if let Some(a) = v.as_array() {
                        if a.len() >= 2 {
                            let arr = pdf.resolve(&a[0]);
                            gs.style.dash = arr.as_array().map(|x| x.iter().map(|o| num(&pdf.resolve(o))).collect()).unwrap_or_default();
                            gs.style.phase = num(&pdf.resolve(&a[1]));
                        }
                    }
                }
                "CA" => gs.stroke_alpha = num(&v).clamp(0.0, 1.0) as f32,
                "ca" => gs.fill_alpha = num(&v).clamp(0.0, 1.0) as f32,
                "BM" => gs.blend = blend_of(pdf, &v),
                "Font" => {
                    if let Some(a) = v.as_array() {
                        if a.len() >= 2 {
                            let key = if let Obj::Ref(n, _) = &a[0] { Some(*n) } else { None };
                            if let Some(f) = key.and_then(|k| self.font_cache.get(&k).cloned()) {
                                gs.text.font = Some(f);
                            } else if let Some(fd) = pdf.resolve(&a[0]).as_dict() {
                                let mut notes = Vec::new();
                                let f = Arc::new(PdfFont::load(pdf, fd, self.fonts, &mut notes));
                                if let Some(k) = key {
                                    self.font_cache.insert(k, f.clone());
                                }
                                gs.text.font = Some(f);
                            }
                            gs.text.size = num(&pdf.resolve(&a[1]));
                        }
                    }
                }
                "SMask" => match &v {
                    Obj::Name(_) => gs.smask = None,
                    Obj::Dict(sm) => gs.smask = self.soft_mask(sm, gs, res),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn soft_mask(&mut self, sm: &Dict, gs: &GState, res: &Dict) -> Option<Arc<SoftMask>> {
        let pdf = self.pdf;
        if self.depth >= MAX_DEPTH {
            return None;
        }
        let Obj::Stream(g) = pdf.dget(sm, "G") else { return None };
        let luminosity = pdf.dget_name(sm, "S").as_deref() != Some("Alpha");
        let mut backdrop = [0.0f32; 3];
        let group = pdf.dget_dict(&g.dict, "Group");
        if let (Some(bc), Some(grp)) = (pdf.dget_numbers(sm, "BC"), group.as_ref()) {
            let cs = grp.get("CS").and_then(|c| ColorSpace::parse(pdf, c, res, 0)).unwrap_or(ColorSpace::Gray);
            if bc.len() >= cs.n_components() {
                backdrop = cs.to_rgb(&bc);
            }
        }
        let transfer = match pdf.dget(sm, "TR") {
            Obj::Name(_) | Obj::Null => None,
            o => Function::parse(pdf, &o),
        };
        let mut ops = Vec::new();
        let mut inner = GState::new(gs.ctm);
        inner.fill_cs = ColorSpace::Gray;
        self.depth += 1;
        self.form(&g, res, &inner, &mut ops, true);
        self.depth -= 1;
        Some(Arc::new(SoftMask { ops, luminosity, backdrop, transfer }))
    }

    /// Run a form XObject: its matrix, clip and resources, with `gs` as the state it inherits.
    fn form(&mut self, s: &Arc<Stream>, parent_res: &Dict, gs: &GState, out: &mut Vec<Op>, force_plain: bool) {
        let pdf = self.pdf;
        if self.depth >= MAX_DEPTH {
            return;
        }
        let Ok((data, _)) = decode_stream(pdf, s, false) else { return };
        let m = pdf.dget_numbers(&s.dict, "Matrix").filter(|v| v.len() >= 6).map_or(Transform::IDENTITY, |v| Transform::new(v[0], v[1], v[2], v[3], v[4], v[5]));
        let res = pdf.dget_dict(&s.dict, "Resources").unwrap_or_else(|| parent_res.clone());
        let mut inner = gs.clone();
        inner.ctm = m.then(&gs.ctm);
        let group = pdf.dget_dict(&s.dict, "Group").filter(|g| pdf.dget_name(g, "S").as_deref() == Some("Transparency"));
        let needs_group = !force_plain && group.is_some() && (gs.fill_alpha < 1.0 || gs.blend != Blend::Normal || gs.smask.is_some());
        let fx = gs.fill_fx();
        let isolated = group.as_ref().and_then(|g| pdf.dget(g, "I").as_bool()).unwrap_or(false);
        if needs_group {
            inner.fill_alpha = 1.0;
            inner.stroke_alpha = 1.0;
            inner.blend = Blend::Normal;
            inner.smask = None;
        }
        let saved_pat = self.pat_base;
        self.pat_base = inner.ctm;
        let mut body = Vec::new();
        body.push(Op::Save);
        if let Some(b) = pdf.dget_numbers(&s.dict, "BBox").filter(|v| v.len() >= 4) {
            let mut p = Path::new();
            p.move_to(b[0], b[1]);
            p.line_to(b[2], b[1]);
            p.line_to(b[2], b[3]);
            p.line_to(b[0], b[3]);
            p.close();
            body.push(Op::Clip { path: Arc::new(p.transform(&inner.ctm)), rule: FillRule::NonZero });
        }
        self.depth += 1;
        self.run(&data, &res, inner, false, &mut body);
        self.depth -= 1;
        body.push(Op::Restore);
        self.pat_base = saved_pat;
        if needs_group {
            out.push(Op::Group { ops: body, fx, isolated });
        } else {
            out.append(&mut body);
        }
    }

    fn annotations(&mut self, page: &PageInfo, out: &mut Vec<Op>) {
        let pdf = self.pdf;
        let Some(annots) = pdf.dget_array(&page.dict, "Annots") else { return };
        for a in annots.iter().take(5000) {
            let Some(d) = pdf.resolve(a).as_dict().cloned() else { continue };
            let flags = pdf.dget_i64(&d, "F").unwrap_or(0);
            if flags & (2 | 32) != 0 || pdf.dget_name(&d, "Subtype").as_deref() == Some("Popup") {
                continue;
            }
            if let Some(oc) = d.get("OC") {
                if !self.oc_visible(oc) {
                    continue;
                }
            }
            let Some(ap) = pdf.dget_dict(&d, "AP") else { continue };
            let n = pdf.dget(&ap, "N");
            let stream = match n {
                Obj::Stream(s) => s,
                Obj::Dict(states) => {
                    let Some(name) = pdf.dget_name(&d, "AS") else { continue };
                    match pdf.dget(&states, &name) {
                        Obj::Stream(s) => s,
                        _ => continue,
                    }
                }
                _ => continue,
            };
            let Some(rect) = pdf.dget_numbers(&d, "Rect").filter(|v| v.len() >= 4) else { continue };
            let (rx0, ry0, rx1, ry1) = (rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3]));
            let bbox = pdf.dget_numbers(&stream.dict, "BBox").filter(|v| v.len() >= 4).unwrap_or_else(|| vec![0.0, 0.0, rx1 - rx0, ry1 - ry0]);
            let m = pdf.dget_numbers(&stream.dict, "Matrix").filter(|v| v.len() >= 6).map_or(Transform::IDENTITY, |v| Transform::new(v[0], v[1], v[2], v[3], v[4], v[5]));
            let corners = [Pt::new(bbox[0], bbox[1]), Pt::new(bbox[2], bbox[1]), Pt::new(bbox[2], bbox[3]), Pt::new(bbox[0], bbox[3])].map(|p| m.apply(p));
            let (mut bx0, mut by0, mut bx1, mut by1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for c in corners {
                bx0 = bx0.min(c.x);
                by0 = by0.min(c.y);
                bx1 = bx1.max(c.x);
                by1 = by1.max(c.y);
            }
            let (sw, sh) = (bx1 - bx0, by1 - by0);
            let sx = if sw.abs() > 1e-9 { (rx1 - rx0) / sw } else { 1.0 };
            let sy = if sh.abs() > 1e-9 { (ry1 - ry0) / sh } else { 1.0 };
            let amat = Transform::new(sx, 0.0, 0.0, sy, rx0 - bx0 * sx, ry0 - by0 * sy);
            let gs = GState::new(amat.then(&self.base));
            let res = Dict::new();
            self.form(&stream, &res, &gs, out, false);
        }
    }

    fn cats(&self, res: &Dict) -> Cats {
        let d = |k: &str| self.pdf.dget_dict(res, k).unwrap_or_default();
        Cats { font: d("Font"), xobject: d("XObject"), extg: d("ExtGState"), pattern: d("Pattern"), shading: d("Shading"), props: d("Properties") }
    }

    #[allow(clippy::cognitive_complexity)]
    fn run(&mut self, data: &[u8], res: &Dict, gs0: GState, ignore_color0: bool, out: &mut Vec<Op>) {
        let pdf = self.pdf;
        let cats = self.cats(res);
        let mut ignore_color = ignore_color0;
        let mut gs = gs0;
        let mut stack: Vec<GState> = Vec::new();
        let mut saves = 0usize;
        let mut st: Vec<Obj> = Vec::new();
        let mut path = Path::new();
        let mut cur = Pt::new(0.0, 0.0);
        let mut start = Pt::new(0.0, 0.0);
        let mut has_cur = false;
        let mut pending_clip: Option<FillRule> = None;
        let mut tm = Transform::IDENTITY;
        let mut tlm = Transform::IDENTITY;
        let mut text_clip = Path::new();
        let mut in_text_clip = false;
        let mut marks: Vec<(String, bool)> = Vec::new();
        let mut hidden = 0usize;
        let mut lex = Lexer::new(data, 0);

        macro_rules! nums {
            ($n:expr) => {
                match last_nums(&st, $n) {
                    Some(v) => v,
                    None => {
                        st.clear();
                        continue;
                    }
                }
            };
        }

        loop {
            let tok = lex.next();
            match tok {
                Token::Eof => break,
                Token::Obj(o) => {
                    if st.len() < MAX_OPERANDS {
                        st.push(o);
                    }
                    continue;
                }
                Token::ArrayStart | Token::DictStart => {
                    if let Some(o) = lex.object_from(tok, 0) {
                        if st.len() < MAX_OPERANDS {
                            st.push(o);
                        }
                    }
                    continue;
                }
                Token::ArrayEnd | Token::DictEnd => continue,
                Token::Keyword(kw) => {
                    if self.budget == 0 {
                        self.note("a page was too complex and was cut short");
                        break;
                    }
                    self.budget -= 1;
                    let op = std::str::from_utf8(&kw).unwrap_or("");
                    let visible = hidden == 0;
                    match op {
                        "q" => {
                            if stack.len() < 256 {
                                stack.push(gs.clone());
                                out.push(Op::Save);
                                saves += 1;
                            }
                        }
                        "Q" => {
                            if let Some(g) = stack.pop() {
                                // text-state parts persist? no: the whole state is restored
                                gs = g;
                                out.push(Op::Restore);
                                saves -= 1;
                            }
                        }
                        "cm" => {
                            let v = nums!(6);
                            let m = Transform::new(v[0], v[1], v[2], v[3], v[4], v[5]);
                            if m.is_finite() {
                                gs.ctm = m.then(&gs.ctm);
                            }
                        }
                        "w" => gs.style.width = nums!(1)[0].abs(),
                        "J" => gs.style.cap = nums!(1)[0].clamp(0.0, 2.0) as u8,
                        "j" => gs.style.join = nums!(1)[0].clamp(0.0, 2.0) as u8,
                        "M" => gs.style.miter = nums!(1)[0].max(1.0),
                        "d" => {
                            if st.len() >= 2 {
                                let arr = st[st.len() - 2].as_array().map(|a| a.iter().map(num).collect::<Vec<f64>>()).unwrap_or_default();
                                gs.style.dash = if arr.iter().all(|v| *v >= 0.0) && arr.iter().any(|v| *v > 0.0) { arr } else { Vec::new() };
                                gs.style.phase = num(&st[st.len() - 1]);
                            }
                        }
                        "gs" => {
                            if let Some(n) = st.last().and_then(Obj::as_name) {
                                let n = n.to_string();
                                self.apply_extg(&cats, &n, &mut gs, res);
                            }
                        }
                        // ---- path construction ----
                        "m" => {
                            let v = nums!(2);
                            path.move_to(v[0], v[1]);
                            cur = Pt::new(v[0], v[1]);
                            start = cur;
                            has_cur = true;
                        }
                        "l" => {
                            let v = nums!(2);
                            if has_cur {
                                path.line_to(v[0], v[1]);
                            } else {
                                path.move_to(v[0], v[1]);
                                start = Pt::new(v[0], v[1]);
                                has_cur = true;
                            }
                            cur = Pt::new(v[0], v[1]);
                        }
                        "c" => {
                            let v = nums!(6);
                            if !has_cur {
                                path.move_to(v[0], v[1]);
                                start = Pt::new(v[0], v[1]);
                                has_cur = true;
                            }
                            path.cubic_to(v[0], v[1], v[2], v[3], v[4], v[5]);
                            cur = Pt::new(v[4], v[5]);
                        }
                        "v" => {
                            let v = nums!(4);
                            if !has_cur {
                                path.move_to(v[0], v[1]);
                                cur = Pt::new(v[0], v[1]);
                                start = cur;
                                has_cur = true;
                            }
                            path.cubic_to(cur.x, cur.y, v[0], v[1], v[2], v[3]);
                            cur = Pt::new(v[2], v[3]);
                        }
                        "y" => {
                            let v = nums!(4);
                            if !has_cur {
                                path.move_to(v[0], v[1]);
                                start = Pt::new(v[0], v[1]);
                                has_cur = true;
                            }
                            path.cubic_to(v[0], v[1], v[2], v[3], v[2], v[3]);
                            cur = Pt::new(v[2], v[3]);
                        }
                        "h" => {
                            if has_cur {
                                path.close();
                                cur = start;
                            }
                        }
                        "re" => {
                            let v = nums!(4);
                            path.move_to(v[0], v[1]);
                            path.line_to(v[0] + v[2], v[1]);
                            path.line_to(v[0] + v[2], v[1] + v[3]);
                            path.line_to(v[0], v[1] + v[3]);
                            path.close();
                            cur = Pt::new(v[0], v[1]);
                            start = cur;
                            has_cur = true;
                        }
                        // ---- path painting ----
                        "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                            if matches!(op, "s" | "b" | "b*") && has_cur {
                                path.close();
                            }
                            let fill_rule = match op {
                                "f" | "F" | "B" | "b" => Some(FillRule::NonZero),
                                "f*" | "B*" | "b*" => Some(FillRule::EvenOdd),
                                _ => None,
                            };
                            let do_stroke = matches!(op, "S" | "s" | "B" | "B*" | "b" | "b*");
                            if !path.is_empty() {
                                if let (Some(rule), true) = (fill_rule, visible) {
                                    out.push(Op::Fill { path: Arc::new(path.transform(&gs.ctm)), rule, paint: gs.fill.clone(), fx: gs.fill_fx() });
                                }
                                if do_stroke && visible {
                                    out.push(Op::Stroke { path: Arc::new(path.clone()), ctm: gs.ctm, style: gs.style.clone(), paint: gs.stroke.clone(), fx: gs.stroke_fx() });
                                }
                            }
                            if let Some(rule) = pending_clip.take() {
                                out.push(Op::Clip { path: Arc::new(path.transform(&gs.ctm)), rule });
                            }
                            path = Path::new();
                            has_cur = false;
                        }
                        "W" => pending_clip = Some(FillRule::NonZero),
                        "W*" => pending_clip = Some(FillRule::EvenOdd),
                        // ---- colour ----
                        "g" | "G" | "rg" | "RG" | "k" | "K" => {
                            let n = match op {
                                "g" | "G" => 1,
                                "rg" | "RG" => 3,
                                _ => 4,
                            };
                            let v = nums!(n);
                            if !ignore_color {
                                let (cs, rgb) = match n {
                                    1 => (ColorSpace::Gray, ColorSpace::Gray.to_rgb(&v)),
                                    3 => (ColorSpace::Rgb, ColorSpace::Rgb.to_rgb(&v)),
                                    _ => (ColorSpace::Cmyk, ColorSpace::Cmyk.to_rgb(&v)),
                                };
                                if op.chars().next().is_some_and(|c| c.is_lowercase()) {
                                    gs.fill_cs = cs;
                                    gs.fill = PaintSrc::Rgb(rgb);
                                } else {
                                    gs.stroke_cs = cs;
                                    gs.stroke = PaintSrc::Rgb(rgb);
                                }
                            }
                        }
                        "cs" | "CS" => {
                            if let (Some(n), false) = (st.last().cloned(), ignore_color) {
                                if let Some(cs) = ColorSpace::parse(pdf, &n, res, 0) {
                                    let init = PaintSrc::Rgb(cs.to_rgb(&cs.initial()));
                                    if op == "cs" {
                                        gs.fill_cs = cs;
                                        gs.fill = init;
                                    } else {
                                        gs.stroke_cs = cs;
                                        gs.stroke = init;
                                    }
                                }
                            }
                        }
                        "sc" | "scn" | "SC" | "SCN" => {
                            if !ignore_color {
                                let fill = op.starts_with('s');
                                let cs = if fill { gs.fill_cs.clone() } else { gs.stroke_cs.clone() };
                                let paint = if let ColorSpace::Pattern(base) = &cs {
                                    let under = base.as_ref().map_or([0.0; 3], |b| self.color_from(b, &st));
                                    st.last().and_then(Obj::as_name).map(str::to_string).and_then(|n| self.make_pattern(&n, &cats, under, res, &gs))
                                } else {
                                    Some(PaintSrc::Rgb(self.color_from(&cs, &st)))
                                };
                                if let Some(p) = paint {
                                    if fill {
                                        gs.fill = p;
                                    } else {
                                        gs.stroke = p;
                                    }
                                }
                            }
                        }
                        "sh" => {
                            if let (Some(n), true) = (st.last().and_then(Obj::as_name), visible) {
                                if let Some(sh) = cats.shading.get(n).and_then(|o| shading::parse(pdf, o, res)) {
                                    out.push(Op::Shade { shading: Arc::new(sh), matrix: gs.ctm, fx: gs.fill_fx() });
                                }
                            }
                        }
                        // ---- text ----
                        "BT" => {
                            tm = Transform::IDENTITY;
                            tlm = Transform::IDENTITY;
                        }
                        "ET" => {
                            if in_text_clip {
                                out.push(Op::Clip { path: Arc::new(std::mem::take(&mut text_clip)), rule: FillRule::NonZero });
                                in_text_clip = false;
                            }
                        }
                        "Tc" => gs.text.tc = nums!(1)[0],
                        "Tw" => gs.text.tw = nums!(1)[0],
                        "Tz" => gs.text.th = nums!(1)[0] / 100.0,
                        "TL" => gs.text.tl = nums!(1)[0],
                        "Ts" => gs.text.rise = nums!(1)[0],
                        "Tr" => gs.text.mode = nums!(1)[0].clamp(0.0, 7.0) as u8,
                        "Tf" => {
                            if st.len() >= 2 {
                                gs.text.size = num(&st[st.len() - 1]);
                                if let Some(n) = st[st.len() - 2].as_name() {
                                    let n = n.to_string();
                                    gs.text.font = self.load_font(&cats, &n);
                                }
                            }
                        }
                        "Td" | "TD" => {
                            let v = nums!(2);
                            if op == "TD" {
                                gs.text.tl = -v[1];
                            }
                            tlm = Transform::translate(v[0], v[1]).then(&tlm);
                            tm = tlm;
                        }
                        "Tm" => {
                            let v = nums!(6);
                            tm = Transform::new(v[0], v[1], v[2], v[3], v[4], v[5]);
                            tlm = tm;
                        }
                        "T*" => {
                            tlm = Transform::translate(0.0, -gs.text.tl).then(&tlm);
                            tm = tlm;
                        }
                        "Tj" | "'" | "\"" | "TJ" => {
                            if op == "\"" && st.len() >= 3 {
                                gs.text.tw = num(&st[st.len() - 3]);
                                gs.text.tc = num(&st[st.len() - 2]);
                            }
                            if op == "'" || op == "\"" {
                                tlm = Transform::translate(0.0, -gs.text.tl).then(&tlm);
                                tm = tlm;
                            }
                            let items: Vec<Obj> = match (op, st.last()) {
                                ("TJ", Some(Obj::Array(a))) => a.clone(),
                                ("TJ", _) => Vec::new(),
                                (_, Some(o)) => vec![o.clone()],
                                _ => Vec::new(),
                            };
                            let mut run = TextAcc::default();
                            for it in &items {
                                match it {
                                    Obj::Str(s) => self.show_string(s, &mut gs, &mut tm, &mut run, visible, res, out),
                                    o if o.as_f64().is_some() => {
                                        let adj = -num(o) / 1000.0 * gs.text.size;
                                        let vertical = gs.text.font.as_ref().is_some_and(|f| f.is_vertical());
                                        let t = if vertical { Transform::translate(0.0, adj) } else { Transform::translate(adj * gs.text.th, 0.0) };
                                        tm = t.then(&tm);
                                    }
                                    _ => {}
                                }
                            }
                            if let Some(font) = gs.text.font.clone() {
                                if !run.glyphs.is_empty() {
                                    let mode = gs.text.mode;
                                    if mode >= 4 && font.kind != Kind::Type3 {
                                        for g in &run.glyphs {
                                            if let Some(p) = g.gid.and_then(|id| font.outline(id)) {
                                                text_clip.append(&p.transform(&g.to_page));
                                            }
                                        }
                                        in_text_clip = true;
                                    }
                                    if visible {
                                        let size = run.size;
                                        out.push(Op::Text(Box::new(TextRun {
                                            font,
                                            glyphs: std::mem::take(&mut run.glyphs),
                                            mode: if run.type3 { 3 } else { mode & 3 },
                                            fill: gs.fill.clone(),
                                            stroke: gs.stroke.clone(),
                                            stroke_style: gs.style.clone(),
                                            ctm: gs.ctm,
                                            fill_fx: gs.fill_fx(),
                                            stroke_fx: gs.stroke_fx(),
                                            size,
                                        })));
                                    }
                                }
                            }
                        }
                        "d0" => {}
                        "d1" => ignore_color = true,
                        // ---- xobjects, images ----
                        "Do" => {
                            if let (Some(n), true) = (st.last().and_then(Obj::as_name), visible) {
                                let n = n.to_string();
                                self.do_xobject(&cats, &n, res, &gs, out);
                            }
                        }
                        "BI" => {
                            let img = self.inline_image(&mut lex, res, &gs);
                            if let (Some(img), true) = (img, visible) {
                                let ctm = Transform::scale(1.0 / img.width as f64, -1.0 / img.height as f64).then(&Transform::translate(0.0, 1.0)).then(&gs.ctm);
                                let stencil_paint = if img.stencil && matches!(gs.fill, PaintSrc::Pattern(..)) { Some(gs.fill.clone()) } else { None };
                                out.push(Op::Image { image: Arc::new(img), ctm, fx: gs.fill_fx(), stencil_paint });
                            }
                        }
                        // ---- marked content ----
                        "BMC" | "BDC" => {
                            let tag = if op == "BMC" { st.last() } else { st.get(st.len().saturating_sub(2)) }.and_then(Obj::as_name).unwrap_or("").to_string();
                            let mut hides = false;
                            let mut actual = None;
                            if op == "BDC" {
                                let props = st.last().map(|o| match o {
                                    Obj::Name(n) => cats.props.get(n).map(|p| (p.clone(), pdf.resolve(p))),
                                    other => Some((other.clone(), other.clone())),
                                });
                                if let Some(Some((raw, d))) = props {
                                    if tag == "OC" && !self.oc_visible(&raw) {
                                        hides = true;
                                    }
                                    if let Some(a) = d.as_dict().and_then(|d| d.get("ActualText")).map(|o| pdf.resolve(o)) {
                                        actual = a.as_str().map(decode_text_string);
                                    }
                                }
                            }
                            if hides {
                                hidden += 1;
                            }
                            marks.push((tag.clone(), hides));
                            out.push(Op::Marked { tag, actual_text: actual, begin: true });
                        }
                        "EMC" => {
                            if let Some((tag, hides)) = marks.pop() {
                                if hides {
                                    hidden = hidden.saturating_sub(1);
                                }
                                out.push(Op::Marked { tag, actual_text: None, begin: false });
                            }
                        }
                        _ => {}
                    }
                    st.clear();
                }
            }
        }
        // balance anything the stream left open
        for _ in 0..saves {
            out.push(Op::Restore);
        }
        if in_text_clip {
            out.push(Op::Clip { path: Arc::new(text_clip), rule: FillRule::NonZero });
        }
        for (tag, _) in marks.into_iter().rev() {
            out.push(Op::Marked { tag, actual_text: None, begin: false });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn show_string(&mut self, s: &[u8], gs: &mut GState, tm: &mut Transform, run: &mut TextAcc, visible: bool, res: &Dict, out: &mut Vec<Op>) {
        let Some(font) = gs.text.font.clone() else { return };
        let ts = gs.text.clone();
        let vertical = font.is_vertical();
        for ch in font.decode(s) {
            let trm_pre = Transform::new(ts.size * ts.th, 0.0, 0.0, ts.size, 0.0, ts.rise);
            let after_tm = trm_pre.then(tm).then(&gs.ctm);
            let (adv_x, adv_y);
            let mut origin_shift = (0.0, 0.0);
            if let (true, Some((w1, vx, vy))) = (vertical, ch.vertical) {
                origin_shift = (-vx / 1000.0, -vy / 1000.0);
                adv_x = 0.0;
                adv_y = w1 / 1000.0 * ts.size + ts.tc + if ch.is_space { ts.tw } else { 0.0 };
            } else {
                adv_x = (ch.width / 1000.0 * ts.size + ts.tc + if ch.is_space { ts.tw } else { 0.0 }) * ts.th;
                adv_y = 0.0;
            }
            if font.kind == Kind::Type3 {
                run.type3 = true;
                let fm = font.type3.as_ref().map_or([0.001, 0.0, 0.0, 0.001, 0.0, 0.0], |t| t.font_matrix);
                let gm = Transform::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]).then(&after_tm);
                let origin = after_tm.apply(Pt::new(0.0, 0.0));
                let adv = after_tm.apply_vec(Pt::new(ch.width / 1000.0, 0.0)).len();
                run.glyphs.push(PlacedGlyph { gid: None, to_page: gm, unicode: ch.unicode.clone(), origin, advance: adv, code: ch.code, is_space: ch.is_space && ch.unicode.trim().is_empty() });
                if visible && self.depth < MAX_DEPTH {
                    let proc_stream = ch.name.as_ref().and_then(|n| font.type3.as_ref().and_then(|t| t.procs.get(n).cloned()));
                    if let (Some(ps), Some(t3)) = (proc_stream, font.type3.as_ref()) {
                        if let Ok((code, _)) = decode_stream(self.pdf, &ps, false) {
                            let pres = t3.resources.clone().unwrap_or_else(|| res.clone());
                            let mut inner = gs.clone();
                            inner.ctm = gm;
                            inner.text = TextState { font: None, ..ts.clone() };
                            self.depth += 1;
                            out.push(Op::Save);
                            self.run(&code, &pres, inner, false, out);
                            out.push(Op::Restore);
                            self.depth -= 1;
                        }
                    }
                }
            } else {
                let us = font.unit_scale;
                let to_page = Transform::scale(us, us).then(&Transform::translate(origin_shift.0, origin_shift.1)).then(&after_tm);
                let origin = to_page.apply(Pt::new(0.0, 0.0));
                let adv = after_tm.apply_vec(Pt::new(ch.width / 1000.0, 0.0)).len();
                run.glyphs.push(PlacedGlyph { gid: ch.gid, to_page, unicode: ch.unicode.clone(), origin, advance: adv, code: ch.code, is_space: ch.is_space && ch.unicode.trim().is_empty() });
            }
            let sc = after_tm.apply_vec(Pt::new(0.0, 1.0)).len();
            run.size = sc;
            *tm = Transform::translate(adv_x, adv_y).then(tm);
        }
    }

    fn do_xobject(&mut self, cats: &Cats, name: &str, res: &Dict, gs: &GState, out: &mut Vec<Op>) {
        let pdf = self.pdf;
        let Some(entry) = cats.xobject.get(name) else { return };
        let key = if let Obj::Ref(n, _) = entry { Some(*n) } else { None };
        let Obj::Stream(s) = pdf.resolve(entry) else { return };
        if let Some(oc) = s.dict.get("OC") {
            if !self.oc_visible(oc) {
                return;
            }
        }
        match pdf.dget_name(&s.dict, "Subtype").as_deref() {
            Some("Form") => self.form(&s, res, gs, out, false),
            Some("Image") => {
                let is_mask = pdf.dget(&s.dict, "ImageMask").as_bool().unwrap_or(false);
                let fill = match &gs.fill {
                    PaintSrc::Rgb(c) => *c,
                    PaintSrc::Pattern(_, c) => *c,
                };
                // stencil masks bake the fill colour in, so they are cached per colour only when
                // the colour is the default; everything else is cached by object
                let cacheable = key.filter(|_| !is_mask);
                let img = match cacheable.and_then(|k| self.image_cache.get(&k).cloned()) {
                    Some(i) => i,
                    None => {
                        let mut notes = Vec::new();
                        let img = decode_image(pdf, &s, res, fill, &mut notes, 0).map(Arc::new);
                        for n in notes {
                            self.note(&n);
                        }
                        if let Some(k) = cacheable {
                            if self.image_cache.len() < 64 {
                                self.image_cache.insert(k, img.clone());
                            }
                        }
                        img
                    }
                };
                if let Some(image) = img {
                    let ctm = Transform::scale(1.0 / image.width as f64, -1.0 / image.height as f64).then(&Transform::translate(0.0, 1.0)).then(&gs.ctm);
                    let stencil_paint = if image.stencil && matches!(gs.fill, PaintSrc::Pattern(..)) { Some(gs.fill.clone()) } else { None };
                    out.push(Op::Image { image, ctm, fx: gs.fill_fx(), stencil_paint });
                }
            }
            _ => {}
        }
    }

    fn inline_image(&mut self, lex: &mut Lexer, res: &Dict, gs: &GState) -> Option<ImageData> {
        let pdf = self.pdf;
        let mut d = Dict::new();
        loop {
            match lex.next() {
                Token::Keyword(k) if k == b"ID" => break,
                Token::Obj(Obj::Name(key)) => {
                    let t = lex.next();
                    if let Some(v) = lex.object_from(t, 0) {
                        d.insert(key, v);
                    }
                }
                Token::Eof => return None,
                _ => {}
            }
            if d.len() > 64 {
                return None;
            }
        }
        // one whitespace byte follows ID
        if lex.peek().is_some_and(crate::object::is_ws) {
            lex.pos += 1;
        }
        let begin = lex.pos;
        let data = lex.data;
        let get = |a: &str, b: &str| d.get(a).or_else(|| d.get(b)).cloned();
        let w = get("W", "Width").and_then(|o| o.as_i64()).unwrap_or(0).max(0) as usize;
        let h = get("H", "Height").and_then(|o| o.as_i64()).unwrap_or(0).max(0) as usize;
        let mask = get("IM", "ImageMask").and_then(|o| o.as_bool()).unwrap_or(false);
        let bpc = if mask { 1 } else { get("BPC", "BitsPerComponent").and_then(|o| o.as_i64()).unwrap_or(8).clamp(1, 16) as usize };
        let has_filter = get("F", "Filter").is_some_and(|f| !f.is_null());
        let mut end = None;
        if !has_filter && w > 0 && h > 0 {
            let n = if mask { Some(1) } else { get("CS", "ColorSpace").and_then(|c| ColorSpace::parse(pdf, &c, res, 0)).map(|c| c.n_components()) };
            if let Some(n) = n {
                let len = (w * n * bpc).div_ceil(8) * h;
                if begin + len <= data.len() {
                    end = Some(begin + len);
                }
            }
        }
        let end = match end {
            Some(e) => e,
            None => {
                let mut i = begin;
                let mut found = None;
                while i + 1 < data.len() {
                    if data[i] == b'E' && data[i + 1] == b'I' && (i == begin || crate::object::is_ws(data[i - 1])) && data.get(i + 2).is_none_or(|b| crate::object::is_ws(*b)) {
                        // what follows should look like content-stream text
                        let tail = &data[(i + 2).min(data.len())..(i + 12).min(data.len())];
                        if tail.iter().all(|b| *b == b'\n' || *b == b'\r' || *b == b'\t' || (32..127).contains(b)) {
                            found = Some(i);
                            break;
                        }
                    }
                    i += 1;
                }
                found.unwrap_or(data.len())
            }
        };
        let raw = data.get(begin..end.min(data.len()))?.to_vec();
        // continue after EI
        lex.pos = end;
        loop {
            match lex.next() {
                Token::Keyword(k) if k == b"EI" => break,
                Token::Eof => break,
                _ => {}
            }
            if lex.pos >= data.len() {
                break;
            }
        }
        let stream = Stream { dict: d, raw, id: (0, 0) };
        let fill = match &gs.fill {
            PaintSrc::Rgb(c) => *c,
            PaintSrc::Pattern(_, c) => *c,
        };
        let mut notes = Vec::new();
        let img = decode_image(pdf, &stream, res, fill, &mut notes, 0);
        for n in notes {
            self.note(&n);
        }
        img
    }
}

#[derive(Default)]
struct TextAcc {
    glyphs: Vec<PlacedGlyph>,
    size: f64,
    type3: bool,
}

/// A PDF text string (PDFDocEncoding or UTF-16BE with a byte order mark) as a `String`.
pub fn decode_text_string(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from(c[0]) << 8 | u16::from(c[1])).collect();
        return String::from_utf16_lossy(&units);
    }
    if b.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from(c[1]) << 8 | u16::from(c[0])).collect();
        return String::from_utf16_lossy(&units);
    }
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&b[3..]).into_owned();
    }
    b.iter()
        .map(|&c| match c {
            0x80 => '\u{2022}',
            0x84 => '\u{2014}',
            0x85 => '\u{2013}',
            0x8D => '\u{201C}',
            0x8E => '\u{201D}',
            0x8F => '\u{2018}',
            0x90 => '\u{2019}',
            0x8B => '\u{2026}',
            _ => char::from(c),
        })
        .collect()
}
