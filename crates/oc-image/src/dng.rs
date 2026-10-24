//! DNG 1.x (Adobe Digital Negative), read only: the raw sensor image is linearised, white
//! balanced, demosaiced and rendered to 16-bit sRGB.
//!
//! Read: the full-resolution raw IFD (in IFD 0 or a SubIFD), tiled or in strips, as a colour
//! filter array (any repeat pattern of red, green and blue) or LinearRaw, uncompressed
//! (8 to 16 bits), lossless JPEG or Deflate, with LinearizationTable, BlackLevel (with the
//! delta tables), WhiteLevel, ActiveArea and DefaultCrop. Colour uses the ForwardMatrix, or the
//! ColorMatrix with Bradford adaptation, interpolated between two calibration illuminants by
//! inverse colour temperature as the specification requires; Orientation is applied.
//!
//! The render is a plain one: bilinear demosaicing, BaselineExposure, no camera profile tone
//! curve, hue/saturation map, opcode lists (gain maps, lens corrections) or noise reduction.
//!
//! Not supported (typed error): lossy JPEG and JPEG XL compressed raw data, floating point
//! samples, cameras with more than three colour planes, planar layouts, BigTIFF.
#![allow(clippy::indexing_slicing, clippy::chunks_exact_to_as_chunks)] // indices are bounded by the image dimensions checked above
use crate::image::{Depth, Image, Layout};
use crate::jpeg;
use oc_compress::zlib;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Progress, Result, Severity};
use std::collections::HashMap;

const DNG_VERSION: u16 = 50706;

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorCode::UnsupportedVariant,
        format!("DNG: {what} is not supported"),
    )
}

#[derive(Clone, Copy)]
struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn u16(&self, i: usize) -> Option<u16> {
        let b: [u8; 2] = self.d.get(i..i.checked_add(2)?)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }
    fn u32(&self, i: usize) -> Option<u32> {
        let b: [u8; 4] = self.d.get(i..i.checked_add(4)?)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
}

struct Entry<'a> {
    typ: u16,
    data: &'a [u8],
    t: Tiff<'a>,
}

impl Entry<'_> {
    fn nums(&self) -> Vec<f64> {
        let t = self.t;
        let d = self.data;
        let rd = |size: usize, f: &dyn Fn(&[u8]) -> f64| -> Vec<f64> {
            d.chunks_exact(size).map(f).collect()
        };
        let u16f = |b: &[u8]| {
            f64::from(if t.le {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            })
        };
        let u32v = |b: &[u8]| -> u32 {
            if t.le {
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            } else {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]])
            }
        };
        let div = |n: f64, m: f64| if m == 0.0 { 0.0 } else { n / m };
        match self.typ {
            1 | 2 | 7 => rd(1, &|b| f64::from(b[0])),
            6 => rd(1, &|b| f64::from(b[0] as i8)),
            3 => rd(2, &u16f),
            8 => rd(2, &|b| u16f(b) as u16 as i16 as f64),
            4 | 13 => rd(4, &|b| f64::from(u32v(b))),
            9 => rd(4, &|b| f64::from(u32v(b) as i32)),
            5 => rd(8, &|b| {
                div(f64::from(u32v(&b[..4])), f64::from(u32v(&b[4..])))
            }),
            10 => rd(8, &|b| {
                div(
                    f64::from(u32v(&b[..4]) as i32),
                    f64::from(u32v(&b[4..]) as i32),
                )
            }),
            11 => rd(4, &|b| f64::from(f32::from_bits(u32v(b)))),
            12 => rd(8, &|b| {
                let (hi, lo) = (u64::from(u32v(&b[..4])), u64::from(u32v(&b[4..])));
                f64::from_bits(if t.le {
                    (lo << 32) | hi
                } else {
                    (hi << 32) | lo
                })
            }),
            _ => Vec::new(),
        }
    }
}

struct Ifd<'a> {
    fields: HashMap<u16, Entry<'a>>,
    next: usize,
}

impl<'a> Ifd<'a> {
    fn nums(&self, tag: u16) -> Vec<f64> {
        self.fields.get(&tag).map(Entry::nums).unwrap_or_default()
    }
    fn num(&self, tag: u16) -> Option<f64> {
        self.nums(tag).first().copied()
    }
    fn uint(&self, tag: u16, default: u32) -> u32 {
        self.num(tag).map_or(default, |v| v as u32)
    }
}

