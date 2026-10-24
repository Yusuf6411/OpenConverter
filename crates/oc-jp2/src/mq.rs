//! The MQ arithmetic decoder (ITU-T T.800 Annex C) and the raw bit reader used by bypass segments.

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

/// Raw (bypass) bits: after an 0xFF byte the next byte carries only seven bits.
pub struct Raw<'a> {
    d: &'a [u8],
    pos: usize,
    buf: u8,
    cnt: u8,
    last_ff: bool,
}

impl<'a> Raw<'a> {
    pub fn new(d: &'a [u8]) -> Raw<'a> {
        Raw {
            d,
            pos: 0,
            buf: 0,
            cnt: 0,
            last_ff: false,
        }
    }

    pub fn bit(&mut self) -> u8 {
        if self.cnt == 0 {
            let b = self.d.get(self.pos).copied().unwrap_or(0xFF);
            self.pos += 1;
            self.cnt = if self.last_ff { 7 } else { 8 };
            self.buf = b;
            self.last_ff = b == 0xFF;
        }
        self.cnt -= 1;
        (self.buf >> self.cnt) & 1
    }
}
