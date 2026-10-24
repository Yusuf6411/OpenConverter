//! Tier-1 coding: the three bit-plane coding passes of a code-block (T.800 Annex D).
use crate::mq::{Cx, Mq, Raw};

const SIG: u8 = 1;
const NEG: u8 = 2;
const VISIT: u8 = 4;
const REFINED: u8 = 8;

pub const STYLE_BYPASS: u8 = 0x01;
pub const STYLE_RESET: u8 = 0x02;
pub const STYLE_TERMALL: u8 = 0x04;
pub const STYLE_VSC: u8 = 0x08;
pub const STYLE_SEGSYM: u8 = 0x20;

/// One codeword segment: the passes it holds and its bytes.
#[derive(Default, Clone)]
pub struct Seg {
    pub passes: u32,
    pub max_passes: u32,
    pub data: Vec<u8>,
}

enum Coder<'a> {
    Mq(Mq<'a>),
    Raw(Raw<'a>),
}

pub struct T1 {
    w: usize,
    h: usize,
    stride: usize,
    flags: Vec<u8>,
    /// Magnitudes in the "doubled" domain: twice the value, so the reconstruction midpoint is exact.
    pub mag: Vec<u32>,
    ctx: [Cx; 19],
    orient: u8,
    vsc: bool,
}

fn reset_ctx() -> [Cx; 19] {
    let mut c = [Cx::default(); 19];
    c[0].idx = 4;
    c[17].idx = 3;
    c[18].idx = 46;
    c
}

fn zc_ctx(orient: u8, h: u32, v: u32, d: u32) -> usize {
    let (h, v) = if orient == 1 { (v, h) } else { (h, v) };
    if orient == 3 {
        let hv = h + v;
        return match d {
            0 => match hv {
                0 => 0,
                1 => 1,
                _ => 2,
            },
            1 => match hv {
                0 => 3,
                1 => 4,
                _ => 5,
            },
            2 => {
                if hv >= 1 {
                    7
                } else {
                    6
                }
            }
            _ => 8,
        };
    }
    match h {
        2 => 8,
        1 => {
            if v >= 1 {
                7
            } else if d >= 1 {
                6
            } else {
                5
            }
        }
        _ => match v {
            2 => 4,
            1 => 3,
            _ => match d {
                0 => 0,
                1 => 1,
                _ => 2,
            },
        },
    }
}

impl T1 {
    pub fn new() -> T1 {
        T1 {
            w: 0,
            h: 0,
            stride: 0,
            flags: Vec::new(),
            mag: Vec::new(),
            ctx: reset_ctx(),
            orient: 0,
            vsc: false,
        }
    }

    fn idx(&self, x: usize, y: usize) -> usize {
        (y + 1) * self.stride + x + 1
    }

    /// (horizontal, vertical, diagonal significant counts, horizontal and vertical sign contributions)
    fn neighbours(&self, i: usize, y: usize) -> (u32, u32, u32, i32, i32) {
        let s = self.stride;
        let f = &self.flags;
        let below = !(self.vsc && (y & 3) == 3);
        let sg = |j: usize| u32::from(f[j] & SIG);
        let sc = |j: usize| -> i32 {
            if f[j] & SIG == 0 {
                0
            } else if f[j] & NEG != 0 {
                -1
            } else {
                1
            }
        };
        let h = sg(i - 1) + sg(i + 1);
        let mut v = sg(i - s);
        let mut d = sg(i - s - 1) + sg(i - s + 1);
        let mut vc = sc(i - s);
        if below {
            v += sg(i + s);
            d += sg(i + s - 1) + sg(i + s + 1);
            vc += sc(i + s);
        }
        (h, v, d, sc(i - 1) + sc(i + 1), vc)
    }

    fn zc(&self, i: usize, y: usize) -> usize {
        let (h, v, d, _, _) = self.neighbours(i, y);
        zc_ctx(self.orient, h, v, d)
    }

    fn sign_ctx(&self, i: usize, y: usize) -> (usize, u8) {
        let (_, _, _, hc, vc) = self.neighbours(i, y);
        let (hc, vc) = (hc.clamp(-1, 1), vc.clamp(-1, 1));
        match (hc, vc) {
            (1, 1) => (13, 0),
            (1, 0) => (12, 0),
            (1, -1) => (11, 0),
            (0, 1) => (10, 0),
            (0, 0) => (9, 0),
            (0, -1) => (10, 1),
            (-1, 1) => (11, 1),
            (-1, 0) => (12, 1),
            _ => (13, 1),
        }
    }

    fn decode_sign(&mut self, c: &mut Coder, i: usize, y: usize) -> bool {
        match c {
            Coder::Mq(m) => {
                let (cx, xor) = self.sign_ctx(i, y);
                m.decode(&mut self.ctx[cx]) ^ xor == 1
            }
            Coder::Raw(r) => r.bit() == 1,
        }
    }

    fn become_significant(&mut self, c: &mut Coder, x: usize, y: usize, plane: u32) {
        let i = self.idx(x, y);
        let neg = self.decode_sign(c, i, y);
        self.flags[i] |= SIG | if neg { NEG } else { 0 };
        self.mag[y * self.w + x] = (3u32) << plane;
    }

    fn sig_pass(&mut self, c: &mut Coder, plane: u32) {
        for f in self.flags.iter_mut() {
            *f &= !VISIT;
        }
        let mut sy = 0;
        while sy < self.h {
            for x in 0..self.w {
                for y in sy..(sy + 4).min(self.h) {
                    let i = self.idx(x, y);
                    if self.flags[i] & SIG != 0 {
                        continue;
                    }
                    let (h, v, d, _, _) = self.neighbours(i, y);
                    if h + v + d == 0 {
                        continue;
                    }
                    let bit = match c {
                        Coder::Mq(m) => {
                            let cx = zc_ctx(self.orient, h, v, d);
                            m.decode(&mut self.ctx[cx])
                        }
                        Coder::Raw(r) => r.bit(),
                    };
                    self.flags[i] |= VISIT;
                    if bit == 1 {
                        self.become_significant(c, x, y, plane);
                    }
                }
            }
            sy += 4;
        }
    }

    fn ref_pass(&mut self, c: &mut Coder, plane: u32) {
        let mut sy = 0;
        while sy < self.h {
            for x in 0..self.w {
                for y in sy..(sy + 4).min(self.h) {
                    let i = self.idx(x, y);
                    if self.flags[i] & (SIG | VISIT) != SIG {
                        continue;
                    }
                    let bit = match c {
                        Coder::Mq(m) => {
                            let cx = if self.flags[i] & REFINED != 0 {
                                16
                            } else {
                                let (h, v, d, _, _) = self.neighbours(i, y);
                                if h + v + d >= 1 {
                                    15
                                } else {
                                    14
                                }
                            };
                            m.decode(&mut self.ctx[cx])
                        }
                        Coder::Raw(r) => r.bit(),
                    };
                    self.flags[i] |= REFINED;
                    let m = &mut self.mag[y * self.w + x];
                    if bit == 1 {
                        *m = m.wrapping_add(1 << plane);
                    } else {
                        *m = m.wrapping_sub(1 << plane);
                    }
                }
            }
            sy += 4;
        }
    }

    fn cleanup_pass(&mut self, c: &mut Coder, plane: u32, segsym: bool) {
        let mut sy = 0;
        while sy < self.h {
            for x in 0..self.w {
                let rows = (self.h - sy).min(4);
                let mut start = 0;
                if rows == 4 {
                    let all_zero = (0..4).all(|r| {
                        let i = self.idx(x, sy + r);
                        let (h, v, d, _, _) = self.neighbours(i, sy + r);
                        self.flags[i] & (SIG | VISIT) == 0 && h + v + d == 0
                    });
                    if all_zero {
                        let Coder::Mq(m) = c else { return };
                        if m.decode(&mut self.ctx[17]) == 0 {
                            continue;
                        }
                        let r = (usize::from(m.decode(&mut self.ctx[18])) << 1)
                            | usize::from(m.decode(&mut self.ctx[18]));
                        self.become_significant(c, x, sy + r, plane);
                        start = r + 1;
                    }
                }
                for r in start..rows {
                    let y = sy + r;
                    let i = self.idx(x, y);
                    if self.flags[i] & (SIG | VISIT) != 0 {
                        continue;
                    }
                    let cx = self.zc(i, y);
                    let Coder::Mq(m) = c else { return };
                    if m.decode(&mut self.ctx[cx]) == 1 {
                        self.become_significant(c, x, y, plane);
                    }
                }
            }
            sy += 4;
        }
        if segsym {
            if let Coder::Mq(m) = c {
                for _ in 0..4 {
                    m.decode(&mut self.ctx[18]);
                }
            }
        }
        for f in self.flags.iter_mut() {
            *f &= !VISIT;
        }
    }

    /// Decode a code-block. `top` is the index of the first coded bit-plane. After this call
    /// `mag` holds the doubled magnitudes and `negative(x, y)` the signs.
    pub fn decode(&mut self, w: usize, h: usize, orient: u8, top: u32, style: u8, segs: &[Seg]) {
        self.w = w;
        self.h = h;
        self.stride = w + 2;
        self.orient = orient;
        self.vsc = style & STYLE_VSC != 0;
        self.flags.clear();
        self.flags.resize((w + 2) * (h + 2), 0);
        self.mag.clear();
        self.mag.resize(w * h, 0);
        self.ctx = reset_ctx();
        let mut pass = 0u32;
        for seg in segs {
            if seg.passes == 0 {
                continue;
            }
            let first_type = (pass + 2) % 3;
            let raw = style & STYLE_BYPASS != 0 && pass >= 10 && first_type != 2;
            let mut coder = if raw {
                Coder::Raw(Raw::new(&seg.data))
            } else {
                Coder::Mq(Mq::new(&seg.data))
            };
            for _ in 0..seg.passes {
                let ty = (pass + 2) % 3;
                let drop = (pass + 2) / 3;
                if drop > top {
                    return;
                }
                let plane = top - drop;
                match ty {
                    0 => self.sig_pass(&mut coder, plane),
                    1 => self.ref_pass(&mut coder, plane),
                    _ => self.cleanup_pass(&mut coder, plane, style & STYLE_SEGSYM != 0),
                }
                if style & STYLE_RESET != 0 {
                    self.ctx = reset_ctx();
                }
                pass += 1;
            }
        }
    }

    pub fn negative(&self, x: usize, y: usize) -> bool {
        self.flags[self.idx(x, y)] & NEG != 0
    }
}
