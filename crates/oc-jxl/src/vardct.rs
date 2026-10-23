//! VarDCT frames: quantiser and block metadata, coefficient decoding, dequantisation and the
//! inverse transforms. The structure follows ISO/IEC 18181-1 with jxl-oxide (MIT/Apache-2.0) as the
//! reference for details the text leaves implicit; the code itself is written from scratch.
use crate::bits::{bad, unpack_signed, Br};
use crate::dct::{self, scale_f, Tables};
use crate::dequant::{Matrices, Ty};
use crate::entropy::{read_clusters, read_permutation, Decoder};
use crate::header::{FrameHeader, ImageHeader};
use crate::modular::{Chan, MaTree, Stream};
use oc_core::{Error, Limits, Result};

// ---------------------------------------------------------------------------------------------
// LfGlobal parts

pub struct BlockCtx {
    qf_thresholds: Vec<u32>,
    lf_thresholds: [Vec<i32>; 3],
    map: Vec<u8>,
    clusters: usize,
}

pub struct Global {
    pub global_scale: u32,
    pub quant_lf: u32,
    pub ctx: BlockCtx,
    colour_factor: u32,
    base_x: f32,
    base_b: f32,
    x_factor_lf: i32,
    b_factor_lf: i32,
}

impl Global {
    pub fn parse(br: &mut Br) -> Result<Self> {
        let global_scale = br.u32([(1, 11), (2049, 11), (4097, 12), (8193, 16)])?;
        let quant_lf = br.u32([(16, 0), (1, 5), (1, 8), (1, 16)])?;

        let ctx = if br.bool()? {
            BlockCtx {
                qf_thresholds: Vec::new(),
                lf_thresholds: [Vec::new(), Vec::new(), Vec::new()],
                map: vec![
                    0, 1, 2, 2, 3, 3, 4, 5, 6, 6, 6, 6, 6, 7, 8, 9, 9, 10, 11, 12, 13, 14, 14, 14, 14, 14, 7, 8, 9, 9, 10,
                    11, 12, 13, 14, 14, 14, 14, 14,
                ],
                clusters: 15,
            }
        } else {
            let mut lf_thresholds = [Vec::new(), Vec::new(), Vec::new()];
            let mut size = 1u32;
            for thr in &mut lf_thresholds {
                let n = br.read(4)?;
                size *= n + 1;
                for _ in 0..n {
                    let t = br.u32([(0, 4), (16, 8), (272, 16), (65808, 32)])?;
                    thr.push(unpack_signed(t));
                }
            }
            let mut qf_thresholds = Vec::new();
            let n = br.read(4)?;
            size *= n + 1;
            for _ in 0..n {
                let t = br.u32([(0, 2), (4, 3), (12, 5), (44, 8)])?;
                qf_thresholds.push(1 + t);
            }
            let (clusters, map) = read_clusters(br, size * 39)?;
            if clusters > 16 || size > 64 {
                return Err(bad("block context map is too large"));
            }
            BlockCtx { qf_thresholds, lf_thresholds, map, clusters: clusters as usize }
        };

        let (mut colour_factor, mut base_x, mut base_b, mut x_factor_lf, mut b_factor_lf) = (84, 0.0, 1.0, 128, 128);
        if !br.bool()? {
            colour_factor = br.u32([(84, 0), (256, 0), (2, 8), (258, 16)])?;
            base_x = br.f16()?;
            base_b = br.f16()?;
            x_factor_lf = br.read(8)? as i32;
            b_factor_lf = br.read(8)? as i32;
        }
        if std::env::var_os("JXL_DEBUG").is_some() {
            eprintln!("global_scale {global_scale} quant_lf {quant_lf} qf {:?} lf {:?} clusters {} corr {colour_factor} {base_x} {base_b} {x_factor_lf} {b_factor_lf}", ctx.qf_thresholds, ctx.lf_thresholds, ctx.clusters);
        }
        Ok(Self { global_scale, quant_lf, ctx, colour_factor, base_x, base_b, x_factor_lf, b_factor_lf })
    }
}

// ---------------------------------------------------------------------------------------------
// HfGlobal parts: coefficient orders and the coefficient distributions

