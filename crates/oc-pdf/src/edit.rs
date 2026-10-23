//! PDF to PDF: choosing, ordering and rotating pages, merging files, recompressing, removing
//! metadata and encrypting. The result is a new file made of copies of what the pages use.
use crate::crypt::Encryptor;
use crate::doc::{PageInfo, Pdf};
use crate::filter::{decode_stream, decrypted_raw, filter_chain};
use crate::object::{Dict, Obj, Stream};
use crate::writer::{dict, name, now_secs, pdf_date, text_string, Builder};
use oc_core::{Error, ErrorCode, Result};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct Edit {
    /// Added to every page's rotation, in degrees (a multiple of 90).
    pub rotate: i64,
    /// Deflate streams that are stored raw or deflated without a predictor.
    pub compress: bool,
    /// Leave out the information dictionary and XMP metadata.
    pub strip_metadata: bool,
    /// User and owner passwords for AES-256 encryption.
    pub encrypt: Option<(Vec<u8>, Vec<u8>)>,
}

/// Page numbers (1-based) from text such as `1-3,5,last`, `7-`, `odd`, `3,1,2`.
pub fn parse_pages(spec: &str, total: usize) -> Result<Vec<usize>> {
    let bad = |m: String| Error::invalid_option("pages", m);
    let num = |t: &str| -> Result<usize> {
        let t = t.trim();
        if t.eq_ignore_ascii_case("last") {
            return Ok(total);
        }
        t.parse::<usize>()
            .map_err(|_| bad(format!("'{t}' is not a page number")))
    };
    let check = |n: usize| -> Result<usize> {
        if n == 0 || n > total {
            Err(bad(format!(
                "page {n} does not exist (the file has {total} pages)"
            )))
        } else {
            Ok(n - 1)
        }
    };
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "all" => out.extend(0..total),
            "odd" => out.extend((0..total).step_by(2)),
            "even" => out.extend((1..total).step_by(2)),
            _ => {
                if let Some((a, b)) = part.split_once('-') {
                    let from = if a.trim().is_empty() { 1 } else { num(a)? };
                    let to = if b.trim().is_empty() { total } else { num(b)? };
                    let (from, to) = (check(from)?, check(to)?);
                    if from <= to {
                        out.extend(from..=to);
                    } else {
                        out.extend((to..=from).rev());
                    }
                } else {
                    out.push(check(num(part)?)?);
                }
            }
        }
        if out.len() > 200_000 {
            return Err(bad("too many pages selected".into()));
        }
    }
    if out.is_empty() {
        return Err(bad("no pages selected".into()));
    }
    Ok(out)
}

struct Copier<'a> {
    pdf: &'a Pdf,
    map: HashMap<u32, u32>,
    pending: Vec<(u32, u32)>,
    page_nums: HashSet<u32>,
    page_map: HashMap<u32, u32>,
    compress: bool,
    dropped: bool,
}

impl<'a> Copier<'a> {
    fn copy_ref(&mut self, b: &mut Builder, n: u32) -> Obj {
        if self.page_nums.contains(&n) {
            return self.page_map.get(&n).map_or(Obj::Null, |m| Obj::Ref(*m, 0));
        }
        if let Some(m) = self.map.get(&n) {
            return Obj::Ref(*m, 0);
        }
        // what would drag the page tree or the catalog along is cut
        if let Obj::Dict(d) = self.pdf.get(n) {
            let ty = d.get("Type").and_then(Obj::as_name);
            if matches!(ty, Some("Pages" | "Catalog" | "Page")) {
                self.dropped = true;
                return Obj::Null;
            }
        }
        let new = b.reserve();
        self.map.insert(n, new);
        self.pending.push((n, new));
        Obj::Ref(new, 0)
    }

    fn copy(&mut self, b: &mut Builder, o: &Obj, depth: u32) -> Obj {
        if depth > 80 {
            return Obj::Null;
        }
        match o {
            Obj::Ref(n, _) => self.copy_ref(b, *n),
            Obj::Array(a) => Obj::Array(a.iter().map(|x| self.copy(b, x, depth + 1)).collect()),
            Obj::Dict(d) => Obj::Dict(self.copy_dict(b, d, depth)),
            Obj::Stream(s) => self.copy_stream(b, s, depth),
            o => o.clone(),
        }
    }

    fn copy_dict(&mut self, b: &mut Builder, d: &Dict, depth: u32) -> Dict {
        d.iter()
            .map(|(k, v)| (k.clone(), self.copy(b, v, depth + 1)))
            .collect()
    }

    fn copy_stream(&mut self, b: &mut Builder, s: &Arc<Stream>, depth: u32) -> Obj {
        let mut without_length = s.dict.clone();
        without_length.remove("Length");
        let mut d = self.copy_dict(b, &without_length, depth);
        let mut data = decrypted_raw(self.pdf, s);
        if self.compress {
            let chain = filter_chain(self.pdf, &s.dict);
            let plain_flate = chain.len() == 1
                && chain[0].0 == "FlateDecode"
                && !chain[0].1.contains_key("Predictor");
            if chain.is_empty() || plain_flate {
                let probe = Stream {
                    dict: s.dict.clone(),
                    raw: data.clone(),
                    id: (0, 0),
                };
                if let Ok((plain, None)) = decode_stream(self.pdf, &probe, false) {
                    let z = oc_compress::zlib::compress(&plain, 9);
                    if z.len() < data.len() || chain.is_empty() {
                        data = z;
                        d.insert("Filter".into(), name("FlateDecode"));
                        d.remove("DecodeParms");
                    }
                }
            }
        }
        Obj::Stream(Arc::new(Stream {
            dict: d,
            raw: data,
            id: (0, 0),
        }))
    }

