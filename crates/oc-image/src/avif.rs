//! AVIF read support: the HEIF item structure on top of the `oc-av1` decoder.
//!
//! Handles the primary image item (a coded `av01` item or a `grid` of them), an auxiliary alpha
//! image, 8- to 12-bit 4:0:0 / 4:2:0 / 4:2:2 / 4:4:4 content, CICP colour (`colr` `nclx`) or ICC
//! (`colr` `prof`), and the `clap`, `irot` and `imir` transformative properties. Animated
//! (`avis`) files are read as their primary still.
#![allow(clippy::indexing_slicing, clippy::needless_range_loop)]
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_av1::Picture;
use oc_container::isobmff::{be16, be32, be64, boxes, child, BoxRef};
use oc_core::{Error, Limits, Notes, Result};

/// True when the buffer starts with an `ftyp` box naming an AVIF brand.
pub fn sniff(data: &[u8]) -> bool {
    if data.len() < 16 || &data[4..8] != b"ftyp" {
        return false;
    }
    let size = (be32(data, 0).unwrap_or(0) as usize).clamp(16, data.len());
    let mut o = 8;
    while o + 4 <= size {
        if o != 12 && matches!(&data[o..o + 4], b"avif" | b"avis") {
            return true;
        }
        o += 4;
    }
    false
}

#[derive(Default, Clone)]
struct Item {
    id: u32,
    kind: [u8; 4],
    /// (construction method, offset, length) extents.
    extents: Vec<(u8, u64, u64)>,
    props: Vec<usize>,
}

#[derive(Default)]
struct Meta<'a> {
    primary: u32,
    items: Vec<Item>,
    /// ipco children, in order (index 0 is property 1).
    props: Vec<BoxRef<'a>>,
    /// (type, from, to)
    refs: Vec<([u8; 4], u32, u32)>,
    idat: &'a [u8],
}

fn version_flags(b: &[u8]) -> Option<(u8, u32)> {
    Some((*b.first()?, be32(b, 0)? & 0xff_ffff))
}

fn read_n(b: &[u8], o: usize, n: usize) -> Option<u64> {
    match n {
        0 => Some(0),
        4 => be32(b, o).map(u64::from),
        8 => be64(b, o),
        _ => None,
    }
}

