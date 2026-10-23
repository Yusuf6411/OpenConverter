//! Converting outlines between TrueType (quadratic) and CFF (cubic) fonts.
use crate::build::build_sfnt;
use crate::reader::{put_u16, put_u32};
use crate::sfnt::{Font, Tag};
use oc_core::{Error, ErrorCode, Result};
use oc_raster::{Path, Pt, Seg};

/// One closed contour with TrueType point flags.
struct Contour {
    pts: Vec<(i32, i32, bool)>,
}

fn cubic_to_quads(p0: Pt, c1: Pt, c2: Pt, p3: Pt, tol: f64, depth: u32, out: &mut Vec<(Pt, Pt)>) {
    // Single quad with the control point that matches the end tangents.
    let q = Pt::new(
        (3.0 * (c1.x + c2.x) - (p0.x + p3.x)) / 4.0,
        (3.0 * (c1.y + c2.y) - (p0.y + p3.y)) / 4.0,
    );
    let mut worst = 0.0f64;
    for k in 1..8 {
        let t = f64::from(k) / 8.0;
        let u = 1.0 - t;
        let cub = Pt::new(
            u * u * u * p0.x + 3.0 * u * u * t * c1.x + 3.0 * u * t * t * c2.x + t * t * t * p3.x,
            u * u * u * p0.y + 3.0 * u * u * t * c1.y + 3.0 * u * t * t * c2.y + t * t * t * p3.y,
        );
        let qd = Pt::new(
            u * u * p0.x + 2.0 * u * t * q.x + t * t * p3.x,
            u * u * p0.y + 2.0 * u * t * q.y + t * t * p3.y,
        );
        worst = worst.max(cub.sub(qd).len());
    }
    if worst <= tol || depth >= 6 {
        out.push((q, p3));
        return;
    }
    // split at t = 1/2
    let m01 = p0.lerp(c1, 0.5);
    let m12 = c1.lerp(c2, 0.5);
    let m23 = c2.lerp(p3, 0.5);
    let m012 = m01.lerp(m12, 0.5);
    let m123 = m12.lerp(m23, 0.5);
    let mid = m012.lerp(m123, 0.5);
    cubic_to_quads(p0, m01, m012, mid, tol, depth + 1, out);
    cubic_to_quads(mid, m123, m23, p3, tol, depth + 1, out);
}

