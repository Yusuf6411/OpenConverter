//! VP8L (WebP lossless) bitstream decoder.
use super::prefix::{Bits, Huffman};
use oc_core::{Error, ErrorCode, Limits, Progress, Result};

const NUM_LITERAL: usize = 256;
const NUM_LENGTH: usize = 24;
const NUM_DISTANCE: usize = 40;
const CODE_LENGTH_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

/// (dx, dy) pairs for the first 120 distance codes; distance = dx + dy * width.
pub(crate) const DISTANCE_MAP: [(i8, i8); 120] = [
    (0, 1),
    (1, 0),
    (1, 1),
    (-1, 1),
    (0, 2),
    (2, 0),
    (1, 2),
    (-1, 2),
    (2, 1),
    (-2, 1),
    (2, 2),
    (-2, 2),
    (0, 3),
    (3, 0),
    (1, 3),
    (-1, 3),
    (3, 1),
    (-3, 1),
    (2, 3),
    (-2, 3),
    (3, 2),
    (-3, 2),
    (0, 4),
    (4, 0),
    (1, 4),
    (-1, 4),
    (4, 1),
    (-4, 1),
    (3, 3),
    (-3, 3),
    (2, 4),
    (-2, 4),
    (4, 2),
    (-4, 2),
    (0, 5),
    (3, 4),
    (-3, 4),
    (4, 3),
    (-4, 3),
    (5, 0),
    (1, 5),
    (-1, 5),
    (5, 1),
    (-5, 1),
    (2, 5),
    (-2, 5),
    (5, 2),
    (-5, 2),
    (4, 4),
    (-4, 4),
    (3, 5),
    (-3, 5),
    (5, 3),
    (-5, 3),
    (0, 6),
    (6, 0),
    (1, 6),
    (-1, 6),
    (6, 1),
    (-6, 1),
    (2, 6),
    (-2, 6),
    (6, 2),
    (-6, 2),
    (4, 5),
    (-4, 5),
    (5, 4),
    (-5, 4),
    (3, 6),
    (-3, 6),
    (6, 3),
    (-6, 3),
    (0, 7),
    (7, 0),
    (1, 7),
    (-1, 7),
    (5, 5),
    (-5, 5),
    (7, 1),
    (-7, 1),
    (4, 6),
    (-4, 6),
    (6, 4),
    (-6, 4),
    (2, 7),
    (-2, 7),
    (7, 2),
    (-7, 2),
    (3, 7),
    (-3, 7),
    (7, 3),
    (-7, 3),
    (5, 6),
    (-5, 6),
    (6, 5),
    (-6, 5),
    (8, 0),
    (4, 7),
    (-4, 7),
    (7, 4),
    (-7, 4),
    (8, 1),
    (8, 2),
    (6, 6),
    (-6, 6),
    (8, 3),
    (5, 7),
    (-5, 7),
    (7, 5),
    (-7, 5),
    (8, 4),
    (6, 7),
    (-6, 7),
    (7, 6),
    (-7, 6),
    (8, 5),
    (7, 7),
    (-7, 7),
    (8, 6),
    (8, 7),
];

/// Number of blocks of `1 << bits` pixels needed to cover `size` pixels.
pub(crate) fn sub_size(size: u32, bits: u32) -> u32 {
    (size + (1 << bits) - 1) >> bits
}

pub(crate) fn prefix_value(symbol: u32, br: &mut Bits) -> u32 {
    if symbol < 4 {
        return symbol + 1;
    }
    let extra = (symbol - 2) >> 1;
    let offset = (2 + (symbol & 1)) << extra;
    offset + br.read(extra) + 1
}

fn distance_from_code(code: u32, xsize: u32) -> u32 {
    if code > 120 {
        return code - 120;
    }
    let (dx, dy) = DISTANCE_MAP
        .get(code as usize - 1)
        .copied()
        .unwrap_or((0, 1));
    let d = i64::from(dx) + i64::from(dy) * i64::from(xsize);
    d.max(1) as u32
}

pub(crate) fn cache_hash(argb: u32, bits: u32) -> usize {
    (argb.wrapping_mul(0x1e35_a7bd) >> (32 - bits)) as usize
}

struct Group {
    green: Huffman,
    red: Huffman,
    blue: Huffman,
    alpha: Huffman,
    dist: Huffman,
}

