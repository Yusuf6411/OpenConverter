//! Bilevel bitmaps (one byte per pixel, 1 = black) and the region decoding procedures of
//! clauses 6.2 (generic regions) and 6.3 (generic refinement regions).
use crate::arith::{Cx, Mq};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bm {
    pub w: usize,
    pub h: usize,
    pub d: Vec<u8>,
}

impl Bm {
    pub fn new(w: usize, h: usize, fill: u8) -> Bm {
        Bm {
            w,
            h,
            d: vec![fill; w * h],
        }
    }

    pub fn get(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            0
        } else {
            self.d[y as usize * self.w + x as usize]
        }
    }

    /// Draws `src` with its top-left corner at `(x, y)`; `op`: 0 OR, 1 AND, 2 XOR, 3 XNOR,
    /// 4 replace.
    pub fn combine(&mut self, src: &Bm, x: i32, y: i32, op: u8) {
        for sy in 0..src.h {
            let dy = y as i64 + sy as i64;
            if dy < 0 || dy >= self.h as i64 {
                continue;
            }
            for sx in 0..src.w {
                let dx = x as i64 + sx as i64;
                if dx < 0 || dx >= self.w as i64 {
                    continue;
                }
                let s = src.d[sy * src.w + sx];
                let t = &mut self.d[dy as usize * self.w + dx as usize];
                *t = match op {
                    0 => *t | s,
                    1 => *t & s,
                    2 => *t ^ s,
                    3 => 1 - (*t ^ s),
                    _ => s,
                };
            }
        }
    }

    pub fn crop(&self, x: i32, y: i32, w: usize, h: usize) -> Bm {
        let mut out = Bm::new(w, h, 0);
        for j in 0..h {
            for i in 0..w {
                out.d[j * w + i] = self.get(x + i as i32, y + j as i32);
            }
        }
        out
    }

    /// Packs into rows of bytes, most significant bit first.
    pub fn pack(&self) -> Vec<u8> {
        let rb = self.w.div_ceil(8);
        let mut out = vec![0u8; rb * self.h];
        for y in 0..self.h {
            for x in 0..self.w {
                if self.d[y * self.w + x] != 0 {
                    out[y * rb + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        out
    }

    pub fn from_packed(data: &[u8], w: usize, h: usize) -> Bm {
        let rb = w.div_ceil(8);
        let mut out = Bm::new(w, h, 0);
        for y in 0..h {
            for x in 0..w {
                let b = data.get(y * rb + x / 8).copied().unwrap_or(0);
                out.d[y * w + x] = (b >> (7 - x % 8)) & 1;
            }
        }
        out
    }
}

/// Pixel offsets, in context bit order from the least significant bit, of the generic region
/// templates (figures 3 to 6). The adaptive pixels are substituted into their slots.
fn generic_offsets(template: u8, at: &[(i8, i8); 4]) -> Vec<(i32, i32)> {
    let a = |i: usize| (i32::from(at[i].0), i32::from(at[i].1));
    match template {
        0 => vec![
            (-1, 0),
            (-2, 0),
            (-3, 0),
            (-4, 0),
            a(0),
            (2, -1),
            (1, -1),
            (0, -1),
            (-1, -1),
            (-2, -1),
            a(1),
            a(2),
            (1, -2),
            (0, -2),
            (-1, -2),
            a(3),
        ],
        1 => vec![
            (-1, 0),
            (-2, 0),
            (-3, 0),
            a(0),
            (2, -1),
            (1, -1),
            (0, -1),
            (-1, -1),
            (-2, -1),
            (2, -2),
            (1, -2),
            (0, -2),
            (-1, -2),
        ],
        2 => vec![
            (-1, 0),
            (-2, 0),
            a(0),
            (1, -1),
            (0, -1),
            (-1, -1),
            (-2, -1),
            (1, -2),
            (0, -2),
            (-1, -2),
        ],
        _ => vec![
            (-1, 0),
            (-2, 0),
            (-3, 0),
            (-4, 0),
            a(0),
            (1, -1),
            (0, -1),
            (-1, -1),
            (-2, -1),
            (-3, -1),
        ],
    }
}

pub fn generic_contexts(template: u8) -> usize {
    match template {
        0 => 1 << 16,
        1 => 1 << 13,
        _ => 1 << 10,
    }
}

/// Decodes a generic region with the arithmetic coder (6.2.5). `skip` marks pixels that are
/// not coded and read as 0.
pub fn decode_generic(
    mq: &mut Mq,
    cx: &mut [Cx],
    w: usize,
    h: usize,
    template: u8,
    tpgdon: bool,
    at: &[(i8, i8); 4],
    skip: Option<&Bm>,
) -> Bm {
    let offs = generic_offsets(template, at);
    let mut bm = Bm::new(w, h, 0);
    let sltp = [0x9B25usize, 0x0795, 0x00E5, 0x0195][usize::from(template.min(3))];
    let mx = offs
        .iter()
        .map(|o| o.0.unsigned_abs() as usize)
        .max()
        .unwrap_or(0);
    let my = offs
        .iter()
        .map(|o| o.1.unsigned_abs() as usize)
        .max()
        .unwrap_or(0);
    let lin: Vec<isize> = offs
        .iter()
        .map(|&(dx, dy)| dy as isize * w as isize + dx as isize)
        .collect();
    let fast_ok = offs.iter().all(|o| o.1 <= 0);
    let mut ltp = 0u8;
    for y in 0..h {
        if tpgdon {
            ltp ^= mq.decode(&mut cx[sltp]);
            if ltp == 1 {
                if y > 0 {
                    let (a, b) = bm.d.split_at_mut(y * w);
                    b[..w].copy_from_slice(&a[(y - 1) * w..]);
                }
                continue;
            }
        }
        let interior_y = fast_ok && y >= my;
        for x in 0..w {
            if let Some(s) = skip {
                if s.get(x as i32, y as i32) != 0 {
                    continue;
                }
            }
            let mut ctx = 0usize;
            if interior_y && x >= mx && x + mx < w {
                let base = (y * w + x) as isize;
                for (i, l) in lin.iter().enumerate() {
                    ctx |= usize::from(bm.d[(base + l) as usize]) << i;
                }
            } else {
                for (i, &(dx, dy)) in offs.iter().enumerate() {
                    ctx |= usize::from(bm.get(x as i32 + dx, y as i32 + dy)) << i;
                }
            }
            bm.d[y * w + x] = mq.decode(&mut cx[ctx]);
        }
    }
    bm
}

pub fn refine_contexts(template: u8) -> usize {
    if template == 0 {
        1 << 13
    } else {
        1 << 10
    }
}

/// Decodes a generic refinement region (6.3.5). The reference bitmap is displaced by `(dx, dy)`.
#[allow(clippy::too_many_arguments)]
pub fn decode_refinement(
    mq: &mut Mq,
    cx: &mut [Cx],
    w: usize,
    h: usize,
    reference: &Bm,
    dx: i32,
    dy: i32,
    template: u8,
    tpgron: bool,
    at: &[(i8, i8); 2],
) -> Bm {
    let mut bm = Bm::new(w, h, 0);
    let a0 = (i32::from(at[0].0), i32::from(at[0].1));
    let a1 = (i32::from(at[1].0), i32::from(at[1].1));
    // (from the reference?, dx, dy) in context bit order
    let spec: Vec<(bool, i32, i32)> = if template == 0 {
        vec![
            (false, -1, 0),
            (false, 1, -1),
            (false, 0, -1),
            (false, a0.0, a0.1),
            (true, 1, 1),
            (true, 0, 1),
            (true, -1, 1),
            (true, 1, 0),
            (true, 0, 0),
            (true, -1, 0),
            (true, 1, -1),
            (true, 0, -1),
            (true, a1.0, a1.1),
        ]
    } else {
        vec![
            (false, -1, 0),
            (false, 1, -1),
            (false, 0, -1),
            (false, -1, -1),
            (true, 1, 1),
            (true, 0, 1),
            (true, 1, 0),
            (true, 0, 0),
            (true, -1, 0),
            (true, 0, -1),
        ]
    };
    let sltp = if template == 0 { 1usize << 8 } else { 1 << 7 };
    let mut ltp = 0u8;
    for y in 0..h as i32 {
        if tpgron {
            ltp ^= mq.decode(&mut cx[sltp]);
        }
        for x in 0..w as i32 {
            if ltp == 1 {
                let (rx, ry) = (x - dx, y - dy);
                let v = reference.get(rx, ry);
                let mut same = true;
                'o: for j in -1..=1 {
                    for i in -1..=1 {
                        if reference.get(rx + i, ry + j) != v {
                            same = false;
                            break 'o;
                        }
                    }
                }
                if same {
                    bm.d[y as usize * w + x as usize] = v;
                    continue;
                }
            }
            let mut ctx = 0usize;
            for (i, &(from_ref, ox, oy)) in spec.iter().enumerate() {
                let p = if from_ref {
                    reference.get(x - dx + ox, y - dy + oy)
                } else {
                    bm.get(x + ox, y + oy)
                };
                ctx |= usize::from(p) << i;
            }
            bm.d[y as usize * w + x as usize] = mq.decode(&mut cx[ctx]);
        }
    }
    bm
}
