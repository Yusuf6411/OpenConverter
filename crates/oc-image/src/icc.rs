//! ICC colour management: reads ICC v2 and v4 profiles and converts through the profile
//! connection space to sRGB.
//!
//! Supported input profiles: RGB matrix/TRC (`rXYZ`..`bTRC`), gray (`kTRC`) and the
//! LUT-based `A2B0`/`A2B1`/`A2B2` tags (`mft1`, `mft2`, `mAB `) for RGB, gray and CMYK, with
//! XYZ or Lab PCS and every curve type (`curv`, `para` 0-4). The destination is sRGB.
//! Intents: perceptual, relative colorimetric and saturation pick the matching tag.
//! Black point compensation scales the source's darkest colour to sRGB black; as in Little
//! CMS it is always on for perceptual and saturation, because sRGB is a v4 profile.
use crate::image::{Depth, Image, Layout};
use oc_core::{Error, ErrorCode, Result};

// Table sizes and strides are validated against the tag length before use.
#[allow(clippy::indexing_slicing, clippy::needless_range_loop)]
mod inner {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Intent {
        Perceptual,
        RelativeColorimetric,
        Saturation,
    }

    const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
    /// Linear sRGB (D50 adapted) to XYZ, the ICC sRGB colorants.
    const SRGB_TO_XYZ: [[f64; 3]; 3] = [
        [0.436_074_7, 0.385_064_9, 0.143_080_4],
        [0.222_504_5, 0.716_878_6, 0.060_616_9],
        [0.013_932_2, 0.097_104_5, 0.714_173_3],
    ];

    fn invert(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
        let [[a, b, c], [d, e, f], [g, h, i]] = *m;
        let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
        let k = if det.abs() < 1e-12 { 0.0 } else { 1.0 / det };
        [
            [
                (e * i - f * h) * k,
                (c * h - b * i) * k,
                (b * f - c * e) * k,
            ],
            [
                (f * g - d * i) * k,
                (a * i - c * g) * k,
                (c * d - a * f) * k,
            ],
            [
                (d * h - e * g) * k,
                (b * g - a * h) * k,
                (a * e - b * d) * k,
            ],
        ]
    }

