//! Writing PDF files: an object builder with Flate compression and optional AES-256 encryption,
//! and the conversion of pictures into pages.
use crate::crypt::Encryptor;
use crate::object::{Dict, Obj, Stream};
use oc_core::{Error, ErrorCode, Result};
use oc_image::{Depth, Image, Layout};
use std::sync::Arc;

pub struct Builder {
    objects: Vec<Option<Obj>>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn name(s: &str) -> Obj {
    Obj::Name(s.to_string())
}

pub fn dict(items: Vec<(&str, Obj)>) -> Dict {
    items.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

pub fn reals(v: &[f64]) -> Obj {
    Obj::Array(v.iter().map(|x| Obj::Real(*x)).collect())
}

/// A PDF text string: ASCII as is, anything else as UTF-16BE with a byte order mark.
pub fn text_string(s: &str) -> Obj {
    if s.is_ascii() {
        return Obj::Str(s.as_bytes().to_vec());
    }
    let mut v = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_be_bytes());
    }
    Obj::Str(v)
}

/// `D:YYYYMMDDHHmmSSZ` for a Unix time.
pub fn pdf_date(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "D:{y:04}{m:02}{d:02}{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            objects: Vec::new(),
        }
    }

    /// Reserve an object number to fill in later.
    pub fn reserve(&mut self) -> u32 {
        self.objects.push(None);
        self.objects.len() as u32
    }

    pub fn set(&mut self, n: u32, o: Obj) {
        if let Some(slot) = self.objects.get_mut((n as usize).wrapping_sub(1)) {
            *slot = Some(o);
        }
    }

    pub fn add(&mut self, o: Obj) -> Obj {
        let n = self.reserve();
        self.set(n, o);
        Obj::Ref(n, 0)
    }

    /// A stream object. With `compress`, data that has no filter yet is deflated.
    pub fn add_stream(&mut self, mut d: Dict, data: Vec<u8>, compress: bool) -> Obj {
        let raw = if compress && !d.contains_key("Filter") && data.len() > 32 {
            let z = oc_compress::zlib::compress(&data, 9);
            if z.len() < data.len() {
                d.insert("Filter".into(), name("FlateDecode"));
                z
            } else {
                data
            }
        } else {
            data
        };
        self.add(Obj::Stream(Arc::new(Stream {
            dict: d,
            raw,
            id: (0, 0),
        })))
    }

    /// Serialise the file. `root` is the catalog and `info` the optional document information.
    pub fn finish(self, root: u32, info: Option<u32>, enc: Option<&Encryptor>) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::with_capacity(self.objects.len());
        for (i, o) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            let num = i as u32 + 1;
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            match o {
                Some(o) => write_obj(&mut out, o, enc),
                None => out.extend_from_slice(b"null"),
            }
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", self.objects.len() + 1).as_bytes(),
        );
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        let id = crate::crypt::random_bytes(16);
        let mut trailer = Dict::new();
        trailer.insert("Size".into(), Obj::Int(self.objects.len() as i64 + 1));
        trailer.insert("Root".into(), Obj::Ref(root, 0));
        if let Some(i) = info {
            trailer.insert("Info".into(), Obj::Ref(i, 0));
        }
        trailer.insert(
            "ID".into(),
            Obj::Array(vec![Obj::Str(id.clone()), Obj::Str(id)]),
        );
        if let Some(e) = enc {
            trailer.insert("Encrypt".into(), Obj::Dict(e.dict.clone()));
        }
        out.extend_from_slice(b"trailer\n");
        // the trailer's own strings stay in the clear
        write_obj(&mut out, &Obj::Dict(trailer), None);
        out.extend_from_slice(format!("\nstartxref\n{xref}\n%%EOF\n").as_bytes());
        out
    }
}

fn write_real(out: &mut Vec<u8>, v: f64) {
    if !v.is_finite() {
        out.push(b'0');
        return;
    }
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    out.extend_from_slice(if s.is_empty() || s == "-" || s == "-0" {
        b"0"
    } else {
        s.as_bytes()
    });
}

