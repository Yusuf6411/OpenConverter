//! JPEG encoder: 8-bit gray or YCbCr, baseline with the standard or optimised Huffman tables,
//! or progressive, with optional chroma subsampling. Colour conversion, downsampling and quantisation follow libjpeg's
//! arithmetic so that quality settings mean what people expect.
// Block, coefficient and table indices are bounded by how the scans are built.
#![allow(clippy::indexing_slicing)]
use super::dct;
use super::tables::{
    HuffSpec, STD_AC_CHROMA, STD_AC_LUMA, STD_CHROMA_Q, STD_DC_CHROMA, STD_DC_LUMA, STD_LUMA_Q,
    ZIGZAG,
};
use super::trellis;
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_core::{Error, ErrorCode, Notes, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsampling {
    /// No chroma subsampling (sharpest colour edges, largest files).
    S444,
    /// Half the chroma resolution horizontally.
    S422,
    /// Half the chroma resolution both ways (the usual default).
    S420,
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    /// 1 (smallest) to 100 (best), on the IJG scale.
    pub quality: u8,
    pub subsampling: Subsampling,
    /// Replaces transparency, which JPEG cannot store.
    pub background: [u8; 3],
    /// Build Huffman tables from the picture's own statistics (smaller files).
    pub optimize: bool,
    /// Write the picture in several refining scans; implies optimised tables.
    pub progressive: bool,
    /// Choose quantised values by rate-distortion cost (smaller files, slower).
    pub trellis: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: 85,
            subsampling: Subsampling::S420,
            background: [255, 255, 255],
            optimize: false,
            progressive: false,
            trellis: false,
        }
    }
}

fn scaled_table(base: &[u8; 64], quality: u8) -> [u16; 64] {
    let q = i32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - q * 2 };
    let mut out = [0u16; 64];
    for (o, &b) in out.iter_mut().zip(base) {
        *o = ((i32::from(b) * scale + 50) / 100).clamp(1, 255) as u16;
    }
    out
}

/// Canonical Huffman encoder table: code and length per symbol.
pub(super) struct HuffEnc {
    code: [u16; 256],
    size: [u8; 256],
}

