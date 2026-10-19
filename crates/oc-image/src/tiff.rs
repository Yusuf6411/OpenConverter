//! TIFF 6.0 (little- and big-endian, classic 32-bit offsets).
//!
//! Read: the first image of the file, as strips or tiles, chunky or planar, uncompressed,
//! LZW, Deflate (both codes) or PackBits, with horizontal prediction, 1/2/4/8/16 bits per
//! sample, in bilevel, gray, palette, RGB and CMYK colour spaces, with or without alpha.
//! Write: gray, gray+alpha, RGB and RGBA at 8 or 16 bits, in strips, with optional
//! compression and predictor.
//!
//! Not supported (typed error): BigTIFF, JPEG-in-TIFF, CCITT fax, YCbCr/Lab, floating point
//! and 32-bit samples. Orientation is reported in a note and not applied.
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_compress::{lzw, packbits, zlib};
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};
use std::collections::HashMap;

pub fn sniff(data: &[u8]) -> bool {
    matches!(data, [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..])
}

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorCode::UnsupportedVariant,
        format!("TIFF: {what} is not supported"),
    )
}

#[derive(Clone, Copy)]
struct Endian {
    little: bool,
}

impl Endian {
    fn u16(self, d: &[u8], i: usize) -> Option<u16> {
        let b = [*d.get(i)?, *d.get(i + 1)?];
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }
    fn u32(self, d: &[u8], i: usize) -> Option<u32> {
        let b = [*d.get(i)?, *d.get(i + 1)?, *d.get(i + 2)?, *d.get(i + 3)?];
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
}

struct Field<'a> {
    kind: u16,
    count: usize,
    bytes: &'a [u8],
}

impl Field<'_> {
    fn numbers(&self, e: Endian) -> Vec<u32> {
        match self.kind {
            1 | 6 | 7 => self.bytes.iter().map(|&b| u32::from(b)).collect(),
            3 | 8 => (0..self.count)
                .filter_map(|i| e.u16(self.bytes, i * 2).map(u32::from))
                .collect(),
            4 | 9 => (0..self.count)
                .filter_map(|i| e.u32(self.bytes, i * 4))
                .collect(),
            _ => Vec::new(),
        }
    }
    fn first(&self, e: Endian) -> Option<u32> {
        self.numbers(e).first().copied()
    }
    fn rational(&self, e: Endian) -> Option<f64> {
        if self.kind != 5 {
            return None;
        }
        let (n, d) = (e.u32(self.bytes, 0)?, e.u32(self.bytes, 4)?);
        (d != 0).then(|| f64::from(n) / f64::from(d))
    }
}

struct Ifd<'a> {
    fields: HashMap<u16, Field<'a>>,
    next: u32,
}

fn read_ifd<'a>(data: &'a [u8], e: Endian, offset: usize, limits: &Limits) -> Result<Ifd<'a>> {
    let n = usize::from(
        e.u16(data, offset)
            .ok_or_else(|| Error::truncated("TIFF directory"))?,
    );
    limits.check_entries(n as u64)?;
    let mut fields = HashMap::new();
    for i in 0..n {
        let at = offset + 2 + i * 12;
        let (Some(tag), Some(kind), Some(count)) =
            (e.u16(data, at), e.u16(data, at + 2), e.u32(data, at + 4))
        else {
            return Err(Error::truncated("TIFF directory entry"));
        };
        let size = match kind {
            1 | 2 | 6 | 7 => 1usize,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => continue, // unknown or 64-bit types: ignore the tag
        };
        let total = size
            .checked_mul(count as usize)
            .ok_or_else(|| Error::corrupt("TIFF tag is too large"))?;
        limits.check_alloc(total as u64)?;
        let bytes = if total <= 4 {
            data.get(at + 8..at + 8 + total)
        } else {
            let off = e
                .u32(data, at + 8)
                .ok_or_else(|| Error::truncated("TIFF directory entry"))?
                as usize;
            off.checked_add(total).and_then(|end| data.get(off..end))
        };
        // A tag pointing outside the file is dropped; whether it matters is decided later.
        if let Some(bytes) = bytes {
            fields.insert(
                tag,
                Field {
                    kind,
                    count: count as usize,
                    bytes,
                },
            );
        }
    }
    let next = e.u32(data, offset + 2 + n * 12).unwrap_or(0);
    Ok(Ifd { fields, next })
}

