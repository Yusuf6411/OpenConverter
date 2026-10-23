//! VP8 decoder: key frames and inter frames (RFC 6386).
//!
//! The decoder follows RFC 6386 and agrees with libwebp bit for bit, including how it treats
//! out-of-range coefficients, so a damaged stream decodes to the same pixels in both.
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use crate::bool::BoolReader;
use crate::filter::{filter_mb, inter_strengths, FilterInfo, PlaneMut};
use crate::inter::{
    find_near, predict_mb, sub_mv_context, Bounds, MbInfo, Mv, MvProbs, ALTREF_FRAME,
    DEFAULT_MV_PROBS, DEFAULT_UVMODE_PROBS, DEFAULT_YMODE_PROBS, GOLDEN_FRAME, INTER_BMODE_PROBS,
    LAST_FRAME, MV_PROB_COUNT, MV_UPDATE_PROBS, M_BPRED, M_NEAR, M_NEAREST, M_NEW, M_SPLIT, M_ZERO,
    SPLIT_COUNT, SPLIT_LAYOUT, SPLIT_PROBS, SUB_MV_REF_PROBS,
};
use crate::predict::{predict_4x4, predict_block, DC_PRED, H_PRED, TM_PRED, V_PRED};
use crate::recon::Planes;
use crate::tables::{
    AC_TABLE, BANDS, BMODE_PROBS, COEFF_PROBS, COEFF_UPDATE_PROBS, DC_TABLE, ZIGZAG,
};
use crate::transform::iwht;
use oc_core::{Error, Progress, Result};
use std::sync::Arc;

/// A decoded frame as three tightly packed planes; chroma is subsampled by two both ways,
/// rounding up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Frame {
    pub fn chroma_width(&self) -> usize {
        self.width.div_ceil(2)
    }
    pub fn chroma_height(&self) -> usize {
        self.height.div_ceil(2)
    }
}

#[derive(Default, Clone, Copy)]
struct Quant {
    y1: [i32; 2],
    y2: [i32; 2],
    uv: [i32; 2],
}

type Probs = [[[[u8; 11]; 3]; 8]; 4];

#[derive(Clone, Copy, Default)]
struct Nz {
    y: [bool; 4],
    u: [bool; 2],
    v: [bool; 2],
    dc: bool,
}

fn large_value(br: &mut BoolReader, p: &[u8; 11]) -> i32 {
    if !br.get(p[3]) {
        if !br.get(p[4]) {
            2
        } else {
            3 + i32::from(br.get(p[5]))
        }
    } else if !br.get(p[6]) {
        if !br.get(p[7]) {
            5 + i32::from(br.get(159))
        } else {
            let mut v = 7 + 2 * i32::from(br.get(165));
            v += i32::from(br.get(145));
            v
        }
    } else {
        let bit1 = br.get(p[8]);
        let bit0 = br.get(p[9 + usize::from(bit1)]);
        let cat = 2 * usize::from(bit1) + usize::from(bit0);
        const CAT: [&[u8]; 4] = [
            &[173, 148, 140],
            &[176, 155, 140, 135],
            &[180, 157, 141, 134, 130],
            &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
        ];
        let mut v = 0;
        for &pr in CAT[cat] {
            v = 2 * v + i32::from(br.get(pr));
        }
        v + 3 + (8 << cat)
    }
}

/// Reads the tokens of one block into `out` (dequantised); returns the position after the
/// last coefficient read (0 when the block is empty).
fn get_coeffs(
    br: &mut BoolReader,
    probs: &[[[u8; 11]; 3]; 8],
    ctx: usize,
    dq: [i32; 2],
    first: usize,
    out: &mut [i16],
) -> usize {
    let mut n = first;
    let mut p = &probs[usize::from(BANDS[n])][ctx];
    while n < 16 {
        if !br.get(p[0]) {
            return n;
        }
        while !br.get(p[1]) {
            n += 1;
            p = &probs[usize::from(BANDS[n])][0];
            if n == 16 {
                return 16;
            }
        }
        let next = &probs[usize::from(BANDS[n + 1])];
        let v;
        if !br.get(p[2]) {
            v = 1;
            p = &next[1];
        } else {
            v = large_value(br, p);
            p = &next[2];
        }
        out[usize::from(ZIGZAG[n])] = (br.sign(v) * dq[usize::from(n > 0)]) as i16;
        n += 1;
    }
    16
}