fn parse_meta<'a>(meta: &'a [u8]) -> Result<Meta<'a>> {
    let mut m = Meta::default();
    // `meta` is a full box: skip version/flags.
    let body = meta
        .get(4..)
        .ok_or_else(|| Error::truncated("AVIF: meta box"))?;
    let bx = boxes(body);
    let find = |k: &[u8; 4]| bx.iter().find(|b| &b.kind == k).copied();
    if let Some(p) = find(b"pitm") {
        let (v, _) = version_flags(p.body).ok_or_else(|| Error::truncated("AVIF: pitm"))?;
        m.primary = if v == 0 {
            be16(p.body, 4).map(u32::from)
        } else {
            be32(p.body, 4)
        }
        .ok_or_else(|| Error::truncated("AVIF: pitm"))?;
    }
    if let Some(i) = find(b"idat") {
        m.idat = i.body;
    }
    if let Some(inf) = find(b"iinf") {
        let (v, _) = version_flags(inf.body).ok_or_else(|| Error::truncated("AVIF: iinf"))?;
        let rest = inf.body.get(if v == 0 { 6 } else { 8 }..).unwrap_or(&[]);
        for e in boxes(rest) {
            if &e.kind != b"infe" {
                continue;
            }
            let Some((ev, _)) = version_flags(e.body) else {
                continue;
            };
            if ev < 2 {
                continue;
            }
            let (id, o) = if ev == 2 {
                (be16(e.body, 4).map(u32::from), 6)
            } else {
                (be32(e.body, 4), 8)
            };
            let (Some(id), Some(t)) = (id, e.body.get(o + 2..o + 6)) else {
                continue;
            };
            let mut kind = [0u8; 4];
            kind.copy_from_slice(t);
            m.items.push(Item {
                id,
                kind,
                ..Default::default()
            });
        }
    }
    if let Some(loc) = find(b"iloc") {
        let b = loc.body;
        let (v, _) = version_flags(b).ok_or_else(|| Error::truncated("AVIF: iloc"))?;
        let t = |e: &str| Error::truncated(format!("AVIF: iloc {e}"));
        let sizes = *b.get(4).ok_or_else(|| t("sizes"))?;
        let sizes2 = *b.get(5).ok_or_else(|| t("sizes"))?;
        let (offset_size, length_size) = (usize::from(sizes >> 4), usize::from(sizes & 15));
        let (base_size, index_size) = (
            usize::from(sizes2 >> 4),
            usize::from(if v == 1 || v == 2 { sizes2 & 15 } else { 0 }),
        );
        let (count, mut o) = if v < 2 {
            (u32::from(be16(b, 6).ok_or_else(|| t("count"))?), 8)
        } else {
            (be32(b, 6).ok_or_else(|| t("count"))?, 10)
        };
        for _ in 0..count {
            let id = if v < 2 {
                let x = u32::from(be16(b, o).ok_or_else(|| t("item"))?);
                o += 2;
                x
            } else {
                let x = be32(b, o).ok_or_else(|| t("item"))?;
                o += 4;
                x
            };
            let mut method = 0u8;
            if v == 1 || v == 2 {
                method = (be16(b, o).ok_or_else(|| t("method"))? & 15) as u8;
                o += 2;
            }
            o += 2; // data_reference_index
            let base = read_n(b, o, base_size).ok_or_else(|| t("base"))?;
            o += base_size;
            let n = be16(b, o).ok_or_else(|| t("extents"))?;
            o += 2;
            let mut extents = Vec::new();
            for _ in 0..n {
                o += index_size;
                let off = read_n(b, o, offset_size).ok_or_else(|| t("extent"))?;
                o += offset_size;
                let len = read_n(b, o, length_size).ok_or_else(|| t("extent"))?;
                o += length_size;
                extents.push((method, base.saturating_add(off), len));
            }
            if let Some(it) = m.items.iter_mut().find(|i| i.id == id) {
                it.extents = extents;
            }
        }
    }
    if let Some(prp) = find(b"iprp") {
        if let Some(co) = child(prp.body, b"ipco") {
            m.props = boxes(co.body);
        }
        if let Some(ma) = child(prp.body, b"ipma") {
            let b = ma.body;
            let (v, flags) = version_flags(b).ok_or_else(|| Error::truncated("AVIF: ipma"))?;
            let count = be32(b, 4).ok_or_else(|| Error::truncated("AVIF: ipma"))?;
            let mut o = 8;
            for _ in 0..count {
                let id = if v < 1 {
                    let x = u32::from(be16(b, o).ok_or_else(|| Error::truncated("AVIF: ipma"))?);
                    o += 2;
                    x
                } else {
                    let x = be32(b, o).ok_or_else(|| Error::truncated("AVIF: ipma"))?;
                    o += 4;
                    x
                };
                let n = *b.get(o).ok_or_else(|| Error::truncated("AVIF: ipma"))?;
                o += 1;
                let mut list = Vec::new();
                for _ in 0..n {
                    let idx = if flags & 1 == 1 {
                        let x = be16(b, o).ok_or_else(|| Error::truncated("AVIF: ipma"))?;
                        o += 2;
                        usize::from(x & 0x7fff)
                    } else {
                        let x = *b.get(o).ok_or_else(|| Error::truncated("AVIF: ipma"))?;
                        o += 1;
                        usize::from(x & 0x7f)
                    };
                    if idx > 0 {
                        list.push(idx - 1);
                    }
                }
                if let Some(it) = m.items.iter_mut().find(|i| i.id == id) {
                    it.props = list;
                }
            }
        }
    }
    if let Some(r) = find(b"iref") {
        let (v, _) = version_flags(r.body).ok_or_else(|| Error::truncated("AVIF: iref"))?;
        for rb in boxes(r.body.get(4..).unwrap_or(&[])) {
            let id = |o: usize| {
                if v == 0 {
                    be16(rb.body, o).map(u32::from)
                } else {
                    be32(rb.body, o)
                }
            };
            let w = if v == 0 { 2 } else { 4 };
            let Some(from) = id(0) else { continue };
            let Some(n) = be16(rb.body, w) else { continue };
            for k in 0..usize::from(n) {
                if let Some(to) = id(w + 2 + k * w) {
                    m.refs.push((rb.kind, from, to));
                }
            }
        }
    }
    Ok(m)
}