const BLOCK_SIZES: [(usize, usize); 13] =
    [(8, 8), (8, 8), (16, 16), (32, 32), (16, 8), (32, 8), (32, 16), (64, 64), (64, 32), (128, 128), (128, 64), (256, 256), (256, 128)];

fn natural_order((bw, bh): (usize, usize)) -> Vec<(u16, u16)> {
    let y_scale = bw / bh;
    let (lbw, lbh) = (bw / 8, bh / 8);
    let mut out = Vec::with_capacity(bw * bh);
    for i in 0..lbw * lbh {
        out.push(((i % lbw) as u16, (i / lbw) as u16));
    }
    for dist in 1..2 * bw {
        let margin = dist.saturating_sub(bw);
        for order in margin..dist - margin {
            let (x, y) = if dist % 2 == 1 { (order, dist - 1 - order) } else { (dist - 1 - order, order) };
            if x < lbw && y < lbw {
                continue;
            }
            if y % y_scale != 0 {
                continue;
            }
            out.push((x as u16, (y / y_scale) as u16));
        }
    }
    out
}

pub struct HfPass {
    orders: Vec<[Vec<(u16, u16)>; 3]>,
    dist: Decoder,
}

impl HfPass {
    pub fn parse(br: &mut Br, global: &Global, presets: u32) -> Result<Self> {
        let mut used = br.u32([(0x5F, 0), (0x13, 0), (0, 0), (0, 13)])?;
        let mut orders: Vec<[Vec<(u16, u16)>; 3]> = (0..13).map(|_| Default::default()).collect();
        if used != 0 {
            let mut dec = Decoder::parse(br, 8)?;
            dec.begin(br)?;
            for (idx, &(bw, bh)) in BLOCK_SIZES.iter().enumerate() {
                if used & 1 != 0 {
                    let size = (bw * bh) as u32;
                    let nat = natural_order((bw, bh));
                    for ch in 0..3 {
                        let perm = read_permutation(br, &mut dec, size, size / 64)?;
                        orders[idx][ch] = perm.iter().map(|&i| nat[i]).collect();
                    }
                }
                used >>= 1;
            }
            dec.finalize()?;
        }
        let dist = Decoder::parse(br, 495 * presets * global.ctx.clusters as u32)?;
        Ok(Self { orders, dist })
    }
}

// ---------------------------------------------------------------------------------------------
// Per-frame state

#[derive(Clone, Copy)]
enum Blk {
    Empty,
    Occupied,
    Data { ty: Ty, mul: i32 },
}

pub struct State {
    /// Frame size in 8x8 blocks.
    bw: usize,
    bh: usize,
    lf_q: [Vec<i32>; 3],
    lf: [Vec<f32>; 3],
    blk: Vec<Blk>,
    tiles_w: usize,
    xfy: Vec<i32>,
    bfy: Vec<i32>,
    sigma: Vec<f32>,
    coef: [Vec<i32>; 3],
    naturals: Vec<Vec<(u16, u16)>>,
    tables: Tables,
}

const COEFF_FREQ_CONTEXT: [u32; 63] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22, 23, 23,
    23, 23, 24, 24, 24, 24, 25, 25, 25, 25, 26, 26, 26, 26, 27, 27, 27, 27, 28, 28, 28, 28, 29, 29, 29, 29, 30, 30, 30, 30,
];
const COEFF_NUM_NONZERO_CONTEXT: [u32; 63] = [
    0, 31, 62, 62, 93, 93, 93, 93, 123, 123, 123, 123, 152, 152, 152, 152, 152, 152, 152, 152, 180, 180, 180, 180, 180, 180, 180,
    180, 180, 180, 180, 180, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206,
    206, 206, 206, 206, 206, 206, 206, 206, 206, 206, 206,
];

fn ceil_log2(v: usize) -> u32 {
    v.next_power_of_two().trailing_zeros()
}