fn write_name(out: &mut Vec<u8>, n: &str) {
    out.push(b'/');
    for b in n.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+') {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}

fn write_str(out: &mut Vec<u8>, s: &[u8], enc: Option<&Encryptor>) {
    let data = match enc {
        Some(e) => e.encrypt(s),
        None => s.to_vec(),
    };
    out.push(b'<');
    for b in data {
        out.extend_from_slice(format!("{b:02X}").as_bytes());
    }
    out.push(b'>');
}

pub fn write_obj(out: &mut Vec<u8>, o: &Obj, enc: Option<&Encryptor>) {
    match o {
        Obj::Null => out.extend_from_slice(b"null"),
        Obj::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Obj::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Obj::Real(r) => write_real(out, *r),
        Obj::Str(s) => write_str(out, s, enc),
        Obj::Name(n) => write_name(out, n),
        Obj::Array(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_obj(out, x, enc);
            }
            out.push(b']');
        }
        Obj::Dict(d) => write_dict(out, d, enc),
        Obj::Stream(s) => {
            let data = match enc {
                Some(e) if s.dict.get("Type").and_then(Obj::as_name) != Some("XRef") => {
                    e.encrypt(&s.raw)
                }
                _ => s.raw.clone(),
            };
            let mut d = s.dict.clone();
            d.insert("Length".into(), Obj::Int(data.len() as i64));
            write_dict(out, &d, enc);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&data);
            out.extend_from_slice(b"\nendstream");
        }
        Obj::Ref(n, g) => out.extend_from_slice(format!("{n} {g} R").as_bytes()),
    }
}

