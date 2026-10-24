//! Vorbis I encoder.
//!
//! One block size and one mode; floor type 1 follows a smoothed, quality-shaped spectral
//! envelope; residue type 1 carries the envelope-normalised spectrum quantised on a uniform
//! grid, six partition classes, and square-polar channel coupling for stereo. The Huffman
//! codebooks are built from the symbol statistics of the file being encoded (two passes), so
//! every stream carries a setup header that fits its own content.
use super::dec::{assign_codewords, ilog, render_floor, Floor1};
use super::mdct::Imdct;
use crate::tags::write_vorbis_comment;
use crate::{dsp, ogg, Audio};
use oc_core::{Error, ErrorCode, Note, Notes, Result, Severity};

#[derive(Debug, Clone)]
pub struct Options {
    /// -1 (smallest) to 10 (best), as in libvorbis.
    pub quality: f32,
    pub vendor: String,
    pub serial: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            quality: 4.0,
            vendor: "OpenConverter".into(),
            serial: 0x4F43_5642,
        }
    }
}

/// LSB-first bit writer.
#[derive(Default)]
struct Bw {
    buf: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bw {
    fn put(&mut self, v: u32, bits: u32) {
        debug_assert!(bits <= 32);
        if bits == 0 {
            return;
        }
        let v = if bits == 32 {
            u64::from(v)
        } else {
            u64::from(v) & ((1u64 << bits) - 1)
        };
        self.acc |= v << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.buf.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// A Huffman codeword: its first bit is the most significant.
    fn code(&mut self, word: u32, len: u32) {
        let mut rev = 0u32;
        for b in 0..len {
            rev |= ((word >> (len - 1 - b)) & 1) << b;
        }
        self.put(rev, len);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.buf.push(self.acc as u8);
        }
        self.buf
    }
}

/// The Vorbis 21-bit-mantissa float for `v`.
fn float32_pack(v: f64) -> u32 {
    if v == 0.0 {
        return 0;
    }
    let sign = if v < 0.0 { 0x8000_0000u32 } else { 0 };
    let a = v.abs();
    let e = a.log2().floor() as i32 + 1; // a = m * 2^e with m in [0.5, 1)
    let mant = (a / 2f64.powi(e) * 2f64.powi(21)).round() as u32;
    let (mant, e) = if mant >= 1 << 21 {
        (mant >> 1, e + 1)
    } else {
        (mant, e)
    };
    sign | (((e + 767) as u32) << 21) | mant
}

fn float32_unpack(x: u32) -> f64 {
    let mant = f64::from(x & 0x1F_FFFF);
    let exp = ((x & 0x7FE0_0000) >> 21) as i32;
    (if x & 0x8000_0000 != 0 { -mant } else { mant }) * 2f64.powi(exp - 788)
}

/// Huffman code lengths for `freq` (zero-frequency symbols get length 0), at most `max` bits.
fn huffman_lengths(freq: &[u64], max: u8) -> Vec<u8> {
    let used: Vec<usize> = (0..freq.len()).filter(|&i| freq[i] > 0).collect();
    let mut lens = vec![0u8; freq.len()];
    match used.len() {
        0 => return lens,
        1 => {
            lens[used[0]] = 1;
            return lens;
        }
        _ => {}
    }
    let mut weights: Vec<u64> = used.iter().map(|&i| freq[i]).collect();
    loop {
        // Classic two-queue construction over sorted weights.
        let n = used.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| weights[i]);
        let mut parent = vec![usize::MAX; 2 * n];
        let mut w: Vec<u64> = order.iter().map(|&i| weights[i]).collect();
        w.resize(2 * n, u64::MAX);
        let (mut a, mut b) = (0usize, n);
        let mut next = n;
        while next < 2 * n - 1 {
            let pick = |a: &mut usize, b: &mut usize| -> usize {
                if *a < n && (*b >= next || w[*a] <= w[*b]) {
                    *a += 1;
                    *a - 1
                } else {
                    *b += 1;
                    *b - 1
                }
            };
            let x = pick(&mut a, &mut b);
            let y = pick(&mut a, &mut b);
            w[next] = w[x] + w[y];
            parent[x] = next;
            parent[y] = next;
            next += 1;
        }
        let mut depth = vec![0u32; 2 * n];
        for i in (0..2 * n - 2).rev() {
            if parent[i] != usize::MAX {
                depth[i] = depth[parent[i]] + 1;
            }
        }
        let longest = (0..n).map(|i| depth[i]).max().unwrap_or(0);
        if longest <= u32::from(max) {
            for (pos, &o) in order.iter().enumerate() {
                lens[used[o]] = depth[pos] as u8;
            }
            return lens;
        }
        // Too deep: flatten the distribution and retry.
        for x in &mut weights {
            *x = (*x >> 1).max(1);
        }
    }
}

