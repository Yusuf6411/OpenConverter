//! Blend modes and Porter-Duff operators on premultiplied RGBA.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Blend {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    // Porter-Duff
    Clear,
    Source,
    DestOut,
    SourceIn,
    SourceOut,
    SourceAtop,
    DestOver,
    DestIn,
    DestAtop,
    Xor,
    Plus,
}

impl Blend {
    pub fn from_name(n: &str) -> Option<Blend> {
        Some(match n {
            "normal" | "Normal" | "Compatible" => Blend::Normal,
            "multiply" | "Multiply" => Blend::Multiply,
            "screen" | "Screen" => Blend::Screen,
            "overlay" | "Overlay" => Blend::Overlay,
            "darken" | "Darken" => Blend::Darken,
            "lighten" | "Lighten" => Blend::Lighten,
            "color-dodge" | "ColorDodge" => Blend::ColorDodge,
            "color-burn" | "ColorBurn" => Blend::ColorBurn,
            "hard-light" | "HardLight" => Blend::HardLight,
            "soft-light" | "SoftLight" => Blend::SoftLight,
            "difference" | "Difference" => Blend::Difference,
            "exclusion" | "Exclusion" => Blend::Exclusion,
            "hue" | "Hue" => Blend::Hue,
            "saturation" | "Saturation" => Blend::Saturation,
            "color" | "Color" => Blend::Color,
            "luminosity" | "Luminosity" => Blend::Luminosity,
            _ => return None,
        })
    }
    fn is_separable(self) -> bool {
        !matches!(
            self,
            Blend::Hue | Blend::Saturation | Blend::Color | Blend::Luminosity
        )
    }
}

fn sep(mode: Blend, cb: f32, cs: f32) -> f32 {
    match mode {
        Blend::Multiply => cb * cs,
        Blend::Screen => cb + cs - cb * cs,
        Blend::Overlay => sep(Blend::HardLight, cs, cb),
        Blend::Darken => cb.min(cs),
        Blend::Lighten => cb.max(cs),
        Blend::ColorDodge => {
            if cb == 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        Blend::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        Blend::HardLight => {
            if cs <= 0.5 {
                cb * 2.0 * cs
            } else {
                let s = 2.0 * cs - 1.0;
                cb + s - cb * s
            }
        }
        Blend::SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        Blend::Difference => (cb - cs).abs(),
        Blend::Exclusion => cb + cs - 2.0 * cb * cs,
        _ => cs,
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut o = c;
    if n < 0.0 {
        for v in &mut o {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut o {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    o
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|a, b| c[*a].total_cmp(&c[*b]));
    let (mn, md, mx) = (idx[0], idx[1], idx[2]);
    let mut o = [0.0f32; 3];
    if c[mx] > c[mn] {
        o[md] = (c[md] - c[mn]) * s / (c[mx] - c[mn]);
        o[mx] = s;
    }
    o
}

fn nonsep(mode: Blend, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        Blend::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        Blend::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        Blend::Color => set_lum(cs, lum(cb)),
        _ => set_lum(cb, lum(cs)),
    }
}

/// Composite premultiplied `src` over `dst` with `mode`.
pub fn composite(mode: Blend, dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let (ab, as_) = (dst[3], src[3]);
    let pd = |fa: f32, fb: f32| {
        [
            src[0] * fa + dst[0] * fb,
            src[1] * fa + dst[1] * fb,
            src[2] * fa + dst[2] * fb,
            src[3] * fa + dst[3] * fb,
        ]
    };
    match mode {
        Blend::Clear => return [0.0; 4],
        Blend::Source => return src,
        Blend::DestOut => return pd(0.0, 1.0 - as_),
        Blend::SourceIn => return pd(ab, 0.0),
        Blend::SourceOut => return pd(1.0 - ab, 0.0),
        Blend::SourceAtop => return pd(ab, 1.0 - as_),
        Blend::DestOver => return pd(1.0 - ab, 1.0),
        Blend::DestIn => return pd(0.0, as_),
        Blend::DestAtop => return pd(1.0 - ab, as_),
        Blend::Xor => return pd(1.0 - ab, 1.0 - as_),
        Blend::Plus => {
            let mut o = pd(1.0, 1.0);
            for v in &mut o {
                *v = v.min(1.0);
            }
            return o;
        }
        Blend::Normal => return pd(1.0, 1.0 - as_),
        _ => {}
    }
    if as_ <= 0.0 {
        return dst;
    }
    let un = |c: [f32; 4]| -> [f32; 3] {
        if c[3] <= 0.0 {
            [0.0; 3]
        } else {
            [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
        }
    };
    let (cb, cs) = (un(dst), un(src));
    let b = if mode.is_separable() {
        [
            sep(mode, cb[0], cs[0]),
            sep(mode, cb[1], cs[1]),
            sep(mode, cb[2], cs[2]),
        ]
    } else {
        nonsep(mode, cb, cs)
    };
    let ao = as_ + ab - as_ * ab;
    let mut o = [0.0f32; 4];
    for k in 0..3 {
        o[k] = as_ * (1.0 - ab) * cs[k] + as_ * ab * b[k] + (1.0 - as_) * ab * cb[k];
    }
    o[3] = ao;
    o
}
