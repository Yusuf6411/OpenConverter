//! Planar 8-bit 4:2:0 pictures (BT.601, studio range): conversions and basic processing.
use crate::{corrupt, unsupported, Res};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub w: usize,
    pub h: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

fn cw(w: usize) -> usize {
    w.div_ceil(2)
}

impl Picture {
    pub fn black(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            y: vec![16; w * h],
            u: vec![128; cw(w) * cw(h)],
            v: vec![128; cw(w) * cw(h)],
        }
    }

    pub fn cw(&self) -> usize {
        cw(self.w)
    }
    pub fn ch(&self) -> usize {
        cw(self.h)
    }

    /// Bytes of a planar 4:2:0 frame of this size.
    pub fn raw_len(w: usize, h: usize) -> usize {
        w * h + 2 * cw(w) * cw(h)
    }

    pub fn from_rgba(w: usize, h: usize, rgba: &[u8]) -> Res<Self> {
        if rgba.len() != w * h * 4 {
            return Err(corrupt("RGBA buffer has the wrong size"));
        }
        let mut p = Self::black(w, h);
        let (cwid, chei) = (p.cw(), p.ch());
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                let (r, g, b) = (
                    i32::from(rgba[o]),
                    i32::from(rgba[o + 1]),
                    i32::from(rgba[o + 2]),
                );
                p.y[y * w + x] =
                    (16 + ((66 * r + 129 * g + 25 * b + 128) >> 8)).clamp(0, 255) as u8;
            }
        }
        for cy in 0..chei {
            for cx in 0..cwid {
                let (mut r, mut g, mut b, mut n) = (0i32, 0i32, 0i32, 0i32);
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (x, y) = ((cx * 2 + dx).min(w - 1), (cy * 2 + dy).min(h - 1));
                        let o = (y * w + x) * 4;
                        r += i32::from(rgba[o]);
                        g += i32::from(rgba[o + 1]);
                        b += i32::from(rgba[o + 2]);
                        n += 1;
                    }
                }
                let (r, g, b) = ((r + n / 2) / n, (g + n / 2) / n, (b + n / 2) / n);
                p.u[cy * cwid + cx] =
                    (128 + ((-38 * r - 74 * g + 112 * b + 128) >> 8)).clamp(0, 255) as u8;
                p.v[cy * cwid + cx] =
                    (128 + ((112 * r - 94 * g - 18 * b + 128) >> 8)).clamp(0, 255) as u8;
            }
        }
        Ok(p)
    }

    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = vec![255u8; self.w * self.h * 4];
        let cwid = self.cw();
        for y in 0..self.h {
            for x in 0..self.w {
                let c = i32::from(self.y[y * self.w + x]) - 16;
                let d = i32::from(self.u[(y / 2) * cwid + x / 2]) - 128;
                let e = i32::from(self.v[(y / 2) * cwid + x / 2]) - 128;
                let o = (y * self.w + x) * 4;
                out[o] = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
                out[o + 1] = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
                out[o + 2] = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
            }
        }
        out
    }

    /// From the planar frame of a raw stream.
    pub fn from_raw(pix_fmt: &str, w: usize, h: usize, data: &[u8]) -> Res<Self> {
        let need = crate::y4m::frame_size(pix_fmt, w, h)
            .ok_or_else(|| unsupported(format!("pixel format {pix_fmt}")))?;
        if data.len() < need {
            return Err(corrupt("raw frame is too short"));
        }
        let (c_w, c_h) = (cw(w), cw(h));
        let mut p = Self::black(w, h);
        p.y.copy_from_slice(&data[..w * h]);
        match pix_fmt {
            "yuv420p" => {
                p.u.copy_from_slice(&data[w * h..w * h + c_w * c_h]);
                p.v.copy_from_slice(&data[w * h + c_w * c_h..w * h + 2 * c_w * c_h]);
            }
            "yuv422p" => {
                let (u, v) = (
                    &data[w * h..w * h + c_w * h],
                    &data[w * h + c_w * h..w * h + 2 * c_w * h],
                );
                for cy in 0..c_h {
                    for cx in 0..c_w {
                        let (a, b) = (cy * 2, (cy * 2 + 1).min(h - 1));
                        p.u[cy * c_w + cx] = (u32::from(u[a * c_w + cx])
                            + u32::from(u[b * c_w + cx]))
                        .div_ceil(2) as u8;
                        p.v[cy * c_w + cx] = (u32::from(v[a * c_w + cx])
                            + u32::from(v[b * c_w + cx]))
                        .div_ceil(2) as u8;
                    }
                }
            }
            "yuv444p" => {
                let (u, v) = (&data[w * h..2 * w * h], &data[2 * w * h..3 * w * h]);
                for cy in 0..c_h {
                    for cx in 0..c_w {
                        let mut su = 0u32;
                        let mut sv = 0u32;
                        for dy in 0..2 {
                            for dx in 0..2 {
                                let (x, y) = ((cx * 2 + dx).min(w - 1), (cy * 2 + dy).min(h - 1));
                                su += u32::from(u[y * w + x]);
                                sv += u32::from(v[y * w + x]);
                            }
                        }
                        p.u[cy * c_w + cx] = ((su + 2) / 4) as u8;
                        p.v[cy * c_w + cx] = ((sv + 2) / 4) as u8;
                    }
                }
            }
            _ => {} // gray: chroma stays neutral
        }
        Ok(p)
    }

    pub fn to_raw(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::raw_len(self.w, self.h));
        out.extend_from_slice(&self.y);
        out.extend_from_slice(&self.u);
        out.extend_from_slice(&self.v);
        out
    }

    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Res<Self> {
        let (x, y) = (x & !1, y & !1);
        if w == 0 || h == 0 || x + w > self.w || y + h > self.h {
            return Err(crate::unsupported(
                "the crop rectangle does not fit the picture",
            ));
        }
        let mut out = Self::black(w, h);
        for r in 0..h {
            out.y[r * w..(r + 1) * w]
                .copy_from_slice(&self.y[(y + r) * self.w + x..(y + r) * self.w + x + w]);
        }
        let (ocw, och, scw) = (out.cw(), out.ch(), self.cw());
        for r in 0..och {
            let sr = (y / 2 + r).min(self.ch() - 1);
            for c in 0..ocw {
                let sc = (x / 2 + c).min(scw - 1);
                out.u[r * ocw + c] = self.u[sr * scw + sc];
                out.v[r * ocw + c] = self.v[sr * scw + sc];
            }
        }
        Ok(out)
    }

    /// Fits the picture inside a `w`×`h` frame without distorting it: scaled to the largest
    /// size that fits, centred, with black bars.
    pub fn pad(&self, w: usize, h: usize) -> Self {
        let (w, h) = (w.max(2) & !1, h.max(2) & !1);
        if (w, h) == (self.w, self.h) {
            return self.clone();
        }
        let k = (w as f64 / self.w as f64).min(h as f64 / self.h as f64);
        let (iw, ih) = (
            (((self.w as f64 * k).round() as usize).max(2) & !1).min(w),
            (((self.h as f64 * k).round() as usize).max(2) & !1).min(h),
        );
        let inner = self.scale(iw, ih);
        let (x0, y0) = (((w - iw) / 2) & !1, ((h - ih) / 2) & !1);
        let mut out = Self::black(w, h);
        for r in 0..ih {
            out.y[(y0 + r) * w + x0..(y0 + r) * w + x0 + iw]
                .copy_from_slice(&inner.y[r * iw..(r + 1) * iw]);
        }
        let (ocw, icw) = (out.cw(), inner.cw());
        for r in 0..inner.ch() {
            let dr = y0 / 2 + r;
            if dr >= out.ch() {
                break;
            }
            let n = icw.min(ocw.saturating_sub(x0 / 2));
            out.u[dr * ocw + x0 / 2..dr * ocw + x0 / 2 + n]
                .copy_from_slice(&inner.u[r * icw..r * icw + n]);
            out.v[dr * ocw + x0 / 2..dr * ocw + x0 / 2 + n]
                .copy_from_slice(&inner.v[r * icw..r * icw + n]);
        }
        out
    }

    /// Removes combing from interlaced frames: every odd line is replaced by the average of
    /// the even lines around it (the second field is dropped).
    pub fn deinterlace(&self) -> Self {
        fn fix(p: &mut [u8], w: usize, h: usize) {
            for r in (1..h).step_by(2) {
                for c in 0..w {
                    let above = u16::from(p[(r - 1) * w + c]);
                    let below = if r + 1 < h {
                        u16::from(p[(r + 1) * w + c])
                    } else {
                        above
                    };
                    p[r * w + c] = ((above + below).div_ceil(2)) as u8;
                }
            }
        }
        let mut out = self.clone();
        fix(&mut out.y, self.w, self.h);
        fix(&mut out.u, self.cw(), self.ch());
        fix(&mut out.v, self.cw(), self.ch());
        out
    }

    pub fn scale(&self, w: usize, h: usize) -> Self {
        if w == self.w && h == self.h {
            return self.clone();
        }
        Self {
            w,
            h,
            y: resample(&self.y, self.w, self.h, w, h),
            u: resample(&self.u, self.cw(), self.ch(), cw(w), cw(h)),
            v: resample(&self.v, self.cw(), self.ch(), cw(w), cw(h)),
        }
    }

    /// Rotate clockwise by 90, 180 or 270 degrees.
    pub fn rotate(&self, deg: u16) -> Self {
        match deg % 360 {
            90 => Self {
                w: self.h,
                h: self.w,
                y: rot90(&self.y, self.w, self.h),
                u: rot90(&self.u, self.cw(), self.ch()),
                v: rot90(&self.v, self.cw(), self.ch()),
            },
            180 => Self {
                w: self.w,
                h: self.h,
                y: self.y.iter().rev().copied().collect(),
                u: self.u.iter().rev().copied().collect(),
                v: self.v.iter().rev().copied().collect(),
            },
            270 => self.rotate(90).rotate(180),
            _ => self.clone(),
        }
    }

    pub fn flip_horizontal(&self) -> Self {
        let f = |p: &[u8], w: usize| -> Vec<u8> {
            p.chunks(w).flat_map(|r| r.iter().rev().copied()).collect()
        };
        Self {
            w: self.w,
            h: self.h,
            y: f(&self.y, self.w),
            u: f(&self.u, self.cw()),
            v: f(&self.v, self.cw()),
        }
    }

    pub fn flip_vertical(&self) -> Self {
        let f = |p: &[u8], w: usize| -> Vec<u8> { p.chunks(w).rev().flatten().copied().collect() };
        Self {
            w: self.w,
            h: self.h,
            y: f(&self.y, self.w),
            u: f(&self.u, self.cw()),
            v: f(&self.v, self.cw()),
        }
    }
}