/// One codebook as the setup header describes it.
struct Book {
    dims: u32,
    lens: Vec<u8>,
    /// For vector books: values per dimension is 2*m+1 and delta is the step.
    vq: Option<(u32, f64)>,
    words: Vec<u32>,
}

impl Book {
    fn new(dims: u32, lens: Vec<u8>, vq: Option<(u32, f64)>) -> Result<Self> {
        let words = assign_codewords(&lens)?;
        Ok(Self {
            dims,
            lens,
            vq,
            words,
        })
    }
    fn emit(&self, w: &mut Bw, entry: usize) {
        w.code(self.words[entry], u32::from(self.lens[entry]));
    }
    fn write_header(&self, w: &mut Bw) {
        w.put(0x56_4342, 24);
        w.put(self.dims, 16);
        w.put(self.lens.len() as u32, 24);
        w.put(0, 1); // not ordered
        w.put(1, 1); // sparse
        for &l in &self.lens {
            if l == 0 {
                w.put(0, 1);
            } else {
                w.put(1, 1);
                w.put(u32::from(l) - 1, 5);
            }
        }
        match self.vq {
            None => w.put(0, 4),
            Some((m, delta)) => {
                w.put(1, 4);
                let d = float32_pack(delta);
                let min = float32_pack(-(f64::from(m)) * float32_unpack(d));
                w.put(min, 32);
                w.put(d, 32);
                let values = 2 * m + 1;
                let bits = ilog(values - 1).max(1);
                w.put(bits - 1, 4);
                w.put(0, 1);
                for i in 0..values {
                    w.put(i, bits);
                }
            }
        }
    }
}

// ---- layout of the stream

const PART: usize = 16; // residue partition size
const NCLASS: usize = 6;
// (max magnitude, dimensions) of the vector book used by each class (class 0 has none).
const CLASS_BOOKS: [(u32, u32); NCLASS - 1] = [(1, 4), (2, 2), (4, 2), (12, 1), (255, 1)];
const FLOOR_CLASS_DIM: usize = 4;

fn floor_positions(half: usize) -> Vec<u32> {
    // Roughly Bark-spaced points, expressed for a 1024-bin block and scaled to `half`.
    const BASE: [u32; 28] = [
        4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        384, 448, 512, 640, 768, 896, 960,
    ];
    let mut v: Vec<u32> = BASE
        .iter()
        .map(|&b| (u64::from(b) * half as u64 / 1024) as u32)
        .collect();
    v.sort_unstable();
    v.dedup();
    // The count must be a multiple of the class dimension; trim from the low end of duplicates.
    while !v.len().is_multiple_of(FLOOR_CLASS_DIM) {
        v.remove(1);
    }
    v
}

/// Order the points so that every new one lies between two already known points.
fn bisect_order(sorted: &[u32]) -> Vec<u32> {
    fn rec(s: &[u32], out: &mut Vec<u32>) {
        if s.is_empty() {
            return;
        }
        let mid = s.len() / 2;
        out.push(s[mid]);
        rec(&s[..mid], out);
        rec(&s[mid + 1..], out);
    }
    // Breadth-first so coarse points come first.
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(sorted);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if s.is_empty() {
            continue;
        }
        let mid = s.len() / 2;
        out.push(s[mid]);
        queue.push_back(&s[..mid]);
        queue.push_back(&s[mid + 1..]);
    }
    let _ = rec;
    out
}

struct Params {
    block: usize,
    cutoff_bins: usize,
    delta: f64,
    dead_zone: f64,
    tilt_db: f64,
}