fn read_code_lengths(
    br: &mut Bits,
    code_len_code: &Huffman,
    num_symbols: usize,
) -> Result<Vec<u8>> {
    let mut lengths = vec![0u8; num_symbols];
    let max_symbol = if br.read(1) == 1 {
        let nbits = 2 + 2 * br.read(3);
        let m = 2 + br.read(nbits) as usize;
        if m > num_symbols {
            return Err(Error::corrupt("VP8L code length run too long"));
        }
        m
    } else {
        num_symbols
    };
    let (mut symbol, mut remaining, mut prev) = (0usize, max_symbol, 8u8);
    while symbol < num_symbols {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        br.check()?;
        let code = code_len_code.read(br) as u8;
        if code < 16 {
            if let Some(l) = lengths.get_mut(symbol) {
                *l = code;
            }
            symbol += 1;
            if code != 0 {
                prev = code;
            }
        } else {
            let (extra, base) = match code {
                16 => (2, 3),
                17 => (3, 3),
                _ => (7, 11),
            };
            let repeat = br.read(extra) as usize + base;
            if symbol + repeat > num_symbols {
                return Err(Error::corrupt("VP8L code length repeat overflows"));
            }
            let value = if code == 16 { prev } else { 0 };
            if let Some(run) = lengths.get_mut(symbol..symbol + repeat) {
                run.fill(value);
            }
            symbol += repeat;
        }
    }
    Ok(lengths)
}

fn read_prefix_code(br: &mut Bits, num_symbols: usize) -> Result<Huffman> {
    if br.read(1) == 1 {
        let two = br.read(1) == 1;
        let first_wide = br.read(1) == 1;
        let s0 = br.read(if first_wide { 8 } else { 1 }) as usize;
        let mut lengths = vec![0u8; num_symbols];
        let mut mark = |s: usize| -> Result<()> {
            match lengths.get_mut(s) {
                Some(l) => {
                    *l = 1;
                    Ok(())
                }
                None => Err(Error::corrupt("VP8L symbol outside its alphabet")),
            }
        };
        mark(s0)?;
        if two {
            mark(br.read(8) as usize)?;
        }
        return Huffman::new(&lengths);
    }
    let n = 4 + br.read(4) as usize;
    if n > CODE_LENGTH_ORDER.len() {
        return Err(Error::corrupt("VP8L code length code too long"));
    }
    let mut cl = [0u8; 19];
    for &slot in CODE_LENGTH_ORDER.iter().take(n) {
        if let Some(c) = cl.get_mut(slot) {
            *c = br.read(3) as u8;
        }
    }
    let code_len_code = Huffman::new(&cl)?;
    let lengths = read_code_lengths(br, &code_len_code, num_symbols)?;
    Huffman::new(&lengths)
}

fn read_group(br: &mut Bits, cache_bits: u32) -> Result<Group> {
    let cache = if cache_bits > 0 {
        1usize << cache_bits
    } else {
        0
    };
    let green = read_prefix_code(br, NUM_LITERAL + NUM_LENGTH + cache)?;
    let red = read_prefix_code(br, NUM_LITERAL)?;
    let blue = read_prefix_code(br, NUM_LITERAL)?;
    let alpha = read_prefix_code(br, NUM_LITERAL)?;
    let dist = read_prefix_code(br, NUM_DISTANCE)?;
    br.check()?;
    Ok(Group {
        green,
        red,
        blue,
        alpha,
        dist,
    })
}

struct Ctx<'a> {
    limits: &'a Limits,
    progress: &'a dyn Progress,
}

#[derive(Debug)]
enum Transform {
    Predictor { bits: u32, data: Vec<u32> },
    Cross { bits: u32, data: Vec<u32> },
    SubtractGreen,
    Palette { table: Vec<u32>, width_bits: u32 },
}

fn check_dims(ctx: &Ctx, w: u32, h: u32) -> Result<usize> {
    let px = ctx.limits.check_pixels(w, h)?;
    ctx.limits.check_alloc(px * 4)?;
    Ok(px as usize)
}

