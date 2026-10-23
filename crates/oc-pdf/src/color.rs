//! PDF colour spaces, all mapped to sRGB.
use crate::doc::Pdf;
use crate::function::Function;
use crate::object::{Dict, Obj};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Lab {
        white: [f64; 3],
        range: [f64; 4],
    },
    Indexed {
        base: Arc<ColorSpace>,
        hival: usize,
        table: Arc<Vec<u8>>,
    },
    Tint {
        n: usize,
        alt: Arc<ColorSpace>,
        func: Option<Arc<Function>>,
        none: bool,
    },
    Pattern(Option<Arc<ColorSpace>>),
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// CMYK to RGB with the polynomial xpdf and Poppler use, which approximates press inks.
pub fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [f32; 3] {
    let (c, m, y, k) = (clamp01(c), clamp01(m), clamp01(y), clamp01(k));
    let (c1, m1, y1, k1) = (1.0 - c, 1.0 - m, 1.0 - y, 1.0 - k);
    let (mut r, mut g, mut b);
    let mut x = c1 * m1 * y1 * k1;
    r = x;
    g = x;
    b = x;
    x = c1 * m1 * y1 * k;
    r += 0.1373 * x;
    g += 0.1216 * x;
    b += 0.1255 * x;
    x = c1 * m1 * y * k1;
    r += x;
    g += 0.9490 * x;
    x = c1 * m1 * y * k;
    r += 0.1098 * x;
    g += 0.1020 * x;
    x = c1 * m * y1 * k1;
    r += 0.9255 * x;
    b += 0.5490 * x;
    x = c1 * m * y1 * k;
    r += 0.1412 * x;
    x = c1 * m * y * k1;
    r += 0.9294 * x;
    g += 0.1098 * x;
    b += 0.1412 * x;
    x = c1 * m * y * k;
    r += 0.1333 * x;
    x = c * m1 * y1 * k1;
    g += 0.6784 * x;
    b += 0.9373 * x;
    x = c * m1 * y1 * k;
    g += 0.0588 * x;
    b += 0.1412 * x;
    x = c * m1 * y * k1;
    g += 0.6510 * x;
    b += 0.3137 * x;
    x = c * m1 * y * k;
    g += 0.0745 * x;
    x = c * m * y1 * k1;
    r += 0.1804 * x;
    g += 0.1922 * x;
    b += 0.5725 * x;
    x = c * m * y1 * k;
    b += 0.0078 * x;
    x = c * m * y * k1;
    r += 0.2118 * x;
    g += 0.2119 * x;
    b += 0.2235 * x;
    [clamp01(r) as f32, clamp01(g) as f32, clamp01(b) as f32]
}