impl<'a> Meta<'a> {
    fn item(&self, id: u32) -> Result<&Item> {
        self.items
            .iter()
            .find(|i| i.id == id)
            .ok_or_else(|| Error::corrupt(format!("AVIF: missing item {id}")))
    }

    fn item_data(&self, file: &[u8], id: u32) -> Result<Vec<u8>> {
        let it = self.item(id)?;
        let mut out = Vec::new();
        for &(method, off, len) in &it.extents {
            let src = match method {
                0 => file,
                1 => self.idat,
                _ => {
                    return Err(Error::new(
                        oc_core::ErrorCode::UnsupportedFormat,
                        "AVIF: item stored by reference",
                    ))
                }
            };
            let len = if len == 0 {
                src.len().saturating_sub(off as usize) as u64
            } else {
                len
            };
            let end = off
                .checked_add(len)
                .ok_or_else(|| Error::corrupt("AVIF: extent overflow"))?;
            let s = src
                .get(off as usize..end as usize)
                .ok_or_else(|| Error::truncated("AVIF: item data"))?;
            out.extend_from_slice(s);
        }
        Ok(out)
    }

    fn prop(&self, it: &Item, kind: &[u8; 4]) -> Option<BoxRef<'a>> {
        it.props
            .iter()
            .filter_map(|&p| self.props.get(p))
            .find(|b| &b.kind == kind)
            .copied()
    }

    fn dimg_children(&self, id: u32) -> Vec<u32> {
        self.refs
            .iter()
            .filter(|r| &r.0 == b"dimg" && r.1 == id)
            .map(|r| r.2)
            .collect()
    }
}

fn decode_av1(data: &[u8], cfg: Option<&[u8]>) -> Result<Picture> {
    let mut dec = oc_av1::Decoder::new();
    if let Some(c) = cfg {
        if c.len() > 4 {
            let _ = dec.decode(&c[4..]);
        }
    }
    let pics = dec.decode(data)?;
    pics.into_iter()
        .last()
        .ok_or_else(|| Error::corrupt("AVIF: the image item holds no picture"))
}