impl State {
    pub fn new(fh: &FrameHeader, limits: &Limits) -> Result<Self> {
        let (w, h) = (fh.color_w() as usize, fh.color_h() as usize);
        let (bw, bh) = (w.div_ceil(8), h.div_ceil(8));
        let padded = bw * 8 * bh * 8;
        // coefficients, dequantised coefficients, output planes, and the block grids
        let bytes = (padded as u64) * 4 * 9 + (bw * bh) as u64 * 64;
        if bytes > limits.max_alloc {
            return Err(Error::limit("max_alloc", "JPEG XL: frame is too large"));
        }
        let tiles_w = bw.div_ceil(8);
        let tiles_h = bh.div_ceil(8);
        Ok(Self {
            bw,
            bh,
            lf_q: std::array::from_fn(|_| vec![0; bw * bh]),
            lf: std::array::from_fn(|_| vec![0.0; bw * bh]),
            blk: vec![Blk::Empty; bw * bh],
            tiles_w,
            xfy: vec![0; tiles_w * tiles_h],
            bfy: vec![0; tiles_w * tiles_h],
            sigma: vec![0.0; bw * bh],
            coef: std::array::from_fn(|_| vec![0; padded]),
            naturals: BLOCK_SIZES.iter().map(|&s| natural_order(s)).collect(),
            tables: Tables::new(),
        })
    }

    /// Read the quantised LF image of LF group `g` (the LfCoeff bundle).
    #[allow(clippy::too_many_arguments)]
    pub fn read_lf_coeff(
        &mut self,
        br: &mut Br,
        fh: &FrameHeader,
        global: &Global,
        lf_raw: [f32; 3],
        tree: Option<&MaTree>,
        g: usize,
        bit_depth: u32,
        max_samples: u64,
    ) -> Result<()> {
        let gd = fh.group_dim() as usize;
        let (lw, lh) = fh.group_size(fh.lf_group_dim(), g as u32);
        let (gw, gh) = ((lw as usize).div_ceil(8), (lh as usize).div_ceil(8));
        let per_row = fh.lf_groups_per_row() as usize;
        let (ox, oy) = ((g % per_row) * gd, (g / per_row) * gd);
        let extra = br.read(2)?;
        let input = (0..3).map(|_| Chan::info(gw, gh, 0, 0)).collect();
        let mut s = Stream::parse(br, tree, input, max_samples)?;
        s.decode(br, tree, 3, 1 + g as u32)?;
        let out = s.finish(bit_depth)?;
        if out.len() != 3 || out.iter().any(|c| c.w != gw || c.h != gh) {
            return Err(bad("LF image has the wrong shape"));
        }
        let scale_inv = f64::from(global.global_scale) * f64::from(global.quant_lf);
        // channel order of the stream: Y, X, B
        for (c, src) in [(0usize, 1usize), (1, 0), (2, 2)] {
            let scale = (f64::from(lf_raw[c]) * f64::from(1u32 << (9 - extra)) / scale_inv) as f32;
            for y in 0..gh {
                let d = (oy + y) * self.bw + ox;
                for x in 0..gw {
                    let q = out[src].d[y * gw + x];
                    self.lf_q[c][d + x] = q;
                    self.lf[c][d + x] = q as f32 * scale;
                }
            }
        }
        Ok(())
    }