    fn mul(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
        let mut o = [0.0; 3];
        for r in 0..3 {
            o[r] = m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2];
        }
        o
    }

    fn srgb_encode(v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    }

    fn corrupt(msg: &'static str) -> Error {
        Error::new(ErrorCode::CorruptFile, msg)
    }

    fn unsupported(msg: &'static str) -> Error {
        Error::new(ErrorCode::UnsupportedVariant, msg)
    }

    fn be16(b: &[u8], at: usize) -> Option<u16> {
        Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
    }

    fn be32(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_be_bytes([
            *b.get(at)?,
            *b.get(at + 1)?,
            *b.get(at + 2)?,
            *b.get(at + 3)?,
        ]))
    }

    fn s15(b: &[u8], at: usize) -> Option<f64> {
        Some(f64::from(be32(b, at)? as i32) / 65536.0)
    }

    #[derive(Debug, Clone)]
    enum Curve {
        Identity,
        Gamma(f64),
        Table(Vec<f64>),
        Para(u16, [f64; 7]),
    }

    impl Curve {
        fn eval(&self, x: f64) -> f64 {
            let x = x.clamp(0.0, 1.0);
            let y = match self {
                Self::Identity => x,
                Self::Gamma(g) => x.powf(*g),
                Self::Table(t) => {
                    let n = t.len();
                    let p = x * (n - 1) as f64;
                    let i = (p as usize).min(n - 2);
                    let f = p - i as f64;
                    t[i] * (1.0 - f) + t[i + 1] * f
                }
                Self::Para(kind, q) => {
                    let [g, a, b, c, d, e, f] = *q;
                    let pw = |base: f64| base.max(0.0).powf(g);
                    match kind {
                        0 => pw(x),
                        1 => {
                            if a != 0.0 && x >= -b / a {
                                pw(a * x + b)
                            } else {
                                0.0
                            }
                        }
                        2 => {
                            if a != 0.0 && x >= -b / a {
                                pw(a * x + b) + c
                            } else {
                                c
                            }
                        }
                        3 => {
                            if x >= d {
                                pw(a * x + b)
                            } else {
                                c * x
                            }
                        }
                        _ => {
                            if x >= d {
                                pw(a * x + b) + e
                            } else {
                                c * x + f
                            }
                        }
                    }
                }
            };
            y.clamp(0.0, 1.0)
        }
    }

    /// Parses one `curv` or `para` element at `at`; returns it and its length (unpadded).
    fn curve(b: &[u8], at: usize) -> Result<(Curve, usize)> {
        let sig = b
            .get(at..at + 4)
            .ok_or_else(|| corrupt("curve is cut short"))?;
        match sig {
            b"curv" => {
                let n = be32(b, at + 8).ok_or_else(|| corrupt("curve is cut short"))? as usize;
                let len = 12 + 2 * n;
                match n {
                    0 => Ok((Curve::Identity, len)),
                    1 => {
                        let g = f64::from(
                            be16(b, at + 12).ok_or_else(|| corrupt("curve is cut short"))?,
                        ) / 256.0;
                        Ok((Curve::Gamma(g), len))
                    }
                    _ => {
                        let body = b
                            .get(at + 12..at + len)
                            .ok_or_else(|| corrupt("curve is cut short"))?;
                        let t = body
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|&c| f64::from(u16::from_be_bytes(c)) / 65535.0)
                            .collect();
                        Ok((Curve::Table(t), len))
                    }
                }
            }
            b"para" => {
                let kind = be16(b, at + 8).ok_or_else(|| corrupt("curve is cut short"))?;
                let count = match kind {
                    0 => 1,
                    1 => 3,
                    2 => 4,
                    3 => 5,
                    4 => 7,
                    _ => return Err(unsupported("unknown parametric curve type")),
                };
                let mut q = [0.0; 7];
                for (k, slot) in q.iter_mut().enumerate().take(count) {
                    *slot = s15(b, at + 12 + 4 * k).ok_or_else(|| corrupt("curve is cut short"))?;
                }
                Ok((Curve::Para(kind, q), 12 + 4 * count))
            }
            _ => Err(corrupt("unknown curve type")),
        }
    }

    /// `n` curves one after the other, each padded to four bytes.
    fn curves(b: &[u8], mut at: usize, n: usize) -> Result<Vec<Curve>> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let (c, len) = curve(b, at)?;
            v.push(c);
            at += len.div_ceil(4) * 4;
        }
        Ok(v)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Pcs {
        Xyz,
        /// Lab as stored by `mft2`: L 0..100 is 0..0xFF00, a and b are offset by 128.
        LabV2,
        /// Lab as stored by `mft1` and `mAB `: L 0..100 and a, b -128..127 span the full range.
        LabFull,
    }

    #[derive(Debug, Clone)]
    struct Clut {
        grid: Vec<usize>,
        /// Normalised 0..1, three outputs per node, first input slowest.
        data: Vec<f32>,
    }

    impl Clut {
        /// Position `p` in 0..1 on an axis with `n` nodes: node index and fraction.
        fn axis(p: f64, n: usize) -> (usize, f64) {
            let t = p.clamp(0.0, 1.0) * (n - 1) as f64;
            let i = (t as usize).min(n.saturating_sub(2));
            (i, if n == 1 { 0.0 } else { t - i as f64 })
        }

        fn node(&self, base: usize, o: usize) -> f64 {
            f64::from(self.data[base * 3 + o])
        }

        fn eval(&self, x: &[f64]) -> [f64; 3] {
            let n = self.grid.len();
            // Strides in nodes, first input slowest.
            let mut stride = vec![1usize; n];
            for k in (0..n.saturating_sub(1)).rev() {
                stride[k] = stride[k + 1] * self.grid[k + 1];
            }
            let mut idx = [0usize; 4];
            let mut frac = [0.0f64; 4];
            for k in 0..n {
                let (i, f) = Self::axis(x[k], self.grid[k]);
                idx[k] = i;
                frac[k] = f;
            }
            let mut out = [0.0; 3];
            if n == 3 {
                self.tetra3(
                    idx[0] * stride[0] + idx[1] * stride[1] + idx[2] * stride[2],
                    [stride[0], stride[1], stride[2]],
                    [frac[0], frac[1], frac[2]],
                    &mut out,
                );
            } else if n == 4 {
                let base = idx[0] * stride[0]
                    + idx[1] * stride[1]
                    + idx[2] * stride[2]
                    + idx[3] * stride[3];
                let s = [stride[1], stride[2], stride[3]];
                let f = [frac[1], frac[2], frac[3]];
                let mut lo = [0.0; 3];
                let mut hi = [0.0; 3];
                self.tetra3(base, s, f, &mut lo);
                if self.grid[0] > 1 {
                    self.tetra3(base + stride[0], s, f, &mut hi);
                } else {
                    hi = lo;
                }
                for o in 0..3 {
                    out[o] = lo[o] * (1.0 - frac[0]) + hi[o] * frac[0];
                }
            } else {
                // Multilinear for one or two inputs, and anything unusual.
                for corner in 0..(1usize << n) {
                    let mut w = 1.0;
                    let mut at = 0;
                    for k in 0..n {
                        let up = (corner >> k) & 1 == 1;
                        let i = if up && self.grid[k] > 1 {
                            idx[k] + 1
                        } else {
                            idx[k]
                        };
                        w *= if up { frac[k] } else { 1.0 - frac[k] };
                        at += i * stride[k];
                    }
                    if w != 0.0 {
                        for o in 0..3 {
                            out[o] += w * self.node(at, o);
                        }
                    }
                }
            }
            out
        }

        /// Tetrahedral interpolation in the cube whose first corner is node `base`.
        fn tetra3(&self, base: usize, s: [usize; 3], f: [f64; 3], out: &mut [f64; 3]) {
            let (rx, ry, rz) = (f[0], f[1], f[2]);
            let at = |dx: usize, dy: usize, dz: usize| base + dx * s[0] + dy * s[1] + dz * s[2];
            for o in 0..3 {
                let v = |dx, dy, dz| self.node(at(dx, dy, dz), o);
                let c0 = v(0, 0, 0);
                let (c1, c2, c3);
                if rx >= ry && ry >= rz {
                    c1 = v(1, 0, 0) - c0;
                    c2 = v(1, 1, 0) - v(1, 0, 0);
                    c3 = v(1, 1, 1) - v(1, 1, 0);
                } else if rx >= rz && rz >= ry {
                    c1 = v(1, 0, 0) - c0;
                    c2 = v(1, 1, 1) - v(1, 0, 1);
                    c3 = v(1, 0, 1) - v(1, 0, 0);
                } else if rz >= rx && rx >= ry {
                    c1 = v(1, 0, 1) - v(0, 0, 1);
                    c2 = v(1, 1, 1) - v(1, 0, 1);
                    c3 = v(0, 0, 1) - c0;
                } else if ry >= rx && rx >= rz {
                    c1 = v(1, 1, 0) - v(0, 1, 0);
                    c2 = v(0, 1, 0) - c0;
                    c3 = v(1, 1, 1) - v(1, 1, 0);
                } else if ry >= rz && rz >= rx {
                    c1 = v(1, 1, 1) - v(0, 1, 1);
                    c2 = v(0, 1, 0) - c0;
                    c3 = v(0, 1, 1) - v(0, 1, 0);
                } else {
                    c1 = v(1, 1, 1) - v(0, 1, 1);
                    c2 = v(0, 1, 1) - v(0, 0, 1);
                    c3 = v(0, 0, 1) - c0;
                }
                out[o] = c0 + c1 * rx + c2 * ry + c3 * rz;
            }
        }
    }

    #[derive(Debug, Clone)]
    struct Lut {
        a: Vec<Curve>,
        clut: Option<Clut>,
        m: Vec<Curve>,
        matrix: Option<[f64; 12]>,
        b: Vec<Curve>,
        pcs: Pcs,
    }

    #[derive(Debug, Clone)]
    enum Body {
        /// Linearised RGB times the colorant matrix gives XYZ.
        Matrix([[f64; 3]; 3]),
        /// Linearised gray is Y.
        Gray,
        Lut(Lut),
    }

    #[derive(Debug, Clone)]
    pub struct Transform {
        channels: usize,
        input: Vec<Curve>,
        body: Body,
        to_srgb: [[f64; 3]; 3],
        identity: bool,
        /// Black point compensation as `xyz * scale + offset`.
        bpc: Option<([f64; 3], [f64; 3])>,
    }

    /// Perceptual reference medium black of ICC v4 (XYZ, D50).
    const V4_BLACK: [f64; 3] = [0.003_36, 0.003_473_1, 0.002_87];

    fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
        let f = |t: f64| {
            if t > (6.0f64 / 29.0).powi(3) {
                t.cbrt()
            } else {
                t / (3.0 * (6.0f64 / 29.0).powi(2)) + 4.0 / 29.0
            }
        };
        let (fx, fy, fz) = (f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2]));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    fn tag<'a>(profile: &'a [u8], sig: &[u8; 4]) -> Option<&'a [u8]> {
        let count = be32(profile, 128)? as usize;
        for k in 0..count.min(1024) {
            let e = 132 + 12 * k;
            if profile.get(e..e + 4)? == sig {
                let off = be32(profile, e + 4)? as usize;
                let len = be32(profile, e + 8)? as usize;
                return profile.get(off..off.checked_add(len)?);
            }
        }
        None
    }

    fn xyz_tag(profile: &[u8], sig: &[u8; 4]) -> Result<[f64; 3]> {
        let t = tag(profile, sig).ok_or_else(|| corrupt("a colorant tag is missing"))?;
        if t.get(..4) != Some(b"XYZ ") {
            return Err(corrupt("colorant tag is not XYZ"));
        }
        let get = |k: usize| s15(t, 8 + 4 * k).ok_or_else(|| corrupt("colorant tag is cut short"));
        Ok([get(0)?, get(1)?, get(2)?])
    }

    fn curve_tag(profile: &[u8], sig: &[u8; 4]) -> Result<Curve> {
        let t = tag(profile, sig).ok_or_else(|| corrupt("a curve tag is missing"))?;
        Ok(curve(t, 0)?.0)
    }

    fn parse_lut(t: &[u8], n_in: usize, pcs_lab: bool) -> Result<Lut> {
        let n_in_tag = usize::from(*t.get(8).ok_or_else(|| corrupt("LUT is cut short"))?);
        let n_out = usize::from(*t.get(9).ok_or_else(|| corrupt("LUT is cut short"))?);
        if n_in_tag != n_in || n_out != 3 || !(1..=4).contains(&n_in) {
            return Err(unsupported("LUT channel counts do not fit the profile"));
        }
        match t.get(..4) {
            Some(b"mft1" | b"mft2") => {
                let wide = t.get(..4) == Some(b"mft2");
                let g = usize::from(*t.get(10).ok_or_else(|| corrupt("LUT is cut short"))?);
                if g < 2 {
                    return Err(corrupt("LUT grid is too small"));
                }
                let (n_tab_in, n_tab_out, mut at) = if wide {
                    (
                        usize::from(be16(t, 48).ok_or_else(|| corrupt("LUT is cut short"))?),
                        usize::from(be16(t, 50).ok_or_else(|| corrupt("LUT is cut short"))?),
                        52,
                    )
                } else {
                    (256, 256, 48)
                };
                if !(2..=4096).contains(&n_tab_in) || !(2..=4096).contains(&n_tab_out) {
                    return Err(corrupt("LUT table size is out of range"));
                }
                let width = if wide { 2 } else { 1 };
                let read = |at: usize| -> Option<f64> {
                    if wide {
                        be16(t, at).map(|v| f64::from(v) / 65535.0)
                    } else {
                        t.get(at).map(|&v| f64::from(v) / 255.0)
                    }
                };
                let mut a = Vec::new();
                for _ in 0..n_in {
                    let mut v = Vec::with_capacity(n_tab_in);
                    for _ in 0..n_tab_in {
                        v.push(read(at).ok_or_else(|| corrupt("LUT is cut short"))?);
                        at += width;
                    }
                    a.push(Curve::Table(v));
                }
                let nodes = g
                    .checked_pow(n_in as u32)
                    .and_then(|v| v.checked_mul(3))
                    .ok_or_else(|| corrupt("LUT is too large"))?;
                if t.len() < at + nodes * width {
                    return Err(corrupt("LUT is cut short"));
                }
                let mut data = Vec::with_capacity(nodes);
                for _ in 0..nodes {
                    data.push(read(at).ok_or_else(|| corrupt("LUT is cut short"))? as f32);
                    at += width;
                }
                let mut b = Vec::new();
                for _ in 0..3 {
                    let mut v = Vec::with_capacity(n_tab_out);
                    for _ in 0..n_tab_out {
                        v.push(read(at).ok_or_else(|| corrupt("LUT is cut short"))?);
                        at += width;
                    }
                    b.push(Curve::Table(v));
                }
                Ok(Lut {
                    a,
                    clut: Some(Clut {
                        grid: vec![g; n_in],
                        data,
                    }),
                    m: Vec::new(),
                    matrix: None,
                    b,
                    pcs: if !pcs_lab {
                        Pcs::Xyz
                    } else if wide {
                        Pcs::LabV2
                    } else {
                        Pcs::LabFull
                    },
                })
            }
            Some(b"mAB ") => {
                let off = |k: usize| -> Result<usize> {
                    Ok(be32(t, 12 + 4 * k).ok_or_else(|| corrupt("LUT is cut short"))? as usize)
                };
                let (ob, om, omc, oc, oa) = (off(0)?, off(1)?, off(2)?, off(3)?, off(4)?);
                if ob == 0 {
                    return Err(corrupt("mAB without B curves"));
                }
                let b = curves(t, ob, 3)?;
                let matrix = if om != 0 {
                    let mut m = [0.0; 12];
                    for (k, slot) in m.iter_mut().enumerate() {
                        *slot = s15(t, om + 4 * k).ok_or_else(|| corrupt("LUT is cut short"))?;
                    }
                    Some(m)
                } else {
                    None
                };
                let m = if omc != 0 {
                    curves(t, omc, 3)?
                } else {
                    Vec::new()
                };
                let clut = if oc != 0 {
                    let mut grid = Vec::new();
                    for k in 0..n_in {
                        let g =
                            usize::from(*t.get(oc + k).ok_or_else(|| corrupt("LUT is cut short"))?);
                        if g < 2 {
                            return Err(corrupt("LUT grid is too small"));
                        }
                        grid.push(g);
                    }
                    let prec = *t.get(oc + 16).ok_or_else(|| corrupt("LUT is cut short"))?;
                    if prec != 1 && prec != 2 {
                        return Err(corrupt("bad CLUT precision"));
                    }
                    let nodes = grid
                        .iter()
                        .try_fold(3usize, |acc, &g| acc.checked_mul(g))
                        .ok_or_else(|| corrupt("LUT is too large"))?;
                    let start = oc + 20;
                    let width = usize::from(prec);
                    let body = t
                        .get(start..start + nodes * width)
                        .ok_or_else(|| corrupt("LUT is cut short"))?;
                    let data = if prec == 2 {
                        body.as_chunks::<2>()
                            .0
                            .iter()
                            .map(|&c| f32::from(u16::from_be_bytes(c)) / 65535.0)
                            .collect()
                    } else {
                        body.iter().map(|&v| f32::from(v) / 255.0).collect()
                    };
                    Some(Clut { grid, data })
                } else {
                    if n_in != 3 {
                        return Err(corrupt("mAB without CLUT needs three inputs"));
                    }
                    None
                };
                let a = if oa != 0 {
                    curves(t, oa, n_in)?
                } else {
                    vec![Curve::Identity; n_in]
                };
                Ok(Lut {
                    a,
                    clut,
                    m,
                    matrix,
                    b,
                    pcs: if pcs_lab { Pcs::LabFull } else { Pcs::Xyz },
                })
            }
            _ => Err(unsupported("unknown LUT type")),
        }
    }

    impl Lut {
        /// `x` has already been through the `a` curves. Returns XYZ (D50).
        fn run(&self, x: &[f64]) -> [f64; 3] {
            let mut v = match &self.clut {
                Some(c) => c.eval(x),
                None => [x[0], x[1], x[2]],
            };
            if !self.m.is_empty() {
                for k in 0..3 {
                    v[k] = self.m[k].eval(v[k]);
                }
            }
            if let Some(m) = &self.matrix {
                let o = v;
                for r in 0..3 {
                    v[r] = (m[3 * r] * o[0] + m[3 * r + 1] * o[1] + m[3 * r + 2] * o[2] + m[9 + r])
                        .clamp(0.0, 1.0);
                }
            }
            for k in 0..3 {
                v[k] = self.b[k].eval(v[k]);
            }
            match self.pcs {
                Pcs::Xyz => [
                    v[0] * 65535.0 / 32768.0,
                    v[1] * 65535.0 / 32768.0,
                    v[2] * 65535.0 / 32768.0,
                ],
                Pcs::LabV2 => lab_to_xyz(
                    v[0] * 65535.0 / 65280.0 * 100.0,
                    v[1] * 65535.0 / 256.0 - 128.0,
                    v[2] * 65535.0 / 256.0 - 128.0,
                ),
                Pcs::LabFull => {
                    lab_to_xyz(v[0] * 100.0, v[1] * 255.0 - 128.0, v[2] * 255.0 - 128.0)
                }
            }
        }
    }

    fn lab_to_xyz(l: f64, a: f64, b: f64) -> [f64; 3] {
        let fy = (l + 16.0) / 116.0;
        let fx = fy + a / 500.0;
        let fz = fy - b / 200.0;
        let inv = |t: f64| {
            if t > 6.0 / 29.0 {
                t * t * t
            } else {
                3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0)
            }
        };
        [D50[0] * inv(fx), D50[1] * inv(fy), D50[2] * inv(fz)]
    }

    impl Transform {
        /// Reads `profile` and prepares a conversion to sRGB.
        pub fn new(profile: &[u8], intent: Intent, bpc: bool) -> Result<Self> {
            if profile.len() < 132 || profile.get(36..40) != Some(b"acsp") {
                return Err(corrupt("not an ICC profile"));
            }
            let space = profile
                .get(16..20)
                .ok_or_else(|| corrupt("not an ICC profile"))?;
            let pcs_lab = match profile.get(20..24) {
                Some(b"XYZ ") => false,
                Some(b"Lab ") => true,
                _ => return Err(unsupported("unknown profile connection space")),
            };
            let channels = match space {
                b"RGB " => 3,
                b"GRAY" => 1,
                b"CMYK" => 4,
                _ => return Err(unsupported("this colour space is not supported")),
            };
            // A LUT for the chosen intent wins over matrix/TRC, as in other CMMs.
            let tags: &[&[u8; 4]] = match intent {
                Intent::Perceptual => &[b"A2B0"],
                Intent::RelativeColorimetric => &[b"A2B1", b"A2B0"],
                Intent::Saturation => &[b"A2B2", b"A2B0"],
            };
            let lut = tags.iter().find_map(|s| tag(profile, s));
            let (input, body) = if let Some(t) = lut {
                let lut = parse_lut(t, channels, pcs_lab)?;
                (lut.a.clone(), Body::Lut(lut))
            } else if channels == 3 {
                let r = xyz_tag(profile, b"rXYZ")?;
                let g = xyz_tag(profile, b"gXYZ")?;
                let b = xyz_tag(profile, b"bXYZ")?;
                let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
                let trc = vec![
                    curve_tag(profile, b"rTRC")?,
                    curve_tag(profile, b"gTRC")?,
                    curve_tag(profile, b"bTRC")?,
                ];
                (trc, Body::Matrix(m))
            } else if channels == 1 {
                (vec![curve_tag(profile, b"kTRC")?], Body::Gray)
            } else {
                return Err(unsupported("CMYK profile without a LUT"));
            };
            // The lut's own first stage is `input`; the body must not apply it again.
            let body = match body {
                Body::Lut(mut l) => {
                    l.a = Vec::new();
                    Body::Lut(l)
                }
                other => other,
            };
            let mut t = Self {
                channels,
                input,
                body,
                to_srgb: invert(&SRGB_TO_XYZ),
                identity: false,
                bpc: None,
            };
            let perceptual = intent != Intent::RelativeColorimetric;
            if bpc || perceptual {
                let v4 = profile.get(8).is_some_and(|&v| v >= 4);
                let black = if perceptual && v4 && lut.is_some() {
                    V4_BLACK
                } else {
                    // The darkest device colour, made neutral and no lighter than L* 50.
                    let dark = vec![if channels == 4 { 1.0 } else { 0.0 }; channels];
                    let lin: Vec<f64> =
                        t.input.iter().zip(&dark).map(|(c, &v)| c.eval(v)).collect();
                    let mut lab = xyz_to_lab(t.xyz(&lin));
                    lab[0] = lab[0].min(50.0);
                    lab_to_xyz(lab[0], 0.0, 0.0)
                };
                let mut a = [1.0; 3];
                let mut b = [0.0; 3];
                for k in 0..3 {
                    let d = black[k] - D50[k];
                    if d.abs() > 1e-9 {
                        a[k] = (0.0 - D50[k]) / d;
                        b[k] = -D50[k] * (0.0 - black[k]) / d;
                    }
                }
                t.bpc = Some((a, b));
            }
            t.identity = channels != 4 && t.looks_like_srgb();
            Ok(t)
        }

        /// Number of device channels per pixel (1 gray, 3 RGB, 4 CMYK).
        pub fn channels(&self) -> usize {
            self.channels
        }

        /// The profile maps colours to themselves, so no conversion is needed.
        pub fn is_identity(&self) -> bool {
            self.identity
        }

        fn looks_like_srgb(&self) -> bool {
            let steps = if self.channels == 1 { 17 } else { 5 };
            let mut worst = 0.0f64;
            let mut check = |c: [f64; 3], n: usize| {
                let lin: Vec<f64> = (0..n).map(|k| self.input[k].eval(c[k])).collect();
                let out = self.rgb_linearised(&lin);
                for k in 0..3 {
                    worst = worst.max((out[k] - c[if n == 1 { 0 } else { k }]).abs());
                }
            };
            if self.channels == 1 {
                for i in 0..steps {
                    let v = i as f64 / (steps - 1) as f64;
                    check([v, v, v], 1);
                }
            } else {
                for r in 0..steps {
                    for g in 0..steps {
                        for b in 0..steps {
                            let d = (steps - 1) as f64;
                            check([r as f64 / d, g as f64 / d, b as f64 / d], 3);
                        }
                    }
                }
            }
            worst * 255.0 < 0.7
        }

        fn xyz(&self, lin: &[f64]) -> [f64; 3] {
            match &self.body {
                Body::Matrix(m) => mul(m, [lin[0], lin[1], lin[2]]),
                Body::Gray => [D50[0] * lin[0], D50[1] * lin[0], D50[2] * lin[0]],
                Body::Lut(l) => l.run(lin),
            }
        }

        /// Device values after the input curves in, encoded sRGB (0..1) out.
        pub fn rgb_linearised(&self, lin: &[f64]) -> [f64; 3] {
            let mut xyz = self.xyz(lin);
            if let Some((a, b)) = &self.bpc {
                for k in 0..3 {
                    xyz[k] = xyz[k] * a[k] + b[k];
                }
            }
            let l = mul(&self.to_srgb, xyz);
            [srgb_encode(l[0]), srgb_encode(l[1]), srgb_encode(l[2])]
        }

        /// Encoded sRGB (0..1) for device values in 0..1.
        pub fn rgb(&self, device: &[f64]) -> [f64; 3] {
            let lin: Vec<f64> = self
                .input
                .iter()
                .zip(device)
                .map(|(c, &v)| c.eval(v))
                .collect();
            self.rgb_linearised(&lin)
        }

        /// A table of `input[c]` at each code value, for converting whole images.
        fn input_tables(&self, max: usize) -> Vec<Vec<f64>> {
            self.input
                .iter()
                .map(|c| (0..=max).map(|v| c.eval(v as f64 / max as f64)).collect())
                .collect()
        }
    }

    /// Converts interleaved device samples to RGB. `src` has `channels` samples per pixel
    /// of `depth`; the result has three samples per pixel in the same depth. For CMYK,
    /// 255 (or 65535) means full ink.
    pub fn convert_samples(t: &Transform, src: &[u8], depth: Depth) -> Vec<u8> {
        let ch = t.channels;
        let bytes = depth.bytes();
        let max = depth.max() as usize;
        let tabs = t.input_tables(max);
        let n = src.len() / (ch * bytes);
        let mut out = Vec::with_capacity(n * 3 * bytes);
        let mut last_in: Vec<usize> = vec![usize::MAX; ch];
        let mut last_out = [0.0f64; 3];
        let mut cur = vec![0usize; ch];
        let mut lin = vec![0.0f64; ch];
        for px in src.chunks_exact(ch * bytes) {
            for k in 0..ch {
                cur[k] = if bytes == 1 {
                    usize::from(px[k])
                } else {
                    usize::from(u16::from_be_bytes([px[2 * k], px[2 * k + 1]]))
                };
            }
            if cur != last_in {
                for k in 0..ch {
                    lin[k] = tabs[k][cur[k].min(max)];
                }
                last_out = t.rgb_linearised(&lin);
                last_in.copy_from_slice(&cur);
            }
            for v in last_out {
                if bytes == 1 {
                    out.push((v * 255.0 + 0.5) as u8);
                } else {
                    out.extend_from_slice(&((v * 65535.0 + 0.5) as u16).to_be_bytes());
                }
            }
        }
        out
    }
}

