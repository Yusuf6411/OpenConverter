//! The MQ arithmetic decoder (ITU-T T.88 Annex E) and the integer decoders of Annex A.

/// Probability states: (Qe, next state after MPS, next state after LPS, switch MPS).
const QE: [(u32, u8, u8, u8); 47] = [
    (0x5601, 1, 1, 1),
    (0x3401, 2, 6, 0),
    (0x1801, 3, 9, 0),
    (0x0AC1, 4, 12, 0),
    (0x0521, 5, 29, 0),
    (0x0221, 38, 33, 0),
    (0x5601, 7, 6, 1),
    (0x5401, 8, 14, 0),
    (0x4801, 9, 14, 0),
    (0x3801, 10, 14, 0),
    (0x3001, 11, 17, 0),
    (0x2401, 12, 18, 0),
    (0x1C01, 13, 20, 0),
    (0x1601, 29, 21, 0),
    (0x5601, 15, 14, 1),
    (0x5401, 16, 14, 0),
    (0x5101, 17, 15, 0),
    (0x4801, 18, 16, 0),
    (0x3801, 19, 17, 0),
    (0x3401, 20, 18, 0),
    (0x3001, 21, 19, 0),
    (0x2801, 22, 19, 0),
    (0x2401, 23, 20, 0),
    (0x2201, 24, 21, 0),
    (0x1C01, 25, 22, 0),
    (0x1801, 26, 23, 0),
    (0x1601, 27, 24, 0),
    (0x1401, 28, 25, 0),
    (0x1201, 29, 26, 0),
    (0x1101, 30, 27, 0),
    (0x0AC1, 31, 28, 0),
    (0x09C1, 32, 29, 0),
    (0x08A1, 33, 30, 0),
    (0x0521, 34, 31, 0),
    (0x0441, 35, 32, 0),
    (0x02A1, 36, 33, 0),
    (0x0221, 37, 34, 0),
    (0x0141, 38, 35, 0),
    (0x0111, 39, 36, 0),
    (0x0085, 40, 37, 0),
    (0x0049, 41, 38, 0),
    (0x0025, 42, 39, 0),
    (0x0015, 43, 40, 0),
    (0x0009, 44, 41, 0),
    (0x0005, 45, 42, 0),
    (0x0001, 45, 43, 0),
    (0x5601, 46, 46, 0),
];

/// A context: index into the state table and the current more probable symbol.
#[derive(Clone, Copy, Default)]
pub struct Cx {
    pub idx: u8,
    pub mps: u8,
}

pub struct Mq<'a> {
    d: &'a [u8],
    bp: usize,
    c: u32,
    a: u32,
    ct: u32,
}

impl<'a> Mq<'a> {
    pub fn new(d: &'a [u8]) -> Mq<'a> {
        let mut m = Mq {
            d,
            bp: 0,
            c: 0,
            a: 0,
            ct: 0,
        };
        m.c = u32::from(m.byte(0)) << 16;
        m.bytein();
        m.c <<= 7;
        m.ct -= 7;
        m.a = 0x8000;
        m
    }

    /// Bytes past the end read as 0xFF, which the decoder treats as a marker (all ones).
    fn byte(&self, i: usize) -> u8 {
        self.d.get(i).copied().unwrap_or(0xFF)
    }

    fn bytein(&mut self) {
        if self.byte(self.bp) == 0xFF {
            if self.byte(self.bp + 1) > 0x8F {
                self.c += 0xFF00;
                self.ct = 8;
            } else {
                self.bp += 1;
                self.c += u32::from(self.byte(self.bp)) << 9;
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.c += u32::from(self.byte(self.bp)) << 8;
            self.ct = 8;
        }
    }

    fn renorm(&mut self) {
        loop {
            if self.ct == 0 {
                self.bytein();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    pub fn decode(&mut self, cx: &mut Cx) -> u8 {
        let (qe, nmps, nlps, sw) = QE[usize::from(cx.idx)];
        self.a -= qe;
        let d;
        if (self.c >> 16) < qe {
            // LPS exchange
            if self.a < qe {
                d = cx.mps;
                cx.idx = nmps;
            } else {
                d = 1 - cx.mps;
                if sw == 1 {
                    cx.mps = 1 - cx.mps;
                }
                cx.idx = nlps;
            }
            self.a = qe;
            self.renorm();
        } else {
            self.c -= qe << 16;
            if self.a & 0x8000 == 0 {
                // MPS exchange
                if self.a < qe {
                    d = 1 - cx.mps;
                    if sw == 1 {
                        cx.mps = 1 - cx.mps;
                    }
                    cx.idx = nlps;
                } else {
                    d = cx.mps;
                    cx.idx = nmps;
                }
                self.renorm();
            } else {
                d = cx.mps;
            }
        }
        d
    }
}

/// Arithmetic integer decoder (Annex A.2): 512 contexts, `None` for the out-of-band value.
pub struct Ia {
    cx: Vec<Cx>,
}

impl Default for Ia {
    fn default() -> Ia {
        Ia {
            cx: vec![Cx::default(); 512],
        }
    }
}

impl Ia {
    pub fn decode(&mut self, mq: &mut Mq) -> Option<i32> {
        let mut prev: usize = 1;
        let mut bit = |mq: &mut Mq, cx: &mut Vec<Cx>| -> u32 {
            let d = mq.decode(&mut cx[prev]);
            prev = if prev < 256 {
                (prev << 1) | usize::from(d)
            } else {
                (((prev << 1) | usize::from(d)) & 511) | 256
            };
            u32::from(d)
        };
        let s = bit(mq, &mut self.cx);
        let (n, off): (u32, i64) = if bit(mq, &mut self.cx) == 0 {
            (2, 0)
        } else if bit(mq, &mut self.cx) == 0 {
            (4, 4)
        } else if bit(mq, &mut self.cx) == 0 {
            (6, 20)
        } else if bit(mq, &mut self.cx) == 0 {
            (8, 84)
        } else if bit(mq, &mut self.cx) == 0 {
            (12, 340)
        } else {
            (32, 4436)
        };
        let mut v: i64 = 0;
        for _ in 0..n {
            v = (v << 1) | i64::from(bit(mq, &mut self.cx));
        }
        v += off;
        if s == 0 {
            Some(v.min(i64::from(i32::MAX)) as i32)
        } else if v > 0 {
            Some((-v).max(i64::from(i32::MIN)) as i32)
        } else {
            None
        }
    }
}

/// Symbol-ID decoder (Annex A.3).
pub struct Iaid {
    cx: Vec<Cx>,
    len: u32,
}

impl Iaid {
    pub fn new(len: u32) -> Iaid {
        Iaid {
            cx: vec![Cx::default(); 1usize << (len + 1).min(24)],
            len,
        }
    }

    pub fn decode(&mut self, mq: &mut Mq) -> usize {
        let mut prev: usize = 1;
        let mask = self.cx.len() - 1;
        for _ in 0..self.len {
            let d = mq.decode(&mut self.cx[prev & mask]);
            prev = (prev << 1) | usize::from(d);
        }
        prev - (1usize << self.len)
    }
}