    /// Read the block metadata of LF group `g` (the HfMetadata bundle).
    #[allow(clippy::too_many_arguments)]
    pub fn read_hf_meta(
        &mut self,
        br: &mut Br,
        fh: &FrameHeader,
        global: &Global,
        tree: Option<&MaTree>,
        g: usize,
        bit_depth: u32,
        max_samples: u64,
    ) -> Result<()> {
        let gd = fh.group_dim() as usize;
        let gdb = gd / 8;
        let (lw, lh) = fh.group_size(fh.lf_group_dim(), g as u32);
        let (lw, lh) = (lw as usize, lh as usize);
        let (gw, gh) = (lw.div_ceil(8), lh.div_ceil(8));
        let per_row = fh.lf_groups_per_row() as usize;
        let (ox, oy) = ((g % per_row) * gd, (g / per_row) * gd);

        let nb_blocks = 1 + br.read(ceil_log2(gw * gh))? as usize;
        let (cw, ch) = (lw.div_ceil(64), lh.div_ceil(64));
        let input = vec![Chan::info(cw, ch, 0, 0), Chan::info(cw, ch, 0, 0), Chan::info(nb_blocks, 2, 0, 0), Chan::info(gw, gh, 0, 0)];
        let mut s = Stream::parse(br, tree, input, max_samples)?;
        s.decode(br, tree, 4, (1 + 2 * fh.num_lf_groups() as usize + g) as u32)?;
        let out = s.finish(bit_depth)?;
        if out.len() != 4 || out[0].w != cw || out[0].h != ch || out[2].w != nb_blocks || out[3].w != gw || out[3].h != gh {
            return Err(bad("HF metadata has the wrong shape"));
        }

        let (tx, ty) = (ox / 8, oy / 8);
        for y in 0..ch {
            for x in 0..cw {
                let d = (ty + y) * self.tiles_w + tx + x;
                self.xfy[d] = out[0].d[y * cw + x];
                self.bfy[d] = out[1].d[y * cw + x];
            }
        }

        let epf = fh.epf.as_ref().map(|e| (e.quant_mul * 65536.0 / global.global_scale as f32, e.sharp_lut));
        let mut idx = 0usize;
        for y in 0..gh {
            let mut x = 0usize;
            while x < gw {
                if !matches!(self.blk[(oy + y) * self.bw + ox + x], Blk::Empty) {
                    x += 1;
                    continue;
                }
                if idx >= nb_blocks {
                    return Err(bad("block list does not cover the LF group"));
                }
                let ty = Ty::from_u8(out[2].d[idx].clamp(0, 255) as u8)?;
                let mul = out[2].d[nb_blocks + idx].checked_add(1).unwrap_or(0);
                if mul <= 0 {
                    return Err(bad("non-positive block multiplier"));
                }
                let (bwid, bhei) = ty.blocks();
                if x % gdb + bwid > gdb || y % gdb + bhei > gdb || x + bwid > gw || y + bhei > gh {
                    return Err(bad("varblock crosses a group border"));
                }
                for dy in 0..bhei {
                    for dx in 0..bwid {
                        let at = (oy + y + dy) * self.bw + ox + x + dx;
                        if !matches!(self.blk[at], Blk::Empty) {
                            return Err(bad("varblocks overlap"));
                        }
                        self.blk[at] = if dx == 0 && dy == 0 { Blk::Data { ty, mul } } else { Blk::Occupied };
                        if let Some((q, lut)) = epf {
                            let sharp = out[3].d[(y + dy) * gw + x + dx];
                            if !(0..8).contains(&sharp) {
                                return Err(bad("invalid EPF sharpness"));
                            }
                            self.sigma[at] = q / mul as f32 * lut[sharp as usize];
                        }
                    }
                }
                idx += 1;
                x += bwid;
            }
        }
        Ok(())
    }