pub use inner::{convert_samples, Intent, Transform};

#[allow(clippy::indexing_slicing)] // pixel and channel counts come from the layout
/// Converts `img` from its embedded profile to sRGB. The result carries no profile (untagged
/// images are read as sRGB). Fails when the image has no profile, the profile cannot be
/// read, or its colour space does not fit the image.
pub fn to_srgb(img: &Image, intent: Intent, bpc: bool) -> Result<Image> {
    let profile =
        img.metadata.icc_profile.as_deref().ok_or_else(|| {
            Error::new(ErrorCode::InvalidOptions, "the image has no colour profile")
        })?;
    let t = Transform::new(profile, intent, bpc)?;
    let colour_channels = match img.layout {
        Layout::Gray | Layout::GrayAlpha => 1,
        Layout::Rgb | Layout::Rgba => 3,
    };
    if t.channels() != colour_channels {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "the colour profile does not match the image's channels",
        ));
    }
    let mut out = img.clone();
    out.metadata.icc_profile = None;
    if t.is_identity() {
        return Ok(out);
    }
    let bytes = img.depth.bytes();
    let all = img.layout.channels();
    let alpha = img.layout.has_alpha();
    // Colour channels go through the transform; alpha is copied.
    let mut colour = Vec::with_capacity(img.data.len() / all * colour_channels * bytes);
    for px in img.data.chunks_exact(all * bytes) {
        colour.extend_from_slice(&px[..colour_channels * bytes]);
    }
    let rgb = convert_samples(&t, &colour, img.depth);
    let mut data = Vec::with_capacity(img.data.len());
    let n = img.pixel_count();
    for i in 0..n {
        let src = &img.data[i * all * bytes..(i + 1) * all * bytes];
        if colour_channels == 1 {
            // Neutral in, neutral out: all three results agree up to rounding, so take green.
            data.extend_from_slice(&rgb[(i * 3 + 1) * bytes..(i * 3 + 2) * bytes]);
        } else {
            data.extend_from_slice(&rgb[i * 3 * bytes..(i + 1) * 3 * bytes]);
        }
        if alpha {
            data.extend_from_slice(&src[colour_channels * bytes..]);
        }
    }
    out.data = data;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_garbage() {
        assert!(Transform::new(&[], Intent::Perceptual, false).is_err());
        assert!(Transform::new(&[0; 200], Intent::Perceptual, false).is_err());
    }
}
