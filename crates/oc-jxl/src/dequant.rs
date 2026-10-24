//! Varblock transform types and the dequantisation matrices of a VarDCT frame.
//!
//! The default band tables and the way matrices are built from them follow jxl-oxide
//! (MIT/Apache-2.0), which implements ISO/IEC 18181-1 section I.2.
use crate::bits::{bad, Br};
use crate::modular::{Chan, MaTree, Stream};
use oc_core::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Ty {
    Dct8,
    Hornuss,
    Dct2,
    Dct4,
    Dct16,
    Dct32,
    Dct16x8,
    Dct8x16,
    Dct32x8,
    Dct8x32,
    Dct32x16,
    Dct16x32,
    Dct4x8,
    Dct8x4,
    Afv0,
    Afv1,
    Afv2,
    Afv3,
    Dct64,
    Dct64x32,
    Dct32x64,
    Dct128,
    Dct128x64,
    Dct64x128,
    Dct256,
    Dct256x128,
    Dct128x256,
}

use Ty::*;

const ALL: [Ty; 27] = [
    Dct8, Hornuss, Dct2, Dct4, Dct16, Dct32, Dct16x8, Dct8x16, Dct32x8, Dct8x32, Dct32x16,
    Dct16x32, Dct4x8, Dct8x4, Afv0, Afv1, Afv2, Afv3, Dct64, Dct64x32, Dct32x64, Dct128, Dct128x64,
    Dct64x128, Dct256, Dct256x128, Dct128x256,
];

impl Ty {
    pub fn from_u8(v: u8) -> Result<Self> {
        ALL.get(usize::from(v))
            .copied()
            .ok_or_else(|| bad("invalid varblock transform"))
    }

    /// Size in 8x8 blocks: `(wide, high)`.
    pub fn blocks(self) -> (usize, usize) {
        match self {
            Dct8 | Hornuss | Dct2 | Dct4 | Dct4x8 | Dct8x4 | Afv0 | Afv1 | Afv2 | Afv3 => (1, 1),
            Dct16 => (2, 2),
            Dct32 => (4, 4),
            Dct16x8 => (1, 2),
            Dct8x16 => (2, 1),
            Dct32x8 => (1, 4),
            Dct8x32 => (4, 1),
            Dct32x16 => (2, 4),
            Dct16x32 => (4, 2),
            Dct64 => (8, 8),
            Dct64x32 => (4, 8),
            Dct32x64 => (8, 4),
            Dct128 => (16, 16),
            Dct128x64 => (8, 16),
            Dct64x128 => (16, 8),
            Dct256 => (32, 32),
            Dct256x128 => (16, 32),
            Dct128x256 => (32, 16),
        }
    }

    /// Index of the matrix (and of its parameters) among the 17 distinct ones.
    pub fn matrix_index(self) -> usize {
        match self {
            Dct8 => 0,
            Hornuss => 1,
            Dct2 => 2,
            Dct4 => 3,
            Dct16 => 4,
            Dct32 => 5,
            Dct16x8 | Dct8x16 => 6,
            Dct32x8 | Dct8x32 => 7,
            Dct32x16 | Dct16x32 => 8,
            Dct4x8 | Dct8x4 => 9,
            Afv0 | Afv1 | Afv2 | Afv3 => 10,
            Dct64 => 11,
            Dct64x32 | Dct32x64 => 12,
            Dct128 => 13,
            Dct128x64 | Dct64x128 => 14,
            Dct256 => 15,
            Dct256x128 | Dct128x256 => 16,
        }
    }