fn type_size(typ: u16) -> usize {
    match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => 0,
    }
}

fn read_ifd<'a>(t: Tiff<'a>, off: usize) -> Result<Ifd<'a>> {
    let n = usize::from(
        t.u16(off)
            .ok_or_else(|| Error::truncated("DNG directory"))?,
    );
    if n > 4096 {
        return Err(Error::corrupt("DNG directory is too large"));
    }
    let mut fields = HashMap::new();
    for i in 0..n {
        let e = off + 2 + 12 * i;
        let (Some(tag), Some(typ), Some(count)) = (t.u16(e), t.u16(e + 2), t.u32(e + 4)) else {
            return Err(Error::truncated("DNG directory entry"));
        };
        let size = type_size(typ).saturating_mul(count as usize);
        let start = if size <= 4 {
            e + 8
        } else {
            t.u32(e + 8)
                .ok_or_else(|| Error::truncated("DNG directory entry"))? as usize
        };
        if size == 0 {
            continue;
        }
        let Some(data) = t.d.get(start..start.saturating_add(size)) else {
            continue; // a value outside the file; the tag is treated as absent
        };
        fields.insert(tag, Entry { typ, data, t });
    }
    let next = t.u32(off + 2 + 12 * n).unwrap_or(0) as usize;
    Ok(Ifd { fields, next })
}

fn header(data: &[u8]) -> Option<(Tiff<'_>, usize)> {
    let le = match data.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let t = Tiff { d: data, le };
    Some((t, t.u32(4)? as usize))
}

/// Whether `data` is a TIFF file whose first directory carries a DNGVersion tag.
pub fn sniff(data: &[u8]) -> bool {
    header(data)
        .and_then(|(t, off)| read_ifd(t, off).ok())
        .is_some_and(|ifd| ifd.fields.contains_key(&DNG_VERSION))
}

type M3 = [[f64; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

fn apply(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| (0..3).map(|k| m[i][k] * v[k]).sum())
}

fn inverse(m: &M3) -> Option<M3> {
    let c = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    if det.abs() < 1e-12 {
        return None;
    }
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = c(j, i) / det;
        }
    }
    Some(o)
}

fn diag(v: [f64; 3]) -> M3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

fn m3(v: &[f64]) -> Option<M3> {
    (v.len() == 9).then(|| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
}

fn xy_to_xyz(x: f64, y: f64) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn xyz_to_xy(v: [f64; 3]) -> (f64, f64) {
    let s = v[0] + v[1] + v[2];
    if s <= 0.0 {
        (0.3457, 0.3585)
    } else {
        (v[0] / s, v[1] / s)
    }
}

/// Correlated colour temperature of a chromaticity (McCamy's approximation).
fn cct(x: f64, y: f64) -> f64 {
    let n = (x - 0.3320) / (0.1858 - y);
    (449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33).clamp(1500.0, 20000.0)
}

fn illuminant_temperature(code: u32) -> f64 {
    match code {
        1 | 4 | 9 => 5500.0,
        2 => 4200.0,
        3 => 2850.0,
        10 => 6500.0,
        11 => 7500.0,
        12 => 6400.0,
        13 => 5000.0,
        14 => 4150.0,
        15 => 3500.0,
        17 => 2856.0,
        18 => 4874.0,
        19 => 6774.0,
        20 => 5503.0,
        21 => 6504.0,
        22 => 7504.0,
        23 => 5003.0,
        24 => 3200.0,
        _ => 5000.0,
    }
}

/// Two calibrations (matrix, illuminant temperature) blended for a white balance at
/// temperature `t`: linear in inverse temperature between the two, the nearer one outside.
struct Pair {
    a: M3,
    ta: f64,
    b: Option<(M3, f64)>,
}

impl Pair {
    fn at(&self, t: f64) -> M3 {
        let Some((b, tb)) = &self.b else {
            return self.a;
        };
        let ((lo, tlo), (hi, thi)) = if self.ta <= *tb {
            ((&self.a, self.ta), (b, *tb))
        } else {
            ((b, *tb), (&self.a, self.ta))
        };
        if t <= tlo {
            return *lo;
        }
        if t >= thi {
            return *hi;
        }
        let w = (1.0 / t - 1.0 / thi) / (1.0 / tlo - 1.0 / thi);
        let mut o = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                o[i][j] = w * lo[i][j] + (1.0 - w) * hi[i][j];
            }
        }
        o
    }
}