/// Decode an entropy-coded image of `xsize` x `ysize` pixels. `top_level` images may
/// carry transforms and a meta prefix image; those are handled by the caller for
/// transforms, here for the meta prefix.
fn decode_coded(
    br: &mut Bits,
    xsize: u32,
    ysize: u32,
    top_level: bool,
    ctx: &Ctx,
) -> Result<Vec<u32>> {
    check_dims(ctx, xsize, ysize)?;
    let cache_bits = if br.read(1) == 1 {
        let b = br.read(4);
        if !(1..=11).contains(&b) {
            return Err(Error::corrupt("VP8L colour cache size out of range"));
        }
        b
    } else {
        0
    };
    // Meta prefix image: which prefix group applies to each block of pixels.
    let (meta_bits, meta_xsize, meta) = if top_level && br.read(1) == 1 {
        let bits = br.read(3) + 2;
        let mx = sub_size(xsize, bits);
        let my = sub_size(ysize, bits);
        let img = decode_coded(br, mx, my, false, ctx)?;
        (bits, mx, Some(img))
    } else {
        (0, 0, None)
    };
    let (groups, group_of): (Vec<Group>, Option<Vec<u32>>) = match meta {
        None => (vec![read_group(br, cache_bits)?], None),
        Some(img) => {
            let ids: Vec<u32> = img.iter().map(|p| (p >> 8) & 0xffff).collect();
            let n = ids.iter().copied().max().map_or(1, |m| m as usize + 1);
            ctx.limits.check_entries(n as u64)?;
            let mut used = vec![false; n];
            for &i in &ids {
                if let Some(u) = used.get_mut(i as usize) {
                    *u = true;
                }
            }
            // Groups no block refers to still have to be parsed, but are not kept.
            let mut map = vec![0u32; n];
            let mut groups = Vec::new();
            for (i, &u) in used.iter().enumerate() {
                let g = read_group(br, cache_bits)?;
                if u {
                    if let Some(m) = map.get_mut(i) {
                        *m = groups.len() as u32;
                    }
                    groups.push(g);
                }
            }
            let mapped = ids
                .iter()
                .map(|&i| map.get(i as usize).copied().unwrap_or(0))
                .collect();
            (groups, Some(mapped))
        }
    };
    let total = xsize as usize * ysize as usize;
    let mut out = vec![0u32; total];
    let mut cache = if cache_bits > 0 {
        vec![0u32; 1 << cache_bits]
    } else {
        Vec::new()
    };
    let mask = if meta_bits > 0 {
        (1u32 << meta_bits) - 1
    } else {
        u32::MAX
    };
    let (mut pos, mut x, mut y) = (0usize, 0u32, 0u32);
    let mut group_idx = usize::MAX;
    let mut group_stale = true;
    let width = xsize as usize;
    let report_every = 1usize << 18;
    let mut next_report = report_every;
    while pos < total {
        if pos >= next_report {
            next_report = pos + report_every;
            ctx.progress.check_cancelled()?;
            ctx.progress.report(pos as u64, total as u64, "decode");
        }
        if group_stale || (x & mask) == 0 {
            group_stale = false;
            group_idx = match &group_of {
                None => 0,
                Some(ids) => ids
                    .get(
                        (y >> meta_bits) as usize * meta_xsize as usize + (x >> meta_bits) as usize,
                    )
                    .copied()
                    .unwrap_or(0) as usize,
            };
        }
        let Some(g) = groups.get(group_idx) else {
            return Err(Error::new(
                ErrorCode::InternalError,
                "VP8L prefix group missing",
            ));
        };
        let s = g.green.read(br) as usize;
        if s < NUM_LITERAL {
            let r = g.red.read(br);
            let b = g.blue.read(br);
            let a = g.alpha.read(br);
            let argb = (a << 24) | (r << 16) | ((s as u32) << 8) | b;
            if let Some(o) = out.get_mut(pos) {
                *o = argb;
            }
            if cache_bits > 0 {
                if let Some(c) = cache.get_mut(cache_hash(argb, cache_bits)) {
                    *c = argb;
                }
            }
            pos += 1;
            x += 1;
            if x == xsize {
                x = 0;
                y += 1;
            }
        } else if s < NUM_LITERAL + NUM_LENGTH {
            let length = prefix_value((s - NUM_LITERAL) as u32, br) as usize;
            let dsym = g.dist.read(br);
            let dcode = prefix_value(dsym, br);
            let dist = distance_from_code(dcode, xsize) as usize;
            if dist > pos || length > total - pos {
                return Err(Error::corrupt("VP8L back reference out of range").at(pos as u64));
            }
            for i in pos..pos + length {
                let v = out.get(i - dist).copied().unwrap_or(0);
                if let Some(o) = out.get_mut(i) {
                    *o = v;
                }
                if cache_bits > 0 {
                    if let Some(c) = cache.get_mut(cache_hash(v, cache_bits)) {
                        *c = v;
                    }
                }
            }
            pos += length;
            y = (pos / width) as u32;
            x = (pos % width) as u32;
            group_stale = true;
        } else {
            let idx = s - NUM_LITERAL - NUM_LENGTH;
            let Some(&argb) = cache.get(idx) else {
                return Err(Error::corrupt("VP8L colour cache index out of range"));
            };
            if let Some(o) = out.get_mut(pos) {
                *o = argb;
            }
            if let Some(c) = cache.get_mut(cache_hash(argb, cache_bits)) {
                *c = argb;
            }
            pos += 1;
            x += 1;
            if x == xsize {
                x = 0;
                y += 1;
            }
        }
        br.check()?;
    }
    Ok(out)
}