fn reverse_bits(data: &mut [u8]) {
    for b in data {
        *b = b.reverse_bits();
    }
}

/// Undo horizontal differencing in place for one row.
fn undo_predictor(row: &mut [u8], spp: usize, bps: u32, e: Endian) {
    match bps {
        8 => {
            for i in spp..row.len() {
                let prev = row.get(i - spp).copied().unwrap_or(0);
                if let Some(b) = row.get_mut(i) {
                    *b = b.wrapping_add(prev);
                }
            }
        }
        16 => {
            let words = row.len() / 2;
            for i in spp..words {
                let (Some(cur), Some(prev)) = (e.u16(row, i * 2), e.u16(row, (i - spp) * 2)) else {
                    break;
                };
                let sum = cur.wrapping_add(prev);
                let bytes = if e.little {
                    sum.to_le_bytes()
                } else {
                    sum.to_be_bytes()
                };
                if let Some(d) = row.get_mut(i * 2..i * 2 + 2) {
                    d.copy_from_slice(&bytes);
                }
            }
        }
        _ => {}
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Comp {
    None,
    Lzw,
    Deflate,
    PackBits,
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    if !sniff(data) {
        if matches!(data, [b'I', b'I', 43, 0, ..] | [b'M', b'M', 0, 43, ..]) {
            return Err(unsupported("BigTIFF"));
        }
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a TIFF file"));
    }
    let e = Endian {
        little: data.starts_with(b"II"),
    };
    let first = e
        .u32(data, 4)
        .ok_or_else(|| Error::truncated("TIFF header"))? as usize;
    let ifd = read_ifd(data, e, first, limits)?;

    // Count further pages (bounded, cycle-safe) so a note can say they were left out.
    let (mut pages, mut next, mut seen) = (1usize, ifd.next as usize, vec![first]);
    while next != 0 && pages < 4096 && !seen.contains(&next) {
        seen.push(next);
        match read_ifd(data, e, next, limits) {
            Ok(i) => {
                pages += 1;
                next = i.next as usize;
            }
            Err(_) => break,
        }
    }
    if pages > 1 {
        notes.push(
            Note::new(
                Severity::Warning,
                "tiff.first_page_only",
                format!("the file has {pages} pages; only the first was read"),
            )
            .with_detail(pages.to_string()),
        );
    }

    let get = |tag: u16| ifd.fields.get(&tag);
    let num = |tag: u16, default: u32| get(tag).and_then(|f| f.first(e)).unwrap_or(default);
    let width = num(256, 0);
    let height = num(257, 0);
    if width == 0 || height == 0 {
        return Err(Error::corrupt("TIFF has no image dimensions"));
    }
    limits.check_pixels(width, height)?;
    let spp = num(277, 1) as usize;
    let bits: Vec<u32> = get(258).map_or_else(|| vec![1], |f| f.numbers(e));
    let bps = bits.first().copied().unwrap_or(1);
    if !(1..=8).contains(&spp)
        || bits.iter().any(|&b| b != bps)
        || !matches!(bps, 1 | 2 | 4 | 8 | 16)
    {
        return Err(unsupported("this sample layout"));
    }
    if get(339).is_some_and(|f| f.numbers(e).iter().any(|&v| v != 1)) {
        return Err(unsupported("signed or floating-point samples"));
    }
    let compression = match num(259, 1) {
        1 => Comp::None,
        5 => Comp::Lzw,
        8 | 32946 => Comp::Deflate,
        32773 => Comp::PackBits,
        7 | 6 => return Err(unsupported("JPEG compression")),
        2..=4 => return Err(unsupported("fax compression")),
        _ => return Err(unsupported("this compression")),
    };
    let photometric = num(262, 1);
    if !matches!(photometric, 0 | 1 | 2 | 3 | 5) {
        return Err(unsupported("this colour space"));
    }
    let predictor = num(317, 1);
    if !matches!(predictor, 1 | 2) || (predictor == 2 && bps < 8) {
        return Err(unsupported("this predictor"));
    }
    let planar = num(284, 1) == 2;
    let fill_lsb = num(266, 1) == 2;
    let tiled = get(322).is_some();

    // Geometry of the chunks (strips or tiles).
    let (cw, chh) = if tiled {
        (num(322, 0), num(323, 0))
    } else {
        (width, num(278, height).min(height).max(1))
    };
    if cw == 0 || chh == 0 {
        return Err(Error::corrupt("TIFF has an empty strip or tile size"));
    }
    let across = width.div_ceil(cw) as usize;
    let down = height.div_ceil(chh) as usize;
    let per_plane = across * down;
    let planes = if planar { spp } else { 1 };
    let chunk_count = per_plane * planes;
    limits.check_entries(chunk_count as u64)?;
    let (off_tag, len_tag) = if tiled { (324, 325) } else { (273, 279) };
    let offsets = get(off_tag).map(|f| f.numbers(e)).unwrap_or_default();
    let lengths = get(len_tag).map(|f| f.numbers(e)).unwrap_or_default();
    if offsets.len() < chunk_count {
        return Err(Error::corrupt("TIFF is missing strip or tile offsets"));
    }

    limits.check_alloc(u64::from(width) * u64::from(height) * spp as u64 * 2)?;
    let (w, h) = (width as usize, height as usize);
    let mut samples = vec![0u16; w * h * spp];
    let chunk_spp = if planar { 1 } else { spp };
    let row_bytes = (cw as usize * chunk_spp * bps as usize).div_ceil(8);
    let mut short = false;

    for idx in 0..chunk_count {
        if idx % 16 == 0 {
            progress.check_cancelled()?;
        }
        let plane = idx / per_plane;
        let within = idx % per_plane;
        let (tx, ty) = (within % across, within / across);
        let (x0, y0) = (tx * cw as usize, ty * chh as usize);
        let rows = if tiled {
            chh as usize
        } else {
            (chh as usize).min(h - y0)
        };
        let want = rows * row_bytes;
        let start = offsets.get(idx).copied().unwrap_or(0) as usize;
        let end = match lengths.get(idx) {
            Some(&l) => start.saturating_add(l as usize).min(data.len()),
            None => data.len(),
        };
        let mut chunk = data.get(start..end).unwrap_or(&[]).to_vec();
        if fill_lsb {
            reverse_bits(&mut chunk);
        }
        let mut lim = limits.clone();
        lim.max_output = (want as u64 + 1024).min(lim.max_output);
        lim.max_ratio = u64::MAX; // bounded by the expected size instead
        let mut raw = match compression {
            Comp::None => chunk,
            Comp::Lzw => lzw::decode(&chunk, lzw::Config::tiff(), &lim).unwrap_or_default(),
            Comp::Deflate => zlib::decompress(&chunk, &lim, progress).unwrap_or_default(),
            Comp::PackBits => packbits::decode(&chunk, Some(want)).unwrap_or_default(),
        };
        if raw.len() < want {
            short = true;
            raw.resize(want, 0);
        }
        for (r, row) in raw.chunks_mut(row_bytes.max(1)).take(rows).enumerate() {
            if predictor == 2 {
                undo_predictor(row, chunk_spp, bps, e);
            }
            let y = y0 + r;
            if y >= h {
                break;
            }
            for x in 0..cw as usize {
                if x0 + x >= w {
                    break;
                }
                for s in 0..chunk_spp {
                    let i = x * chunk_spp + s;
                    let v = match bps {
                        8 => u16::from(row.get(i).copied().unwrap_or(0)),
                        16 => e.u16(row, i * 2).unwrap_or(0),
                        _ => {
                            let bit = i * bps as usize;
                            let byte = row.get(bit / 8).copied().unwrap_or(0);
                            let shift = 8 - bps as usize - bit % 8;
                            u16::from((byte >> shift) & ((1u16 << bps) - 1) as u8)
                        }
                    };
                    let ch = if planar { plane } else { s };
                    if let Some(d) = samples.get_mut(((y * w) + x0 + x) * spp + ch) {
                        *d = v;
                    }
                }
            }
        }
    }
    if short {
        notes.push(Note::new(
            Severity::Warning,
            "tiff.truncated",
            "some image data was missing; the affected area is black",
        ));
    }

    let extra = get(338).map(|f| f.numbers(e)).unwrap_or_default();
    let img = assemble(
        &samples,
        w,
        h,
        spp,
        bps,
        photometric,
        &extra,
        get(320),
        e,
        &mut notes,
    )?;
    let mut img = img;
    if let (Some(x), Some(y)) = (
        get(282).and_then(|f| f.rational(e)),
        get(283).and_then(|f| f.rational(e)),
    ) {
        let unit = num(296, 2);
        let scale = match unit {
            2 => Some(1.0),
            3 => Some(2.54),
            _ => None,
        };
        if let Some(s) = scale.filter(|_| x > 0.0 && y > 0.0) {
            img.metadata.dpi = Some((x * s, y * s));
        }
    }
    if let Some(icc) = get(34675) {
        img.metadata.icc_profile = Some(icc.bytes.to_vec());
    }
    let orientation = num(274, 1);
    if orientation > 1 {
        notes.push(
            Note::new(
                Severity::Info,
                "tiff.orientation_ignored",
                "the file asks to be rotated or mirrored when shown; the pixels are stored unrotated",
            )
            .with_detail(orientation.to_string()),
        );
    }
    Ok((img, notes))
}

#[allow(clippy::too_many_arguments)] // all values come straight from tags
fn assemble(
    s: &[u16],
    w: usize,
    h: usize,
    spp: usize,
    bps: u32,
    photometric: u32,
    extra: &[u32],
    colormap: Option<&Field<'_>>,
    e: Endian,
    notes: &mut Notes,
) -> Result<Image> {
    let max = (1u32 << bps) - 1;
    let sixteen = bps == 16;
    // Scale a raw sample to 8 or 16 bits.
    let scale = |v: u16| -> u16 {
        if sixteen {
            v
        } else {
            ((u32::from(v) * 255 + max / 2) / max) as u16
        }
    };
    let alpha_kind = extra.first().copied().unwrap_or(0); // 1 associated, 2 unassociated
    let has_alpha = |colour_channels: usize| spp > colour_channels && matches!(alpha_kind, 1 | 2);
    let px = |p: usize, c: usize| s.get(p * spp + c).copied().unwrap_or(0);
    let mut out16: Vec<u16> = Vec::new();
    let layout;
    match photometric {
        0 | 1 => {
            let a = has_alpha(1);
            layout = if a { Layout::GrayAlpha } else { Layout::Gray };
            for p in 0..w * h {
                let v = px(p, 0);
                let v = if photometric == 0 {
                    (max as u16).wrapping_sub(v)
                } else {
                    v
                };
                out16.push(scale(v));
                if a {
                    out16.push(scale(px(p, 1)));
                }
            }
        }
        3 => {
            if bps > 8 {
                return Err(unsupported("palette images deeper than 8 bits"));
            }
            let cm =
                colormap.ok_or_else(|| Error::corrupt("TIFF palette image has no colour map"))?;
            let n = 1usize << bps;
            let table = cm.numbers(e);
            if table.len() < n * 3 {
                return Err(Error::corrupt("TIFF colour map is too short"));
            }
            let a = has_alpha(1);
            layout = if a { Layout::Rgba } else { Layout::Rgb };
            for p in 0..w * h {
                let i = usize::from(px(p, 0)).min(n - 1);
                for c in 0..3 {
                    out16.push(
                        // 16-bit map entry to 8 bits: 65535 -> 255, rounding down like libtiff-based tools.
                        (table.get(c * n + i).copied().unwrap_or(0) / 257) as u16,
                    );
                }
                if a {
                    out16.push(scale(px(p, 1)));
                }
            }
        }
        2 => {
            let a = has_alpha(3);
            layout = if a { Layout::Rgba } else { Layout::Rgb };
            if spp < 3 {
                return Err(Error::corrupt("TIFF RGB image has fewer than 3 samples"));
            }
            for p in 0..w * h {
                for c in 0..3 {
                    out16.push(scale(px(p, c)));
                }
                if a {
                    out16.push(scale(px(p, 3)));
                }
            }
        }
        _ => {
            // CMYK, without colour management.
            if spp < 4 {
                return Err(Error::corrupt("TIFF CMYK image has fewer than 4 samples"));
            }
            notes.push(Note::new(
                Severity::Warning,
                "tiff.cmyk_converted",
                "the file is CMYK; it was converted to RGB without colour management",
            ));
            let a = has_alpha(4);
            layout = if a { Layout::Rgba } else { Layout::Rgb };
            let top = if sixteen { 65535u64 } else { 255 };
            for p in 0..w * h {
                let k = u64::from(scale(px(p, 3)));
                for c in 0..3 {
                    let ink = u64::from(scale(px(p, c)));
                    out16.push((((top - ink) * (top - k) + top / 2) / top) as u16);
                }
                if a {
                    out16.push(scale(px(p, 4)));
                }
            }
        }
    }
    if alpha_kind == 1 && layout.has_alpha() {
        // Associated (premultiplied) alpha: convert to the straight alpha the model uses.
        let top = if sixteen { 65535u32 } else { 255 };
        for px in out16.chunks_exact_mut(layout.channels()) {
            if let Some((&mut a, colours)) = px.split_last_mut() {
                let a = u32::from(a);
                if a > 0 && a < top {
                    for v in colours {
                        *v = (u32::from(*v) * top / a).min(top) as u16;
                    }
                }
            }
        }
        notes.push(Note::new(
            Severity::Info,
            "tiff.alpha_unpremultiplied",
            "the file stores premultiplied alpha; colours were converted to straight alpha",
        ));
    }
    let img = if sixteen {
        let mut d = Vec::with_capacity(out16.len() * 2);
        for v in &out16 {
            d.extend_from_slice(&v.to_be_bytes());
        }
        Image::new(w as u32, h as u32, layout, Depth::Sixteen, d)?
    } else {
        Image::new(
            w as u32,
            h as u32,
            layout,
            Depth::Eight,
            out16.iter().map(|&v| v as u8).collect(),
        )?
    };
    Ok(img)
}

// ---------------------------------------------------------------- encoder

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Lzw,
    Deflate,
    PackBits,
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    pub compression: Compression,
    /// Horizontal differencing; helps LZW and Deflate on photographic data.
    pub predictor: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            compression: Compression::Lzw,
            predictor: true,
        }
    }
}

struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

fn short(tag: u16, v: u16) -> Entry {
    Entry {
        tag,
        kind: 3,
        count: 1,
        data: v.to_le_bytes().to_vec(),
    }
}

fn long(tag: u16, v: u32) -> Entry {
    Entry {
        tag,
        kind: 4,
        count: 1,
        data: v.to_le_bytes().to_vec(),
    }
}

fn rational(tag: u16, v: f64) -> Entry {
    let den = 1000u32;
    let num = (v * f64::from(den)).round().clamp(1.0, f64::from(u32::MAX)) as u32;
    let mut data = num.to_le_bytes().to_vec();
    data.extend_from_slice(&den.to_le_bytes());
    Entry {
        tag,
        kind: 5,
        count: 1,
        data,
    }
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let notes = Notes::new();
    let img = if img.layout == Layout::GrayAlpha && !img.has_translucency() {
        ops::convert_layout(img, Layout::Gray, [0, 0, 0]).0
    } else if img.layout == Layout::Rgba && !img.has_translucency() {
        ops::convert_layout(img, Layout::Rgb, [0, 0, 0]).0
    } else {
        img.clone()
    };
    let (w, h) = (img.width as usize, img.height as usize);
    let spp = img.layout.channels();
    let sixteen = img.depth == Depth::Sixteen;
    let bps: u16 = if sixteen { 16 } else { 8 };
    let row_bytes = w * spp * usize::from(bps / 8);
    // PackBits already works row by row; differencing only helps the dictionary coders.
    let predictor =
        opts.predictor && matches!(opts.compression, Compression::Lzw | Compression::Deflate);

    // Samples in file order (little-endian words), as rows.
    let mut rows: Vec<Vec<u8>> = Vec::with_capacity(h);
    for y in 0..img.height {
        let src = img.row(y);
        let mut row = src.to_vec();
        if sixteen {
            for pair in row.as_chunks_mut::<2>().0 {
                pair.reverse();
            }
        }
        if predictor {
            if sixteen {
                let words = row.len() / 2;
                let le = Endian { little: true };
                for i in (spp..words).rev() {
                    let (Some(cur), Some(prev)) =
                        (le.u16(&row, i * 2), le.u16(&row, (i - spp) * 2))
                    else {
                        continue;
                    };
                    if let Some(d) = row.get_mut(i * 2..i * 2 + 2) {
                        d.copy_from_slice(&cur.wrapping_sub(prev).to_le_bytes());
                    }
                }
            } else {
                for i in (spp..row.len()).rev() {
                    let prev = row.get(i - spp).copied().unwrap_or(0);
                    if let Some(b) = row.get_mut(i) {
                        *b = b.wrapping_sub(prev);
                    }
                }
            }
        }
        rows.push(row);
    }

    let rps = (65536 / row_bytes.max(1)).clamp(1, h);
    let mut out = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
    let (mut offsets, mut counts) = (Vec::new(), Vec::new());
    for strip in rows.chunks(rps) {
        let raw: Vec<u8> = strip.concat();
        let packed = match opts.compression {
            Compression::None => raw,
            Compression::Lzw => lzw::encode(&raw, lzw::Config::tiff())?,
            Compression::Deflate => zlib::compress(&raw, 6),
            Compression::PackBits => strip
                .iter()
                .flat_map(|r| packbits::encode(r))
                .collect::<Vec<u8>>(),
        };
        offsets.push(out.len() as u32);
        counts.push(packed.len() as u32);
        out.extend_from_slice(&packed);
        if out.len() % 2 == 1 {
            out.push(0);
        }
    }
    if out.len() > u32::MAX as usize - 4096 {
        return Err(unsupported("files over 4 GB (BigTIFF)"));
    }

    let photometric = if img.layout.is_gray() { 1 } else { 2 };
    let mut entries = vec![
        long(256, w as u32),
        long(257, h as u32),
        Entry {
            tag: 258,
            kind: 3,
            count: spp as u32,
            data: (0..spp).flat_map(|_| bps.to_le_bytes()).collect(),
        },
        short(
            259,
            match opts.compression {
                Compression::None => 1,
                Compression::Lzw => 5,
                Compression::Deflate => 8,
                Compression::PackBits => 32773,
            },
        ),
        short(262, photometric),
        Entry {
            tag: 273,
            kind: 4,
            count: offsets.len() as u32,
            data: offsets.iter().flat_map(|v| v.to_le_bytes()).collect(),
        },
        short(277, spp as u16),
        long(278, rps as u32),
        Entry {
            tag: 279,
            kind: 4,
            count: counts.len() as u32,
            data: counts.iter().flat_map(|v| v.to_le_bytes()).collect(),
        },
        short(284, 1),
    ];
    if let Some((x, y)) = img.metadata.dpi {
        entries.push(rational(282, x));
        entries.push(rational(283, y));
        entries.push(short(296, 2));
    }
    if predictor {
        entries.push(short(317, 2));
    }
    if img.layout.has_alpha() {
        entries.push(short(338, 2)); // unassociated alpha
    }
    entries.push(Entry {
        tag: 339,
        kind: 3,
        count: spp as u32,
        data: (0..spp).flat_map(|_| 1u16.to_le_bytes()).collect(),
    });
    if let Some(icc) = &img.metadata.icc_profile {
        entries.push(Entry {
            tag: 34675,
            kind: 7,
            count: icc.len() as u32,
            data: icc.clone(),
        });
    }
    entries.sort_by_key(|e| e.tag);

    // Directory, then the values that do not fit in four bytes.
    let ifd_at = out.len();
    let mut ifd = Vec::new();
    ifd.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut extra = Vec::new();
    let extra_base = ifd_at + 2 + entries.len() * 12 + 4;
    for en in &entries {
        ifd.extend_from_slice(&en.tag.to_le_bytes());
        ifd.extend_from_slice(&en.kind.to_le_bytes());
        ifd.extend_from_slice(&en.count.to_le_bytes());
        if en.data.len() <= 4 {
            ifd.extend_from_slice(&en.data);
            ifd.extend(std::iter::repeat_n(0, 4 - en.data.len()));
        } else {
            ifd.extend_from_slice(&((extra_base + extra.len()) as u32).to_le_bytes());
            extra.extend_from_slice(&en.data);
            if extra.len() % 2 == 1 {
                extra.push(0);
            }
        }
    }
    ifd.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&ifd);
    out.extend_from_slice(&extra);
    if let Some(d) = out.get_mut(4..8) {
        d.copy_from_slice(&(ifd_at as u32).to_le_bytes());
    }
    Ok((out, notes))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn dir() -> String {
        format!("{}/tests/data/tiff", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn matches_libtiff_via_imagemagick() {
        let mut names: Vec<_> = std::fs::read_dir(dir())
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                (p.extension()? == "tif").then_some(p)
            })
            .collect();
        names.sort();
        assert!(names.len() >= 30, "expected the generated fixtures");
        let mut bad = Vec::new();
        for p in &names {
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let data = std::fs::read(p).unwrap();
            assert!(sniff(&data), "{name}");
            let (img, _) = match decode(&data, &Limits::default(), &NoProgress) {
                Ok(r) => r,
                Err(e) => {
                    bad.push(format!("{name}: {e}"));
                    continue;
                }
            };
            let mut want = std::fs::read(p.with_extension("rgba")).unwrap();
            let sixteen = name.contains("16");
            if sixteen {
                // ImageMagick dumps 16-bit samples in native (little-endian) order.
                for pair in want.as_chunks_mut::<2>().0 {
                    pair.reverse();
                }
            }
            let (img, _) = ops::convert_depth(
                &img,
                if sixteen {
                    Depth::Sixteen
                } else {
                    Depth::Eight
                },
            );
            let got = ops::convert_layout(&img, Layout::Rgba, [0, 0, 0]).0.data;
            if got.len() != want.len() {
                bad.push(format!(
                    "{name}: {} bytes, expected {}",
                    got.len(),
                    want.len()
                ));
                continue;
            }
            // Colour management and premultiplication round differently between libraries.
            let tolerance: u32 = if name.contains("cmyk") || name.contains("assoc") {
                2
            } else {
                0
            };
            let step = if sixteen { 8 } else { 4 };
            let mut worst = 0u32;
            for (g, w) in got.chunks_exact(step).zip(want.chunks_exact(step)) {
                let alpha_zero = if sixteen {
                    w[6] == 0 && w[7] == 0
                } else {
                    w[3] == 0
                };
                for (i, (a, b)) in g.iter().zip(w).enumerate() {
                    let is_alpha = i >= step - if sixteen { 2 } else { 1 };
                    if alpha_zero && !is_alpha {
                        continue;
                    }
                    let d = u32::from(a.abs_diff(*b));
                    if is_alpha && d != 0 && tolerance == 0 {
                        worst = worst.max(d + 1000);
                    }
                    worst = worst.max(d);
                }
            }
            if worst > tolerance {
                bad.push(format!("{name}: max diff {worst}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    fn picture(w: u32, h: u32, layout: Layout, depth: Depth) -> Image {
        let ch = layout.channels();
        let bytes = if depth == Depth::Sixteen { 2 } else { 1 };
        let mut d = Vec::new();
        for i in 0..(w * h) as usize * ch {
            let v = ((i * 37 + i / 7 * 11) % 251) as u16 * if bytes == 2 { 257 } else { 1 };
            if bytes == 2 {
                d.extend_from_slice(&v.to_be_bytes());
            } else {
                d.push(v as u8);
            }
        }
        // A flat area helps the run-length paths.
        for b in d.iter_mut().take(ch * bytes * 20) {
            *b = 9;
        }
        let mut img = Image::new(w, h, layout, depth, d).unwrap();
        if layout.has_alpha() {
            // Make sure translucency is real, so the alpha channel is kept.
            let last = img.data.len() - 1;
            img.data[last] = 3;
        }
        img
    }

    #[test]
    fn roundtrip_every_layout_depth_and_compression() {
        for layout in [Layout::Gray, Layout::GrayAlpha, Layout::Rgb, Layout::Rgba] {
            for depth in [Depth::Eight, Depth::Sixteen] {
                let img = picture(61, 29, layout, depth);
                for compression in [
                    Compression::None,
                    Compression::Lzw,
                    Compression::Deflate,
                    Compression::PackBits,
                ] {
                    for predictor in [false, true] {
                        let (bytes, _) = encode(
                            &img,
                            &EncodeOptions {
                                compression,
                                predictor,
                            },
                        )
                        .unwrap();
                        let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
                        assert_eq!(
                            (back.layout, back.depth, &back.data),
                            (img.layout, img.depth, &img.data),
                            "{layout:?} {depth:?} {compression:?} pred={predictor}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn metadata_survives_and_multipage_is_reported() {
        let mut img = picture(8, 8, Layout::Rgb, Depth::Eight);
        img.metadata.dpi = Some((300.0, 150.0));
        img.metadata.icc_profile = Some((0..100).collect());
        let (bytes, _) = encode(&img, &EncodeOptions::default()).unwrap();
        let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        assert_eq!(back.metadata.dpi, Some((300.0, 150.0)));
        assert_eq!(back.metadata.icc_profile, img.metadata.icc_profile);

        // Append a second directory by hand: the first page still decodes, with a note.
        let mut two = bytes.clone();
        let first_ifd = u32::from_le_bytes(two[4..8].try_into().unwrap()) as usize;
        let n = u16::from_le_bytes([two[first_ifd], two[first_ifd + 1]]) as usize;
        let next_at = first_ifd + 2 + n * 12;
        let second = two.len() as u32;
        two[next_at..next_at + 4].copy_from_slice(&second.to_le_bytes());
        two.extend_from_slice(&0u16.to_le_bytes());
        two.extend_from_slice(&0u32.to_le_bytes());
        let (_, notes) = decode(&two, &Limits::default(), &NoProgress).unwrap();
        assert!(notes.iter().any(|n| n.code == "tiff.first_page_only"));
    }

    #[test]
    fn damaged_files_error_or_warn_without_panicking() {
        let img = picture(40, 30, Layout::Rgb, Depth::Eight);
        let (good, _) = encode(&img, &EncodeOptions::default()).unwrap();
        for cut in [0, 3, 7, 8, 20, good.len() / 2, good.len() - 1] {
            let _ = decode(&good[..cut], &Limits::default(), &NoProgress);
        }
        let mut s = 99u32;
        for _ in 0..500 {
            let mut bad = good.clone();
            for _ in 0..3 {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                let i = (s >> 8) as usize % bad.len();
                bad[i] ^= (s >> 3) as u8 | 1;
            }
            let _ = decode(&bad, &Limits::default(), &NoProgress);
        }
        let tight = Limits {
            max_pixels: 10,
            ..Limits::default()
        };
        assert!(decode(&good, &tight, &NoProgress).is_err());
        assert_eq!(
            decode(
                b"II\x2b\x00\x08\x00\x00\x00",
                &Limits::default(),
                &NoProgress
            )
            .err()
            .map(|e| e.code),
            Some(ErrorCode::UnsupportedVariant)
        );
    }
}