    fn drain(&mut self, b: &mut Builder) {
        while let Some((src, new)) = self.pending.pop() {
            let obj = self.pdf.get(src);
            let c = self.copy(b, &obj, 0);
            b.set(new, c);
        }
    }
}

/// One input of [`assemble`]: a file and which of its pages (0-based, in output order).
pub struct Part<'a> {
    pub pdf: &'a Pdf,
    pub pages: Vec<usize>,
}

pub struct Assembled {
    pub data: Vec<u8>,
    pub notes: Vec<String>,
}

/// Build one PDF from the chosen pages of the given files.
pub fn assemble(parts: &[Part], edit: &Edit) -> Result<Assembled> {
    if edit.rotate % 90 != 0 {
        return Err(Error::invalid_option("rotate", "must be a multiple of 90"));
    }
    let mut b = Builder::new();
    let catalog = b.reserve();
    let pages_node = b.reserve();
    let mut kids = Vec::new();
    let mut notes = Vec::new();
    let mut lost_structure = false;
    let mut xmp: Option<Obj> = None;
    for (pi, part) in parts.iter().enumerate() {
        let infos: Vec<PageInfo> = part.pdf.pages();
        let mut c = Copier {
            pdf: part.pdf,
            map: HashMap::new(),
            pending: Vec::new(),
            page_nums: infos.iter().map(|p| p.num).collect(),
            page_map: HashMap::new(),
            compress: edit.compress,
            dropped: false,
        };
        // page objects first so links between the chosen pages can be kept
        let mut new_pages = Vec::new();
        for &i in &part.pages {
            let info = infos.get(i).ok_or_else(|| {
                Error::new(
                    ErrorCode::InvalidOptions,
                    format!("page {} does not exist", i + 1),
                )
            })?;
            let n = b.reserve();
            c.page_map.entry(info.num).or_insert(n);
            new_pages.push((n, info));
        }
        for (n, info) in &new_pages {
            let mut d = Dict::new();
            for (k, v) in &info.dict {
                if k == "Parent" {
                    continue;
                }
                d.insert(k.clone(), c.copy(&mut b, v, 1));
            }
            d.insert("Type".into(), name("Page"));
            d.insert("Parent".into(), Obj::Ref(pages_node, 0));
            let rot = (part.pdf.dget_i64(&info.dict, "Rotate").unwrap_or(0) + edit.rotate)
                .rem_euclid(360);
            if rot != 0 {
                d.insert("Rotate".into(), Obj::Int(rot));
            } else {
                d.remove("Rotate");
            }
            if !d.contains_key("MediaBox") {
                d.insert(
                    "MediaBox".into(),
                    Obj::Array([0, 0, 612, 792].iter().map(|v| Obj::Int(*v)).collect()),
                );
            }
            b.set(*n, Obj::Dict(d));
            kids.push(Obj::Ref(*n, 0));
        }
        c.drain(&mut b);
        if pi == 0 && !edit.strip_metadata {
            if let Some(cat) = part.pdf.catalog_dict() {
                if let Some(m) = cat.get("Metadata") {
                    let o = c.copy(&mut b, m, 1);
                    c.drain(&mut b);
                    xmp = Some(o);
                }
            }
        }
        if let Some(cat) = part.pdf.catalog_dict() {
            let all = part.pages.len() == infos.len();
            for k in [
                "Outlines",
                "AcroForm",
                "Names",
                "StructTreeRoot",
                "PageLabels",
            ] {
                if cat.contains_key(k) && (!all || parts.len() > 1 || k == "StructTreeRoot") {
                    lost_structure = true;
                }
            }
            if cat.contains_key("AcroForm")
                || cat.contains_key("Names")
                || cat.contains_key("Outlines")
            {
                lost_structure = true;
            }
        }
    }
    if lost_structure {
        notes.push("bookmarks, form fields, attachments and tagged structure are not carried over; the pages keep their look and links between the chosen pages".to_string());
    }
    let count = kids.len() as i64;
    b.set(
        pages_node,
        Obj::Dict(dict(vec![
            ("Type", name("Pages")),
            ("Kids", Obj::Array(kids)),
            ("Count", Obj::Int(count)),
        ])),
    );
    let mut cat = dict(vec![
        ("Type", name("Catalog")),
        ("Pages", Obj::Ref(pages_node, 0)),
    ]);
    if let Some(x) = xmp {
        cat.insert("Metadata".into(), x);
    }
    b.set(catalog, Obj::Dict(cat));
    let mut info = Dict::new();
    if !edit.strip_metadata {
        if let Some(p) = parts.first() {
            for (k, v) in p.pdf.info_dict() {
                if let Obj::Str(_) | Obj::Name(_) = p.pdf.resolve(&v) {
                    info.insert(k, p.pdf.resolve(&v));
                }
            }
        }
    }
    info.insert("Producer".into(), text_string("OpenConverter"));
    info.insert("ModDate".into(), text_string(&pdf_date(now_secs())));
    let info_ref = b.add(Obj::Dict(info));
    let Obj::Ref(info_num, _) = info_ref else {
        return Err(Error::new(ErrorCode::InternalError, "info object"));
    };
    let enc = edit.encrypt.as_ref().map(|(u, o)| Encryptor::new(u, o, -4));
    Ok(Assembled {
        data: b.finish(catalog, Some(info_num), enc.as_ref()),
        notes,
    })
}