    /// Which coefficient order the transform uses.
    pub fn order_id(self) -> usize {
        match self {
            Dct8 => 0,
            Hornuss | Dct2 | Dct4 | Dct4x8 | Dct8x4 | Afv0 | Afv1 | Afv2 | Afv3 => 1,
            Dct16 => 2,
            Dct32 => 3,
            Dct16x8 | Dct8x16 => 4,
            Dct32x8 | Dct8x32 => 5,
            Dct32x16 | Dct16x32 => 6,
            Dct64 => 7,
            Dct64x32 | Dct32x64 => 8,
            Dct128 => 9,
            Dct128x64 | Dct64x128 => 10,
            Dct256 => 11,
            Dct256x128 | Dct128x256 => 12,
        }
    }

    /// Whether the coefficient coordinates of the order are swapped on placement.
    pub fn transposed(self) -> bool {
        if matches!(
            self,
            Hornuss | Dct2 | Dct4 | Dct4x8 | Dct8x4 | Afv0 | Afv1 | Afv2 | Afv3
        ) {
            false
        } else {
            let (w, h) = self.blocks();
            h >= w
        }
    }

    /// `(width, height)` of the stored matrix.
    fn matrix_size(self) -> (usize, usize) {
        let (w, h) = self.blocks();
        let (w, h) = (w * 8, h * 8);
        // the matrix is always stored wide
        (w.max(h), w.min(h))
    }
}

/// How one matrix is described in the bitstream.
enum Enc {
    Hornuss([[f32; 3]; 3]),
    Dct2([[f32; 6]; 3]),
    Dct4 {
        p: [[f32; 2]; 3],
        dct: [Vec<f32>; 3],
    },
    Dct4x8 {
        p: [[f32; 1]; 3],
        dct: [Vec<f32>; 3],
    },
    Afv {
        p: [[f32; 9]; 3],
        dct: [Vec<f32>; 3],
        dct4x4: [Vec<f32>; 3],
    },
    Dct([Vec<f32>; 3]),
    Raw {
        denominator: f32,
        planes: [Vec<i32>; 3],
    },
}

const SEQ_A: [f32; 7] = [
    -1.025,
    -0.78,
    -0.65012,
    -0.19041574,
    -0.20819396,
    -0.421064,
    -0.32733846,
];
const SEQ_B: [f32; 7] = [
    -0.30419582,
    -0.36330363,
    -0.3566038,
    -0.34430745,
    -0.33699593,
    -0.30180866,
    -0.27321684,
];
const SEQ_C: [f32; 7] = [-1.2, -1.2, -0.8, -0.7, -0.7, -0.4, -0.5];
const DCT4X8_PARAMS: [[f32; 4]; 3] = [
    [2198.0505, -0.96269625, -0.7619425, -0.65511405],
    [764.36554, -0.926302, -0.967523, -0.2784529],
    [527.10754, -1.4594386, -1.4500821, -1.5843723],
];
const DCT4_PARAMS: [[f32; 4]; 3] = [
    [2200.0, 0.0, 0.0, 0.0],
    [392.0, 0.0, 0.0, 0.0],
    [112.0, -0.25, -0.25, -0.5],
];

fn common_seq(a: f32, b: f32, c: f32) -> Enc {
    let mk = |first: f32, rest: &[f32; 7]| {
        let mut v = vec![first];
        v.extend_from_slice(rest);
        v
    };
    Enc::Dct([mk(a, &SEQ_A), mk(b, &SEQ_B), mk(c, &SEQ_C)])
}

fn dct_rows(rows: [&[f32]; 3]) -> Enc {
    Enc::Dct(rows.map(<[f32]>::to_vec))
}