fn nz_code(nz: usize, dc: i16) -> u8 {
    if nz > 3 {
        3
    } else if nz > 1 {
        2
    } else {
        u8::from(dc != 0)
    }
}

struct Tag {
    key: bool,
    version: u32,
    show: bool,
    first_part: usize,
    width: usize,
    height: usize,
    /// Bytes before the first partition: 10 for key frames, 3 otherwise.
    header: usize,
}

fn parse_tag(data: &[u8]) -> Result<Tag> {
    let b = data
        .get(..3)
        .ok_or_else(|| Error::truncated("VP8 frame header"))?;
    let tag = u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16;
    let key = tag & 1 == 0;
    let version = (tag >> 1) & 7;
    if version > 3 {
        return Err(Error::corrupt("unknown VP8 profile"));
    }
    let mut t = Tag {
        key,
        version,
        show: (tag >> 4) & 1 != 0,
        first_part: (tag >> 5) as usize,
        width: 0,
        height: 0,
        header: 3,
    };
    if key {
        let b = data
            .get(..10)
            .ok_or_else(|| Error::truncated("VP8 frame header"))?;
        if b[3..6] != [0x9d, 0x01, 0x2a] {
            return Err(Error::corrupt("bad VP8 start code"));
        }
        t.width = usize::from(u16::from_le_bytes([b[6], b[7]]) & 0x3fff);
        t.height = usize::from(u16::from_le_bytes([b[8], b[9]]) & 0x3fff);
        if t.width == 0 || t.height == 0 {
            return Err(Error::corrupt("VP8 frame has no pixels"));
        }
        t.header = 10;
    }
    Ok(t)
}

/// Width and height of the key frame at the start of `data`.
pub fn dimensions(data: &[u8]) -> Result<(usize, usize)> {
    let t = parse_tag(data)?;
    if !t.key {
        return Err(Error::corrupt("not a VP8 key frame"));
    }
    Ok((t.width, t.height))
}

#[derive(Default, Clone)]
struct Segments {
    enabled: bool,
    update_map: bool,
    absolute: bool,
    quant: [i32; 4],
    filter: [i32; 4],
    probs: [u8; 3],
}

/// Probabilities that a frame may change and that survive into the next frame unless the
/// frame asks for them to be dropped.
#[derive(Clone)]
struct Entropy {
    coeff: Probs,
    mv: MvProbs,
    ymode: [u8; 4],
    uvmode: [u8; 3],
}

impl Entropy {
    fn new() -> Self {
        Self {
            coeff: COEFF_PROBS,
            mv: DEFAULT_MV_PROBS,
            ymode: DEFAULT_YMODE_PROBS,
            uvmode: DEFAULT_UVMODE_PROBS,
        }
    }
}