fn write_dict(out: &mut Vec<u8>, d: &Dict, enc: Option<&Encryptor>) {
    out.extend_from_slice(b"<<");
    for (k, v) in d {
        write_name(out, k);
        out.push(b' ');
        write_obj(out, v, enc);
        out.push(b' ');
    }
    out.extend_from_slice(b">>");
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageSize {
    /// The page is as large as the picture at its resolution.
    Fit,
    A4,
    Letter,
}

#[derive(Debug, Clone)]
pub struct ImageOptions {
    pub page_size: PageSize,
    /// Margin in points (A4 and Letter only).
    pub margin: f64,
    /// Pixels per inch; 0 uses what the picture says, else 96.
    pub dpi: f64,
    pub title: Option<String>,
    /// Clockwise rotation of every page, in degrees.
    pub rotate: i64,
    pub encrypt: Option<(Vec<u8>, Vec<u8>)>,
}

impl Default for ImageOptions {
    fn default() -> Self {
        ImageOptions {
            page_size: PageSize::Fit,
            margin: 0.0,
            dpi: 0.0,
            title: None,
            rotate: 0,
            encrypt: None,
        }
    }
}

/// A picture to place on a page: a JPEG that is stored as it is, or decoded pixels.
pub enum Source {
    Jpeg(Vec<u8>, Option<f64>),
    Raster(Image),
}

struct JpegInfo {
    width: u32,
    height: u32,
    components: u8,
    precision: u8,
    adobe: bool,
}

fn jpeg_info(d: &[u8]) -> Option<JpegInfo> {
    if d.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    let mut adobe = false;
    while i + 4 <= d.len() {
        if d[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = d[i + 1];
        if m == 0xFF || m == 0x00 || (0xD0..=0xD8).contains(&m) || m == 0x01 {
            i += if m == 0xFF { 1 } else { 2 };
            continue;
        }
        let len = usize::from(d[i + 2]) << 8 | usize::from(d[i + 3]);
        let seg = d.get(i + 4..i + 2 + len)?;
        if m == 0xEE && seg.starts_with(b"Adobe") {
            adobe = true;
        }
        if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            let precision = *seg.first()?;
            let height = u32::from(*seg.get(1)?) << 8 | u32::from(*seg.get(2)?);
            let width = u32::from(*seg.get(3)?) << 8 | u32::from(*seg.get(4)?);
            let components = *seg.get(5)?;
            if matches!(m, 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) && m != 0xC2 {
                // lossless and arithmetic-coded variants are not valid in DCTDecode
                return None;
            }
            return Some(JpegInfo {
                width,
                height,
                components,
                precision,
                adobe,
            });
        }
        i += 2 + len;
    }
    None
}

fn to_eight(img: &Image) -> Image {
    if img.depth == Depth::Eight {
        return img.clone();
    }
    oc_image::ops::convert_depth(img, Depth::Eight).0
}

/// The colour channels and, when there is one, the alpha channel, each 8-bit.
fn split_alpha(img: &Image) -> (Image, Option<Vec<u8>>) {
    let n = img.layout.channels();
    if !img.layout.has_alpha() {
        return (img.clone(), None);
    }
    let base_layout = if img.layout.is_gray() {
        Layout::Gray
    } else {
        Layout::Rgb
    };
    let keep = n - 1;
    let mut color = Vec::with_capacity(img.data.len() / n * keep);
    let mut alpha = Vec::with_capacity(img.data.len() / n);
    for px in img.data.chunks_exact(n) {
        color.extend_from_slice(&px[..keep]);
        alpha.push(px[keep]);
    }
    let opaque = alpha.iter().all(|a| *a == 255);
    let base = Image {
        width: img.width,
        height: img.height,
        layout: base_layout,
        depth: Depth::Eight,
        data: color,
        metadata: img.metadata.clone(),
    };
    (base, (!opaque).then_some(alpha))
}

fn icc_colorspace(b: &mut Builder, img: &Image) -> Obj {
    let plain = if img.layout.is_gray() {
        name("DeviceGray")
    } else {
        name("DeviceRGB")
    };
    let Some(p) = &img.metadata.icc_profile else {
        return plain;
    };
    let space = p.get(16..20).unwrap_or_default();
    let n = img.layout.channels() - usize::from(img.layout.has_alpha());
    let ok = (space == b"GRAY" && n == 1) || (space == b"RGB " && n == 3);
    if !ok || p.len() < 128 {
        return plain;
    }
    let s = b.add_stream(
        dict(vec![
            ("N", Obj::Int(n as i64)),
            ("Alternate", plain.clone()),
        ]),
        p.clone(),
        true,
    );
    Obj::Array(vec![name("ICCBased"), s])
}

fn flate_image(b: &mut Builder, img: &Image, extra: Vec<(&str, Obj)>) -> Obj {
    let img8 = to_eight(img);
    let (w, h) = (img8.width as i64, img8.height as i64);
    let bilevel = img8.layout == Layout::Gray && img8.data.iter().all(|v| *v == 0 || *v == 255);
    let mut d = dict(vec![
        ("Type", name("XObject")),
        ("Subtype", name("Image")),
        ("Width", Obj::Int(w)),
        ("Height", Obj::Int(h)),
        ("Filter", name("FlateDecode")),
    ]);
    let data;
    if bilevel {
        let rb = (img8.width as usize).div_ceil(8);
        let mut packed = Vec::with_capacity(rb * img8.height as usize);
        for row in img8.data.chunks_exact(img8.width as usize) {
            let mut bytes = vec![0u8; rb];
            for (x, v) in row.iter().enumerate() {
                if *v == 255 {
                    bytes[x / 8] |= 0x80 >> (x % 8);
                }
            }
            packed.extend(bytes);
        }
        let packed_img = Image {
            width: img8.width.div_ceil(8),
            height: img8.height,
            layout: Layout::Gray,
            depth: Depth::Eight,
            data: packed,
            metadata: Default::default(),
        };
        data = oc_image::png::deflate_filtered(
            &packed_img,
            &oc_image::png::EncodeOptions {
                level: 9,
                adaptive_filter: true,
            },
        );
        d.insert("ColorSpace".into(), name("DeviceGray"));
        d.insert("BitsPerComponent".into(), Obj::Int(1));
        d.insert(
            "DecodeParms".into(),
            Obj::Dict(dict(vec![
                ("Predictor", Obj::Int(15)),
                ("Colors", Obj::Int(1)),
                ("BitsPerComponent", Obj::Int(1)),
                ("Columns", Obj::Int(w)),
            ])),
        );
    } else {
        let colors = img8.layout.channels();
        data = oc_image::png::deflate_filtered(
            &img8,
            &oc_image::png::EncodeOptions {
                level: 9,
                adaptive_filter: true,
            },
        );
        d.insert("ColorSpace".into(), icc_colorspace(b, img));
        d.insert("BitsPerComponent".into(), Obj::Int(8));
        d.insert(
            "DecodeParms".into(),
            Obj::Dict(dict(vec![
                ("Predictor", Obj::Int(15)),
                ("Colors", Obj::Int(colors as i64)),
                ("BitsPerComponent", Obj::Int(8)),
                ("Columns", Obj::Int(w)),
            ])),
        );
    }
    for (k, v) in extra {
        d.insert(k.to_string(), v);
    }
    b.add_stream(d, data, false)
}

/// Add a picture as an image XObject: JPEG data as it is, pixels as Flate (with a soft mask for
/// alpha). Returns the object and the size in pixels, with the resolution the picture states.
pub fn add_image_source(b: &mut Builder, src: Source) -> Result<(Obj, u32, u32, Option<f64>)> {
    Ok(match src {
        Source::Jpeg(data, dpi) => {
            let info = jpeg_info(&data).ok_or_else(|| {
                Error::new(
                    ErrorCode::CorruptFile,
                    "the JPEG has no readable frame header",
                )
            })?;
            if info.precision != 8 || !matches!(info.components, 1 | 3 | 4) {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "only 8-bit JPEG with 1, 3 or 4 components can be stored in a PDF as it is",
                ));
            }
            let cs = match info.components {
                1 => name("DeviceGray"),
                3 => name("DeviceRGB"),
                _ => name("DeviceCMYK"),
            };
            let mut d = dict(vec![
                ("Type", name("XObject")),
                ("Subtype", name("Image")),
                ("Width", Obj::Int(i64::from(info.width))),
                ("Height", Obj::Int(i64::from(info.height))),
                ("ColorSpace", cs),
                ("BitsPerComponent", Obj::Int(8)),
                ("Filter", name("DCTDecode")),
            ]);
            if info.components == 4 && info.adobe {
                // Adobe writes CMYK JPEG inverted
                d.insert(
                    "Decode".into(),
                    Obj::Array(
                        [1, 0, 1, 0, 1, 0, 1, 0]
                            .iter()
                            .map(|v| Obj::Int(*v))
                            .collect(),
                    ),
                );
            }
            (b.add_stream(d, data, false), info.width, info.height, dpi)
        }
        Source::Raster(img) => {
            let img = to_eight(&img);
            let (base, alpha) = split_alpha(&img);
            let mut extra = Vec::new();
            if let Some(a) = alpha {
                let amask = Image {
                    width: img.width,
                    height: img.height,
                    layout: Layout::Gray,
                    depth: Depth::Eight,
                    data: a,
                    metadata: Default::default(),
                };
                let sm = flate_image(b, &amask, Vec::new());
                extra.push(("SMask", sm));
            }
            let dpi = img.metadata.dpi.map(|(x, _)| x);
            (flate_image(b, &base, extra), img.width, img.height, dpi)
        }
    })
}