fn default_enc(ty: Ty) -> Enc {
    match ty {
        Dct8 => dct_rows([
            &[3150.0, 0.0, -0.4, -0.4, -0.4, -2.0],
            &[560.0, 0.0, -0.3, -0.3, -0.3, -0.3],
            &[512.0, -2.0, -1.0, 0.0, -1.0, -2.0],
        ]),
        Hornuss => Enc::Hornuss([
            [280.0, 3160.0, 3160.0],
            [60.0, 864.0, 864.0],
            [18.0, 200.0, 200.0],
        ]),
        Dct2 => Enc::Dct2([
            [3840.0, 2560.0, 1280.0, 640.0, 480.0, 300.0],
            [960.0, 640.0, 320.0, 180.0, 140.0, 120.0],
            [640.0, 320.0, 128.0, 64.0, 32.0, 16.0],
        ]),
        Dct4 => Enc::Dct4 {
            p: [[1.0; 2]; 3],
            dct: DCT4_PARAMS.map(|v| v.to_vec()),
        },
        Dct16 => dct_rows([
            &[
                8996.873,
                -1.3000778,
                -0.4942453,
                -0.43909377,
                -0.6350102,
                -0.9017726,
                -1.6162099,
            ],
            &[
                3191.4836,
                -0.67424583,
                -0.80745816,
                -0.4492584,
                -0.3586544,
                -0.3132239,
                -0.37615025,
            ],
            &[
                1157.504, -2.0531423, -1.4, -0.5068713, -0.4270873, -1.4856834, -4.920914,
            ],
        ]),
        Dct32 => dct_rows([
            &[
                15718.408,
                -1.025,
                -0.98,
                -0.9012,
                -0.4,
                -0.48819396,
                -0.421064,
                -0.27,
            ],
            &[
                7305.7637,
                -0.8041958,
                -0.76330364,
                -0.5566038,
                -0.49785304,
                -0.43699592,
                -0.40180868,
                -0.27321684,
            ],
            &[
                3803.5317,
                -3.0607336,
                -2.041327,
                -2.023565,
                -0.54953897,
                -0.4,
                -0.4,
                -0.3,
            ],
        ]),
        Dct8x16 | Dct16x8 => dct_rows([
            &[7240.7734, -0.7, -0.7, -0.2, -0.2, -0.2, -0.5],
            &[1448.1547, -0.5, -0.5, -0.5, -0.2, -0.2, -0.2],
            &[506.85413, -1.4, -0.2, -0.5, -0.5, -1.5, -3.6],
        ]),
        Dct8x32 | Dct32x8 => dct_rows([
            &[
                16283.249, -1.7812846, -1.6309059, -1.0382179, -0.85, -0.7, -0.9, -1.2360638,
            ],
            &[
                5089.1577, -0.3200494, -0.3536285, -0.3034, -0.61, -0.5, -0.5, -0.6,
            ],
            &[
                3397.7761,
                -0.32132736,
                -0.3450762,
                -0.7034,
                -0.9,
                -1.0,
                -1.0,
                -1.1754606,
            ],
        ]),
        Dct16x32 | Dct32x16 => dct_rows([
            &[
                13844.971, -0.971138, -0.658, -0.42026, -0.22712, -0.2206, -0.226, -0.6,
            ],
            &[
                4798.964,
                -0.6112531,
                -0.8377079,
                -0.7901486,
                -0.26927274,
                -0.38272768,
                -0.22924222,
                -0.20719099,
            ],
            &[1807.2369, -1.2, -1.2, -0.7, -0.7, -0.7, -0.4, -0.5],
        ]),
        Dct4x8 | Dct8x4 => Enc::Dct4x8 {
            p: [[1.0]; 3],
            dct: DCT4X8_PARAMS.map(|v| v.to_vec()),
        },
        Afv0 | Afv1 | Afv2 | Afv3 => Enc::Afv {
            p: [
                [3072.0, 3072.0, 256.0, 256.0, 256.0, 414.0, 0.0, 0.0, 0.0],
                [1024.0, 1024.0, 50.0, 50.0, 50.0, 58.0, 0.0, 0.0, 0.0],
                [384.0, 384.0, 12.0, 12.0, 12.0, 22.0, -0.25, -0.25, -0.25],
            ],
            dct: DCT4X8_PARAMS.map(|v| v.to_vec()),
            dct4x4: DCT4_PARAMS.map(|v| v.to_vec()),
        },
        Dct64 => common_seq(23966.166, 8380.191, 4493.024),
        Dct32x64 | Dct64x32 => common_seq(15358.898, 5597.3604, 2919.9617),
        Dct128 => common_seq(47932.332, 16760.383, 8986.048),
        Dct64x128 | Dct128x64 => common_seq(30717.797, 11194.721, 5839.9233),
        Dct256 => common_seq(95864.664, 33520.766, 17972.096),
        Dct128x256 | Dct256x128 => common_seq(61435.594, 24209.441, 12979.847),
    }
}