/// A VP8 decoder that keeps the reference frames between calls, so inter frames decode.
pub struct Decoder {
    size: Option<(usize, usize)>,
    last: Option<Arc<Planes>>,
    golden: Option<Arc<Planes>>,
    alt: Option<Arc<Planes>>,
    entropy: Entropy,
    seg: Segments,
    seg_map: Vec<u8>,
    lf_ref: [i32; 4],
    lf_mode: [i32; 4],
    sign_bias: [bool; 4],
    mi: Vec<MbInfo>,
    shown: Option<Frame>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn read_bmode(br: &mut BoolReader, pr: &[u8; 9]) -> u8 {
    const TREE: [i8; 18] = [
        0, 1, -1, 2, -2, 3, 4, 6, -3, 5, -4, -5, -6, 7, -7, 8, -8, -9,
    ];
    let mut i = TREE[usize::from(br.get(pr[0]))];
    while i > 0 {
        let idx = 2 * i as usize + usize::from(br.get(pr[i as usize]));
        i = TREE[idx];
    }
    (-i) as u8
}

fn read_mv_component(br: &mut BoolReader, p: &[u8; MV_PROB_COUNT]) -> i32 {
    let mut x = 0i32;
    if br.get(p[0]) {
        for i in 0..3 {
            x += i32::from(br.get(p[9 + i])) << i;
        }
        for i in (4..10).rev() {
            x += i32::from(br.get(p[9 + i])) << i;
        }
        if x & 0xfff0 == 0 || br.get(p[9 + 3]) {
            x += 8;
        }
    } else {
        // small_mvtree: three levels over p[2..9].
        let hi = br.get(p[2]);
        x = if !hi {
            if !br.get(p[3]) {
                i32::from(br.get(p[4]))
            } else {
                2 + i32::from(br.get(p[5]))
            }
        } else if !br.get(p[6]) {
            4 + i32::from(br.get(p[7]))
        } else {
            6 + i32::from(br.get(p[8]))
        };
    }
    if x != 0 && br.get(p[1]) {
        -x
    } else {
        x
    }
}

fn read_mv(br: &mut BoolReader, probs: &MvProbs) -> Mv {
    let row = read_mv_component(br, &probs[0]) * 2;
    let col = read_mv_component(br, &probs[1]) * 2;
    Mv { row, col }
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            size: None,
            last: None,
            golden: None,
            alt: None,
            entropy: Entropy::new(),
            seg: Segments::default(),
            seg_map: Vec::new(),
            lf_ref: [0; 4],
            lf_mode: [0; 4],
            sign_bias: [false; 4],
            mi: Vec::new(),
            shown: None,
        }
    }

    /// Decodes one frame. Returns `None` for a frame that is not meant to be displayed (an
    /// alternate reference) and repeats the previous picture for an empty packet.
    pub fn decode(&mut self, data: &[u8], progress: &dyn Progress) -> Result<Option<Frame>> {
        if data.is_empty() {
            return Ok(self.shown.clone());
        }
        let tag = parse_tag(data)?;
        if tag.key {
            if self.size != Some((tag.width, tag.height)) {
                self.size = Some((tag.width, tag.height));
                let (mb_w, mb_h) = (tag.width.div_ceil(16), tag.height.div_ceil(16));
                self.seg_map = vec![0; mb_w * mb_h];
                self.mi = vec![MbInfo::default(); (mb_w + 1) * (mb_h + 1)];
                self.shown = None;
            }
            self.entropy = Entropy::new();
            self.seg = Segments {
                absolute: false,
                probs: [255; 3],
                ..Segments::default()
            };
            self.lf_ref = [0; 4];
            self.lf_mode = [0; 4];
            self.sign_bias = [false; 4];
        } else if self.last.is_none() {
            return Err(Error::corrupt("VP8 inter frame without a key frame"));
        }
        let (width, height) = self
            .size
            .ok_or_else(|| Error::corrupt("VP8 stream starts with an inter frame"))?;
        let part0 = data
            .get(tag.header..tag.header + tag.first_part)
            .ok_or_else(|| Error::truncated("VP8 first partition"))?;
        let rest = &data[tag.header + tag.first_part..];
        let mb_w = width.div_ceil(16);
        let mb_h = height.div_ceil(16);
        let stride = mb_w + 1;
        let mut br = BoolReader::new(part0);

        // Frame header.
        if tag.key {
            let _colorspace = br.get_bit();
            let _clamp = br.get_bit();
        }
        self.seg.enabled = br.get_bit();
        self.seg.update_map = false;
        if self.seg.enabled {
            self.seg.update_map = br.get_bit();
            if br.get_bit() {
                self.seg.absolute = br.get_bit();
                for q in &mut self.seg.quant {
                    *q = if br.get_bit() { br.signed(7) } else { 0 };
                }
                for f in &mut self.seg.filter {
                    *f = if br.get_bit() { br.signed(6) } else { 0 };
                }
            }
            if self.seg.update_map {
                for p in &mut self.seg.probs {
                    *p = if br.get_bit() { br.value(8) as u8 } else { 255 };
                }
            }
        }
        let simple = br.get_bit();
        let level = br.value(6) as i32;
        let sharpness = br.value(3) as i32;
        let use_delta = br.get_bit();
        if use_delta && br.get_bit() {
            for d in &mut self.lf_ref {
                if br.get_bit() {
                    *d = br.signed(6);
                }
            }
            for d in &mut self.lf_mode {
                if br.get_bit() {
                    *d = br.signed(6);
                }
            }
        }
        let filter_type = if level == 0 {
            0
        } else if simple {
            1
        } else {
            2
        };

        // Token partitions.
        let num_parts = 1usize << br.value(2);
        let sizes_len = 3 * (num_parts - 1);
        if rest.len() < sizes_len {
            return Err(Error::truncated("VP8 partition sizes"));
        }
        let (sizes, mut body) = rest.split_at(sizes_len);
        let mut parts: Vec<&[u8]> = Vec::with_capacity(num_parts);
        for i in 0..num_parts - 1 {
            let s = &sizes[3 * i..3 * i + 3];
            let size = (usize::from(s[0]) | usize::from(s[1]) << 8 | usize::from(s[2]) << 16)
                .min(body.len());
            let (a, b) = body.split_at(size);
            parts.push(a);
            body = b;
        }
        if body.is_empty() {
            return Err(Error::truncated("VP8 token partition"));
        }
        parts.push(body);
        let mut token_brs: Vec<BoolReader> = parts.into_iter().map(BoolReader::new).collect();

        // Quantisers.
        let base_q = br.value(7) as i32;
        let mut delta = || if br.get_bit() { br.signed(4) } else { 0 };
        let (dqy1_dc, dqy2_dc, dqy2_ac, dquv_dc, dquv_ac) =
            (delta(), delta(), delta(), delta(), delta());
        let clip = |v: i32, m: i32| v.clamp(0, m) as usize;
        let mut quants = [Quant::default(); 4];
        for (i, m) in quants.iter_mut().enumerate() {
            let q = if self.seg.enabled {
                let mut q = self.seg.quant[i];
                if !self.seg.absolute {
                    q += base_q;
                }
                q
            } else {
                base_q
            };
            m.y1 = [
                i32::from(DC_TABLE[clip(q + dqy1_dc, 127)]),
                i32::from(AC_TABLE[clip(q, 127)]),
            ];
            m.y2 = [
                i32::from(DC_TABLE[clip(q + dqy2_dc, 127)]) * 2,
                ((i32::from(AC_TABLE[clip(q + dqy2_ac, 127)]) * 101_581) >> 16).max(8),
            ];
            m.uv = [
                i32::from(DC_TABLE[clip(q + dquv_dc, 117)]),
                i32::from(AC_TABLE[clip(q + dquv_ac, 127)]),
            ];
        }

        // Reference updates (inter frames) and entropy persistence.
        let (mut refresh_golden, mut refresh_alt, mut refresh_last) = (true, true, true);
        let (mut copy_golden, mut copy_alt) = (0u32, 0u32);
        if !tag.key {
            refresh_golden = br.get_bit();
            refresh_alt = br.get_bit();
            if !refresh_golden {
                copy_golden = br.value(2);
            }
            if !refresh_alt {
                copy_alt = br.value(2);
            }
            self.sign_bias[usize::from(GOLDEN_FRAME)] = br.get_bit();
            self.sign_bias[usize::from(ALTREF_FRAME)] = br.get_bit();
        }
        let refresh_entropy = br.get_bit();
        if !tag.key {
            refresh_last = br.get_bit();
        }
        let saved = (!refresh_entropy).then(|| self.entropy.clone());

        // Token probabilities.
        for t in 0..4 {
            for b in 0..8 {
                for c in 0..3 {
                    for p in 0..11 {
                        if br.get(COEFF_UPDATE_PROBS[t][b][c][p]) {
                            self.entropy.coeff[t][b][c][p] = br.value(8) as u8;
                        }
                    }
                }
            }
        }
        let use_skip = br.get_bit();
        let skip_prob = if use_skip { br.value(8) as u8 } else { 0 };
        let (mut prob_intra, mut prob_last, mut prob_gf) = (0u8, 0u8, 0u8);
        if !tag.key {
            prob_intra = br.value(8) as u8;
            prob_last = br.value(8) as u8;
            prob_gf = br.value(8) as u8;
            if br.get_bit() {
                for p in &mut self.entropy.ymode {
                    *p = br.value(8) as u8;
                }
            }
            if br.get_bit() {
                for p in &mut self.entropy.uvmode {
                    *p = br.value(8) as u8;
                }
            }
            for i in 0..2 {
                for j in 0..MV_PROB_COUNT {
                    if br.get(MV_UPDATE_PROBS[i][j]) {
                        let x = br.value(7) as u8;
                        self.entropy.mv[i][j] = if x != 0 { x << 1 } else { 1 };
                    }
                }
            }
        }
        if br.eof() {
            return Err(Error::truncated("VP8 frame header"));
        }
        let probs = self.entropy.coeff;
        let ymode_probs = self.entropy.ymode;
        let uvmode_probs = self.entropy.uvmode;
        let mv_probs = self.entropy.mv;
        let bilinear = tag.version != 0;
        let full_pixel = tag.version == 3;

        let fstrength = if filter_type > 0 {
            inter_strengths(
                tag.key,
                level,
                sharpness,
                self.seg
                    .enabled
                    .then_some((&self.seg.filter, self.seg.absolute)),
                use_delta.then_some((&self.lf_ref, &self.lf_mode)),
            )
        } else {
            [[[FilterInfo::default(); 4]; 4]; 4]
        };

        let refs = [
            None,
            self.last.clone(),
            self.golden.clone(),
            self.alt.clone(),
        ];
        let mut pl = Planes::new(mb_w, mb_h);
        let (ys, uvs) = (pl.ys, pl.uvs);

        let mut top_modes = vec![0u8; mb_w * 4];
        let mut top_nz = vec![Nz::default(); mb_w];
        let mut finfo: Vec<FilterInfo> = vec![FilterInfo::default(); mb_w * mb_h];
        let mut coeffs = [0i16; 384];

        for mb_y in 0..mb_h {
            progress.check_cancelled()?;
            progress.report(mb_y as u64, mb_h as u64, "decode");
            let mut left_modes = [0u8; 4];
            let mut left_nz = Nz::default();
            let tbr = token_brs.len() - 1;
            let tok = &mut token_brs[mb_y & tbr];
            for mb_x in 0..mb_w {
                let idx = (mb_y + 1) * stride + mb_x + 1;
                // Modes (first partition).
                let map_pos = mb_y * mb_w + mb_x;
                if self.seg.update_map {
                    self.seg_map[map_pos] = if !br.get(self.seg.probs[0]) {
                        u8::from(br.get(self.seg.probs[1]))
                    } else {
                        u8::from(br.get(self.seg.probs[2])) + 2
                    };
                } else if tag.key {
                    self.seg_map[map_pos] = 0;
                }
                let segment = if self.seg.enabled {
                    usize::from(self.seg_map[map_pos])
                } else {
                    0
                };
                let skip_flag = use_skip && br.get(skip_prob);
                let mut info = MbInfo::default();
                let mut ymode = 0u8;
                let mut bmodes = [0u8; 16];
                let mut is_inter = false;
                if !tag.key {
                    is_inter = br.get(prob_intra);
                }
                if is_inter {
                    let mut rf = LAST_FRAME;
                    if br.get(prob_last) {
                        rf = GOLDEN_FRAME + u8::from(br.get(prob_gf));
                    }
                    let bounds = Bounds::new(mb_x, mb_y, mb_w, mb_h);
                    let near = find_near(&self.mi, idx, stride, rf, &self.sign_bias, &bounds);
                    info.ref_frame = rf;
                    if !br.get(near.probs[0]) {
                        info.mode = M_ZERO;
                    } else if !br.get(near.probs[1]) {
                        info.mode = M_NEAREST;
                        info.mv = near.nearest;
                    } else if !br.get(near.probs[2]) {
                        info.mode = M_NEAR;
                        info.mv = near.near;
                    } else if !br.get(near.probs[3]) {
                        info.mode = M_NEW;
                        let d = read_mv(&mut br, &mv_probs);
                        info.mv = Mv {
                            row: d.row + near.best.row,
                            col: d.col + near.best.col,
                        };
                    } else {
                        info.mode = M_SPLIT;
                    }
                    if info.mode == M_SPLIT {
                        let shape = if !br.get(SPLIT_PROBS[0]) {
                            3
                        } else if !br.get(SPLIT_PROBS[1]) {
                            2
                        } else if !br.get(SPLIT_PROBS[2]) {
                            0
                        } else {
                            1
                        };
                        let layout = &SPLIT_LAYOUT[shape];
                        let mut bmv = [Mv::ZERO; 16];
                        for part in 0..SPLIT_COUNT[shape] {
                            let k = layout
                                .iter()
                                .position(|&p| usize::from(p) == part)
                                .unwrap_or(0);
                            let l = if k & 3 == 0 {
                                self.mi[idx - 1].bmv[k + 3]
                            } else {
                                bmv[k - 1]
                            };
                            let a = if k < 4 {
                                self.mi[idx - stride].bmv[k + 12]
                            } else {
                                bmv[k - 4]
                            };
                            let pr = &SUB_MV_REF_PROBS[sub_mv_context(l, a)];
                            let mv = if !br.get(pr[0]) {
                                l
                            } else if !br.get(pr[1]) {
                                a
                            } else if !br.get(pr[2]) {
                                Mv::ZERO
                            } else {
                                let d = read_mv(&mut br, &mv_probs);
                                Mv {
                                    row: d.row + near.best.row,
                                    col: d.col + near.best.col,
                                }
                            };
                            for (b, &p) in layout.iter().enumerate() {
                                if usize::from(p) == part {
                                    bmv[b] = mv;
                                }
                            }
                        }
                        info.bmv = bmv;
                        info.mv = bmv[15];
                    } else {
                        info.bmv = [info.mv; 16];
                    }
                } else {
                    // Intra macroblock.
                    let is_i4;
                    if tag.key {
                        is_i4 = !br.get(145);
                        if !is_i4 {
                            ymode = if br.get(156) {
                                if br.get(128) {
                                    TM_PRED
                                } else {
                                    H_PRED
                                }
                            } else if br.get(163) {
                                V_PRED
                            } else {
                                DC_PRED
                            };
                        }
                    } else if !br.get(ymode_probs[0]) {
                        is_i4 = false;
                        ymode = DC_PRED;
                    } else if !br.get(ymode_probs[1]) {
                        is_i4 = false;
                        ymode = if br.get(ymode_probs[2]) {
                            H_PRED
                        } else {
                            V_PRED
                        };
                    } else if !br.get(ymode_probs[3]) {
                        is_i4 = false;
                        ymode = TM_PRED;
                    } else {
                        is_i4 = true;
                    }
                    if !is_i4 {
                        top_modes[mb_x * 4..mb_x * 4 + 4].fill(ymode);
                        left_modes.fill(ymode);
                        info.mode = ymode;
                    } else {
                        info.mode = M_BPRED;
                        for by in 0..4 {
                            let mut left = left_modes[by];
                            for bx in 0..4 {
                                let mode = if tag.key {
                                    let pr = &BMODE_PROBS[usize::from(top_modes[mb_x * 4 + bx])]
                                        [usize::from(left)];
                                    read_bmode(&mut br, pr)
                                } else {
                                    read_bmode(&mut br, &INTER_BMODE_PROBS)
                                };
                                top_modes[mb_x * 4 + bx] = mode;
                                left = mode;
                                bmodes[by * 4 + bx] = mode;
                            }
                            left_modes[by] = left;
                        }
                    }
                }
                let has_y2 = info.mode != M_BPRED && info.mode != M_SPLIT;
                self.mi[idx] = info;
                let uvmode = if is_inter {
                    DC_PRED
                } else if tag.key {
                    if !br.get(142) {
                        DC_PRED
                    } else if !br.get(114) {
                        V_PRED
                    } else if br.get(183) {
                        TM_PRED
                    } else {
                        H_PRED
                    }
                } else if !br.get(uvmode_probs[0]) {
                    DC_PRED
                } else if !br.get(uvmode_probs[1]) {
                    V_PRED
                } else if br.get(uvmode_probs[2]) {
                    TM_PRED
                } else {
                    H_PRED
                };

                // Residuals (token partition).
                coeffs.fill(0);
                let mut codes = [0u8; 24];
                let top = &mut top_nz[mb_x];
                let mut skip = skip_flag;
                if !skip {
                    let q = &quants[segment];
                    let mut first = 0;
                    let mut ac_type = 3;
                    if has_y2 {
                        let mut dc = [0i16; 16];
                        let ctx = usize::from(top.dc) + usize::from(left_nz.dc);
                        let nz = get_coeffs(tok, &probs[1], ctx, q.y2, 0, &mut dc);
                        top.dc = nz > 0;
                        left_nz.dc = nz > 0;
                        if nz > 1 {
                            iwht(&dc, &mut coeffs);
                        } else {
                            let dc0 = ((i32::from(dc[0]) + 3) >> 3) as i16;
                            for b in 0..16 {
                                coeffs[b * 16] = dc0;
                            }
                        }
                        first = 1;
                        ac_type = 0;
                    }
                    let mut any = 0u8;
                    for by in 0..4 {
                        let mut l = left_nz.y[by];
                        for bx in 0..4 {
                            let b = by * 4 + bx;
                            let ctx = usize::from(l) + usize::from(top.y[bx]);
                            let nz = get_coeffs(
                                tok,
                                &probs[ac_type],
                                ctx,
                                q.y1,
                                first,
                                &mut coeffs[b * 16..b * 16 + 16],
                            );
                            l = nz > first;
                            top.y[bx] = l;
                            codes[b] = nz_code(nz, coeffs[b * 16]);
                            any |= codes[b];
                        }
                        left_nz.y[by] = l;
                    }
                    for ch in 0..2 {
                        let (tn, ln) = if ch == 0 {
                            (&mut top.u, &mut left_nz.u)
                        } else {
                            (&mut top.v, &mut left_nz.v)
                        };
                        for by in 0..2 {
                            let mut l = ln[by];
                            for bx in 0..2 {
                                let b = 16 + ch * 4 + by * 2 + bx;
                                let ctx = usize::from(l) + usize::from(tn[bx]);
                                let nz = get_coeffs(
                                    tok,
                                    &probs[2],
                                    ctx,
                                    q.uv,
                                    0,
                                    &mut coeffs[b * 16..b * 16 + 16],
                                );
                                l = nz > 0;
                                tn[bx] = l;
                                codes[b] = nz_code(nz, coeffs[b * 16]);
                                any |= codes[b];
                            }
                            ln[by] = l;
                        }
                    }
                    skip = any == 0;
                } else {
                    top.y = [false; 4];
                    top.u = [false; 2];
                    top.v = [false; 2];
                    left_nz.y = [false; 4];
                    left_nz.u = [false; 2];
                    left_nz.v = [false; 2];
                    if has_y2 {
                        top.dc = false;
                        left_nz.dc = false;
                    }
                }
                if tok.eof() {
                    return Err(Error::truncated("VP8 token partition"));
                }
                if filter_type > 0 {
                    let class = match info.mode {
                        M_BPRED => 0,
                        M_SPLIT => 3,
                        M_NEAREST | M_NEAR | M_NEW => 2,
                        _ => 1,
                    };
                    let mut f = fstrength[segment][usize::from(info.ref_frame)][class];
                    f.inner = !has_y2 || !skip;
                    finfo[mb_y * mb_w + mb_x] = f;
                }

                // Reconstruction.
                if is_inter {
                    let refp = refs[usize::from(info.ref_frame)]
                        .as_deref()
                        .ok_or_else(|| Error::corrupt("VP8 reference frame is missing"))?;
                    let (ly, cb, cr) = predict_mb(refp, mb_x, mb_y, &info, bilinear, full_pixel);
                    pl.put_luma(mb_x, mb_y, &ly);
                    pl.put_chroma(0, mb_x, mb_y, &cb);
                    pl.put_chroma(1, mb_x, mb_y, &cr);
                    for b in 0..16 {
                        if codes[b] != 0 {
                            pl.add_luma_block(mb_x, mb_y, b, &coeffs[b * 16..b * 16 + 16]);
                        }
                    }
                    for ch in 0..2 {
                        for b in 0..4 {
                            let blk = 16 + ch * 4 + b;
                            if codes[blk] != 0 {
                                pl.add_chroma_block(
                                    ch,
                                    mb_x,
                                    mb_y,
                                    b,
                                    &coeffs[blk * 16..blk * 16 + 16],
                                );
                            }
                        }
                    }
                } else {
                    reconstruct_luma(
                        &mut pl,
                        mb_x,
                        mb_y,
                        info.mode == M_BPRED,
                        ymode,
                        &bmodes,
                        &coeffs,
                        &codes,
                    );
                    reconstruct_chroma(&mut pl, mb_x, mb_y, uvmode, &coeffs, &codes);
                }
            }
            if br.eof() {
                return Err(Error::truncated("VP8 first partition"));
            }
        }

        if filter_type > 0 {
            let (yo, uo) = (ys + 1, uvs + 1);
            let Planes { y, u, v, .. } = &mut pl;
            let mut yp = PlaneMut {
                data: y,
                stride: ys,
                origin: yo,
            };
            let mut up = PlaneMut {
                data: u,
                stride: uvs,
                origin: uo,
            };
            let mut vp = PlaneMut {
                data: v,
                stride: uvs,
                origin: uo,
            };
            for mb_y in 0..mb_h {
                progress.check_cancelled()?;
                for mb_x in 0..mb_w {
                    filter_mb(
                        filter_type == 1,
                        finfo[mb_y * mb_w + mb_x],
                        mb_x,
                        mb_y,
                        &mut yp,
                        &mut up,
                        &mut vp,
                    );
                }
            }
        }
        progress.report(mb_h as u64, mb_h as u64, "decode");

        // Crop to the visible area.
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let crop = |src: &[u8], stride: usize, w: usize, h: usize| -> Vec<u8> {
            let mut out = Vec::with_capacity(w * h);
            for row in 0..h {
                let s = (row + 1) * stride + 1;
                out.extend_from_slice(&src[s..s + w]);
            }
            out
        };
        let frame = Frame {
            width,
            height,
            y: crop(&pl.y, ys, width, height),
            u: crop(&pl.u, uvs, cw, ch),
            v: crop(&pl.v, uvs, cw, ch),
        };

        // Reference buffer updates use the buffers as they were before this frame.
        let new = Arc::new(pl);
        let (old_last, old_golden, old_alt) =
            (self.last.clone(), self.golden.clone(), self.alt.clone());
        match copy_golden {
            1 => self.golden = old_last.clone(),
            2 => self.golden = old_alt.clone(),
            _ => {}
        }
        match copy_alt {
            1 => self.alt = old_last,
            2 => self.alt = old_golden,
            _ => {}
        }
        if refresh_golden {
            self.golden = Some(new.clone());
        }
        if refresh_alt {
            self.alt = Some(new.clone());
        }
        if refresh_last {
            self.last = Some(new);
        }
        if let Some(e) = saved {
            self.entropy = e;
        }
        if !tag.show {
            return Ok(None);
        }
        self.shown = Some(frame.clone());
        Ok(Some(frame))
    }
}

