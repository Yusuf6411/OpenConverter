//! Shading dictionaries (types 1 to 7) into `Shading` values; meshes become triangles.
use crate::color::ColorSpace;
use crate::doc::Pdf;
use crate::function::Function;
use crate::object::{Dict, Obj};
use crate::ops::{MeshVertex, Shading};
use oc_raster::Transform;

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: usize) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if self.pos + n > self.d.len() * 8 {
            return None;
        }
        let mut v = 0u64;
        for _ in 0..n {
            let b = self.d[self.pos / 8];
            v = (v << 1) | u64::from((b >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v as u32)
    }
    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

type P = [f64; 2];

fn bez(p: [P; 4], t: f64) -> P {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    [
        w[0] * p[0][0] + w[1] * p[1][0] + w[2] * p[2][0] + w[3] * p[3][0],
        w[0] * p[0][1] + w[1] * p[1][1] + w[2] * p[2][1] + w[3] * p[3][1],
    ]
}

/// A Coons or tensor patch on a 4x4 control net, split into triangles.
fn patch_triangles(net: &[[P; 4]; 4], col: &[[f32; 3]; 4], out: &mut Vec<[MeshVertex; 3]>) {
    const N: usize = 8;
    let mut grid = vec![[MeshVertex { p: [0.0; 2], c: [0.0; 3] }; 0]; 0];
    let _ = &mut grid;
    let mut pts = Vec::with_capacity((N + 1) * (N + 1));
    for i in 0..=N {
        let u = i as f64 / N as f64;
        // curves in v for each row of the net, evaluated at u across rows
        let rows: Vec<P> = (0..4).map(|r| bez([net[r][0], net[r][1], net[r][2], net[r][3]], u)).collect();
        for j in 0..=N {
            let v = j as f64 / N as f64;
            // evaluate along the column direction
            let pt = {
                let cols: [P; 4] = [rows[0], rows[1], rows[2], rows[3]];
                let _ = cols;
                let mut acc = [0.0, 0.0];
                let bu = |k: usize, t: f64| -> f64 {
                    let uu = 1.0 - t;
                    match k {
                        0 => uu * uu * uu,
                        1 => 3.0 * uu * uu * t,
                        2 => 3.0 * uu * t * t,
                        _ => t * t * t,
                    }
                };
                for r in 0..4 {
                    for c in 0..4 {
                        let w = bu(r, u) * bu(c, v);
                        acc[0] += w * net[r][c][0];
                        acc[1] += w * net[r][c][1];
                    }
                }
                acc
            };
            // bilinear colour: corners c0 (u=0,v=0), c1 (u=0,v=1), c2 (u=1,v=1), c3 (u=1,v=0)
            let mut c = [0.0f32; 3];
            for k in 0..3 {
                let a = col[0][k] * (1.0 - v as f32) + col[1][k] * v as f32;
                let b = col[3][k] * (1.0 - v as f32) + col[2][k] * v as f32;
                c[k] = a * (1.0 - u as f32) + b * u as f32;
            }
            pts.push(MeshVertex { p: pt, c });
        }
    }
    for i in 0..N {
        for j in 0..N {
            let a = pts[i * (N + 1) + j].clone();
            let b = pts[i * (N + 1) + j + 1].clone();
            let c = pts[(i + 1) * (N + 1) + j].clone();
            let d = pts[(i + 1) * (N + 1) + j + 1].clone();
            out.push([a.clone(), b.clone(), c.clone()]);
            out.push([b, d, c]);
        }
    }
}

pub fn parse(pdf: &Pdf, obj: &Obj, res: &Dict) -> Option<Shading> {
    let o = pdf.resolve(obj);
    let d = o.as_dict()?.clone();
    let st = pdf.dget_i64(&d, "ShadingType")?;
    let cs = ColorSpace::parse(pdf, d.get("ColorSpace")?, res, 0)?;
    let background = pdf.dget_numbers(&d, "Background").map(|b| cs.to_rgb(&b));
    let bbox = pdf.dget_numbers(&d, "BBox").filter(|b| b.len() >= 4).map(|b| [b[0], b[1], b[2], b[3]]);
    let func = d.get("Function").and_then(|f| Function::parse(pdf, f));
    let domain2 = pdf.dget_numbers(&d, "Domain").filter(|v| v.len() >= 2).map_or([0.0, 1.0], |v| [v[0], v[1]]);
    let extend = pdf
        .dget_array(&d, "Extend")
        .map(|a| [pdf.resolve(&a[0]).as_bool().unwrap_or(false), a.get(1).map(|x| pdf.resolve(x).as_bool().unwrap_or(false)).unwrap_or(false)])
        .unwrap_or([false, false]);
    match st {
        1 => {
            let dom = pdf.dget_numbers(&d, "Domain").filter(|v| v.len() >= 4).map_or([0.0, 1.0, 0.0, 1.0], |v| [v[0], v[1], v[2], v[3]]);
            let m = pdf.dget_numbers(&d, "Matrix").filter(|v| v.len() >= 6).map_or(Transform::IDENTITY, |v| Transform::new(v[0], v[1], v[2], v[3], v[4], v[5]));
            Some(Shading::FunctionBased { domain: dom, matrix: m, func: func?, cs, background })
        }
        2 => {
            let c = pdf.dget_numbers(&d, "Coords").filter(|v| v.len() >= 4)?;
            Some(Shading::Axial { coords: [c[0], c[1], c[2], c[3]], domain: domain2, func, extend, cs, background, bbox })
        }
        3 => {
            let c = pdf.dget_numbers(&d, "Coords").filter(|v| v.len() >= 6)?;
            Some(Shading::Radial { coords: [c[0], c[1], c[2], c[3], c[4], c[5]], domain: domain2, func, extend, cs, background, bbox })
        }
        4..=7 => {
            let Obj::Stream(s) = &o else { return None };
            let data = crate::filter::decode_stream(pdf, s, false).ok()?.0;
            let bpco = pdf.dget_i64(&d, "BitsPerCoordinate")?.clamp(1, 32) as usize;
            let bpcm = pdf.dget_i64(&d, "BitsPerComponent")?.clamp(1, 16) as usize;
            let bpf = pdf.dget_i64(&d, "BitsPerFlag").unwrap_or(8).clamp(0, 8) as usize;
            let dec = pdf.dget_numbers(&d, "Decode")?;
            let ncomp = if func.is_some() { 1 } else { cs.n_components() };
            if dec.len() < 4 + 2 * ncomp {
                return None;
            }
            let maxco = ((1u64 << bpco) - 1) as f64;
            let maxcm = ((1u64 << bpcm) - 1) as f64;
            let mut br = Bits { d: &data, pos: 0 };
            let read_pt = |br: &mut Bits| -> Option<P> {
                let x = f64::from(br.read(bpco)?) / maxco;
                let y = f64::from(br.read(bpco)?) / maxco;
                Some([dec[0] + x * (dec[1] - dec[0]), dec[2] + y * (dec[3] - dec[2])])
            };
            let to_c = |comps: &[f64]| -> [f32; 3] {
                if func.is_some() {
                    [comps[0] as f32, 0.0, 0.0]
                } else {
                    cs.to_rgb(comps)
                }
            };
            let read_col = |br: &mut Bits| -> Option<[f32; 3]> {
                let mut comps = Vec::with_capacity(ncomp);
                for k in 0..ncomp {
                    let v = f64::from(br.read(bpcm)?) / maxcm;
                    comps.push(dec[4 + 2 * k] + v * (dec[5 + 2 * k] - dec[4 + 2 * k]));
                }
                Some(to_c(&comps))
            };
            let mut tris: Vec<[MeshVertex; 3]> = Vec::new();
            match st {
                4 => {
                    let mut vs: Vec<MeshVertex> = Vec::new();
                    let mut pending: Vec<MeshVertex> = Vec::new();
                    while let Some(flag) = br.read(bpf) {
                        let (Some(p), Some(c)) = (read_pt(&mut br), read_col(&mut br)) else { break };
                        br.align();
                        let v = MeshVertex { p, c };
                        match flag {
                            0 => {
                                pending.push(v);
                                if pending.len() == 3 {
                                    tris.push([pending[0].clone(), pending[1].clone(), pending[2].clone()]);
                                    vs = pending.clone();
                                    pending.clear();
                                }
                            }
                            1 if vs.len() == 3 => {
                                let t = [vs[1].clone(), vs[2].clone(), v];
                                tris.push(t.clone());
                                vs = t.to_vec();
                            }
                            2 if vs.len() == 3 => {
                                let t = [vs[0].clone(), vs[2].clone(), v];
                                tris.push(t.clone());
                                vs = t.to_vec();
                            }
                            _ => {}
                        }
                        if tris.len() > 2_000_000 {
                            break;
                        }
                    }
                }
                5 => {
                    let per_row = pdf.dget_i64(&d, "VerticesPerRow")?.clamp(2, 1 << 16) as usize;
                    let mut verts: Vec<MeshVertex> = Vec::new();
                    while let (Some(p), Some(c)) = (read_pt(&mut br), read_col(&mut br)) {
                        br.align();
                        verts.push(MeshVertex { p, c });
                        if verts.len() > 4_000_000 {
                            break;
                        }
                    }
                    let rows = verts.len() / per_row;
                    for r in 0..rows.saturating_sub(1) {
                        for c in 0..per_row - 1 {
                            let a = verts[r * per_row + c].clone();
                            let b = verts[r * per_row + c + 1].clone();
                            let cc = verts[(r + 1) * per_row + c].clone();
                            let dd = verts[(r + 1) * per_row + c + 1].clone();
                            tris.push([a.clone(), b.clone(), cc.clone()]);
                            tris.push([b, dd, cc]);
                        }
                    }
                }
                _ => {
                    // 6: Coons (12 control points), 7: tensor (16)
                    let npts = if st == 6 { 12 } else { 16 };
                    let mut prev_net: Option<[P; 16]> = None;
                    let mut prev_col: Option<[[f32; 3]; 4]> = None;
                    let mut count = 0;
                    while let Some(flag) = br.read(bpf) {
                        let f = flag.min(3) as usize;
                        let new_pts = if f == 0 { npts } else { npts - 4 };
                        let new_cols = if f == 0 { 4 } else { 2 };
                        let mut pts: Vec<P> = Vec::new();
                        let mut ok = true;
                        for _ in 0..new_pts {
                            match read_pt(&mut br) {
                                Some(p) => pts.push(p),
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        let mut cols: Vec<[f32; 3]> = Vec::new();
                        if ok {
                            for _ in 0..new_cols {
                                match read_col(&mut br) {
                                    Some(c) => cols.push(c),
                                    None => {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                        }
                        if !ok {
                            break;
                        }
                        br.align();
                        // the 12 boundary points in stream order p0..p11 (Coons order)
                        let mut b: Vec<P> = Vec::with_capacity(12);
                        let mut c4: [[f32; 3]; 4] = [[0.0; 3]; 4];
                        if f == 0 {
                            b.extend_from_slice(&pts[..12]);
                            c4.copy_from_slice(&cols[..4]);
                        } else {
                            let (Some(pn), Some(pc)) = (prev_net_boundary(&prev_net), prev_col) else { continue };
                            let (e0, e1, e2, e3, k0, k1) = match f {
                                1 => (pn[3], pn[4], pn[5], pn[6], pc[1], pc[2]),
                                2 => (pn[6], pn[7], pn[8], pn[9], pc[2], pc[3]),
                                _ => (pn[9], pn[10], pn[11], pn[0], pc[3], pc[0]),
                            };
                            b.extend([e0, e1, e2, e3]);
                            b.extend_from_slice(&pts[..8]);
                            c4[0] = k0;
                            c4[1] = k1;
                            c4[2] = cols[0];
                            c4[3] = cols[1];
                        }
                        let net = coons_net(&b, if st == 7 { pts.get(npts - 4..).map(<[P]>::to_vec) } else { None });
                        let mut flat = [[0.0; 2]; 16];
                        for r in 0..4 {
                            for c in 0..4 {
                                flat[r * 4 + c] = net[r][c];
                            }
                        }
                        // remember boundary order for the next patch
                        prev_net = Some(boundary_to_flat(&b));
                        prev_col = Some(c4);
                        patch_triangles(&net, &[c4[0], c4[1], c4[2], c4[3]], &mut tris);
                        count += 1;
                        if count > 100_000 || tris.len() > 4_000_000 {
                            break;
                        }
                    }
                }
            }
            Some(Shading::Mesh { triangles: tris, func, cs, background })
        }
        _ => None,
    }
}

fn boundary_to_flat(b: &[P]) -> [P; 16] {
    let mut out = [[0.0; 2]; 16];
    for (i, p) in b.iter().take(12).enumerate() {
        out[i] = *p;
    }
    out
}

fn prev_net_boundary(p: &Option<[P; 16]>) -> Option<[P; 12]> {
    let p = p.as_ref()?;
    let mut o = [[0.0; 2]; 12];
    o.copy_from_slice(&p[..12]);
    Some(o)
}

/// Build the 4x4 net from the 12 boundary points (stream order) and, for tensor patches, the
/// four interior points; Coons interior points follow from the boundary.
fn coons_net(b: &[P], inner: Option<Vec<P>>) -> [[P; 4]; 4] {
    // boundary points in stream order go around the patch starting at (0,0):
    // p0..p3 along the left edge (u=0, v 0..1), p3..p6 top (v=1), p6..p9 right (reverse), p9..p11,p0 bottom
    let mut n = [[[0.0; 2]; 4]; 4];
    n[0][0] = b[0];
    n[0][1] = b[1];
    n[0][2] = b[2];
    n[0][3] = b[3];
    n[1][3] = b[4];
    n[2][3] = b[5];
    n[3][3] = b[6];
    n[3][2] = b[7];
    n[3][1] = b[8];
    n[3][0] = b[9];
    n[2][0] = b[10];
    n[1][0] = b[11];
    match inner {
        Some(q) if q.len() >= 4 => {
            n[1][1] = q[0];
            n[1][2] = q[1];
            n[2][2] = q[2];
            n[2][1] = q[3];
        }
        _ => {
            let f = |a: usize, c: usize, e: usize, g: usize, h: usize, i: usize, j: usize, k: usize| -> P {
                let _ = (a, c, e, g, h, i, j, k);
                [0.0, 0.0]
            };
            let _ = f;
            let p = |r: usize, c: usize, n: &[[P; 4]; 4]| n[r][c];
            let calc = |n: &[[P; 4]; 4], k: usize| -> P {
                let (i, j) = if k == 0 { (1, 1) } else if k == 1 { (1, 2) } else if k == 2 { (2, 2) } else { (2, 1) };
                // formulas from the PDF reference (4.6.3)
                let get = |r: usize, c: usize| p(r, c, n);
                let comb = |terms: &[(f64, P)]| -> P {
                    let mut o = [0.0, 0.0];
                    for (w, pt) in terms {
                        o[0] += w * pt[0];
                        o[1] += w * pt[1];
                    }
                    [o[0] / 9.0, o[1] / 9.0]
                };
                match (i, j) {
                    (1, 1) => comb(&[(-4.0, get(0, 0)), (6.0, get(0, 1)), (6.0, get(1, 0)), (-2.0, get(0, 3)), (-2.0, get(3, 0)), (3.0, get(3, 1)), (3.0, get(1, 3)), (-1.0, get(3, 3))]),
                    (1, 2) => comb(&[(-4.0, get(0, 3)), (6.0, get(0, 2)), (6.0, get(1, 3)), (-2.0, get(0, 0)), (-2.0, get(3, 3)), (3.0, get(3, 2)), (3.0, get(1, 0)), (-1.0, get(3, 0))]),
                    (2, 1) => comb(&[(-4.0, get(3, 0)), (6.0, get(3, 1)), (6.0, get(2, 0)), (-2.0, get(3, 3)), (-2.0, get(0, 0)), (3.0, get(0, 1)), (3.0, get(2, 3)), (-1.0, get(0, 3))]),
                    _ => comb(&[(-4.0, get(3, 3)), (6.0, get(3, 2)), (6.0, get(2, 3)), (-2.0, get(3, 0)), (-2.0, get(0, 3)), (3.0, get(0, 2)), (3.0, get(2, 0)), (-1.0, get(0, 0))]),
                }
            };
            let a = calc(&n, 0);
            let bb = calc(&n, 1);
            let c = calc(&n, 3);
            let d = calc(&n, 2);
            n[1][1] = a;
            n[1][2] = bb;
            n[2][1] = c;
            n[2][2] = d;
        }
    }
    n
}