fn rot90(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    // New picture is h wide and w tall; destination (x', y') takes source (y', h-1-x')... clockwise.
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            out[x * h + (h - 1 - y)] = src[y * w + x];
        }
    }
    out
}

/// Separable resampling with a triangle filter, widened when shrinking.
fn resample(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    fn taps(sn: usize, dn: usize) -> Vec<(usize, Vec<i32>)> {
        let ratio = sn as f64 / dn as f64;
        let support = ratio.max(1.0);
        (0..dn)
            .map(|d| {
                let centre = (d as f64 + 0.5) * ratio - 0.5;
                let lo = ((centre - support).ceil().max(0.0)) as usize;
                let hi = (((centre + support).floor()) as i64).clamp(0, sn as i64 - 1) as usize;
                let mut w: Vec<f64> = (lo..=hi.max(lo))
                    .map(|s| (1.0 - ((s as f64 - centre).abs() / support)).max(0.0))
                    .collect();
                let sum: f64 = w.iter().sum();
                if sum <= 0.0 {
                    w = vec![1.0];
                }
                let sum: f64 = w.iter().sum();
                (
                    lo.min(sn - 1),
                    w.iter()
                        .map(|x| (x / sum * 16384.0).round() as i32)
                        .collect(),
                )
            })
            .collect()
    }
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return vec![0; dw * dh];
    }
    let tx = taps(sw, dw);
    let ty = taps(sh, dh);
    let mut tmp = vec![0u8; dw * sh];
    for y in 0..sh {
        for (x, (lo, w)) in tx.iter().enumerate() {
            let mut acc = 0i32;
            for (i, wt) in w.iter().enumerate() {
                acc += wt * i32::from(src[y * sw + (lo + i).min(sw - 1)]);
            }
            tmp[y * dw + x] = ((acc + 8192) >> 14).clamp(0, 255) as u8;
        }
    }
    let mut out = vec![0u8; dw * dh];
    for (y, (lo, w)) in ty.iter().enumerate() {
        for x in 0..dw {
            let mut acc = 0i32;
            for (i, wt) in w.iter().enumerate() {
                acc += wt * i32::from(tmp[(lo + i).min(sh - 1) * dw + x]);
            }
            out[y * dw + x] = ((acc + 8192) >> 14).clamp(0, 255) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_round_trip_is_close() {
        let (w, h) = (16, 16);
        let mut rgba = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                rgba[o] = (x * 16) as u8;
                rgba[o + 1] = (y * 16) as u8;
                rgba[o + 2] = 128;
            }
        }
        // Flat 2x2 blocks survive 4:2:0 exactly enough.
        let flat: Vec<u8> = (0..w * h * 4)
            .map(|i| rgba[((i / 4 / w / 2 * 2) * w + (i / 4 % w) / 2 * 2) * 4 + i % 4])
            .collect();
        let p = Picture::from_rgba(w, h, &flat).unwrap_or_else(|_| Picture::black(w, h));
        let back = p.to_rgba();
        let worst = flat
            .chunks(4)
            .zip(back.chunks(4))
            .flat_map(|(a, b)| (0..3).map(move |i| (i32::from(a[i]) - i32::from(b[i])).abs()))
            .max()
            .unwrap_or(0);
        assert!(worst <= 4, "worst error {worst}");
    }

    #[test]
    fn rotation_and_scale_shapes() {
        let mut p = Picture::black(6, 4);
        p.y[0] = 200;
        let r = p.rotate(90);
        assert_eq!((r.w, r.h), (4, 6));
        assert_eq!(r.y[3], 200, "top-left goes to top-right");
        assert_eq!(p.rotate(180).y[23], 200);
        let s = p.scale(3, 2);
        assert_eq!((s.w, s.h, s.y.len(), s.u.len()), (3, 2, 6, 2));
        assert_eq!(p.rotate(270).rotate(90), p);
    }
}