/// Decodes a single key frame, as found in a WebP file.
pub fn decode(data: &[u8], progress: &dyn Progress) -> Result<Frame> {
    let tag = parse_tag(data)?;
    if !tag.key {
        return Err(Error::new(
            oc_core::ErrorCode::UnsupportedVariant,
            "VP8 inter frames are not supported in this container",
        ));
    }
    if !tag.show {
        return Err(Error::corrupt("VP8 frame is not displayable"));
    }
    Decoder::new()
        .decode(data, progress)?
        .ok_or_else(|| Error::corrupt("VP8 frame is not displayable"))
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_luma(
    pl: &mut Planes,
    mb_x: usize,
    mb_y: usize,
    is_i4: bool,
    ymode: u8,
    bmodes: &[u8; 16],
    coeffs: &[i16; 384],
    codes: &[u8; 24],
) {
    if !is_i4 {
        let (top, left, tl) = pl.luma_edges(mb_x, mb_y);
        let mut pred = [0u8; 256];
        predict_block(ymode, 16, &top, &left, tl, mb_y > 0, mb_x > 0, &mut pred);
        pl.put_luma(mb_x, mb_y, &pred);
        for b in 0..16 {
            if codes[b] != 0 {
                pl.add_luma_block(mb_x, mb_y, b, &coeffs[b * 16..b * 16 + 16]);
            }
        }
        return;
    }
    let tr = pl.top_right(mb_x, mb_y);
    for b in 0..16 {
        let (top, left, x) = pl.i4_edges(mb_x, mb_y, b, &tr);
        pl.put_4x4(mb_x, mb_y, b, &predict_4x4(bmodes[b], x, &top, &left));
        if codes[b] != 0 {
            pl.add_luma_block(mb_x, mb_y, b, &coeffs[b * 16..b * 16 + 16]);
        }
    }
}

fn reconstruct_chroma(
    pl: &mut Planes,
    mb_x: usize,
    mb_y: usize,
    mode: u8,
    coeffs: &[i16; 384],
    codes: &[u8; 24],
) {
    for ch in 0..2 {
        let (top, left, tl) = pl.chroma_edges(ch, mb_x, mb_y);
        let mut pred = [0u8; 64];
        predict_block(mode, 8, &top, &left, tl, mb_y > 0, mb_x > 0, &mut pred);
        pl.put_chroma(ch, mb_x, mb_y, &pred);
        for b in 0..4 {
            let blk = 16 + ch * 4 + b;
            if codes[blk] != 0 {
                pl.add_chroma_block(ch, mb_x, mb_y, b, &coeffs[blk * 16..blk * 16 + 16]);
            }
        }
    }
}