fn srgb_gamma(v: f64) -> f64 {
    let v = clamp01(v);
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

impl ColorSpace {
    pub fn n_components(&self) -> usize {
        match self {
            ColorSpace::Gray | ColorSpace::Indexed { .. } => 1,
            ColorSpace::Rgb | ColorSpace::Lab { .. } => 3,
            ColorSpace::Cmyk => 4,
            ColorSpace::Tint { n, .. } => *n,
            ColorSpace::Pattern(_) => 0,
        }
    }

    pub fn initial(&self) -> Vec<f64> {
        match self {
            ColorSpace::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Tint { n, .. } => vec![1.0; *n],
            ColorSpace::Pattern(_) => vec![],
            c => vec![0.0; c.n_components()],
        }
    }

    /// The default Decode array for images in this space with `bpc` bits.
    pub fn default_decode(&self, bpc: usize) -> Vec<f64> {
        match self {
            ColorSpace::Indexed { .. } => vec![0.0, ((1u64 << bpc.min(16)) - 1) as f64],
            ColorSpace::Lab { range, .. } => {
                vec![0.0, 100.0, range[0], range[1], range[2], range[3]]
            }
            c => (0..c.n_components()).flat_map(|_| [0.0, 1.0]).collect(),
        }
    }

    pub fn to_rgb(&self, c: &[f64]) -> [f32; 3] {
        let g = |i: usize| c.get(i).copied().unwrap_or(0.0);
        match self {
            ColorSpace::Gray => {
                let v = clamp01(g(0)) as f32;
                [v, v, v]
            }
            ColorSpace::Rgb => [
                clamp01(g(0)) as f32,
                clamp01(g(1)) as f32,
                clamp01(g(2)) as f32,
            ],
            ColorSpace::Cmyk => cmyk_to_rgb(g(0), g(1), g(2), g(3)),
            ColorSpace::Lab { white, range } => {
                let l = g(0).clamp(0.0, 100.0);
                let a = g(1).clamp(range[0], range[1]);
                let b = g(2).clamp(range[2], range[3]);
                let t1 = (l + 16.0) / 116.0;
                let f = |t: f64| {
                    if t >= 6.0 / 29.0 {
                        t * t * t
                    } else {
                        (108.0 / 841.0) * (t - 4.0 / 29.0)
                    }
                };
                let x = f(t1 + a / 500.0) * white[0];
                let y = f(t1) * white[1];
                let z = f(t1 - b / 200.0) * white[2];
                // D50-ish whitepoints are adapted to D65 with the Bradford matrix
                let (x, y, z) = if white[0] < 0.97 {
                    (
                        0.955_576_6 * x - 0.023_039_3 * y + 0.063_163_6 * z,
                        -0.028_289_5 * x + 1.009_941_6 * y + 0.021_007_7 * z,
                        0.012_298_2 * x - 0.020_483_0 * y + 1.329_909_8 * z,
                    )
                } else {
                    (x, y, z)
                };
                let r = 3.240_449 * x - 1.537_136 * y - 0.498_531 * z;
                let gg = -0.969_265 * x + 1.876_011 * y + 0.041_556 * z;
                let bb = 0.055_643 * x - 0.204_026 * y + 1.057_229 * z;
                [
                    srgb_gamma(r) as f32,
                    srgb_gamma(gg) as f32,
                    srgb_gamma(bb) as f32,
                ]
            }
            ColorSpace::Indexed { base, hival, table } => {
                let i = (g(0).round().max(0.0) as usize).min(*hival);
                let n = base.n_components();
                let dec = base.default_decode(8);
                let comps: Vec<f64> = (0..n)
                    .map(|k| {
                        let v = f64::from(table.get(i * n + k).copied().unwrap_or(0)) / 255.0;
                        dec[2 * k] + v * (dec[2 * k + 1] - dec[2 * k])
                    })
                    .collect();
                base.to_rgb(&comps)
            }
            ColorSpace::Tint {
                alt, func, none, ..
            } => {
                if *none {
                    return [1.0, 1.0, 1.0];
                }
                match func {
                    Some(f) => alt.to_rgb(&f.eval(c)),
                    None => {
                        let v = clamp01(1.0 - g(0)) as f32;
                        [v, v, v]
                    }
                }
            }
            ColorSpace::Pattern(_) => [0.0, 0.0, 0.0],
        }
    }

    pub fn parse(pdf: &Pdf, o: &Obj, res: &Dict, depth: u32) -> Option<ColorSpace> {
        if depth > 8 {
            return None;
        }
        let o = pdf.resolve(o);
        match &o {
            Obj::Name(n) => match n.as_str() {
                "DeviceGray" | "G" | "CalGray" => Some(ColorSpace::Gray),
                "DeviceRGB" | "RGB" | "CalRGB" => Some(ColorSpace::Rgb),
                "DeviceCMYK" | "CMYK" => Some(ColorSpace::Cmyk),
                "Pattern" => Some(ColorSpace::Pattern(None)),
                other => {
                    let cs = pdf.dget_dict(res, "ColorSpace")?;
                    let target = cs.get(other)?;
                    if matches!(pdf.resolve(target).as_name(), Some(n) if n == other) {
                        return None;
                    }
                    Self::parse(pdf, target, res, depth + 1)
                }
            },
            Obj::Array(a) => {
                let kind = pdf.resolve(a.first()?);
                let kind = kind.as_name()?;
                match kind {
                    "DeviceGray" | "G" | "CalGray" => Some(ColorSpace::Gray),
                    "DeviceRGB" | "RGB" | "CalRGB" => Some(ColorSpace::Rgb),
                    "DeviceCMYK" | "CMYK" => Some(ColorSpace::Cmyk),
                    "Lab" => {
                        let d = pdf.resolve(a.get(1)?).as_dict()?.clone();
                        let w = pdf
                            .dget_numbers(&d, "WhitePoint")
                            .unwrap_or_else(|| vec![0.9505, 1.0, 1.089]);
                        let r = pdf
                            .dget_numbers(&d, "Range")
                            .unwrap_or_else(|| vec![-100.0, 100.0, -100.0, 100.0]);
                        Some(ColorSpace::Lab {
                            white: [
                                w.first().copied().unwrap_or(0.9505),
                                w.get(1).copied().unwrap_or(1.0),
                                w.get(2).copied().unwrap_or(1.089),
                            ],
                            range: [
                                r.first().copied().unwrap_or(-100.0),
                                r.get(1).copied().unwrap_or(100.0),
                                r.get(2).copied().unwrap_or(-100.0),
                                r.get(3).copied().unwrap_or(100.0),
                            ],
                        })
                    }
                    "ICCBased" => {
                        let s = pdf.resolve(a.get(1)?);
                        let d = s.as_dict()?;
                        let n = pdf.dget_i64(d, "N").unwrap_or(3);
                        if let Some(alt) = d
                            .get("Alternate")
                            .and_then(|alt| Self::parse(pdf, alt, res, depth + 1))
                        {
                            if alt.n_components() as i64 == n
                                && !matches!(alt, ColorSpace::Pattern(_))
                            {
                                return Some(alt);
                            }
                        }
                        Some(match n {
                            1 => ColorSpace::Gray,
                            4 => ColorSpace::Cmyk,
                            _ => ColorSpace::Rgb,
                        })
                    }
                    "Indexed" | "I" => {
                        let base = Self::parse(pdf, a.get(1)?, res, depth + 1)?;
                        let hival = pdf.resolve(a.get(2)?).as_i64()?.clamp(0, 255) as usize;
                        let lookup = pdf.resolve(a.get(3)?);
                        let table = match &lookup {
                            Obj::Str(s) => s.clone(),
                            Obj::Stream(s) => crate::filter::decode_stream(pdf, s, false).ok()?.0,
                            _ => return None,
                        };
                        Some(ColorSpace::Indexed {
                            base: Arc::new(base),
                            hival,
                            table: Arc::new(table),
                        })
                    }
                    "Separation" => {
                        let name = pdf.resolve(a.get(1)?);
                        let alt = Self::parse(pdf, a.get(2)?, res, depth + 1)?;
                        let func = Function::parse(pdf, a.get(3)?).map(Arc::new);
                        Some(ColorSpace::Tint {
                            n: 1,
                            alt: Arc::new(alt),
                            func,
                            none: name.as_name() == Some("None"),
                        })
                    }
                    "DeviceN" => {
                        let names = pdf.resolve(a.get(1)?);
                        let n = names.as_array()?.len();
                        let alt = Self::parse(pdf, a.get(2)?, res, depth + 1)?;
                        let func = Function::parse(pdf, a.get(3)?).map(Arc::new);
                        Some(ColorSpace::Tint {
                            n,
                            alt: Arc::new(alt),
                            func,
                            none: false,
                        })
                    }
                    "Pattern" => {
                        let base = a
                            .get(1)
                            .and_then(|b| Self::parse(pdf, b, res, depth + 1))
                            .map(Arc::new);
                        Some(ColorSpace::Pattern(base))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}