fn decode_item(meta: &Meta, file: &[u8], id: u32, limits: &Limits, depth: u32) -> Result<Picture> {
    limits.check_depth(depth)?;
    let it = meta.item(id)?;
    match &it.kind {
        b"av01" => {
            if let Some(e) = meta.prop(it, b"ispe") {
                if let (Some(w), Some(h)) = (be32(e.body, 4), be32(e.body, 8)) {
                    limits.check_pixels(w, h)?;
                }
            }
            let data = meta.item_data(file, id)?;
            let cfg = meta.prop(it, b"av1C");
            decode_av1(&data, cfg.as_ref().map(|c| c.body))
        }
        b"grid" => {
            let d = meta.item_data(file, id)?;
            let (rows, cols) = (
                usize::from(*d.get(2).unwrap_or(&0)) + 1,
                usize::from(*d.get(3).unwrap_or(&0)) + 1,
            );
            let wide = d.first().copied().unwrap_or(0) == 0 && d.get(1).is_some_and(|f| f & 1 == 1);
            let (ow, oh) = if wide {
                (be32(&d, 4), be32(&d, 8))
            } else {
                (be16(&d, 4).map(u32::from), be16(&d, 6).map(u32::from))
            };
            let (Some(ow), Some(oh)) = (ow, oh) else {
                return Err(Error::truncated("AVIF: grid"));
            };
            limits.check_pixels(ow, oh)?;
            let tiles = meta.dimg_children(id);
            if tiles.len() < rows * cols {
                return Err(Error::corrupt("AVIF: grid references too few tiles"));
            }
            let mut out: Option<Picture> = None;
            let (mut tw, mut th) = (0usize, 0usize);
            for (n, &t) in tiles.iter().take(rows * cols).enumerate() {
                let p = decode_item(meta, file, t, limits, depth + 1)?;
                if out.is_none() {
                    tw = p.width;
                    th = p.height;
                    let mut o = Picture {
                        width: ow as usize,
                        height: oh as usize,
                        ..p.clone()
                    };
                    o.planes.clear();
                    o.plane_widths.clear();
                    o.plane_heights.clear();
                    for k in 0..p.planes.len() {
                        let (sx, sy) = if k == 0 {
                            (0, 0)
                        } else {
                            (p.subsampling_x, p.subsampling_y)
                        };
                        let (pw, ph) = ((ow as usize + sx) >> sx, (oh as usize + sy) >> sy);
                        o.planes.push(vec![0; pw * ph]);
                        o.plane_widths.push(pw);
                        o.plane_heights.push(ph);
                    }
                    out = Some(o);
                }
                let o = out.as_mut().ok_or_else(|| Error::corrupt("AVIF: grid"))?;
                if p.planes.len() != o.planes.len() || p.bit_depth != o.bit_depth {
                    return Err(Error::corrupt("AVIF: grid tiles differ in format"));
                }
                let (r, c) = (n / cols, n % cols);
                for k in 0..p.planes.len() {
                    let (sx, sy) = if k == 0 {
                        (0, 0)
                    } else {
                        (p.subsampling_x, p.subsampling_y)
                    };
                    let (x0, y0) = ((c * tw) >> sx, (r * th) >> sy);
                    let (dw, dh) = (o.plane_widths[k], o.plane_heights[k]);
                    for y in 0..p.plane_heights[k] {
                        if y0 + y >= dh {
                            break;
                        }
                        let n = p.plane_widths[k].min(dw.saturating_sub(x0));
                        if n == 0 {
                            continue;
                        }
                        let s = &p.planes[k][y * p.plane_widths[k]..y * p.plane_widths[k] + n];
                        o.planes[k][(y0 + y) * dw + x0..(y0 + y) * dw + x0 + n].copy_from_slice(s);
                    }
                }
            }
            out.ok_or_else(|| Error::corrupt("AVIF: empty grid"))
        }
        other => Err(Error::new(
            oc_core::ErrorCode::UnsupportedFormat,
            format!(
                "AVIF: image item type {} is not supported",
                String::from_utf8_lossy(other)
            ),
        )),
    }
}

struct Coeffs {
    kr: f32,
    kb: f32,
    identity: bool,
}

fn coeffs(mc: u8) -> Coeffs {
    let (kr, kb, identity) = match mc {
        0 => (0.2126, 0.0722, true),
        2 | 5 | 6 => (0.299, 0.114, false),
        7 => (0.212, 0.087, false),
        9 | 10 => (0.2627, 0.0593, false),
        _ => (0.2126, 0.0722, false),
    };
    Coeffs { kr, kb, identity }
}

/// Position tables for bilinear chroma upsampling (chroma co-sited with luma column 0 when
/// `cosited`, the "left" siting that 4:2:0 and 4:2:2 content uses by default): for each output index, the two source
/// indices and the weight of the second.
fn taps(n: usize, sub: usize, src: usize, cosited: bool) -> Vec<(usize, usize, f32)> {
    (0..n)
        .map(|i| {
            if sub == 0 {
                return (i.min(src - 1), i.min(src - 1), 0.0);
            }
            let f = if cosited {
                i as f32 / 2.0
            } else {
                (i as f32 + 0.5) / 2.0 - 0.5
            };
            let f0 = f.floor();
            let a = (f0.max(0.0) as usize).min(src - 1);
            let b = ((f0 + 1.0).max(0.0) as usize).min(src - 1);
            (a, b, (f - f0).clamp(0.0, 1.0))
        })
        .collect()
}