#[inline]
fn add_px(a: u32, b: u32) -> u32 {
    let ag = (a & 0xff00_ff00).wrapping_add(b & 0xff00_ff00) & 0xff00_ff00;
    let rb = (a & 0x00ff_00ff).wrapping_add(b & 0x00ff_00ff) & 0x00ff_00ff;
    ag | rb
}

#[inline]
fn avg2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xfefe_fefe) >> 1).wrapping_add(a & b)
}

#[inline]
fn channels(p: u32) -> [i32; 4] {
    [
        (p >> 24) as i32,
        ((p >> 16) & 0xff) as i32,
        ((p >> 8) & 0xff) as i32,
        (p & 0xff) as i32,
    ]
}

#[inline]
fn pack(c: [i32; 4]) -> u32 {
    ((c[0] as u32 & 0xff) << 24)
        | ((c[1] as u32 & 0xff) << 16)
        | ((c[2] as u32 & 0xff) << 8)
        | (c[3] as u32 & 0xff)
}

#[inline]
fn select(l: u32, t: u32, tl: u32) -> u32 {
    let mut pl = 0;
    let mut pt = 0;
    for ((a, b), c) in channels(l).into_iter().zip(channels(t)).zip(channels(tl)) {
        let p = a + b - c;
        pl += (p - a).abs();
        pt += (p - b).abs();
    }
    if pl < pt {
        l
    } else {
        t
    }
}

#[inline]
fn clamp_add_sub_full(a: u32, b: u32, c: u32) -> u32 {
    let mut o = [0i32; 4];
    for (((o, a), b), c) in o
        .iter_mut()
        .zip(channels(a))
        .zip(channels(b))
        .zip(channels(c))
    {
        *o = (a + b - c).clamp(0, 255);
    }
    pack(o)
}

#[inline]
fn clamp_add_sub_half(a: u32, b: u32) -> u32 {
    let mut o = [0i32; 4];
    for ((o, a), b) in o.iter_mut().zip(channels(a)).zip(channels(b)) {
        *o = (a + (a - b) / 2).clamp(0, 255);
    }
    pack(o)
}

/// The predictor for `mode` given the left, top, top-right and top-left pixels.
#[inline]
pub(crate) fn predict(mode: u32, l: u32, t: u32, tr: u32, tl: u32) -> u32 {
    match mode {
        1 => l,
        2 => t,
        3 => tr,
        4 => tl,
        5 => avg2(avg2(l, tr), t),
        6 => avg2(l, tl),
        7 => avg2(l, t),
        8 => avg2(tl, t),
        9 => avg2(t, tr),
        10 => avg2(avg2(l, tl), avg2(t, tr)),
        11 => select(l, t, tl),
        12 => clamp_add_sub_full(l, t, tl),
        13 => clamp_add_sub_half(avg2(l, t), tl),
        _ => 0xff00_0000,
    }
}

/// All fourteen predictions at once; shares the intermediate averages.
#[inline]
pub(crate) fn predict_all(l: u32, t: u32, tr: u32, tl: u32) -> [u32; 14] {
    let (lt, tt) = (avg2(l, t), avg2(t, tr));
    [
        0xff00_0000,
        l,
        t,
        tr,
        tl,
        avg2(avg2(l, tr), t),
        avg2(l, tl),
        lt,
        avg2(tl, t),
        tt,
        avg2(avg2(l, tl), tt),
        select(l, t, tl),
        clamp_add_sub_full(l, t, tl),
        clamp_add_sub_half(lt, tl),
    ]
}