fn params(rate: u32, q: f32) -> Params {
    let q = f64::from(q.clamp(-1.0, 10.0));
    // Block of about 46 ms, a power of two between 256 and 2048.
    let mut block = 256;
    while block < 2048 && (block as f64) < f64::from(rate) * 0.04 {
        block *= 2;
    }
    const CUT: [f64; 12] = [
        13000.0, 14500.0, 15500.0, 16500.0, 17500.0, 18500.0, 19500.0, 20500.0, 21000.0, 21500.0,
        22000.0, 23000.0,
    ];
    let t = q + 1.0;
    let i = (t.floor() as usize).min(10);
    let f = t - i as f64;
    let cutoff = CUT[i] + (CUT[i + 1] - CUT[i]) * f;
    let nyq = f64::from(rate) / 2.0;
    let cutoff_bins =
        (((cutoff / nyq) * (block / 2) as f64).ceil() as usize).clamp(PART, block / 2);
    Params {
        block,
        cutoff_bins,
        delta: 0.9 * 2f64.powf(-(q + 1.0) * 0.33),
        dead_zone: 0.12,
        tilt_db: 0.0,
    }
}

/// Vorbis channel order to ours (the inverse of the decoder's table).
fn vorbis_order(ch: usize) -> Vec<usize> {
    match ch {
        3 => vec![0, 2, 1],
        5 => vec![0, 2, 1, 3, 4],
        6 => vec![0, 2, 1, 4, 5, 3],
        7 => vec![0, 2, 1, 5, 6, 4, 3],
        8 => vec![0, 2, 1, 6, 7, 4, 5, 3],
        n => (0..n).collect(),
    }
}

/// Inverse of the decoder's value reconstruction: the coded value that makes the decoder
/// arrive at `target` from `predicted`.
fn floor_val(predicted: i32, target: i32, range: i32) -> i32 {
    let d = target - predicted;
    if d == 0 {
        return 0;
    }
    let highroom = range - predicted;
    let lowroom = predicted;
    let room = if highroom < lowroom {
        highroom * 2
    } else {
        lowroom * 2
    };
    let sym = if d > 0 { 2 * d } else { -2 * d - 1 };
    if sym < room {
        sym
    } else if highroom > lowroom {
        d + lowroom
    } else {
        -d + highroom - 1
    }
}

struct FloorCoding {
    y0: i32,
    y1: i32,
    vals: Vec<i32>,
}

struct Block {
    /// Per channel floor data, or `None` when the whole block is silent.
    floors: Option<Vec<FloorCoding>>,
    /// Per coded vector (channel or coupled channel) the quantised values below the cutoff.
    vectors: Vec<Vec<i32>>,
}

fn classify(vals: &[i32]) -> usize {
    let m = vals.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
    match m {
        0 => 0,
        1 => 1,
        2 => 2,
        3..=4 => 3,
        5..=12 => 4,
        _ => 5,
    }
}