fn pair(i0: &Ifd, ifd: &Ifd, m1: u16, m2: u16, ill1: u16, ill2: u16) -> Option<Pair> {
    let get = |tag: u16| {
        let mut v = i0.nums(tag);
        if v.is_empty() {
            v = ifd.nums(tag);
        }
        m3(&v)
    };
    let ill = |tag: u16, default: u32| illuminant_temperature(i0.uint(tag, ifd.uint(tag, default)));
    let a = get(m1)?;
    Some(Pair {
        a,
        ta: ill(ill1, 21),
        b: get(m2).map(|b| (b, ill(ill2, 17))),
    })
}

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
const BRADFORD: M3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];
const XYZ_D50_TO_SRGB: M3 = [
    [3.1338561, -1.6168667, -0.4906146],
    [-0.9787684, 1.9161415, 0.0334540],
    [0.0719453, -0.2289914, 1.4052427],
];

fn adaptation(src: [f64; 3], dst: [f64; 3]) -> Option<M3> {
    let s = apply(&BRADFORD, src);
    let d = apply(&BRADFORD, dst);
    let scale = diag([d[0] / s[0], d[1] / s[1], d[2] / s[2]]);
    Some(mul(&inverse(&BRADFORD)?, &mul(&scale, &BRADFORD)))
}

/// The matrix taking linear camera values to linear sRGB (D50 adapted), following chapter 6.
fn camera_to_srgb(
    i0: &Ifd,
    raw: &Ifd,
    neutral: Option<[f64; 3]>,
    white_xy: Option<(f64, f64)>,
    notes: &mut Notes,
) -> Result<M3> {
    let analog = {
        let v = i0.nums(50727);
        if v.len() == 3 {
            [v[0], v[1], v[2]]
        } else {
            [1.0; 3]
        }
    };
    let ab = diag(analog);
    let color = pair(i0, raw, 50721, 50722, 50778, 50779);
    let forward = pair(i0, raw, 50964, 50965, 50778, 50779);
    // The camera neutral: given, or derived from the white point through the colour matrix.
    let xyz_to_cam = |t: f64| color.as_ref().map(|c| mul(&ab, &c.at(t)));
    let neutral = match (neutral, white_xy) {
        (Some(n), _) => Some(n),
        (None, Some((x, y))) => xyz_to_cam(cct(x, y)).map(|m| apply(&m, xy_to_xyz(x, y))),
        _ => None,
    };
    let Some(n) = neutral.filter(|n| n.iter().all(|v| *v > 0.0)) else {
        notes.push(Note::new(
            Severity::Warning,
            "dng.no_white_balance",
            "the file has no usable white balance; camera colours were shown as they are",
        ));
        return Ok(mul(
            &XYZ_D50_TO_SRGB,
            &[
                [0.4360747, 0.3850649, 0.1430804],
                [0.2225045, 0.7168786, 0.0606169],
                [0.0139322, 0.0971045, 0.7141733],
            ],
        ));
    };
    // The white balance's chromaticity, found by iterating as the specification describes.
    let mut xy = (0.3457, 0.3585);
    if let Some(c) = &color {
        for _ in 0..30 {
            let Some(inv) = inverse(&mul(&ab, &c.at(cct(xy.0, xy.1)))) else {
                break;
            };
            let next = xyz_to_xy(apply(&inv, n));
            let done = (next.0 - xy.0).abs() < 1e-6 && (next.1 - xy.1).abs() < 1e-6;
            xy = next;
            if done {
                break;
            }
        }
    }
    let t = cct(xy.0, xy.1);
    let to_xyz_d50 = if let Some(f) = &forward {
        // CameraToXYZ_D50 = FM * D * inverse(AB), with D = inverse(diag(inverse(AB) * neutral)).
        let reference = [n[0] / analog[0], n[1] / analog[1], n[2] / analog[2]];
        let d = diag(reference.map(|v| 1.0 / v));
        mul(&f.at(t), &mul(&d, &diag(analog.map(|v| 1.0 / v))))
    } else if let Some(c) = &color {
        let inv = inverse(&mul(&ab, &c.at(t)))
            .ok_or_else(|| Error::corrupt("DNG: ColorMatrix is singular"))?;
        let white = apply(&inv, n);
        let w = [white[0] / white[1], 1.0, white[2] / white[1]];
        let ca = adaptation(w, D50).ok_or_else(|| Error::corrupt("DNG: bad white point"))?;
        mul(&ca, &inv)
    } else {
        notes.push(Note::new(
            Severity::Warning,
            "dng.no_color_matrix",
            "the file has no colour matrix; only the white balance was applied",
        ));
        return Ok(diag([1.0 / n[0], 1.0 / n[1], 1.0 / n[2]]));
    };
    Ok(mul(&XYZ_D50_TO_SRGB, &to_xyz_d50))
}