fn inverse_predictor(data: &mut [u32], w: usize, h: usize, bits: u32, modes: &[u32]) {
    if w == 0 || h == 0 {
        return;
    }
    let bw = sub_size(w as u32, bits) as usize;
    // Top row: black for the first pixel, then the left neighbour.
    let mut left = 0xff00_0000u32;
    for p in data.iter_mut().take(w) {
        *p = add_px(*p, left);
        left = *p;
    }
    for y in 1..h {
        let (head, tail) = data.split_at_mut(y * w);
        let Some(top) = head.get((y - 1) * w..) else {
            return;
        };
        let Some(cur) = tail.get_mut(..w) else { return };
        // First pixel predicts from the top neighbour.
        if let (Some(c), Some(&t)) = (cur.first_mut(), top.first()) {
            *c = add_px(*c, t);
        }
        let mode_row = (y >> bits) * bw;
        let mut x = 1;
        while x < w {
            let block_end = (((x >> bits) + 1) << bits).min(w);
            let mode = modes
                .get(mode_row + (x >> bits))
                .map_or(0, |m| (m >> 8) & 0xf);
            for xx in x..block_end {
                let l = cur.get(xx - 1).copied().unwrap_or(0);
                let t = top.get(xx).copied().unwrap_or(0);
                let tl = top.get(xx - 1).copied().unwrap_or(0);
                let tr = if xx + 1 < w {
                    top.get(xx + 1).copied().unwrap_or(0)
                } else {
                    cur.first().copied().unwrap_or(0)
                };
                if let Some(c) = cur.get_mut(xx) {
                    *c = add_px(*c, predict(mode, l, t, tr, tl));
                }
            }
            x = block_end;
        }
    }
}

#[inline]
fn color_delta(t: u32, c: u32) -> i32 {
    (i32::from(t as u8 as i8) * i32::from(c as u8 as i8)) >> 5
}

fn inverse_cross_color(data: &mut [u32], w: usize, h: usize, bits: u32, elems: &[u32]) {
    if w == 0 {
        return;
    }
    let bw = sub_size(w as u32, bits) as usize;
    for y in 0..h {
        let Some(row) = data.get_mut(y * w..(y + 1) * w) else {
            return;
        };
        for (x, p) in row.iter_mut().enumerate() {
            let e = elems
                .get((y >> bits) * bw + (x >> bits))
                .copied()
                .unwrap_or(0);
            let (g2r, g2b, r2b) = (e & 0xff, (e >> 8) & 0xff, (e >> 16) & 0xff);
            let a = *p >> 24;
            let green = (*p >> 8) & 0xff;
            let mut red = (*p >> 16) & 0xff;
            let mut blue = *p & 0xff;
            red = (red as i32 + color_delta(g2r, green)) as u32 & 0xff;
            blue = (blue as i32 + color_delta(g2b, green) + color_delta(r2b, red)) as u32 & 0xff;
            *p = (a << 24) | (red << 16) | (green << 8) | blue;
        }
    }
}

fn inverse_subtract_green(data: &mut [u32]) {
    for p in data {
        let green = (*p >> 8) & 0xff;
        let rb = ((*p & 0x00ff_00ff).wrapping_add((green << 16) | green)) & 0x00ff_00ff;
        *p = (*p & 0xff00_ff00) | rb;
    }
}

/// Expand colour-indexed pixels (possibly several per packed pixel) to ARGB.
fn inverse_palette(
    packed: &[u32],
    width: usize,
    height: usize,
    packed_w: usize,
    table: &[u32],
    width_bits: u32,
) -> Vec<u32> {
    let mut out = vec![0u32; width * height];
    let bits_per = 8 >> width_bits;
    let mask = (1u32 << bits_per) - 1;
    for y in 0..height {
        let Some(src) = packed.get(y * packed_w..(y + 1) * packed_w) else {
            break;
        };
        let Some(dst) = out.get_mut(y * width..(y + 1) * width) else {
            break;
        };
        for (x, d) in dst.iter_mut().enumerate() {
            let p = src.get(x >> width_bits).copied().unwrap_or(0);
            let shift = bits_per * (x as u32 & ((1 << width_bits) - 1));
            let idx = ((p >> 8) >> shift) & mask;
            *d = table.get(idx as usize).copied().unwrap_or(0);
        }
    }
    out
}