/// Convert the decoded planes to an RGB (or grey) image of the same size.
fn to_image(p: &Picture, mc: u8, full_range: bool, alpha: Option<&Picture>) -> Result<Image> {
    let (w, h) = (p.width, p.height);
    let bd = p.bit_depth;
    let mono = p.mono || p.planes.len() < 3;
    let layout = match (mono, alpha.is_some()) {
        (true, false) => Layout::Gray,
        (true, true) => Layout::GrayAlpha,
        (false, false) => Layout::Rgb,
        (false, true) => Layout::Rgba,
    };
    let depth = if bd > 8 { Depth::Sixteen } else { Depth::Eight };
    let maxv = ((1u32 << bd) - 1) as f32;
    let out_max = depth.max() as f32;
    let shift = (bd - 8) as i32;
    let (y_off, y_rng, c_rng) = if full_range {
        (0.0, maxv, maxv)
    } else {
        (
            (16 << shift) as f32,
            (219 << shift) as f32,
            (224 << shift) as f32,
        )
    };
    let mid = (128 << shift) as f32;
    let k = coeffs(mc);
    let ch = layout.channels();
    let mut data = Vec::with_capacity(w * h * ch * depth.bytes());
    let put = |data: &mut Vec<u8>, v: f32| {
        let q = (v * out_max + 0.5).clamp(0.0, out_max) as u32;
        match depth {
            Depth::Eight => data.push(q as u8),
            Depth::Sixteen => data.extend_from_slice(&(q as u16).to_be_bytes()),
        }
    };
    let (cxs, cys, cw, cwid) = if mono {
        (Vec::new(), Vec::new(), 0, 0)
    } else {
        (
            taps(w, p.subsampling_x, p.plane_widths[1], false),
            taps(h, p.subsampling_y, p.plane_heights[1], false),
            p.plane_widths[1],
            p.plane_widths[2],
        )
    };
    let a_scale = |a: &Picture, x: usize, y: usize| -> f32 {
        let ax = x.min(a.plane_widths[0] - 1);
        let ay = y.min(a.plane_heights[0] - 1);
        f32::from(a.planes[0][ay * a.plane_widths[0] + ax]) / ((1u32 << a.bit_depth) - 1) as f32
    };
    for y in 0..h {
        for x in 0..w {
            let yv = f32::from(p.planes[0][y * p.plane_widths[0] + x]);
            if mono {
                put(&mut data, ((yv - y_off) / y_rng).clamp(0.0, 1.0));
            } else {
                let (x0, x1, wx) = cxs[x];
                let (y0, y1, wy) = cys[y];
                let sample = |pl: &[u16], stride: usize| -> f32 {
                    let a = f32::from(pl[y0 * stride + x0]) * (1.0 - wx)
                        + f32::from(pl[y0 * stride + x1]) * wx;
                    let b = f32::from(pl[y1 * stride + x0]) * (1.0 - wx)
                        + f32::from(pl[y1 * stride + x1]) * wx;
                    a * (1.0 - wy) + b * wy
                };
                let u = sample(&p.planes[1], cw);
                let v = sample(&p.planes[2], cwid);
                let (r, g, b);
                if k.identity {
                    let n = |s: f32| ((s - y_off) / y_rng).clamp(0.0, 1.0);
                    g = n(yv);
                    b = n(u);
                    r = n(v);
                } else {
                    let yn = (yv - y_off) / y_rng;
                    let cb = (u - mid) / c_rng;
                    let cr = (v - mid) / c_rng;
                    r = yn + 2.0 * (1.0 - k.kr) * cr;
                    b = yn + 2.0 * (1.0 - k.kb) * cb;
                    g = (yn - k.kr * r - k.kb * b) / (1.0 - k.kr - k.kb);
                }
                put(&mut data, r.clamp(0.0, 1.0));
                put(&mut data, g.clamp(0.0, 1.0));
                put(&mut data, b.clamp(0.0, 1.0));
            }
            if let Some(a) = alpha {
                put(&mut data, a_scale(a, x, y));
            }
        }
    }
    Image::new(w as u32, h as u32, layout, depth, data)
}