    /// Read the HF coefficients of pass group `group` for one pass.
    #[allow(clippy::too_many_arguments)]
    pub fn read_hf_coeffs(
        &mut self,
        br: &mut Br,
        fh: &FrameHeader,
        global: &Global,
        pass: &HfPass,
        presets: u32,
        group: usize,
        shift: u32,
    ) -> Result<()> {
        let gdb = fh.group_dim() as usize / 8;
        let per_row = fh.groups_per_row() as usize;
        let (x0, y0) = ((group % per_row) * gdb, (group / per_row) * gdb);
        let gw = (self.bw - x0).min(gdb);
        let gh = (self.bh - y0).min(gdb);
        let ctx = &global.ctx;
        let lf_mul = (ctx.lf_thresholds[0].len() + 1) * (ctx.lf_thresholds[1].len() + 1) * (ctx.lf_thresholds[2].len() + 1);
        let hf_mul = ctx.qf_thresholds.len() + 1;
        let clusters = ctx.clusters;

        let hfp = br.read(ceil_log2(presets as usize))?;
        if hfp >= presets {
            return Err(bad("HF preset out of range"));
        }
        let ctx_size = 495 * clusters;
        let mut dist = pass.dist.clone();
        let cmap: Vec<u8> = dist
            .clusters()
            .get(ctx_size * hfp as usize..ctx_size * (hfp as usize + 1))
            .ok_or_else(|| bad("coefficient context map is too short"))?
            .to_vec();
        dist.begin(br)?;

        let stride = self.bw * 8;
        let mut nz: [Vec<u32>; 3] = std::array::from_fn(|_| vec![0; gw]);
        for y in 0..gh {
            for x in 0..gw {
                let Blk::Data { ty, mul } = self.blk[(y0 + y) * self.bw + x0 + x] else { continue };
                let (w8, h8) = ty.blocks();
                let nb = (w8 * h8) as u32;
                let nb_log = nb.trailing_zeros();
                let order_id = ty.order_id();

                let mut lf_idx = 0usize;
                for c in [0usize, 2, 1] {
                    let thr = &ctx.lf_thresholds[c];
                    lf_idx *= thr.len() + 1;
                    let q = self.lf_q[c][(y0 + y) * self.bw + x0 + x];
                    lf_idx += thr.iter().filter(|&&t| q > t).count();
                }
                let hf_idx = ctx.qf_thresholds.iter().filter(|&&t| mul > t as i32).count();

                for ci in 0..3usize {
                    let c = [1usize, 0, 2][ci];
                    let block_ctx = *ctx
                        .map
                        .get(((ci * 13 + order_id) * hf_mul + hf_idx) * lf_mul + lf_idx)
                        .ok_or_else(|| bad("block context out of range"))? as usize;
                    let predicted = if y == 0 {
                        if x == 0 {
                            32
                        } else {
                            nz[c][x - 1]
                        }
                    } else if x == 0 {
                        nz[c][x]
                    } else {
                        (nz[c][x] + nz[c][x - 1] + 1) >> 1
                    } as usize;
                    let pidx = if predicted >= 8 { 4 + predicted / 2 } else { predicted };
                    let nz_ctx = block_ctx + pidx * clusters;
                    let mut non_zeros = dist.read_clustered(br, *cmap.get(nz_ctx).ok_or_else(|| bad("context out of range"))?, 0)?;
                    if non_zeros > (63 << nb_log) {
                        return Err(bad("too many non-zero coefficients"));
                    }
                    let nz_val = (non_zeros + nb - 1) >> nb_log;
                    for dx in 0..w8 {
                        nz[c][x + dx] = nz_val;
                    }
                    if non_zeros == 0 {
                        continue;
                    }
                    let mut prev_nonzero = u32::from(non_zeros <= nb * 4);
                    let natural = &self.naturals[order_id];
                    let permuted = &pass.orders[order_id][c];
                    let order = if permuted.is_empty() { natural } else { permuted };
                    let base = block_ctx * 458 + 37 * clusters;
                    let cm = cmap.get(base..base + 458).ok_or_else(|| bad("context out of range"))?;
                    for (k, &(ox, oy)) in order[nb as usize..].iter().enumerate() {
                        let cctx = (COEFF_NUM_NONZERO_CONTEXT[((non_zeros - 1) >> nb_log) as usize] + COEFF_FREQ_CONTEXT[k >> nb_log]) * 2
                            + prev_nonzero;
                        let cluster = *cm.get(cctx as usize).ok_or_else(|| bad("too many zeros in a varblock"))?;
                        let u = dist.read_clustered(br, cluster, 0)?;
                        if u == 0 {
                            prev_nonzero = 0;
                            continue;
                        }
                        let coeff = unpack_signed(u).wrapping_shl(shift);
                        let (dx, dy) = if ty.transposed() { (oy, ox) } else { (ox, oy) };
                        let px = (x0 + x) * 8 + usize::from(dx);
                        let py = (y0 + y) * 8 + usize::from(dy);
                        let at = &mut self.coef[c][py * stride + px];
                        *at = at.wrapping_add(coeff);
                        prev_nonzero = 1;
                        non_zeros -= 1;
                        if non_zeros == 0 {
                            break;
                        }
                    }
                }
            }
        }
        dist.finalize()
    }