pub(crate) struct Decoded {
    pub width: u32,
    pub height: u32,
    pub has_alpha_hint: bool,
    pub argb: Vec<u32>,
}

/// Read the 5-byte VP8L header: signature, size and the alpha hint.
pub(crate) fn read_header(data: &[u8]) -> Result<(u32, u32, bool)> {
    let mut br = Bits::new(data);
    if br.read(8) != 0x2f {
        return Err(Error::corrupt("missing VP8L signature"));
    }
    let w = br.read(14) + 1;
    let h = br.read(14) + 1;
    let alpha = br.read(1) == 1;
    if br.read(3) != 0 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "unknown VP8L version",
        ));
    }
    br.check()?;
    Ok((w, h, alpha))
}

/// An image stream without the five-byte header, as stored by the WebP `ALPH` chunk.
pub(crate) fn decode_headerless(
    data: &[u8],
    width: u32,
    height: u32,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Vec<u32>> {
    let ctx = Ctx { limits, progress };
    check_dims(&ctx, width, height)?;
    let mut br = Bits::new(data);
    decode_stream(&mut br, width, height, &ctx)
}

fn decode_stream(br: &mut Bits, width: u32, height: u32, ctx: &Ctx) -> Result<Vec<u32>> {
    let mut transforms: Vec<Transform> = Vec::new();
    let mut xsize = width;
    let mut seen = [false; 4];
    while br.read(1) == 1 {
        let kind = br.read(2) as usize;
        if seen.get(kind).copied().unwrap_or(true) {
            return Err(Error::corrupt("VP8L transform used twice"));
        }
        if let Some(s) = seen.get_mut(kind) {
            *s = true;
        }
        match kind {
            0 | 1 => {
                let bits = br.read(3) + 2;
                let data = decode_coded(
                    br,
                    sub_size(xsize, bits),
                    sub_size(height, bits),
                    false,
                    ctx,
                )?;
                transforms.push(if kind == 0 {
                    Transform::Predictor { bits, data }
                } else {
                    Transform::Cross { bits, data }
                });
            }
            2 => transforms.push(Transform::SubtractGreen),
            _ => {
                let n = br.read(8) as usize + 1;
                let mut table = decode_coded(br, n as u32, 1, false, ctx)?;
                for i in 1..table.len() {
                    let prev = table.get(i - 1).copied().unwrap_or(0);
                    if let Some(t) = table.get_mut(i) {
                        *t = add_px(*t, prev);
                    }
                }
                let width_bits = match n {
                    0..=2 => 3,
                    3..=4 => 2,
                    5..=16 => 1,
                    _ => 0,
                };
                table.resize(256, 0);
                transforms.push(Transform::Palette { table, width_bits });
                xsize = sub_size(xsize, width_bits);
            }
        }
        br.check()?;
    }
    let mut argb = decode_coded(br, xsize, height, true, ctx)?;
    let mut cur_w = xsize as usize;
    let h = height as usize;
    for t in transforms.iter().rev() {
        match t {
            Transform::Predictor { bits, data } => {
                inverse_predictor(&mut argb, cur_w, h, *bits, data);
            }
            Transform::Cross { bits, data } => {
                inverse_cross_color(&mut argb, cur_w, h, *bits, data);
            }
            Transform::SubtractGreen => inverse_subtract_green(&mut argb),
            Transform::Palette { table, width_bits } => {
                argb = inverse_palette(&argb, width as usize, h, cur_w, table, *width_bits);
                cur_w = width as usize;
            }
        }
    }
    Ok(argb)
}

pub(crate) fn decode(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Decoded> {
    let (width, height, has_alpha_hint) = read_header(data)?;
    let ctx = Ctx { limits, progress };
    check_dims(&ctx, width, height)?;
    let mut br = Bits::new(data);
    br.read(8 + 14 + 14 + 1 + 3);
    let argb = decode_stream(&mut br, width, height, &ctx)?;
    Ok(Decoded {
        width,
        height,
        has_alpha_hint,
        argb,
    })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn predict_all_matches_predict() {
        let mut seed = 99u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        for _ in 0..2000 {
            let (l, t, tr, tl) = (next(), next(), next(), next());
            let all = predict_all(l, t, tr, tl);
            for (m, &v) in all.iter().enumerate() {
                assert_eq!(v, predict(m as u32, l, t, tr, tl), "mode {m}");
            }
        }
    }
}