/// Place each picture on its own page.
pub fn images_to_pdf(images: Vec<Source>, opts: &ImageOptions) -> Result<Vec<u8>> {
    if images.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidOptions,
            "no pictures to write",
        ));
    }
    let mut b = Builder::new();
    let catalog = b.reserve();
    let pages_node = b.reserve();
    let mut page_refs = Vec::new();
    for src in images {
        let (xobj, wpx, hpx, meta_dpi) = add_image_source(&mut b, src)?;
        let dpi = if opts.dpi > 0.0 {
            opts.dpi
        } else {
            meta_dpi
                .filter(|d| *d >= 10.0 && *d <= 10000.0)
                .unwrap_or(96.0)
        };
        let (iw, ih) = (f64::from(wpx) * 72.0 / dpi, f64::from(hpx) * 72.0 / dpi);
        let (pw, ph, x, y, w, h) = match opts.page_size {
            PageSize::Fit => (iw, ih, 0.0, 0.0, iw, ih),
            ps => {
                let (a, bb): (f64, f64) = if ps == PageSize::A4 {
                    (595.276, 841.89)
                } else {
                    (612.0, 792.0)
                };
                let (pw, ph) = if iw > ih { (bb, a) } else { (a, bb) };
                let m = opts.margin.clamp(0.0, pw.min(ph) / 3.0);
                let s = ((pw - 2.0 * m) / iw).min((ph - 2.0 * m) / ih);
                let (w, h) = (iw * s, ih * s);
                (pw, ph, (pw - w) / 2.0, (ph - h) / 2.0, w, h)
            }
        };
        let content = format!("q {w:.4} 0 0 {h:.4} {x:.4} {y:.4} cm /Im0 Do Q");
        let cs = b.add_stream(Dict::new(), content.into_bytes(), true);
        let res = dict(vec![("XObject", Obj::Dict(dict(vec![("Im0", xobj)])))]);
        let mut page = dict(vec![
            ("Type", name("Page")),
            ("Parent", Obj::Ref(pages_node, 0)),
            ("MediaBox", reals(&[0.0, 0.0, pw, ph])),
            ("Resources", Obj::Dict(res)),
            ("Contents", cs),
        ]);
        if opts.rotate.rem_euclid(360) != 0 {
            page.insert("Rotate".into(), Obj::Int(opts.rotate.rem_euclid(360)));
        }
        let page = b.add(Obj::Dict(page));
        page_refs.push(page);
    }
    let count = page_refs.len() as i64;
    b.set(
        pages_node,
        Obj::Dict(dict(vec![
            ("Type", name("Pages")),
            ("Kids", Obj::Array(page_refs)),
            ("Count", Obj::Int(count)),
        ])),
    );
    b.set(
        catalog,
        Obj::Dict(dict(vec![
            ("Type", name("Catalog")),
            ("Pages", Obj::Ref(pages_node, 0)),
        ])),
    );
    let mut info = dict(vec![
        ("Producer", text_string("OpenConverter")),
        ("CreationDate", text_string(&pdf_date(now_secs()))),
    ]);
    if let Some(t) = &opts.title {
        info.insert("Title".into(), text_string(t));
    }
    let info = b.add(Obj::Dict(info));
    let enc = opts.encrypt.as_ref().map(|(u, o)| Encryptor::new(u, o, -4));
    let Obj::Ref(info_num, _) = info else {
        return Err(Error::new(ErrorCode::InternalError, "info object"));
    };
    Ok(b.finish(catalog, Some(info_num), enc.as_ref()))
}