fn contours_of(path: &Path, tol: f64) -> Vec<Contour> {
    let mut out: Vec<Contour> = Vec::new();
    let mut cur: Vec<(i32, i32, bool)> = Vec::new();
    let mut last = Pt::default();
    let r = |v: f64| v.round().clamp(-32000.0, 32000.0) as i32;
    let flush = |cur: &mut Vec<(i32, i32, bool)>, out: &mut Vec<Contour>| {
        // Drop a duplicated closing point.
        if cur.len() > 1 && cur.first() == cur.last() {
            cur.pop();
        }
        if cur.len() >= 2 {
            out.push(Contour {
                pts: std::mem::take(cur),
            });
        } else {
            cur.clear();
        }
    };
    for s in &path.segs {
        match *s {
            Seg::Move(p) => {
                flush(&mut cur, &mut out);
                cur.push((r(p.x), r(p.y), true));
                last = p;
            }
            Seg::Line(p) => {
                cur.push((r(p.x), r(p.y), true));
                last = p;
            }
            Seg::Quad(c, p) => {
                cur.push((r(c.x), r(c.y), false));
                cur.push((r(p.x), r(p.y), true));
                last = p;
            }
            Seg::Cubic(c1, c2, p) => {
                let mut quads = Vec::new();
                cubic_to_quads(last, c1, c2, p, tol, 0, &mut quads);
                for (q, e) in quads {
                    cur.push((r(q.x), r(q.y), false));
                    cur.push((r(e.x), r(e.y), true));
                }
                last = p;
            }
            Seg::Close => flush(&mut cur, &mut out),
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn encode_glyph(contours: &[Contour]) -> (Vec<u8>, [i32; 4], usize) {
    let mut bbox = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let mut npts = 0usize;
    for c in contours {
        for &(x, y, _) in &c.pts {
            bbox = [
                bbox[0].min(x),
                bbox[1].min(y),
                bbox[2].max(x),
                bbox[3].max(y),
            ];
            npts += 1;
        }
    }
    let mut g = Vec::new();
    put_u16(&mut g, contours.len() as u16);
    for v in bbox {
        put_u16(&mut g, v as i16 as u16);
    }
    let mut end = 0usize;
    for c in contours {
        end += c.pts.len();
        put_u16(&mut g, (end - 1) as u16);
    }
    put_u16(&mut g, 0); // no instructions
    let all: Vec<&(i32, i32, bool)> = contours.iter().flat_map(|c| c.pts.iter()).collect();
    for p in &all {
        g.push(u8::from(p.2));
    }
    let mut prev = 0;
    for p in &all {
        put_u16(&mut g, (p.0 - prev) as i16 as u16);
        prev = p.0;
    }
    prev = 0;
    for p in &all {
        put_u16(&mut g, (p.1 - prev) as i16 as u16);
        prev = p.1;
    }
    (g, bbox, npts)
}

/// Rebuild an OpenType/CFF font as TrueType outlines. `tolerance` is in font units.
pub fn cff_to_truetype(font: &Font, tolerance: f64) -> Result<Vec<u8>> {
    let cff = font
        .cff_table()
        .ok_or_else(|| Error::new(ErrorCode::UnsupportedVariant, "font has no CFF outlines"))?;
    let n = font.num_glyphs as usize;
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    let (mut max_pts, mut max_cont) = (0usize, 0usize);
    for gid in 0..n {
        put_u32(&mut loca, glyf.len() as u32);
        let Some(g) = cff.glyph(gid) else { continue };
        let contours = contours_of(&g.path, tolerance);
        if contours.is_empty() {
            continue;
        }
        let (data, _, npts) = encode_glyph(&contours);
        max_pts = max_pts.max(npts);
        max_cont = max_cont.max(contours.len());
        glyf.extend_from_slice(&data);
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    put_u32(&mut loca, glyf.len() as u32);
    let mut tables: Vec<(Tag, Vec<u8>)> = Vec::new();
    for t in &font.tables {
        if matches!(
            &t.tag,
            b"CFF " | b"CFF2" | b"VORG" | b"post" | b"maxp" | b"head"
        ) {
            continue;
        }
        if let Some(d) = font.data.get(t.offset..t.offset + t.length) {
            tables.push((t.tag, d.to_vec()));
        }
    }
    // head: long loca offsets, glyph data format 0.
    let mut head = font.table(b"head").unwrap_or_default().to_vec();
    if head.len() >= 54 {
        head[50] = 0;
        head[51] = 1;
    }
    tables.push((*b"head", head));
    // maxp 1.0
    let mut maxp = Vec::new();
    put_u32(&mut maxp, 0x0001_0000);
    put_u16(&mut maxp, n as u16);
    for v in [max_pts, max_cont, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0] {
        put_u16(&mut maxp, v.min(65535) as u16);
    }
    tables.push((*b"maxp", maxp));
    // post 3.0 keeps the metrics fields of the original and drops glyph names.
    let mut post = font.table(b"post").unwrap_or_default().to_vec();
    post.truncate(32);
    if post.len() < 32 {
        post.resize(32, 0);
    }
    post[..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
    tables.push((*b"post", post));
    tables.push((*b"glyf", glyf));
    tables.push((*b"loca", loca));
    Ok(build_sfnt([0, 1, 0, 0], &mut tables))
}

// ---- TrueType to CFF ----

fn enc_num(out: &mut Vec<u8>, v: i32) {
    match v {
        -107..=107 => out.push((v + 139) as u8),
        108..=1131 => {
            let w = v - 108;
            out.push((w / 256 + 247) as u8);
            out.push((w % 256) as u8);
        }
        -1131..=-108 => {
            let w = -v - 108;
            out.push((w / 256 + 251) as u8);
            out.push((w % 256) as u8);
        }
        _ => {
            out.push(28);
            out.extend_from_slice(&(v.clamp(-32768, 32767) as i16).to_be_bytes());
        }
    }
}

fn charstring(path: &Path, width: i32) -> Vec<u8> {
    let mut cs = Vec::new();
    let (mut cx, mut cy) = (0i32, 0i32);
    let r = |v: f64| v.round() as i32;
    let mut last = Pt::default();
    let mut width_pending = Some(width);
    for s in &path.segs {
        match *s {
            Seg::Move(p) => {
                let (x, y) = (r(p.x), r(p.y));
                if let Some(w) = width_pending.take() {
                    enc_num(&mut cs, w);
                }
                enc_num(&mut cs, x - cx);
                enc_num(&mut cs, y - cy);
                cs.push(21);
                cx = x;
                cy = y;
                last = p;
            }
            Seg::Line(p) => {
                let (x, y) = (r(p.x), r(p.y));
                enc_num(&mut cs, x - cx);
                enc_num(&mut cs, y - cy);
                cs.push(5);
                cx = x;
                cy = y;
                last = p;
            }
            Seg::Quad(q, p) => {
                let c1 = Pt::new(
                    last.x + 2.0 / 3.0 * (q.x - last.x),
                    last.y + 2.0 / 3.0 * (q.y - last.y),
                );
                let c2 = Pt::new(p.x + 2.0 / 3.0 * (q.x - p.x), p.y + 2.0 / 3.0 * (q.y - p.y));
                let pts = [(r(c1.x), r(c1.y)), (r(c2.x), r(c2.y)), (r(p.x), r(p.y))];
                for (x, y) in pts {
                    enc_num(&mut cs, x - cx);
                    enc_num(&mut cs, y - cy);
                    cx = x;
                    cy = y;
                }
                cs.push(8);
                last = p;
            }
            Seg::Cubic(c1, c2, p) => {
                for q in [c1, c2, p] {
                    let (x, y) = (r(q.x), r(q.y));
                    enc_num(&mut cs, x - cx);
                    enc_num(&mut cs, y - cy);
                    cx = x;
                    cy = y;
                }
                cs.push(8);
                last = p;
            }
            // Contours close implicitly in Type 2 and the current point stays where it is.
            Seg::Close => {}
        }
    }
    if let Some(w) = width_pending {
        enc_num(&mut cs, w);
    }
    cs.push(14);
    cs
}

fn cff_index(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u16(&mut out, items.len() as u16);
    if items.is_empty() {
        return out;
    }
    let total: usize = items.iter().map(Vec::len).sum::<usize>() + 1;
    let osz = if total < 256 {
        1
    } else if total < 65536 {
        2
    } else if total < 1 << 24 {
        3
    } else {
        4
    };
    out.push(osz as u8);
    let mut off = 1usize;
    let put = |out: &mut Vec<u8>, v: usize| {
        out.extend_from_slice(&(v as u32).to_be_bytes()[4 - osz..]);
    };
    put(&mut out, off);
    for it in items {
        off += it.len();
        put(&mut out, off);
    }
    for it in items {
        out.extend_from_slice(it);
    }
    out
}

fn dict_int(out: &mut Vec<u8>, v: i32) {
    out.push(29);
    out.extend_from_slice(&v.to_be_bytes());
}

/// Rebuild a TrueType font with CFF outlines (cubic curves converted exactly).
pub fn truetype_to_cff(font: &Font) -> Result<Vec<u8>> {
    if font.is_cff {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "font already has CFF outlines",
        ));
    }
    let n = font.num_glyphs as usize;
    let mut charstrings = Vec::with_capacity(n);
    for gid in 0..n {
        let path = crate::glyf::outline(font, gid as u16).unwrap_or_default();
        let mut cs = charstring(&path, i32::from(font.advance(gid as u16)));
        // The first glyph is .notdef; a glyph with no outline is just a width and endchar.
        if cs.is_empty() {
            cs.push(14);
        }
        charstrings.push(cs);
    }
    // Glyph names: standard strings where they match, custom strings otherwise.
    let mut strings: Vec<Vec<u8>> = Vec::new();
    let mut charset = vec![0u8];
    for gid in 1..n {
        let name = font
            .glyph_name(gid as u16)
            .filter(|s| !s.is_empty() && s.len() < 60 && s.is_ascii())
            .unwrap_or_else(|| format!("glyph{gid}"));
        let sid = match crate::cff::STD_STRINGS.iter().position(|s| *s == name) {
            Some(i) => i as u16,
            None => {
                strings.push(name.into_bytes());
                (390 + strings.len()) as u16
            }
        };
        charset.extend_from_slice(&sid.to_be_bytes());
    }
    let ps_name = font
        .name(6)
        .filter(|s| s.is_ascii() && !s.is_empty())
        .unwrap_or_else(|| "Converted".to_string());
    let header = vec![1u8, 0, 4, 4];
    let name_index = cff_index(&[ps_name.into_bytes()]);
    let string_index = cff_index(&strings);
    let gsubr_index = cff_index(&[]);
    let cs_index = cff_index(&charstrings);
    // Private DICT: defaultWidthX 0 and nominalWidthX 0 are the defaults, so it is empty.
    let private: Vec<u8> = Vec::new();
    // Top DICT with fixed-size offsets, so its own length is known before it is placed.
    let mk_top = |charset_off: i32, cs_off: i32, priv_off: i32| -> Vec<u8> {
        let mut d = Vec::new();
        dict_int(&mut d, charset_off);
        d.push(15);
        dict_int(&mut d, cs_off);
        d.push(17);
        dict_int(&mut d, 0);
        dict_int(&mut d, priv_off);
        d.push(18);
        d
    };
    let top_probe = cff_index(&[mk_top(0, 0, 0)]);
    let base =
        header.len() + name_index.len() + top_probe.len() + string_index.len() + gsubr_index.len();
    let charset_off = base;
    let cs_off = charset_off + charset.len();
    let priv_off = cs_off + cs_index.len();
    let top = cff_index(&[mk_top(charset_off as i32, cs_off as i32, priv_off as i32)]);
    let mut cff = Vec::new();
    cff.extend_from_slice(&header);
    cff.extend_from_slice(&name_index);
    cff.extend_from_slice(&top);
    cff.extend_from_slice(&string_index);
    cff.extend_from_slice(&gsubr_index);
    cff.extend_from_slice(&charset);
    cff.extend_from_slice(&cs_index);
    cff.extend_from_slice(&private);
    let mut tables: Vec<(Tag, Vec<u8>)> = Vec::new();
    for t in &font.tables {
        if matches!(
            &t.tag,
            b"glyf"
                | b"loca"
                | b"maxp"
                | b"post"
                | b"cvt "
                | b"fpgm"
                | b"prep"
                | b"gasp"
                | b"hdmx"
                | b"LTSH"
                | b"VDMX"
                | b"gvar"
                | b"fvar"
                | b"avar"
                | b"cvar"
                | b"STAT"
                | b"HVAR"
                | b"MVAR"
        ) {
            continue;
        }
        if let Some(d) = font.data.get(t.offset..t.offset + t.length) {
            tables.push((t.tag, d.to_vec()));
        }
    }
    let mut maxp = Vec::new();
    put_u32(&mut maxp, 0x0000_5000);
    put_u16(&mut maxp, n as u16);
    tables.push((*b"maxp", maxp));
    let mut post = font.table(b"post").unwrap_or_default().to_vec();
    post.truncate(32);
    if post.len() < 32 {
        post.resize(32, 0);
    }
    post[..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
    tables.push((*b"post", post));
    tables.push((*b"CFF ", cff));
    Ok(build_sfnt(*b"OTTO", &mut tables))
}