    /// Dequantise, undo chroma-from-luma and run the inverse transforms. Returns the X, Y and B
    /// planes cropped to the frame size.
    pub fn reconstruct(&mut self, fh: &FrameHeader, img: &ImageHeader, global: &Global, mats: &Matrices, lf_raw: [f32; 3]) -> [Vec<f32>; 3] {
        let (w, h) = (fh.color_w() as usize, fh.color_h() as usize);
        let stride = self.bw * 8;
        let rows = self.bh * 8;
        let qm = [0.8f32.powi(fh.x_qm_scale as i32 - 2), 1.0, 0.8f32.powi(fh.b_qm_scale as i32 - 2)];
        let gs = global.global_scale as f32;
        let bias = img.opsin.quant_bias;
        let bias_num = img.opsin.quant_bias_num;

        // dequantise the HF coefficients
        let mut cf: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; stride * rows]);
        for by in 0..self.bh {
            for bx in 0..self.bw {
                let Blk::Data { ty, mul } = self.blk[by * self.bw + bx] else { continue };
                let (w8, h8) = ty.blocks();
                let (bwid, bhei) = (w8 * 8, h8 * 8);
                for c in 0..3 {
                    let m = 65536.0 / (gs * mul as f32) * qm[c];
                    let mat = mats.get(c, ty);
                    for y in 0..bhei {
                        let row = (by * 8 + y) * stride + bx * 8;
                        for x in 0..bwid {
                            let mut q = self.coef[c][row + x] as f32;
                            if q.abs() <= 1.0 {
                                q *= bias[c];
                            } else {
                                q -= bias_num / q;
                            }
                            cf[c][row + x] = q * mat[y * bwid + x] * m;
                        }
                    }
                }
            }
        }
        self.coef = [Vec::new(), Vec::new(), Vec::new()];

        // chroma from luma on the HF coefficients
        for y in 0..rows {
            for x in 0..stride {
                let t = (y / 64) * self.tiles_w + x / 64;
                let kx = global.base_x + self.xfy[t] as f32 / global.colour_factor as f32;
                let kb = global.base_b + self.bfy[t] as f32 / global.colour_factor as f32;
                let yv = cf[1][y * stride + x];
                cf[0][y * stride + x] += kx * yv;
                cf[2][y * stride + x] += kb * yv;
            }
        }

        // LF: chroma from luma, then adaptive smoothing
        {
            let kx = global.base_x + (global.x_factor_lf - 128) as f32 / global.colour_factor as f32;
            let kb = global.base_b + (global.b_factor_lf - 128) as f32 / global.colour_factor as f32;
            let [lx, ly, lb] = &mut self.lf;
            for i in 0..lx.len() {
                lx[i] += kx * ly[i];
                lb[i] += kb * ly[i];
            }
        }
        if fh.flags & crate::header::FLAG_SKIP_ADAPTIVE_LF == 0 {
            self.smooth_lf(global, lf_raw);
        }

        // insert the LF coefficients and run the inverse transforms
        let mut out: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; stride * rows]);
        let mut buf = Vec::new();
        for by in 0..self.bh {
            for bx in 0..self.bw {
                let Blk::Data { ty, .. } = self.blk[by * self.bw + bx] else { continue };
                let (w8, h8) = ty.blocks();
                let (bwid, bhei) = (w8 * 8, h8 * 8);
                for c in 0..3 {
                    buf.clear();
                    buf.resize(bwid * bhei, 0.0);
                    for y in 0..bhei {
                        let row = (by * 8 + y) * stride + bx * 8;
                        buf[y * bwid..(y + 1) * bwid].copy_from_slice(&cf[c][row..row + bwid]);
                    }
                    if w8 * h8 == 1 {
                        buf[0] = self.lf[c][by * self.bw + bx];
                    } else {
                        for y in 0..h8 {
                            for x in 0..w8 {
                                buf[y * bwid + x] = self.lf[c][(by + y) * self.bw + bx + x];
                            }
                        }
                        dct::dct2d(&mut buf, bwid, w8, h8, false, &self.tables);
                        let (lw, lh) = (w8.trailing_zeros(), h8.trailing_zeros());
                        for y in 0..h8 {
                            for x in 0..w8 {
                                buf[y * bwid + x] /= scale_f(y, lh) * scale_f(x, lw);
                            }
                        }
                    }
                    self.inverse(ty, &mut buf, bwid, bhei);
                    for y in 0..bhei {
                        let row = (by * 8 + y) * stride + bx * 8;
                        out[c][row..row + bwid].copy_from_slice(&buf[y * bwid..(y + 1) * bwid]);
                    }
                }
            }
        }

        // crop
        let mut planes: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::with_capacity(w * h));
        for c in 0..3 {
            for y in 0..h {
                planes[c].extend_from_slice(&out[c][y * stride..y * stride + w]);
            }
        }
        planes
    }

    fn inverse(&self, ty: Ty, buf: &mut [f32], w: usize, h: usize) {
        let t = &self.tables;
        if w == 8 && h == 8 {
            let mut b = [0f32; 64];
            b.copy_from_slice(buf);
            match ty {
                Ty::Dct2 => dct::transform_dct2(&mut b),
                Ty::Dct4 => dct::transform_dct4(&mut b, t),
                Ty::Hornuss => dct::transform_hornuss(&mut b),
                Ty::Dct4x8 => dct::transform_dct4x8(&mut b, false, t),
                Ty::Dct8x4 => dct::transform_dct4x8(&mut b, true, t),
                Ty::Afv0 => dct::transform_afv(&mut b, 0, t),
                Ty::Afv1 => dct::transform_afv(&mut b, 1, t),
                Ty::Afv2 => dct::transform_afv(&mut b, 2, t),
                Ty::Afv3 => dct::transform_afv(&mut b, 3, t),
                _ => dct::dct2d(&mut b, 8, 8, 8, true, t),
            }
            buf.copy_from_slice(&b);
        } else {
            dct::dct2d(buf, w, w, h, true, t);
        }
    }

    /// Adaptive LF smoothing of the dequantised LF image.
    fn smooth_lf(&mut self, global: &Global, lf_raw: [f32; 3]) {
        const SELF_W: f32 = 0.052262735;
        const SIDE_W: f32 = 0.2034514;
        const DIAG_W: f32 = 0.03348292;
        let (w, h) = (self.bw, self.bh);
        if w <= 2 || h <= 2 {
            return;
        }
        let scale_inv = f64::from(global.global_scale) * f64::from(global.quant_lf);
        let lim: [f32; 3] = std::array::from_fn(|c| (512.0 * f64::from(lf_raw[c]) / scale_inv) as f32);
        let src = self.lf.clone();
        let mut wa = [0f32; 3];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let mut gap = 0.5f32;
                let i = y * w + x;
                for c in 0..3 {
                    let p = &src[c];
                    let side = p[i - 1] + p[i + 1] + p[i - w] + p[i + w];
                    let diag = p[i - w - 1] + p[i - w + 1] + p[i + w - 1] + p[i + w + 1];
                    wa[c] = p[i] * SELF_W + side * SIDE_W + diag * DIAG_W;
                    gap = gap.max((wa[c] - p[i]).abs() / lim[c]);
                }
                let scale = (3.0 - 4.0 * gap).max(0.0);
                for c in 0..3 {
                    self.lf[c][i] = (wa[c] - src[c][i]) * scale + src[c][i];
                }
            }
        }
    }

    /// Per-block EPF strengths and the block grid width, for the filter.
    pub fn sigma(&self) -> (&[f32], usize) {
        (&self.sigma, self.bw)
    }
}

/// Parse HfGlobal: the dequantisation matrices and the per-pass coefficient decoders.
pub fn read_hf_global(
    br: &mut Br,
    fh: &FrameHeader,
    global: &Global,
    tree: Option<&MaTree>,
    bit_depth: u32,
    max_samples: u64,
) -> Result<(Matrices, u32, Vec<HfPass>)> {
    let first_stream = 1 + 3 * fh.num_lf_groups();
    let mats = Matrices::parse(br, tree, first_stream, bit_depth, max_samples)?;
    if std::env::var_os("JXL_DEBUG").is_some() {
        eprintln!("after matrices: bit {}", br.bits_read());
    }
    let presets = 1 + br.read(ceil_log2(fh.num_groups() as usize))?;
    let mut passes = Vec::with_capacity(fh.passes.num_passes as usize);
    for _ in 0..fh.passes.num_passes {
        passes.push(HfPass::parse(br, global, presets)?);
    }
    Ok((mats, presets, passes))
}
