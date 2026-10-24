//! TrueType (`glyf`) outlines, including composite glyphs.
use crate::reader::R;
use crate::sfnt::Font;
use oc_raster::{Path, Pt};

#[derive(Debug, Clone, Copy)]
struct P {
    x: f64,
    y: f64,
    on: bool,
}

pub fn glyph_range(font: &Font, gid: u16) -> Option<(usize, usize)> {
    let loca = R::new(font.table(b"loca")?);
    let (a, b) = if font.loca_long {
        (
            loca.u32(gid as usize * 4)? as usize,
            loca.u32(gid as usize * 4 + 4)? as usize,
        )
    } else {
        (
            loca.u16(gid as usize * 2)? as usize * 2,
            loca.u16(gid as usize * 2 + 2)? as usize * 2,
        )
    };
    (b > a).then(|| (a, b - a))
}

/// Outline of `gid` in font units (y up).
pub fn outline(font: &Font, gid: u16) -> Option<Path> {
    let mut path = Path::new();
    add_glyph(
        font,
        gid,
        [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        0,
        &mut path,
        &mut 0,
    )?;
    Some(path)
}

fn add_glyph(
    font: &Font,
    gid: u16,
    m: [f64; 6],
    depth: u32,
    out: &mut Path,
    budget: &mut u32,
) -> Option<()> {
    if depth > 8 || *budget > 5000 {
        return Some(());
    }
    *budget += 1;
    let glyf = font.table(b"glyf")?;
    let Some((off, len)) = glyph_range(font, gid) else {
        return Some(());
    };
    let g = R::new(glyf.get(off..off.checked_add(len)?)?);
    let nc = g.i16(0)?;
    if nc >= 0 {
        simple(&g, nc as usize, m, out)
    } else {
        let mut o = 10usize;
        loop {
            let flags = g.u16(o)?;
            let sub = g.u16(o + 2)?;
            o += 4;
            let (dx, dy) = if flags & 1 != 0 {
                let v = (g.i16(o)?, g.i16(o + 2)?);
                o += 4;
                (f64::from(v.0), f64::from(v.1))
            } else {
                let v = (g.i8(o)?, g.i8(o + 1)?);
                o += 2;
                (f64::from(v.0), f64::from(v.1))
            };
            // Without ARGS_ARE_XY_VALUES the arguments are point numbers; matching is not
            // supported, so those components are placed at the origin.
            let (dx, dy) = if flags & 2 != 0 { (dx, dy) } else { (0.0, 0.0) };
            let f2 = |r: &R, o: usize| r.i16(o).map(|v| f64::from(v) / 16384.0);
            let (mut a, mut b, mut c, mut d) = (1.0, 0.0, 0.0, 1.0);
            if flags & 8 != 0 {
                a = f2(&g, o)?;
                d = a;
                o += 2;
            } else if flags & 0x40 != 0 {
                a = f2(&g, o)?;
                d = f2(&g, o + 2)?;
                o += 4;
            } else if flags & 0x80 != 0 {
                a = f2(&g, o)?;
                b = f2(&g, o + 2)?;
                c = f2(&g, o + 4)?;
                d = f2(&g, o + 6)?;
                o += 8;
            }
            // component transform, then the parent's
            let (e, f) = if flags & 0x800 != 0 {
                (dx * (a * a + b * b).sqrt(), dy * (c * c + d * d).sqrt())
            } else {
                (dx, dy)
            };
            let local = [a, b, c, d, e, f];
            let combined = [
                local[0] * m[0] + local[1] * m[2],
                local[0] * m[1] + local[1] * m[3],
                local[2] * m[0] + local[3] * m[2],
                local[2] * m[1] + local[3] * m[3],
                local[4] * m[0] + local[5] * m[2] + m[4],
                local[4] * m[1] + local[5] * m[3] + m[5],
            ];
            add_glyph(font, sub, combined, depth + 1, out, budget)?;
            if flags & 0x20 == 0 {
                break;
            }
        }
        Some(())
    }
}

fn simple(g: &R, nc: usize, m: [f64; 6], out: &mut Path) -> Option<()> {
    if nc == 0 {
        return Some(());
    }
    let mut ends = Vec::with_capacity(nc);
    for i in 0..nc {
        ends.push(g.u16(10 + i * 2)? as usize);
    }
    let npts = ends.last().copied()? + 1;
    if npts > 20_000 {
        return None;
    }
    let ilen = g.u16(10 + nc * 2)? as usize;
    let mut o = 12 + nc * 2 + ilen;
    let mut flags = Vec::with_capacity(npts);
    while flags.len() < npts {
        let f = g.u8(o)?;
        o += 1;
        flags.push(f);
        if f & 8 != 0 {
            let rep = g.u8(o)?;
            o += 1;
            for _ in 0..rep {
                if flags.len() < npts {
                    flags.push(f);
                }
            }
        }
    }
    let mut xs = Vec::with_capacity(npts);
    let mut v = 0i32;
    for f in &flags {
        if f & 2 != 0 {
            let d = i32::from(g.u8(o)?);
            o += 1;
            v += if f & 16 != 0 { d } else { -d };
        } else if f & 16 == 0 {
            v += i32::from(g.i16(o)?);
            o += 2;
        }
        xs.push(v);
    }
    let mut ys = Vec::with_capacity(npts);
    v = 0;
    for f in &flags {
        if f & 4 != 0 {
            let d = i32::from(g.u8(o)?);
            o += 1;
            v += if f & 32 != 0 { d } else { -d };
        } else if f & 32 == 0 {
            v += i32::from(g.i16(o)?);
            o += 2;
        }
        ys.push(v);
    }
    let pts: Vec<P> = (0..npts)
        .map(|i| {
            let (x, y) = (f64::from(xs[i]), f64::from(ys[i]));
            P {
                x: x * m[0] + y * m[2] + m[4],
                y: x * m[1] + y * m[3] + m[5],
                on: flags[i] & 1 != 0,
            }
        })
        .collect();
    let mut start = 0usize;
    for e in ends {
        if e < start || e >= npts {
            return None;
        }
        contour(&pts[start..=e], out);
        start = e + 1;
    }
    Some(())
}

fn contour(c: &[P], out: &mut Path) {
    let n = c.len();
    if n < 2 {
        return;
    }
    // Start on an on-curve point, or on the midpoint of two off-curve points.
    let (first, begin) = match c.iter().position(|p| p.on) {
        Some(i) => (Pt::new(c[i].x, c[i].y), i + 1),
        None => (Pt::new((c[0].x + c[1].x) / 2.0, (c[0].y + c[1].y) / 2.0), 1),
    };
    out.move_to(first.x, first.y);
    let mut ctrl: Option<Pt> = None;
    let count = if c.iter().any(|p| p.on) { n - 1 } else { n };
    for k in 0..count {
        let p = c[(begin + k) % n];
        let pt = Pt::new(p.x, p.y);
        match (ctrl, p.on) {
            (None, true) => out.line_to(pt.x, pt.y),
            (None, false) => ctrl = Some(pt),
            (Some(q), true) => {
                out.quad_to(q.x, q.y, pt.x, pt.y);
                ctrl = None;
            }
            (Some(q), false) => {
                let mid = q.lerp(pt, 0.5);
                out.quad_to(q.x, q.y, mid.x, mid.y);
                ctrl = Some(pt);
            }
        }
    }
    if let Some(q) = ctrl {
        out.quad_to(q.x, q.y, first.x, first.y);
    }
    out.close();
}