fn crop(img: &Image, x: usize, y: usize, w: usize, h: usize) -> Result<Image> {
    let bpp = img.layout.channels() * img.depth.bytes();
    let iw = img.width as usize;
    let mut data = Vec::with_capacity(w * h * bpp);
    for row in y..y + h {
        data.extend_from_slice(&img.data[(row * iw + x) * bpp..(row * iw + x + w) * bpp]);
    }
    let mut out = Image::new(w as u32, h as u32, img.layout, img.depth, data)?;
    out.metadata = img.metadata.clone();
    Ok(out)
}

pub fn decode(file: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let notes = Notes::new();
    let top = boxes(file);
    let meta_box = top.iter().find(|b| &b.kind == b"meta").ok_or_else(|| {
        Error::corrupt("AVIF: no meta box (animated-only files are not supported)")
    })?;
    let meta = parse_meta(meta_box.body)?;
    let primary = meta.primary;
    let pit = meta.item(primary)?.clone();
    let pic = decode_item(&meta, file, primary, limits, 0)?;

    // Auxiliary alpha image.
    let mut alpha = None;
    for &(_, from, _) in meta
        .refs
        .iter()
        .filter(|r| &r.0 == b"auxl" && r.2 == primary)
    {
        let Ok(ai) = meta.item(from) else { continue };
        let is_alpha = meta.prop(ai, b"auxC").is_some_and(|a| {
            let s = a.body.get(4..).unwrap_or(&[]);
            s.windows(5).any(|w| w == b"alpha")
        });
        if is_alpha {
            alpha = Some(decode_item(&meta, file, from, limits, 0)?);
            break;
        }
    }

    // Colour information: an nclx `colr` property wins over the sequence header.
    let (mut mc, mut full) = (pic.matrix_coefficients, pic.full_range);
    let mut icc = None;
    for &pi in &pit.props {
        let Some(b) = meta.props.get(pi) else {
            continue;
        };
        if &b.kind != b"colr" {
            continue;
        }
        match b.body.get(0..4) {
            Some(b"nclx") if b.body.len() >= 11 => {
                mc = (be16(b.body, 8).unwrap_or(u16::from(mc))) as u8;
                full = b.body[10] & 0x80 != 0;
            }
            Some(b"prof") | Some(b"rICC") => icc = Some(b.body[4..].to_vec()),
            _ => {}
        }
    }
    let mut img = to_image(&pic, mc, full, alpha.as_ref())?;
    img.metadata.icc_profile = icc;

    // Transformative properties, in association order.
    for &pi in &pit.props {
        let Some(b) = meta.props.get(pi) else {
            continue;
        };
        match &b.kind {
            b"clap" => {
                let g = |i: usize| {
                    (
                        be32(b.body, i * 4).unwrap_or(0) as i32,
                        be32(b.body, i * 4 + 4).unwrap_or(1).max(1) as i32,
                    )
                };
                let (iw, ih) = (img.width as f64, img.height as f64);
                let cw = (f64::from(g(0).0) / f64::from(g(0).1))
                    .round()
                    .clamp(1.0, iw);
                let ch = (f64::from(g(1).0) / f64::from(g(1).1))
                    .round()
                    .clamp(1.0, ih);
                let cx = (iw - 1.0) / 2.0 + f64::from(g(2).0) / f64::from(g(2).1);
                let cy = (ih - 1.0) / 2.0 + f64::from(g(3).0) / f64::from(g(3).1);
                let x = (cx - (cw - 1.0) / 2.0).round().clamp(0.0, iw - cw) as usize;
                let y = (cy - (ch - 1.0) / 2.0).round().clamp(0.0, ih - ch) as usize;
                img = crop(&img, x, y, cw as usize, ch as usize)?;
            }
            b"irot" => {
                img = match b.body.first().map_or(0, |v| v & 3) {
                    1 => ops::rotate270(&img),
                    2 => ops::rotate180(&img),
                    3 => ops::rotate90(&img),
                    _ => img,
                };
            }
            b"imir" => {
                img = if b.body.first().map_or(0, |v| v & 1) == 0 {
                    ops::flip_horizontal(&img)
                } else {
                    ops::flip_vertical(&img)
                };
            }
            _ => {}
        }
    }
    Ok((img, notes))
}