impl HuffEnc {
    fn new(bits: &[u8; 16], vals: &[u8]) -> Self {
        let mut e = HuffEnc {
            code: [0; 256],
            size: [0; 256],
        };
        let (mut code, mut k) = (0u16, 0usize);
        for len in 1..=16u8 {
            for _ in 0..bits.get(usize::from(len) - 1).copied().unwrap_or(0) {
                if let Some(&sym) = vals.get(k) {
                    if let (Some(c), Some(s)) = (
                        e.code.get_mut(usize::from(sym)),
                        e.size.get_mut(usize::from(sym)),
                    ) {
                        *c = code;
                        *s = len;
                    }
                }
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        e
    }
}

struct BitSink {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitSink {
    fn put(&mut self, value: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | (value & ((1 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            let b = (self.acc >> (self.n - 8)) as u8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
            self.n -= 8;
        }
    }

    fn symbol(&mut self, t: &HuffEnc, sym: u8) {
        self.put(
            u32::from(t.code.get(usize::from(sym)).copied().unwrap_or(0)),
            u32::from(t.size.get(usize::from(sym)).copied().unwrap_or(0)),
        );
    }

    fn finish(&mut self) {
        if self.n > 0 {
            // Pad the last byte with 1 bits, as the standard requires.
            self.put((1 << (8 - self.n)) - 1, 8 - self.n);
        }
    }
}

/// Magnitude category and the extra bits for a coefficient.
fn category(v: i32) -> (u8, u32) {
    let a = v.unsigned_abs();
    let cat = (32 - a.leading_zeros()) as u8;
    let bits = if v >= 0 { v as u32 } else { (v - 1) as u32 };
    (cat, bits)
}

fn quantise(coef: &[i32; 64], q: &[u16; 64]) -> [i32; 64] {
    let mut out = [0i32; 64];
    for ((o, &c), &qq) in out.iter_mut().zip(coef).zip(q) {
        let div = i32::from(qq) << 3;
        *o = if c < 0 {
            -((-c + (div >> 1)) / div)
        } else {
            (c + (div >> 1)) / div
        };
    }
    out
}

struct Plane {
    data: Vec<u8>,
    w: usize,
    h: usize,
}

impl Plane {
    /// Pad to `w` x `h` by repeating the last column and row.
    fn padded(&self, w: usize, h: usize) -> Plane {
        let mut data = Vec::with_capacity(w * h);
        for y in 0..h {
            let sy = y.min(self.h - 1);
            for x in 0..w {
                data.push(
                    self.data
                        .get(sy * self.w + x.min(self.w - 1))
                        .copied()
                        .unwrap_or(0),
                );
            }
        }
        Plane { data, w, h }
    }

    fn at(&self, x: usize, y: usize) -> u32 {
        u32::from(self.data.get(y * self.w + x).copied().unwrap_or(0))
    }

    fn block(&self, bx: usize, by: usize) -> [u8; 64] {
        let mut b = [0u8; 64];
        for (i, o) in b.iter_mut().enumerate() {
            *o = self.at(bx * 8 + i % 8, by * 8 + i / 8) as u8;
        }
        b
    }
}

fn downsample(p: &Plane, hf: usize, vf: usize, out_w: usize, out_h: usize) -> Plane {
    let mut data = Vec::with_capacity(out_w * out_h);
    for y in 0..out_h {
        let mut bias = if hf == 2 && vf == 2 { 1 } else { 0 };
        for x in 0..out_w {
            let mut sum = 0;
            for dy in 0..vf {
                for dx in 0..hf {
                    sum += p.at(x * hf + dx, y * vf + dy);
                }
            }
            let div = (hf * vf) as u32;
            data.push(((sum + if div == 4 { bias } else { bias.min(1) }) / div) as u8);
            // libjpeg alternates the rounding bias so errors do not accumulate.
            bias = if div == 4 { bias ^ 3 } else { bias ^ 1 };
        }
    }
    Plane {
        data,
        w: out_w,
        h: out_h,
    }
}

pub(super) fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
}

/// A quantised component: its blocks in raster order over the padded grid.
pub(super) struct Comp {
    /// Component identifier written to the frame and scan headers.
    pub(super) id: u8,
    pub(super) blocks: Vec<[i16; 64]>,
    /// Blocks per row of the padded grid.
    pub(super) stride: usize,
    /// Blocks per MCU, across and down.
    pub(super) hb: usize,
    pub(super) vb: usize,
    /// Blocks that cover the picture, across and down (single-component scans stop here).
    pub(super) real_w: usize,
    pub(super) real_h: usize,
    /// Which Huffman table pair the component uses: 0 luma, 1 chroma.
    pub(super) table: usize,
}

pub(super) struct Scan {
    pub(super) comps: Vec<usize>,
    pub(super) ss: u8,
    pub(super) se: u8,
    pub(super) ah: u8,
    pub(super) al: u8,
}

/// Receives the symbols and raw bits of a scan. Tables 0 and 1 are DC, 2 and 3 are AC.
trait Out {
    fn sym(&mut self, table: usize, s: u8);
    fn bits(&mut self, v: u32, n: u32);
}

struct Count {
    freq: [[u64; 257]; 4],
}

impl Out for Count {
    fn sym(&mut self, table: usize, s: u8) {
        if let Some(f) = self
            .freq
            .get_mut(table)
            .and_then(|t| t.get_mut(usize::from(s)))
        {
            *f += 1;
        }
    }
    fn bits(&mut self, _v: u32, _n: u32) {}
}

struct Write<'a> {
    sink: &'a mut BitSink,
    tables: &'a [HuffEnc; 4],
}

impl Out for Write<'_> {
    fn sym(&mut self, table: usize, s: u8) {
        if let Some(t) = self.tables.get(table) {
            self.sink.symbol(t, s);
        }
    }
    fn bits(&mut self, v: u32, n: u32) {
        self.sink.put(v, n);
    }
}

/// Huffman code lengths from symbol counts, limited to 16 bits (ITU T.81 Annex K.2, as
/// libjpeg's `jpeg_gen_optimal_table` does it). One extra pseudo-symbol keeps the all-ones
/// code unused.
fn optimal_table(counts: &[u64; 257]) -> ([u8; 16], Vec<u8>) {
    let mut freq = *counts;
    freq[256] = 1;
    let mut size = [0u8; 257];
    let mut next = [-1i32; 257];
    loop {
        let (mut c1, mut c2) = (-1i32, -1i32);
        let mut v = u64::MAX;
        for (i, &f) in freq.iter().enumerate() {
            if f != 0 && f <= v {
                v = f;
                c1 = i as i32;
            }
        }
        v = u64::MAX;
        for (i, &f) in freq.iter().enumerate() {
            if f != 0 && f <= v && i as i32 != c1 {
                v = f;
                c2 = i as i32;
            }
        }
        if c2 < 0 {
            break;
        }
        freq[c1 as usize] += freq[c2 as usize];
        freq[c2 as usize] = 0;
        let mut c = c1 as usize;
        size[c] += 1;
        while next[c] >= 0 {
            c = next[c] as usize;
            size[c] += 1;
        }
        next[c] = c2;
        let mut c = c2 as usize;
        size[c] += 1;
        while next[c] >= 0 {
            c = next[c] as usize;
            size[c] += 1;
        }
    }
    let mut bits = [0u32; 40];
    for &z in &size {
        if z > 0 {
            bits[usize::from(z).min(39)] += 1;
        }
    }
    for i in (17..40).rev() {
        while bits[i] > 0 {
            let mut j = i - 2;
            while bits[j] == 0 {
                j -= 1;
            }
            bits[i] -= 2;
            bits[i - 1] += 1;
            bits[j + 1] += 2;
            bits[j] -= 1;
        }
    }
    let mut i = 16;
    while bits[i] == 0 {
        i -= 1;
    }
    bits[i] -= 1;
    let mut out_bits = [0u8; 16];
    for (o, &b) in out_bits.iter_mut().zip(&bits[1..17]) {
        *o = b as u8;
    }
    let mut vals = Vec::new();
    for len in 1..=39u8 {
        for (sym, &z) in size.iter().enumerate().take(256) {
            if z == len {
                vals.push(sym as u8);
            }
        }
    }
    (out_bits, vals)
}

fn bit_length(v: u32) -> u32 {
    32 - v.leading_zeros()
}

fn baseline_block<O: Out>(o: &mut O, b: &[i16; 64], pred: &mut i32, t: usize) {
    let dc = i32::from(b[0]);
    let (cat, bits) = category(dc - *pred);
    *pred = dc;
    o.sym(t, cat);
    o.bits(bits, u32::from(cat));
    let mut run = 0u8;
    for &z in &ZIGZAG[1..] {
        let v = i32::from(b[z]);
        if v == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            o.sym(2 + t, 0xF0);
            run -= 16;
        }
        let (cat, bits) = category(v);
        o.sym(2 + t, (run << 4) | cat);
        o.bits(bits, u32::from(cat));
        run = 0;
    }
    if run > 0 {
        o.sym(2 + t, 0x00);
    }
}

/// Progressive-scan state: the pending end-of-band run and the refinement bits held back
/// until it is written.
#[derive(Default)]
struct Prog {
    eobrun: u32,
    held: Vec<u8>,
    pred: [i32; 3],
}

impl Prog {
    fn flush<O: Out>(&mut self, o: &mut O, ac: usize) {
        if self.eobrun > 0 {
            let nbits = bit_length(self.eobrun) - 1;
            o.sym(ac, (nbits << 4) as u8);
            if nbits > 0 {
                o.bits(self.eobrun, nbits);
            }
            self.eobrun = 0;
            for &b in &self.held {
                o.bits(u32::from(b), 1);
            }
            self.held.clear();
        }
    }
}

fn ac_first<O: Out>(o: &mut O, st: &mut Prog, b: &[i16; 64], scan: &Scan, ac: usize) {
    let mut run = 0u8;
    for &z in &ZIGZAG[usize::from(scan.ss)..=usize::from(scan.se)] {
        let v = i32::from(b[z]);
        let (mag, bits) = if v < 0 {
            let m = (-v) >> scan.al;
            (m, !m)
        } else {
            (v >> scan.al, v >> scan.al)
        };
        if mag == 0 {
            run += 1;
            continue;
        }
        st.flush(o, ac);
        while run > 15 {
            o.sym(ac, 0xF0);
            run -= 16;
        }
        let n = bit_length(mag as u32);
        o.sym(ac, (run << 4) | n as u8);
        o.bits(bits as u32, n);
        run = 0;
    }
    if run > 0 {
        st.eobrun += 1;
        if st.eobrun == 0x7FFF {
            st.flush(o, ac);
        }
    }
}

fn ac_refine<O: Out>(o: &mut O, st: &mut Prog, b: &[i16; 64], scan: &Scan, ac: usize) {
    let (ss, se) = (usize::from(scan.ss), usize::from(scan.se));
    let mut mags = [0i32; 64];
    let mut eob = 0usize;
    for k in ss..=se {
        let m = i32::from(b[ZIGZAG[k]]).abs() >> scan.al;
        mags[k] = m;
        if m == 1 {
            eob = k;
        }
    }
    let mut run = 0u8;
    let mut pending: Vec<u8> = Vec::new();
    for k in ss..=se {
        let m = mags[k];
        if m == 0 {
            run += 1;
            continue;
        }
        while run > 15 && k <= eob {
            st.flush(o, ac);
            o.sym(ac, 0xF0);
            run -= 16;
            for &bit in &pending {
                o.bits(u32::from(bit), 1);
            }
            pending.clear();
        }
        if m > 1 {
            pending.push((m & 1) as u8);
            continue;
        }
        st.flush(o, ac);
        o.sym(ac, (run << 4) | 1);
        o.bits(u32::from(b[ZIGZAG[k]] >= 0), 1);
        for &bit in &pending {
            o.bits(u32::from(bit), 1);
        }
        pending.clear();
        run = 0;
    }
    if run > 0 || !pending.is_empty() {
        st.eobrun += 1;
        st.held.extend_from_slice(&pending);
        if st.eobrun == 0x7FFF || st.held.len() > 1000 - 64 + 1 {
            st.flush(o, ac);
        }
    }
}

/// Calls `f` for every block of the scan in stream order: interleaved MCUs for several
/// components, the component's own raster for one.
fn for_blocks(
    comps: &[Comp],
    scan: &Scan,
    mcus: (usize, usize),
    mut f: impl FnMut(usize, &[i16; 64]),
) {
    if let [only] = scan.comps[..] {
        let Some(c) = comps.get(only) else { return };
        for by in 0..c.real_h {
            for bx in 0..c.real_w {
                if let Some(b) = c.blocks.get(by * c.stride + bx) {
                    f(only, b);
                }
            }
        }
        return;
    }
    for my in 0..mcus.1 {
        for mx in 0..mcus.0 {
            for &ci in &scan.comps {
                let Some(c) = comps.get(ci) else { continue };
                for by in 0..c.vb {
                    for bx in 0..c.hb {
                        if let Some(b) = c.blocks.get((my * c.vb + by) * c.stride + mx * c.hb + bx)
                        {
                            f(ci, b);
                        }
                    }
                }
            }
        }
    }
}

fn run_scan<O: Out>(
    o: &mut O,
    comps: &[Comp],
    scan: &Scan,
    mcus: (usize, usize),
    progressive: bool,
) {
    let mut st = Prog::default();
    let mut last_ac = 2;
    for_blocks(comps, scan, mcus, |ci, b| {
        let t = comps.get(ci).map_or(0, |c| c.table);
        last_ac = 2 + t;
        if !progressive {
            if let Some(p) = st.pred.get_mut(ci) {
                let mut pred = *p;
                baseline_block(o, b, &mut pred, t);
                *p = pred;
            }
        } else if scan.ss == 0 {
            if scan.ah == 0 {
                let v = i32::from(b[0]) >> scan.al;
                if let Some(p) = st.pred.get_mut(ci) {
                    let (cat, bits) = category(v - *p);
                    *p = v;
                    o.sym(t, cat);
                    o.bits(bits, u32::from(cat));
                }
            } else {
                o.bits(u32::from((b[0] >> scan.al) & 1 != 0), 1);
            }
        } else if scan.ah == 0 {
            ac_first(o, &mut st, b, scan, 2 + t);
        } else {
            ac_refine(o, &mut st, b, scan, 2 + t);
        }
    });
    if progressive {
        st.flush(o, last_ac);
    }
}

/// Writes one scan: tables (optimised from a counting pass, or the standard ones already in
/// the file), the SOS header and the entropy-coded data.
pub(super) fn write_scan(
    out: &mut Vec<u8>,
    comps: &[Comp],
    scan: &Scan,
    mcus: (usize, usize),
    progressive: bool,
    standard: Option<&[HuffEnc; 4]>,
) {
    let mut owned;
    let tables = match standard {
        Some(t) => t,
        None => {
            let mut count = Count {
                freq: [[0; 257]; 4],
            };
            run_scan(&mut count, comps, scan, mcus, progressive);
            owned = [
                HuffEnc::new(&[0; 16], &[]),
                HuffEnc::new(&[0; 16], &[]),
                HuffEnc::new(&[0; 16], &[]),
                HuffEnc::new(&[0; 16], &[]),
            ];
            for (t, freq) in count.freq.iter().enumerate() {
                if freq.iter().take(256).all(|&f| f == 0) {
                    continue;
                }
                let (bits, vals) = optimal_table(freq);
                let mut body = vec![(((t >= 2) as u8) << 4) | (t & 1) as u8];
                body.extend_from_slice(&bits);
                body.extend_from_slice(&vals);
                segment(out, 0xC4, &body);
                if let Some(slot) = owned.get_mut(t) {
                    *slot = HuffEnc::new(&bits, &vals);
                }
            }
            &owned
        }
    };
    let mut sos = vec![scan.comps.len() as u8];
    for &ci in &scan.comps {
        let t = comps.get(ci).map_or(0, |c| c.table) as u8;
        sos.extend_from_slice(&[comps.get(ci).map_or(ci as u8 + 1, |c| c.id), (t << 4) | t]);
    }
    sos.extend_from_slice(&[scan.ss, scan.se, (scan.ah << 4) | scan.al]);
    segment(out, 0xDA, &sos);
    let mut sink = BitSink {
        out: std::mem::take(out),
        acc: 0,
        n: 0,
    };
    run_scan(
        &mut Write {
            sink: &mut sink,
            tables,
        },
        comps,
        scan,
        mcus,
        progressive,
    );
    sink.finish();
    *out = sink.out;
}

/// libjpeg's default progressive script (`jpeg_simple_progression`).
pub(super) fn progressive_script(colour: bool) -> Vec<Scan> {
    let s = |comps: &[usize], ss, se, ah, al| Scan {
        comps: comps.to_vec(),
        ss,
        se,
        ah,
        al,
    };
    if colour {
        vec![
            s(&[0, 1, 2], 0, 0, 0, 1),
            s(&[0], 1, 5, 0, 2),
            s(&[2], 1, 63, 0, 1),
            s(&[1], 1, 63, 0, 1),
            s(&[0], 6, 63, 0, 2),
            s(&[0], 1, 63, 2, 1),
            s(&[0, 1, 2], 0, 0, 1, 0),
            s(&[2], 1, 63, 1, 0),
            s(&[1], 1, 63, 1, 0),
            s(&[0], 1, 63, 1, 0),
        ]
    } else {
        vec![
            s(&[0], 0, 0, 0, 1),
            s(&[0], 1, 5, 0, 2),
            s(&[0], 6, 63, 0, 2),
            s(&[0], 1, 63, 2, 1),
            s(&[0], 0, 0, 1, 0),
            s(&[0], 1, 63, 1, 0),
        ]
    }
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    let target = if e8.layout.is_gray() {
        Layout::Gray
    } else {
        Layout::Rgb
    };
    let (e, n) = ops::convert_layout(&e8, target, opts.background);
    notes.extend(n);
    let (w, h) = (e.width as usize, e.height as usize);
    if w > 0xFFFF || h > 0xFFFF || w == 0 || h == 0 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "JPEG images are 1 to 65535 pixels on a side",
        ));
    }
    let colour = target == Layout::Rgb;
    let (hs, vs) = match (colour, opts.subsampling) {
        (false, _) | (_, Subsampling::S444) => (1, 1),
        (true, Subsampling::S422) => (2, 1),
        (true, Subsampling::S420) => (2, 2),
    };
    let (mcu_w, mcu_h) = (8 * hs, 8 * vs);
    let (pw, ph) = (w.div_ceil(mcu_w) * mcu_w, h.div_ceil(mcu_h) * mcu_h);

    // Planes at full resolution, then chroma reduced.
    let mut planes: Vec<Plane> = Vec::new();
    if colour {
        let (mut y_p, mut cb_p, mut cr_p) = (
            Vec::with_capacity(w * h),
            Vec::with_capacity(w * h),
            Vec::with_capacity(w * h),
        );
        for px in e.data.as_chunks::<3>().0 {
            let (r, g, b) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
            // jccolor.c: FIX(0.299)=19595, FIX(0.587)=38470, FIX(0.114)=7471, ...
            y_p.push(((19595 * r + 38470 * g + 7471 * b + 32768) >> 16) as u8);
            let off = (128 << 16) + 32767;
            cb_p.push(((-11059 * r - 21709 * g + 32768 * b + off) >> 16).clamp(0, 255) as u8);
            cr_p.push(((32768 * r - 27439 * g - 5329 * b + off) >> 16).clamp(0, 255) as u8);
        }
        planes.push(Plane { data: y_p, w, h }.padded(pw, ph));
        for c in [cb_p, cr_p] {
            let full = Plane { data: c, w, h }.padded(pw, ph);
            planes.push(if hs * vs > 1 {
                downsample(&full, hs, vs, pw / hs, ph / vs)
            } else {
                full
            });
        }
    } else {
        planes.push(
            Plane {
                data: e.data.clone(),
                w,
                h,
            }
            .padded(pw, ph),
        );
    }

    let q_luma = scaled_table(&STD_LUMA_Q, opts.quality);
    let q_chroma = scaled_table(&STD_CHROMA_Q, opts.quality);

    // ---- transform and quantise every block
    let mut comps: Vec<Comp> = Vec::new();
    let ac_len = [
        HuffEnc::new(&STD_AC_LUMA.bits, STD_AC_LUMA.vals).size,
        HuffEnc::new(&STD_AC_CHROMA.bits, STD_AC_CHROMA.vals).size,
    ];
    let t_lambda = trellis::lambda(&q_luma);
    let mut fcoef = [0i32; 64];
    for (ci, plane) in planes.iter().enumerate() {
        let (hb, vb) = if ci == 0 { (hs, vs) } else { (1, 1) };
        let q = if ci == 0 { &q_luma } else { &q_chroma };
        let (gw, gh) = (plane.w / 8, plane.h / 8);
        let mut blocks = Vec::with_capacity(gw * gh);
        for by in 0..gh {
            for bx in 0..gw {
                dct::fdct(&plane.block(bx, by), &mut fcoef);
                let qc = if opts.trellis {
                    trellis::quantise(&fcoef, q, &ac_len[usize::from(ci > 0)], t_lambda)
                } else {
                    quantise(&fcoef, q)
                };
                let mut b = [0i16; 64];
                for (o, &v) in b.iter_mut().zip(&qc) {
                    *o = v.clamp(-32768, 32767) as i16;
                }
                blocks.push(b);
            }
        }
        comps.push(Comp {
            id: ci as u8 + 1,
            blocks,
            stride: gw,
            hb,
            vb,
            real_w: (w * hb).div_ceil(hs).div_ceil(8),
            real_h: (h * vb).div_ceil(vs).div_ceil(8),
            table: usize::from(ci > 0),
        });
    }

    // ---- headers
    let mut out = vec![0xFF, 0xD8];
    let (units, dx, dy) = match img.metadata.dpi {
        Some((x, y)) if x >= 1.0 && y >= 1.0 => (1u8, x.round() as u16, y.round() as u16),
        _ => (0, 1, 1),
    };
    let mut jfif = b"JFIF\0\x01\x01".to_vec();
    jfif.push(units);
    jfif.extend_from_slice(&dx.to_be_bytes());
    jfif.extend_from_slice(&dy.to_be_bytes());
    jfif.extend_from_slice(&[0, 0]);
    segment(&mut out, 0xE0, &jfif);
    if let Some(icc) = &img.metadata.icc_profile {
        let chunks: Vec<&[u8]> = icc.chunks(65519 - 14).collect();
        if chunks.len() < 256 {
            for (i, c) in chunks.iter().enumerate() {
                let mut body = b"ICC_PROFILE\0".to_vec();
                body.extend_from_slice(&[(i + 1) as u8, chunks.len() as u8]);
                body.extend_from_slice(c);
                segment(&mut out, 0xE2, &body);
            }
        }
    }
    let dqt = |id: u8, q: &[u16; 64]| {
        let mut b = vec![id];
        b.extend(ZIGZAG.iter().map(|&z| q.get(z).copied().unwrap_or(1) as u8));
        b
    };
    segment(&mut out, 0xDB, &dqt(0, &q_luma));
    if colour {
        segment(&mut out, 0xDB, &dqt(1, &q_chroma));
    }
    let mut sof = vec![8];
    sof.extend_from_slice(&(h as u16).to_be_bytes());
    sof.extend_from_slice(&(w as u16).to_be_bytes());
    if colour {
        sof.extend_from_slice(&[3, 1, ((hs << 4) | vs) as u8, 0, 2, 0x11, 1, 3, 0x11, 1]);
    } else {
        sof.extend_from_slice(&[1, 1, 0x11, 0]);
    }
    segment(&mut out, if opts.progressive { 0xC2 } else { 0xC0 }, &sof);

    // ---- scans
    let mcus = (pw / mcu_w, ph / mcu_h);
    if opts.progressive {
        for scan in progressive_script(colour) {
            write_scan(&mut out, &comps, &scan, mcus, true, None);
        }
    } else {
        let all = Scan {
            comps: (0..comps.len()).collect(),
            ss: 0,
            se: 63,
            ah: 0,
            al: 0,
        };
        if opts.optimize {
            write_scan(&mut out, &comps, &all, mcus, false, None);
        } else {
            let dht = |class_id: u8, s: &HuffSpec| {
                let mut b = vec![class_id];
                b.extend_from_slice(&s.bits);
                b.extend_from_slice(s.vals);
                b
            };
            segment(&mut out, 0xC4, &dht(0x00, &STD_DC_LUMA));
            segment(&mut out, 0xC4, &dht(0x10, &STD_AC_LUMA));
            segment(&mut out, 0xC4, &dht(0x01, &STD_DC_CHROMA));
            segment(&mut out, 0xC4, &dht(0x11, &STD_AC_CHROMA));
            let std = [
                HuffEnc::new(&STD_DC_LUMA.bits, STD_DC_LUMA.vals),
                HuffEnc::new(&STD_DC_CHROMA.bits, STD_DC_CHROMA.vals),
                HuffEnc::new(&STD_AC_LUMA.bits, STD_AC_LUMA.vals),
                HuffEnc::new(&STD_AC_CHROMA.bits, STD_AC_CHROMA.vals),
            ];
            write_scan(&mut out, &comps, &all, mcus, false, Some(&std));
        }
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok((out, notes))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::jpeg::decode;
    use oc_core::{Limits, NoProgress};

    fn source() -> Image {
        // The libjpeg-decoded picture from the decoder fixtures serves as a natural-looking input.
        let dir = format!("{}/tests/data/jpeg", env!("CARGO_MANIFEST_DIR"));
        let data = std::fs::read(format!("{dir}/q100.jpg")).unwrap();
        decode(&data, &Limits::default(), &NoProgress).unwrap().0
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| f64::from(x.abs_diff(y)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        if mse == 0.0 {
            99.0
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        }
    }

    fn roundtrip(img: &Image, opts: &EncodeOptions) -> (Vec<u8>, Image) {
        let (bytes, _) = encode(img, opts).unwrap();
        let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        (bytes, back)
    }

    /// Sizes and PSNR that libjpeg 6.2 (via Pillow 12) produced for this exact input:
    /// (quality, 4:4:4 bytes, 4:4:4 dB, 4:2:0 bytes, 4:2:0 dB).
    const LIBJPEG: [(u8, usize, f64, usize, f64); 4] = [
        (95, 21193, 35.9, 11879, 21.7),
        (75, 8758, 23.8, 5556, 20.5),
        (40, 4251, 20.6, 3179, 19.4),
        (10, 1734, 18.6, 1461, 18.0),
    ];

    #[test]
    fn trellis_is_smaller_at_the_same_look() {
        let img = source();
        for (q, sub) in [(75, Subsampling::S420), (90, Subsampling::S444)] {
            let plain = EncodeOptions {
                quality: q,
                subsampling: sub,
                ..EncodeOptions::default()
            };
            let (a, a_img) = roundtrip(&img, &plain);
            let (b, b_img) = roundtrip(
                &img,
                &EncodeOptions {
                    trellis: true,
                    ..plain
                },
            );
            assert!(b.len() < a.len(), "q{q}: {} vs {}", b.len(), a.len());
            let (pa, pb) = (psnr(&img.data, &a_img.data), psnr(&img.data, &b_img.data));
            assert!(pb > pa - 3.0, "q{q}: {pb} dB vs {pa} dB");
        }
    }

    #[test]
    fn size_and_quality_track_libjpeg() {
        let img = source();
        for (q, s444, db444, s420, db420) in LIBJPEG {
            for (ss, size, db) in [
                (Subsampling::S444, s444, db444),
                (Subsampling::S420, s420, db420),
            ] {
                let (bytes, back) = roundtrip(
                    &img,
                    &EncodeOptions {
                        quality: q,
                        subsampling: ss,
                        ..Default::default()
                    },
                );
                let ours = psnr(&img.data, &back.data);
                let ratio = bytes.len() as f64 / size as f64;
                assert!(
                    (0.95..=1.05).contains(&ratio),
                    "q{q} {ss:?}: {} vs {size} bytes",
                    bytes.len()
                );
                assert!((ours - db).abs() < 0.3, "q{q} {ss:?}: {ours:.1} dB vs {db}");
            }
        }
    }

    #[test]
    fn odd_sizes_and_gray() {
        for (w, h) in [(1u32, 1u32), (7, 5), (16, 16), (17, 33), (200, 3)] {
            let d: Vec<u8> = (0..w * h * 3).map(|i| (i * 7) as u8).collect();
            let img = Image::new(w, h, Layout::Rgb, Depth::Eight, d).unwrap();
            for ss in [Subsampling::S444, Subsampling::S422, Subsampling::S420] {
                let (_, back) = roundtrip(
                    &img,
                    &EncodeOptions {
                        subsampling: ss,
                        ..Default::default()
                    },
                );
                assert_eq!((back.width, back.height, back.layout), (w, h, Layout::Rgb));
            }
        }
        let g = ops::convert_layout(&source(), Layout::Gray, [0, 0, 0]).0;
        let (_, back) = roundtrip(&g, &EncodeOptions::default());
        assert_eq!(back.layout, Layout::Gray);
        assert!(psnr(&g.data, &back.data) > 30.0);
    }

    #[test]
    fn every_entropy_mode_decodes_to_the_same_pixels() {
        let colour = source();
        let gray = ops::convert_layout(&colour, Layout::Gray, [0, 0, 0]).0;
        let odd = {
            let d: Vec<u8> = (0..37 * 29 * 3)
                .map(|i| ((i * 31) ^ (i >> 3)) as u8)
                .collect();
            Image::new(37, 29, Layout::Rgb, Depth::Eight, d).unwrap()
        };
        for img in [&colour, &gray, &odd] {
            for ss in [Subsampling::S444, Subsampling::S422, Subsampling::S420] {
                let base = EncodeOptions {
                    subsampling: ss,
                    ..Default::default()
                };
                let (plain, reference) = roundtrip(img, &base);
                let (opt, a) = roundtrip(
                    img,
                    &EncodeOptions {
                        optimize: true,
                        ..base
                    },
                );
                let (prog, b) = roundtrip(
                    img,
                    &EncodeOptions {
                        progressive: true,
                        ..base
                    },
                );
                assert_eq!(a.data, reference.data, "optimised {ss:?}");
                assert_eq!(b.data, reference.data, "progressive {ss:?}");
                if img.width > 100 {
                    assert!(opt.len() < plain.len(), "{} !< {}", opt.len(), plain.len());
                    assert!(prog.len() < plain.len());
                }
            }
        }
    }

    #[test]
    fn optimal_tables_obey_the_length_limit() {
        // Fibonacci-like counts force very deep trees.
        let mut counts = [0u64; 257];
        let (mut a, mut b) = (1u64, 1u64);
        for c in counts.iter_mut().take(40) {
            *c = a;
            (a, b) = (b, a + b);
        }
        let (bits, vals) = optimal_table(&counts);
        assert_eq!(vals.len(), 40);
        assert_eq!(bits.iter().map(|&b| usize::from(b)).sum::<usize>(), 40);
        // Kraft sum stays at or below one with room for the reserved all-ones code.
        let kraft: f64 = bits
            .iter()
            .enumerate()
            .map(|(i, &n)| f64::from(n) / f64::from(1u32 << (i + 1)))
            .sum();
        assert!(kraft < 1.0, "{kraft}");
    }

    #[test]
    fn alpha_is_flattened_with_a_note_and_metadata_survives() {
        let mut img = Image::new(
            9,
            9,
            Layout::Rgba,
            Depth::Eight,
            [10, 200, 30, 0].repeat(81),
        )
        .unwrap();
        img.metadata.dpi = Some((300.0, 300.0));
        img.metadata.icc_profile = Some(vec![7; 70_000]); // spans two APP2 segments
        let (bytes, notes) = encode(
            &img,
            &EncodeOptions {
                background: [255, 0, 0],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(notes.has_loss());
        let (back, _) = decode(&bytes, &Limits::default(), &NoProgress).unwrap();
        assert_eq!(back.metadata.dpi, Some((300.0, 300.0)));
        assert_eq!(back.metadata.icc_profile, img.metadata.icc_profile);
        assert!(back
            .data
            .as_chunks::<3>()
            .0
            .iter()
            .all(|p| p[0] > 240 && p[1] < 15));
    }
}