fn srgb_encode(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Raw sensor samples of the chosen IFD, `spp` interleaved per pixel.
struct Raw {
    w: usize,
    h: usize,
    spp: usize,
    data: Vec<u16>,
}

fn unpack(
    bytes: &[u8],
    le: bool,
    bps: u32,
    row_samples: usize,
    rows: usize,
    out: &mut Vec<u16>,
) -> Result<()> {
    let row_bytes = (row_samples * bps as usize).div_ceil(8);
    for r in 0..rows {
        let row = bytes
            .get(r * row_bytes..(r + 1) * row_bytes)
            .ok_or_else(|| Error::truncated("DNG image data"))?;
        match bps {
            8 => out.extend(row.iter().map(|&b| u16::from(b))),
            16 => out.extend(row.chunks_exact(2).map(|b| {
                if le {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })),
            _ => {
                let (mut acc, mut n) = (0u32, 0u32);
                let mut it = row.iter();
                for _ in 0..row_samples {
                    while n < bps {
                        acc = (acc << 8) | u32::from(*it.next().unwrap_or(&0));
                        n += 8;
                    }
                    n -= bps;
                    out.push(((acc >> n) & ((1 << bps) - 1)) as u16);
                    acc &= (1 << n) - 1;
                }
            }
        }
    }
    Ok(())
}

fn read_raw(t: Tiff, ifd: &Ifd, limits: &Limits, progress: &dyn Progress) -> Result<Raw> {
    let w = ifd.uint(256, 0) as usize;
    let h = ifd.uint(257, 0) as usize;
    if w == 0 || h == 0 {
        return Err(Error::corrupt("DNG raw image has no dimensions"));
    }
    limits.check_pixels(w as u32, h as u32)?;
    let spp = ifd.uint(277, 1) as usize;
    let bps = ifd.uint(258, 16);
    if !(1..=4).contains(&spp) || !(2..=16).contains(&bps) {
        return Err(unsupported("this sample layout"));
    }
    if ifd.uint(339, 1) != 1 {
        return Err(unsupported("signed or floating point samples"));
    }
    if ifd.uint(284, 1) != 1 && spp > 1 {
        return Err(unsupported("planar raw data"));
    }
    limits.check_alloc((w * h * spp * 2) as u64)?;
    let compression = ifd.uint(259, 1);
    let predictor = ifd.uint(317, 1);
    match compression {
        1 | 7 => {}
        8 | 32946 if matches!(predictor, 1 | 2) && matches!(bps, 8 | 16) => {}
        8 | 32946 => return Err(unsupported("Deflate with this predictor or bit depth")),
        34892 => return Err(unsupported("lossy JPEG raw data")),
        52546 => return Err(unsupported("JPEG XL raw data")),
        _ => return Err(unsupported("this raw compression")),
    }
    // Chunks: tiles or strips, each with its position and size.
    let tiled = ifd.fields.contains_key(&322);
    let (offsets, counts, cw, ch, across) = if tiled {
        let (tw, th) = (ifd.uint(322, 0) as usize, ifd.uint(323, 0) as usize);
        if tw == 0 || th == 0 {
            return Err(Error::corrupt("DNG tile size is zero"));
        }
        (ifd.nums(324), ifd.nums(325), tw, th, w.div_ceil(tw))
    } else {
        let rps = (ifd.uint(278, h as u32) as usize).clamp(1, h);
        (ifd.nums(273), ifd.nums(279), w, rps, 1)
    };
    if offsets.is_empty() || offsets.len() != counts.len() {
        return Err(Error::corrupt("DNG has no image data"));
    }
    let mut data = vec![0u16; w * h * spp];
    for (i, (&off, &len)) in offsets.iter().zip(&counts).enumerate() {
        if i % 8 == 0 {
            progress.check_cancelled()?;
        }
        let (x0, y0) = ((i % across) * cw, (i / across) * ch);
        if x0 >= w || y0 >= h {
            break;
        }
        let raw =
            t.d.get(off as usize..(off as usize).saturating_add(len as usize))
                .ok_or_else(|| Error::truncated("DNG image data"))?;
        let rows = if tiled { ch } else { ch.min(h - y0) };
        let mut chunk: Vec<u16> = Vec::with_capacity(cw * rows * spp);
        match compression {
            7 => {
                let s = jpeg::decode_lossless_samples(raw, limits, progress)?;
                if s.height < rows || s.width * s.components < cw * spp {
                    return Err(Error::corrupt("DNG JPEG tile is smaller than the tile"));
                }
                for r in 0..rows {
                    let row = &s.samples[r * s.width * s.components..];
                    chunk.extend_from_slice(&row[..cw * spp]);
                }
            }
            1 => unpack(raw, t.le, bps, cw * spp, rows, &mut chunk)?,
            _ => {
                let mut plain = zlib::decompress(raw, limits, progress)?;
                if predictor == 2 {
                    let row_bytes = cw * spp * (bps as usize / 8);
                    for row in plain.chunks_mut(row_bytes.max(1)) {
                        if bps == 8 {
                            for k in spp..row.len() {
                                row[k] = row[k].wrapping_add(row[k - spp]);
                            }
                        } else {
                            let mut prev = vec![0u16; spp];
                            for (k, c) in row.chunks_exact_mut(2).enumerate() {
                                let v = if t.le {
                                    u16::from_le_bytes([c[0], c[1]])
                                } else {
                                    u16::from_be_bytes([c[0], c[1]])
                                };
                                let v = v.wrapping_add(prev[k % spp]);
                                prev[k % spp] = v;
                                let b = if t.le {
                                    v.to_le_bytes()
                                } else {
                                    v.to_be_bytes()
                                };
                                c.copy_from_slice(&b);
                            }
                        }
                    }
                }
                unpack(&plain, t.le, bps, cw * spp, rows, &mut chunk)?;
            }
        }
        for r in 0..rows.min(h - y0) {
            let n = (cw.min(w - x0)) * spp;
            let src = &chunk[r * cw * spp..r * cw * spp + n];
            let dst = ((y0 + r) * w + x0) * spp;
            data[dst..dst + n].copy_from_slice(src);
        }
    }
    Ok(Raw { w, h, spp, data })
}

pub fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    let (t, off) =
        header(data).ok_or_else(|| Error::new(ErrorCode::UnsupportedFormat, "not a DNG file"))?;
    let i0 = read_ifd(t, off)?;
    if !i0.fields.contains_key(&DNG_VERSION) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a DNG file"));
    }
    // The raw IFD: IFD 0, a SubIFD of it or a later directory, the largest full-resolution one.
    let mut cands: Vec<Ifd> = Vec::new();
    let mut next = i0.next;
    let subs = i0.nums(330);
    let mut seen = vec![off];
    let first = read_ifd(t, off)?;
    cands.push(first);
    while next != 0 && cands.len() < 8 && !seen.contains(&next) {
        seen.push(next);
        match read_ifd(t, next) {
            Ok(i) => {
                let n = i.next;
                cands.push(i);
                next = n;
            }
            Err(_) => break,
        }
    }
    for s in subs {
        if let Ok(i) = read_ifd(t, s as usize) {
            cands.push(i);
        }
    }
    let raw_ifd = cands
        .iter()
        .filter(|i| matches!(i.uint(262, 0), 32803 | 34892) && i.uint(254, 0) & 1 == 0)
        .max_by_key(|i| u64::from(i.uint(256, 0)) * u64::from(i.uint(257, 0)))
        .ok_or_else(|| unsupported("a file without a raw image"))?;
    let cfa = raw_ifd.uint(262, 0) == 32803;
    let raw = read_raw(t, raw_ifd, limits, progress)?;
    let (w, h, spp) = (raw.w, raw.h, raw.spp);
    if cfa && spp != 1 || !cfa && spp != 3 {
        return Err(unsupported("this number of colour planes"));
    }
    let planes = raw_ifd.nums(50710);
    if !planes.is_empty() && planes != [0.0, 1.0, 2.0] {
        return Err(unsupported(
            "cameras with colour planes other than red, green and blue",
        ));
    }
    // The CFA pattern.
    let (pr, pc, pattern) = if cfa {
        let dim = raw_ifd.nums(33421);
        let pat = raw_ifd.nums(33422);
        let (pr, pc) = (
            dim.first().copied().unwrap_or(2.0) as usize,
            dim.get(1).copied().unwrap_or(2.0) as usize,
        );
        if pr == 0 || pc == 0 || pat.len() < pr * pc || pr > 16 || pc > 16 {
            return Err(Error::corrupt("DNG colour filter pattern is invalid"));
        }
        if pat.iter().any(|&c| c > 2.0) {
            return Err(unsupported("colour filters other than red, green and blue"));
        }
        (pr, pc, pat.iter().map(|&c| c as usize).collect::<Vec<_>>())
    } else {
        (1, 1, vec![0])
    };

    // Linearise, subtract black, scale to 0..1.
    let lin: Vec<f64> = raw_ifd.nums(50712);
    let bdim = raw_ifd.nums(50713);
    let (br, bc) = (
        bdim.first().copied().unwrap_or(1.0).max(1.0) as usize,
        bdim.get(1).copied().unwrap_or(1.0).max(1.0) as usize,
    );
    let mut black = raw_ifd.nums(50714);
    if black.len() < br * bc * spp {
        black.resize(br * bc * spp, black.first().copied().unwrap_or(0.0));
    }
    let delta_h = raw_ifd.nums(50715);
    let delta_v = raw_ifd.nums(50716);
    let bps = raw_ifd.uint(258, 16);
    let mut white = raw_ifd.nums(50717);
    if white.is_empty() {
        white = vec![f64::from((1u32 << bps) - 1)];
    }
    let area = raw_ifd.nums(50829);
    let (at, al, ab_, ar) = if area.len() == 4 {
        (
            area[0] as usize,
            area[1] as usize,
            area[2] as usize,
            area[3] as usize,
        )
    } else {
        (0, 0, h, w)
    };
    if at >= ab_ || al >= ar || ab_ > h || ar > w {
        return Err(Error::corrupt("DNG active area is outside the image"));
    }
    let lin_at = |v: u16| -> f64 {
        if lin.is_empty() {
            f64::from(v)
        } else {
            lin[usize::from(v).min(lin.len() - 1)]
        }
    };
    let mut max_black = vec![0.0f64; spp];
    for (i, b) in black.iter().enumerate() {
        let s = i % spp;
        max_black[s] = max_black[s].max(*b);
    }
    let mut norm = vec![0.0f32; w * h * spp];
    for y in 0..h {
        for x in 0..w {
            for s in 0..spp {
                let r = if y >= at { (y - at) % br } else { 0 };
                let c = if x >= al { (x - al) % bc } else { 0 };
                let mut b = black[(r * bc + c) * spp + s];
                b += delta_h.get(x.wrapping_sub(al)).copied().unwrap_or(0.0)
                    + delta_v.get(y.wrapping_sub(at)).copied().unwrap_or(0.0);
                let wl = white.get(s).copied().unwrap_or(white[0]);
                let v =
                    (lin_at(raw.data[(y * w + x) * spp + s]) - b) / (wl - max_black[s]).max(1.0);
                norm[(y * w + x) * spp + s] = v.clamp(0.0, 1.0) as f32;
            }
        }
    }
    drop(raw);

    let pat_at =
        |y: usize, x: usize| pattern[((y + pr - at % pr) % pr) * pc + (x + pc - al % pc) % pc];

    // Demosaic into three planes (bilinear: the mean of the nearest samples of each colour).
    let rgb: Vec<f32> = if cfa {
        limits.check_alloc((w * h * 12) as u64)?;
        let mut out = vec![0.0f32; w * h * 3];
        for y in 0..h {
            if y % 64 == 0 {
                progress.check_cancelled()?;
            }
            for x in 0..w {
                let own = pat_at(y, x);
                for ch in 0..3 {
                    out[(y * w + x) * 3 + ch] = if ch == own {
                        norm[y * w + x]
                    } else {
                        let (mut sum, mut cnt) = (0.0f32, 0u32);
                        for rad in 1..=3usize {
                            for yy in y.saturating_sub(rad)..=(y + rad).min(h - 1) {
                                for xx in x.saturating_sub(rad)..=(x + rad).min(w - 1) {
                                    if pat_at(yy, xx) == ch {
                                        sum += norm[yy * w + xx];
                                        cnt += 1;
                                    }
                                }
                            }
                            if cnt > 0 {
                                break;
                            }
                        }
                        if cnt > 0 {
                            sum / cnt as f32
                        } else {
                            0.0
                        }
                    };
                }
            }
        }
        out
    } else {
        norm
    };

    // Colour: camera RGB to linear sRGB.
    let neutral = {
        let v = i0.nums(50728);
        (v.len() == 3).then(|| [v[0], v[1], v[2]])
    };
    let wxy = {
        let v = i0.nums(50729);
        (v.len() == 2).then(|| (v[0], v[1]))
    };
    let m = camera_to_srgb(&i0, raw_ifd, neutral, wxy, &mut notes)?;
    let exposure = 2f64.powf(i0.num(50730).unwrap_or(0.0).clamp(-4.0, 4.0));

    // Crop to the default crop of the active area.
    let origin = i0.nums(50719);
    let origin = if origin.len() == 2 {
        origin
    } else {
        raw_ifd.nums(50719)
    };
    let size = {
        let s = raw_ifd.nums(50720);
        if s.len() == 2 {
            s
        } else {
            i0.nums(50720)
        }
    };
    let (aw, ah) = (ar - al, ab_ - at);
    let (cx, cy) = (
        origin.first().copied().unwrap_or(0.0) as usize,
        origin.get(1).copied().unwrap_or(0.0) as usize,
    );
    let (cw, chh) = (
        size.first().map_or(aw, |v| *v as usize),
        size.get(1).map_or(ah, |v| *v as usize),
    );
    let (cw, chh) = (
        cw.min(aw.saturating_sub(cx)),
        chh.min(ah.saturating_sub(cy)),
    );
    if cw == 0 || chh == 0 {
        return Err(Error::corrupt("DNG crop is empty"));
    }
    let (x0, y0) = (al + cx, at + cy);

    let mut out: Vec<u16> = Vec::with_capacity(cw * chh * 3);
    for y in 0..chh {
        for x in 0..cw {
            let p = ((y0 + y) * w + x0 + x) * 3;
            let cam = [
                f64::from(rgb[p]),
                f64::from(rgb[p + 1]),
                f64::from(rgb[p + 2]),
            ];
            for v in apply(&m, cam) {
                out.push((srgb_encode((v * exposure).clamp(0.0, 1.0)) * 65535.0 + 0.5) as u16);
            }
        }
    }

    // Orientation.
    let orientation = i0.uint(274, 1);
    let (mut ow, mut oh) = (cw, chh);
    if matches!(orientation, 2..=8) {
        let swap = orientation >= 5;
        (ow, oh) = if swap { (chh, cw) } else { (cw, chh) };
        let mut o = vec![0u16; out.len()];
        for y in 0..chh {
            for x in 0..cw {
                let (dx, dy) = match orientation {
                    2 => (cw - 1 - x, y),
                    3 => (cw - 1 - x, chh - 1 - y),
                    4 => (x, chh - 1 - y),
                    5 => (y, x),
                    6 => (chh - 1 - y, x),
                    7 => (chh - 1 - y, cw - 1 - x),
                    _ => (y, cw - 1 - x),
                };
                let s = (y * cw + x) * 3;
                let d = (dy * ow + dx) * 3;
                o[d..d + 3].copy_from_slice(&out[s..s + 3]);
            }
        }
        out = o;
    }
    notes.push(Note::new(
        Severity::Info,
        "dng.rendered",
        "the raw sensor data was rendered with bilinear demosaicing and a plain sRGB curve; camera tone curves, profiles and lens corrections were not applied",
    ));
    let mut bytes = Vec::with_capacity(out.len() * 2);
    for v in out {
        bytes.extend_from_slice(&v.to_be_bytes());
    }
    let img = Image::new(ow as u32, oh as u32, Layout::Rgb, Depth::Sixteen, bytes)?;
    Ok((img, notes))
}