pub fn write(audio: &Audio, opts: &Options, notes: &mut Notes) -> Result<Vec<u8>> {
    let ch = usize::from(audio.channels);
    if !(1..=8).contains(&ch) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "Vorbis holds 1 to 8 channels",
        ));
    }
    if !(2000..=200_000).contains(&audio.sample_rate) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "Vorbis encoding needs a sample rate between 2 kHz and 200 kHz",
        ));
    }
    let p = params(audio.sample_rate, opts.quality);
    let n = p.block;
    let half = n / 2;
    let pcm = dsp::to_float(&audio.samples);
    let frames = pcm.len() / ch;
    let order = vorbis_order(ch);
    let chans: Vec<Vec<f32>> = (0..ch)
        .map(|c| (0..frames).map(|i| pcm[i * ch + order[c]]).collect())
        .collect();

    // Floor layout.
    let positions = floor_positions(half);
    let mut xlist = vec![0u32, half as u32];
    xlist.extend(bisect_order(&positions));
    let floor_multiplier = 2u32;
    let nclass_parts = (xlist.len() - 2) / FLOOR_CLASS_DIM;
    let floor = Floor1::new(
        vec![0; nclass_parts],
        vec![FLOOR_CLASS_DIM],
        vec![0],
        vec![0],
        vec![vec![0]], // subclass book 0 is book number 0 (the floor value book)
        floor_multiplier,
        xlist.clone(),
    )?;
    let range = floor.range();
    // X values are read with `rangebits` bits where X[1] = 1 << rangebits must equal `half`.
    let rb = half.trailing_zeros();

    // Analysis.
    let imdct = Imdct::new(n);
    let slope: Vec<f32> = (0..half)
        .map(|i| {
            let x = (i as f64 + 0.5) / half as f64 * std::f64::consts::FRAC_PI_2;
            (std::f64::consts::FRAC_PI_2 * x.sin().powi(2)).sin() as f32
        })
        .collect();
    let mut window = vec![0.0f32; n];
    for i in 0..half {
        window[i] = slope[i];
        window[n - 1 - i] = slope[i];
    }
    let hop = half;
    let nblocks = frames.div_ceil(hop) + 1;
    let sorted_pos: Vec<u32> = {
        let mut v = xlist.clone();
        v.sort_unstable();
        v
    };
    let ln_first = (1.064_986_3e-7_f64).ln();
    let amp_to_index = |a: f64| -> f64 { 255.0 + 255.0 * a.max(1e-12).ln() / (-ln_first) };
    let coupled = ch == 2;
    let mut blocks: Vec<Block> = Vec::with_capacity(nblocks);
    let mut buf = vec![0.0f32; n];
    for k in 0..nblocks {
        let center = k * hop; // block k is centred on sample k*hop
        let mut spectra: Vec<Vec<f32>> = Vec::with_capacity(ch);
        let mut silent = true;
        for c in chans.iter() {
            for (i, b) in buf.iter_mut().enumerate() {
                let t = (center + i) as isize - half as isize;
                *b = if t >= 0 && (t as usize) < frames {
                    c[t as usize] * window[i]
                } else {
                    0.0
                };
            }
            let mut spec = vec![0.0f32; half];
            imdct.forward(&buf, &mut spec);
            for v in spec[p.cutoff_bins..].iter_mut() {
                *v = 0.0;
            }
            if spec.iter().any(|v| v.abs() > 1e-7) {
                silent = false;
            }
            spectra.push(spec);
        }
        if silent {
            blocks.push(Block {
                floors: None,
                vectors: Vec::new(),
            });
            continue;
        }
        let mut floors = Vec::with_capacity(ch);
        let mut quant: Vec<Vec<i32>> = Vec::with_capacity(ch);
        for spec in &spectra {
            // Envelope at every point: RMS over the band halfway to its neighbours.
            let np = sorted_pos.len();
            let mut env_idx = vec![0i32; xlist.len()];
            for (si, &pos) in sorted_pos.iter().enumerate() {
                let lo = if si == 0 {
                    0
                } else {
                    (sorted_pos[si - 1] + pos) / 2
                } as usize;
                let hi = if si + 1 == np {
                    half
                } else {
                    ((pos + sorted_pos[si + 1]) / 2) as usize
                };
                let hi = hi.min(half).max(lo + 1);
                let lo = lo.min(half - 1);
                let e: f64 = spec[lo..hi.min(half)]
                    .iter()
                    .map(|&v| f64::from(v).powi(2))
                    .sum::<f64>()
                    / (hi - lo) as f64;
                let f_rel = (pos as f64 / half as f64).min(1.0);
                // Shape: more noise allowed toward high frequencies.
                let shaped = e.sqrt() * 10f64.powf(p.tilt_db * f_rel.powf(1.5) / 20.0);
                // Quantisation noise follows the spectral envelope only partly (exponent ALPHA): a flat
                // floor is best for SNR, a fully shaped one for the least audible noise.
                const ALPHA: f64 = 0.35;
                const REFERENCE: f64 = 0.003;
                let (alpha, refi) = (ALPHA, REFERENCE);
                let idx = alpha * amp_to_index(shaped) + (1.0 - alpha) * amp_to_index(refi);
                let y = (idx / f64::from(floor_multiplier)).round() as i32;
                // Map back onto the point's slot in the (unsorted) list.
                let slot = xlist.iter().position(|&x| x == pos).unwrap_or(0);
                env_idx[slot] = y.clamp(0, range - 1);
            }
            // Closed-loop coding of the values in list order.
            let mut y = env_idx.clone();
            let mut fy = vec![0i32; xlist.len()];
            fy[0] = y[0];
            fy[1] = y[1];
            let mut vals = Vec::with_capacity(xlist.len() - 2);
            for i in 2..xlist.len() {
                let (lo, hi) = (floor.low[i], floor.high[i]);
                let predicted = super::dec::render_point(
                    xlist[lo] as i32,
                    fy[lo],
                    xlist[hi] as i32,
                    fy[hi],
                    xlist[i] as i32,
                );
                let target = if (env_idx[i] - predicted).abs() <= 1 {
                    predicted
                } else {
                    env_idx[i]
                };
                let v = floor_val(predicted, target, range);
                vals.push(v);
                fy[i] = if v == 0 { predicted } else { target };
                y[i] = v;
            }
            y[0] = env_idx[0];
            y[1] = env_idx[1];
            let pts = floor.points(&y);
            let curve = render_floor(&pts, half);
            let mut q = vec![0i32; p.cutoff_bins];
            for (i, qv) in q.iter_mut().enumerate() {
                let r = f64::from(spec[i]) / f64::from(curve[i]) / p.delta;
                let m = (r.abs() + 0.5 - p.dead_zone).floor().clamp(0.0, 255.0) as i32;
                *qv = if r < 0.0 { -m } else { m };
            }
            floors.push(FloorCoding {
                y0: y[0],
                y1: y[1],
                vals,
            });
            quant.push(q);
        }
        let vectors = if coupled {
            let (l, r) = (&quant[0], &quant[1]);
            let mut mag = vec![0i32; l.len()];
            let mut ang = vec![0i32; l.len()];
            for i in 0..l.len() {
                let (a, b) = (l[i], r[i]);
                if a.abs() > b.abs() {
                    mag[i] = a;
                    ang[i] = if a > 0 { a - b } else { b - a };
                } else {
                    mag[i] = b;
                    ang[i] = if b > 0 { a - b } else { b - a };
                }
            }
            // The angle can reach twice the magnitude; the largest book holds 255.
            for v in ang.iter_mut() {
                *v = (*v).clamp(-255, 255);
            }
            vec![mag, ang]
        } else {
            quant
        };
        blocks.push(Block {
            floors: Some(floors),
            vectors,
        });
    }

    // Statistics for the codebooks.
    let nparts = p.cutoff_bins.div_ceil(PART);
    let end = nparts * PART;
    let mut floor_freq = vec![0u64; range as usize];
    let mut class_freq = vec![0u64; NCLASS * NCLASS];
    let mut book_freq: Vec<Vec<u64>> = CLASS_BOOKS
        .iter()
        .map(|&(m, d)| vec![0u64; (2 * m as usize + 1).pow(d)])
        .collect();
    let entry_of = |vals: &[i32], m: u32, d: u32| -> usize {
        let base = 2 * m as usize + 1;
        let mut e = 0usize;
        let mut mul = 1usize;
        for k in 0..d as usize {
            e += (vals[k] + m as i32) as usize * mul;
            mul *= base;
        }
        e
    };
    // Per block: classes per vector per partition.
    let mut all_classes: Vec<Vec<Vec<usize>>> = Vec::with_capacity(blocks.len());
    for b in &blocks {
        let mut per_vec = Vec::new();
        if let Some(fl) = &b.floors {
            for f in fl {
                for &v in &f.vals {
                    floor_freq[v as usize] += 1;
                }
            }
            for v in &b.vectors {
                let mut padded = v.clone();
                padded.resize(end, 0);
                let cls: Vec<usize> = padded.chunks(PART).map(classify).collect();
                for (pi, chunk) in padded.chunks(PART).enumerate() {
                    let c = cls[pi];
                    if c > 0 {
                        let (m, d) = CLASS_BOOKS[c - 1];
                        for sub in chunk.chunks(d as usize) {
                            book_freq[c - 1][entry_of(sub, m, d)] += 1;
                        }
                    }
                }
                per_vec.push(cls);
            }
            // Class words: two partitions per codeword.
            for cls in &per_vec {
                for pair in cls.chunks(2) {
                    let a = pair[0];
                    let bb = pair.get(1).copied().unwrap_or(0);
                    class_freq[a * NCLASS + bb] += 1;
                }
            }
        }
        all_classes.push(per_vec);
    }
    // Make sure every book has at least two used entries so the tree is complete.
    let ensure = |f: &mut Vec<u64>| {
        let used = f.iter().filter(|&&x| x > 0).count();
        if used < 2 {
            for x in f.iter_mut().take(2) {
                *x += 1;
            }
        }
    };
    ensure(&mut floor_freq);
    ensure(&mut class_freq);
    for f in book_freq.iter_mut() {
        ensure(f);
    }
    let floor_book = Book::new(1, huffman_lengths(&floor_freq, 24), None)?;
    let class_book = Book::new(2, huffman_lengths(&class_freq, 24), None)?;
    let delta_q = float32_unpack(float32_pack(p.delta));
    let vq_books: Vec<Book> = CLASS_BOOKS
        .iter()
        .zip(&book_freq)
        .map(|(&(m, d), f)| Book::new(d, huffman_lengths(f, 24), Some((m, delta_q))))
        .collect::<Result<_>>()?;

    // Header packets.
    let mut id = Bw::default();
    id.put(1, 8);
    for b in b"vorbis" {
        id.put(u32::from(*b), 8);
    }
    id.put(0, 32);
    id.put(ch as u32, 8);
    id.put(audio.sample_rate, 32);
    id.put(0, 32);
    id.put(0, 32);
    id.put(0, 32);
    id.put(n.trailing_zeros(), 4);
    id.put(n.trailing_zeros(), 4);
    id.put(1, 1);
    let id = id.finish();

    let mut comment = vec![3u8];
    comment.extend_from_slice(b"vorbis");
    comment.extend_from_slice(&write_vorbis_comment(
        &opts.vendor,
        &audio.tags,
        &audio.pictures,
        true,
    ));
    comment.push(1); // framing bit

    let mut su = Bw::default();
    su.put(5, 8);
    for b in b"vorbis" {
        su.put(u32::from(*b), 8);
    }
    let books_total = 2 + vq_books.len();
    su.put(books_total as u32 - 1, 8);
    floor_book.write_header(&mut su);
    class_book.write_header(&mut su);
    for b in &vq_books {
        b.write_header(&mut su);
    }
    su.put(0, 6); // one time-domain transform
    su.put(0, 16);
    su.put(0, 6); // one floor
    su.put(1, 16);
    su.put(nclass_parts as u32, 5);
    for _ in 0..nclass_parts {
        su.put(0, 4);
    }
    su.put(FLOOR_CLASS_DIM as u32 - 1, 3);
    su.put(0, 2); // no subclasses
    su.put(1, 8); // subclass book 0 = book number 0, stored +1
    su.put(floor_multiplier - 1, 2);
    su.put(rb, 4);
    for &x in &xlist[2..] {
        su.put(x, rb);
    }
    su.put(0, 6); // one residue
    su.put(1, 16);
    su.put(0, 24);
    su.put(end as u32, 24);
    su.put(PART as u32 - 1, 24);
    su.put(NCLASS as u32 - 1, 6);
    su.put(1, 8); // class book number
    for c in 0..NCLASS {
        if c == 0 {
            su.put(0, 3);
            su.put(0, 1);
        } else {
            su.put(1, 3); // cascade bit 0: one pass
            su.put(0, 1);
        }
    }
    for c in 1..NCLASS {
        su.put(1 + c as u32, 8); // book numbers 2..=6
    }
    su.put(0, 6); // one mapping
    su.put(0, 16);
    su.put(0, 1); // one submap
    if coupled {
        su.put(1, 1);
        su.put(0, 8); // one coupling step
        su.put(0, 1); // magnitude channel 0 (ilog(1) = 1 bit)
        su.put(1, 1); // angle channel 1
    } else {
        su.put(0, 1);
    }
    su.put(0, 2);
    su.put(0, 8);
    su.put(0, 8); // floor 0
    su.put(0, 8); // residue 0
    su.put(0, 6); // one mode
    su.put(0, 1); // short block flag (blocks are one size)
    su.put(0, 16);
    su.put(0, 16);
    su.put(0, 8);
    su.put(1, 1);
    let setup = su.finish();

    let mut w = ogg::Writer::new(opts.serial);
    w.packet(&id, 0, true, false);
    // Comment and setup share pages; the first audio packet starts a new one.
    w.packet(&comment, 0, false, false);
    w.packet(&setup, 0, true, false);

    let floor_class_book = 0usize;
    let _ = floor_class_book;
    for (k, b) in blocks.iter().enumerate() {
        let mut bw = Bw::default();
        bw.put(0, 1); // audio packet
        match &b.floors {
            None => {
                for _ in 0..ch {
                    bw.put(0, 1);
                }
            }
            Some(fl) => {
                for f in fl {
                    bw.put(1, 1);
                    bw.put(f.y0 as u32, ilog(range as u32 - 1));
                    bw.put(f.y1 as u32, ilog(range as u32 - 1));
                    for &v in &f.vals {
                        floor_book.emit(&mut bw, v as usize);
                    }
                }
                let classes = &all_classes[k];
                let vecs: Vec<Vec<i32>> = b
                    .vectors
                    .iter()
                    .map(|v| {
                        let mut padded = v.clone();
                        padded.resize(end, 0);
                        padded
                    })
                    .collect();
                // Class words, then partitions, in the decoder's order.
                let mut part = 0;
                while part < nparts {
                    for cls in classes {
                        let a = cls[part];
                        let bb = cls.get(part + 1).copied().unwrap_or(0);
                        class_book.emit(&mut bw, a * NCLASS + bb);
                    }
                    for i in 0..2 {
                        if part + i >= nparts {
                            break;
                        }
                        for (vi, cls) in classes.iter().enumerate() {
                            let c = cls[part + i];
                            if c == 0 {
                                continue;
                            }
                            let (m, d) = CLASS_BOOKS[c - 1];
                            let chunk = &vecs[vi][(part + i) * PART..(part + i + 1) * PART];
                            for sub in chunk.chunks(d as usize) {
                                vq_books[c - 1].emit(&mut bw, entry_of(sub, m, d));
                            }
                        }
                    }
                    part += 2;
                }
            }
        }
        let granule = if k == 0 {
            0
        } else {
            ((k * hop) as i64).min(frames as i64)
        };
        let last = k + 1 == blocks.len();
        w.packet(
            &bw.finish(),
            if last { frames as i64 } else { granule },
            false,
            last,
        );
    }
    if opts.quality > 10.0 || opts.quality < -1.0 {
        notes.push(Note::new(
            Severity::Info,
            "vorbis.quality_clamped",
            "quality was limited to the range -1 to 10",
        ));
    }
    Ok(w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_values_invert_the_decoder() {
        for range in [64, 128, 256] {
            for p in 0..range {
                for t in 0..range {
                    let v = floor_val(p, t, range);
                    // The decoder's mapping.
                    let highroom = range - p;
                    let lowroom = p;
                    let room = if highroom < lowroom {
                        highroom * 2
                    } else {
                        lowroom * 2
                    };
                    let got = if v == 0 {
                        p
                    } else if v >= room {
                        if highroom > lowroom {
                            v - lowroom + p
                        } else {
                            p - v + highroom - 1
                        }
                    } else if v & 1 == 1 {
                        p - ((v + 1) >> 1)
                    } else {
                        p + (v >> 1)
                    };
                    assert_eq!(got, t, "range {range} predicted {p} target {t} val {v}");
                    assert!(v < range, "val {v} does not fit the book (range {range})");
                }
            }
        }
    }

    #[test]
    fn huffman_is_complete_and_bounded() {
        let f: Vec<u64> = (0..300)
            .map(|i| {
                if i % 7 == 0 {
                    0
                } else {
                    1 + (i * i) as u64 % 1000
                }
            })
            .collect();
        let l = huffman_lengths(&f, 24);
        let kraft: f64 = l
            .iter()
            .filter(|&&x| x > 0)
            .map(|&x| 2f64.powi(-i32::from(x)))
            .sum();
        assert!((kraft - 1.0).abs() < 1e-12, "{kraft}");
        assert!(l.iter().all(|&x| x <= 24));
        assert!(assign_codewords(&l).is_ok());
    }

    #[test]
    fn float_packing_round_trips() {
        for v in [0.5, 1.0, 0.9, 0.123, 7.0, 255.0, 1e-3, 0.074] {
            let x = float32_unpack(float32_pack(v));
            assert!((x - v).abs() <= v * 1e-6, "{v} -> {x}");
        }
    }
}