fn interpolate(pos: f32, max: f32, bands: &[f32]) -> f32 {
    if let [v] = bands {
        return *v;
    }
    let scaled = pos * (bands.len() - 1) as f32 / max;
    let i = (scaled as usize).min(bands.len() - 2);
    let frac = scaled - i as f32;
    bands[i] * (bands[i + 1] / bands[i]).powf(frac)
}

fn mult(x: f32) -> f32 {
    if x > 0.0 {
        1.0 + x
    } else {
        1.0 / (1.0 - x)
    }
}

fn dct_weights(params: &[f32], width: usize, height: usize) -> Result<Vec<f32>> {
    let mut bands = vec![params[0]];
    for &v in &params[1..] {
        let band = bands[bands.len() - 1] * mult(v);
        if band <= 0.0 {
            return Err(bad("dequantisation matrix band is not positive"));
        }
        bands.push(band);
    }
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 / (width - 1) as f32;
            let dy = y as f32 / (height - 1) as f32;
            let d = (dx * dx + dy * dy).sqrt();
            out.push(interpolate(d, std::f32::consts::SQRT_2 + 1e-6, &bands));
        }
    }
    Ok(out)
}

fn build(ty: Ty, enc: Enc) -> Result<[Vec<f32>; 3]> {
    let recip = !matches!(enc, Enc::Raw { .. });
    let (width, height) = ty.matrix_size();
    let mut w: [Vec<f32>; 3] = match enc {
        Enc::Dct(p) => [
            dct_weights(&p[0], width, height)?,
            dct_weights(&p[1], width, height)?,
            dct_weights(&p[2], width, height)?,
        ],
        Enc::Hornuss(p) => p.map(|p| {
            let mut r = vec![p[0]; 64];
            r[0] = 1.0;
            r[1] = p[1];
            r[8] = p[1];
            r[9] = p[2];
            r
        }),
        Enc::Dct2(p) => p.map(|p| {
            let mut r = vec![0f32; 64];
            r[0] = 1.0;
            for (idx, &val) in p.iter().enumerate() {
                let dim = 1usize << (idx / 2);
                if idx % 2 == 0 {
                    for y in 0..dim {
                        for x in dim..dim * 2 {
                            r[y * 8 + x] = val;
                            r[x * 8 + y] = val;
                        }
                    }
                } else {
                    for y in dim..dim * 2 {
                        for x in dim..dim * 2 {
                            r[y * 8 + x] = val;
                        }
                    }
                }
            }
            r
        }),
        Enc::Dct4 { p, dct } => {
            let mut out: [Vec<f32>; 3] = Default::default();
            for c in 0..3 {
                let mat = dct_weights(&dct[c], 4, 4)?;
                let mut r = vec![0f32; 64];
                for y in 0..4 {
                    for x in 0..4 {
                        let v = mat[y * 4 + x];
                        r[y * 16 + x * 2] = v;
                        r[y * 16 + x * 2 + 1] = v;
                        r[(y * 2 + 1) * 8 + x * 2] = v;
                        r[(y * 2 + 1) * 8 + x * 2 + 1] = v;
                    }
                }
                r[1] /= p[c][0];
                r[8] /= p[c][0];
                r[9] /= p[c][1];
                out[c] = r;
            }
            out
        }
        Enc::Dct4x8 { p, dct } => {
            let mut out: [Vec<f32>; 3] = Default::default();
            for c in 0..3 {
                let mat = dct_weights(&dct[c], 8, 4)?;
                let mut r: Vec<f32> = mat
                    .chunks_exact(8)
                    .flat_map(|row| [row, row])
                    .flatten()
                    .copied()
                    .collect();
                r[8] /= p[c][0];
                out[c] = r;
            }
            out
        }
        Enc::Afv { p, dct, dct4x4 } => {
            const FREQS: [f32; 16] = [
                0.0, 0.0, 0.8517779, 5.3777843, 0.0, 0.0, 4.734748, 5.4492455, 1.659827, 4.0,
                7.275749, 10.423227, 2.6629324, 7.6306577, 8.962389, 12.971662,
            ];
            let (lo, hi) = (FREQS[2], FREQS[15]);
            let mut out: [Vec<f32>; 3] = Default::default();
            for c in 0..3 {
                let w48 = dct_weights(&dct[c], 8, 4)?;
                let w44 = dct_weights(&dct4x4[c], 4, 4)?;
                let mut bands = [p[c][5], 0.0, 0.0, 0.0];
                for i in 1..4 {
                    bands[i] = bands[i - 1] * mult(p[c][5 + i]);
                }
                let mut r = vec![0f32; 64];
                for y in 0..4 {
                    for x in 0..4 {
                        r[16 * y + 2 * x] = match (x, y) {
                            (0, 0) => 1.0,
                            (0, 1) => p[c][2],
                            (1, 0) => p[c][3],
                            (1, 1) => p[c][4],
                            _ => interpolate(FREQS[y * 4 + x] - lo, hi - lo + 1e-6, &bands),
                        };
                    }
                }
                for y in 0..4 {
                    for x in 0..8 {
                        r[16 * y + 8 + x] = if y == 0 && x == 0 {
                            p[c][0]
                        } else {
                            w48[y * 8 + x]
                        };
                    }
                    for x in 0..4 {
                        r[16 * y + 2 * x + 1] = if y == 0 && x == 0 {
                            p[c][1]
                        } else {
                            w44[y * 4 + x]
                        };
                    }
                }
                out[c] = r;
            }
            out
        }
        Enc::Raw {
            denominator,
            planes,
        } => planes.map(|pl| pl.iter().map(|&v| v as f32 * denominator).collect()),
    };
    if recip {
        for v in w.iter_mut().flatten() {
            *v = 1.0 / *v;
        }
    }
    for v in w.iter().flatten() {
        if *v >= 1e8 || *v <= 0.0 || v.is_nan() {
            return Err(bad("dequantisation matrix has an out-of-range element"));
        }
    }
    for pl in &w {
        if pl.len() != width * height {
            return Err(bad("dequantisation matrix has the wrong size"));
        }
    }
    Ok(w)
}

/// The 17 matrices, each for the three channels, in raster order for the wide orientation and
/// again transposed (for blocks taller than wide).
pub struct Matrices {
    plain: Vec<[Vec<f32>; 3]>,
    transposed: Vec<[Vec<f32>; 3]>,
}

const LIST: [Ty; 17] = [
    Dct8, Hornuss, Dct2, Dct4, Dct16, Dct32, Dct8x16, Dct8x32, Dct16x32, Dct4x8, Afv0, Dct64,
    Dct32x64, Dct128, Dct64x128, Dct256, Dct128x256,
];

fn read_fixed<const N: usize>(br: &mut Br) -> Result<[[f32; N]; 3]> {
    let mut out = [[0f32; N]; 3];
    for v in out.iter_mut().flatten() {
        *v = br.f16()?;
    }
    Ok(out)
}

fn read_dct_params(br: &mut Br) -> Result<[Vec<f32>; 3]> {
    let n = br.read(4)? as usize + 1;
    let mut p = [vec![0f32; n], vec![0f32; n], vec![0f32; n]];
    for v in p.iter_mut().flatten() {
        *v = br.f16()?;
    }
    for pl in &mut p {
        pl[0] *= 64.0;
    }
    Ok(p)
}

impl Matrices {
    pub fn defaults() -> Result<Self> {
        Self::from_encodings(LIST.iter().map(|&t| default_enc(t)).collect())
    }

    /// Read the matrix set of an HfGlobal section. `first_stream` is the stream index of the
    /// first matrix that is stored as a Modular image.
    pub fn parse(
        br: &mut Br,
        tree: Option<&MaTree>,
        first_stream: u32,
        bit_depth: u32,
        max_samples: u64,
    ) -> Result<Self> {
        if br.bool()? {
            return Self::defaults();
        }
        let mut encs = Vec::with_capacity(17);
        for (i, &ty) in LIST.iter().enumerate() {
            let mode = br.read(3)?;
            if (1..=5).contains(&mode) && !matches!(ty.matrix_index(), 0 | 1 | 2 | 3 | 9 | 10) {
                return Err(bad(
                    "invalid dequantisation encoding mode for this transform",
                ));
            }
            encs.push(match mode {
                0 => default_enc(ty),
                1 => Enc::Hornuss(read_fixed(br)?),
                2 => Enc::Dct2(read_fixed(br)?),
                3 => Enc::Dct4 {
                    p: read_fixed(br)?,
                    dct: read_dct_params(br)?,
                },
                4 => Enc::Dct4x8 {
                    p: read_fixed(br)?,
                    dct: read_dct_params(br)?,
                },
                5 => {
                    let mut p = read_fixed::<9>(br)?;
                    for q in &mut p {
                        for v in &mut q[..6] {
                            *v *= 64.0;
                        }
                    }
                    Enc::Afv {
                        p,
                        dct: read_dct_params(br)?,
                        dct4x4: read_dct_params(br)?,
                    }
                }
                6 => Enc::Dct(read_dct_params(br)?),
                7 => {
                    let (w, h) = ty.matrix_size();
                    let denominator = br.f16()?;
                    let input = (0..3).map(|_| Chan::info(w, h, 0, 0)).collect();
                    let mut s = Stream::parse(br, tree, input, max_samples)?;
                    s.decode(br, tree, 3, first_stream + i as u32)?;
                    let chans = s.finish(bit_depth)?;
                    if chans.len() != 3 {
                        return Err(bad("raw dequantisation matrix has the wrong channel count"));
                    }
                    let mut it = chans.into_iter().map(|c| c.d);
                    Enc::Raw {
                        denominator,
                        planes: [
                            it.next().unwrap_or_default(),
                            it.next().unwrap_or_default(),
                            it.next().unwrap_or_default(),
                        ],
                    }
                }
                _ => return Err(bad("invalid dequantisation encoding mode")),
            });
        }
        Self::from_encodings(encs)
    }

    fn from_encodings(encs: Vec<Enc>) -> Result<Self> {
        let mut plain = Vec::with_capacity(17);
        let mut transposed = Vec::with_capacity(17);
        for (enc, &ty) in encs.into_iter().zip(LIST.iter()) {
            let m = build(ty, enc)?;
            let (w, h) = ty.matrix_size();
            let tr = std::array::from_fn(|c| {
                let src = &m[c];
                let mut out = vec![0f32; src.len()];
                for (i, v) in out.iter_mut().enumerate() {
                    // the transposed matrix is `h` wide and `w` high
                    let (x, y) = (i % h, i / h);
                    *v = src[x * w + y];
                }
                out
            });
            plain.push(m);
            transposed.push(tr);
        }
        Ok(Self { plain, transposed })
    }

    /// The matrix for a block of this type, oriented like the block's coefficients.
    pub fn get(&self, channel: usize, ty: Ty) -> &[f32] {
        let i = ty.matrix_index();
        if ty.transposed() {
            &self.transposed[i][channel]
        } else {
            &self.plain[i][channel]
        }
    }
}
