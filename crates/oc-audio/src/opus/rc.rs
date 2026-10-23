//! The range coder of RFC 6716 section 4.1: a decoder, an encoder and one wrapper that lets
//! the CELT layer run the same code for both directions (each call takes the value to write
//! and returns the value that was read or written).

const SYM_BITS: u32 = 8;
const CODE_BITS: u32 = 32;
const SYM_MAX: u32 = (1 << SYM_BITS) - 1;
const CODE_TOP: u32 = 1 << (CODE_BITS - 1);
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
const CODE_SHIFT: u32 = CODE_BITS - SYM_BITS - 1;
const CODE_EXTRA: u32 = (CODE_BITS - 2) % SYM_BITS + 1;
const UINT_BITS: u32 = 8;
pub const BITRES: i32 = 3;

pub fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

#[derive(Clone)]
pub struct Dec<'a> {
    buf: &'a [u8],
    end_offs: usize,
    end_window: u32,
    nend_bits: u32,
    nbits_total: i32,
    offs: usize,
    pub rng: u32,
    val: u32,
    ext: u32,
    rem: i32,
}

impl<'a> Dec<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut d = Self {
            buf,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: (CODE_BITS + 1 - ((CODE_BITS - CODE_EXTRA) / SYM_BITS) * SYM_BITS) as i32,
            offs: 0,
            rng: 1 << CODE_EXTRA,
            val: 0,
            ext: 0,
            rem: 0,
        };
        d.rem = d.read_byte();
        d.val = d.rng - 1 - (d.rem as u32 >> (SYM_BITS - CODE_EXTRA));
        d.normalize();
        d
    }
    pub fn storage(&self) -> usize {
        self.buf.len()
    }
    fn read_byte(&mut self) -> i32 {
        let b = self.buf.get(self.offs).copied();
        if b.is_some() {
            self.offs += 1;
        }
        i32::from(b.unwrap_or(0))
    }
    fn read_byte_from_end(&mut self) -> u32 {
        if self.end_offs < self.buf.len() {
            self.end_offs += 1;
            u32::from(self.buf[self.buf.len() - self.end_offs])
        } else {
            0
        }
    }
    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.nbits_total += SYM_BITS as i32;
            self.rng <<= SYM_BITS;
            let sym = self.rem as u32;
            self.rem = self.read_byte();
            let sym = ((sym << SYM_BITS) | self.rem as u32) >> (SYM_BITS - CODE_EXTRA);
            self.val = ((self.val << SYM_BITS).wrapping_add(SYM_MAX & !sym)) & (CODE_TOP - 1);
        }
    }
    pub fn decode(&mut self, ft: u32) -> u32 {
        self.ext = self.rng / ft;
        let s = self.val / self.ext;
        ft - (s + 1).min(ft)
    }
    pub fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext;
        (1 << bits) - (s + 1).min(1 << bits)
    }
    pub fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let s = self.ext * (ft - fh);
        self.val = self.val.wrapping_sub(s);
        self.rng = if fl > 0 { self.ext * (fh - fl) } else { self.rng - s };
        self.normalize();
    }
    pub fn bit_logp(&mut self, logp: u32) -> bool {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let ret = d < s;
        if !ret {
            self.val = d - s;
        }
        self.rng = if ret { s } else { r - s };
        self.normalize();
        ret
    }
    pub fn icdf(&mut self, icdf: &[u8], ftb: u32) -> usize {
        let mut s = self.rng;
        let d = self.val;
        let r = s >> ftb;
        let mut ret = 0usize;
        let mut t;
        loop {
            t = s;
            s = r * u32::from(icdf[ret]);
            if d >= s {
                break;
            }
            ret += 1;
        }
        self.val = d - s;
        self.rng = t - s;
        self.normalize();
        ret
    }
    pub fn uint(&mut self, ft: u32) -> u32 {
        let ft = ft - 1;
        let mut ftb = ilog(ft);
        if ftb > UINT_BITS {
            ftb -= UINT_BITS;
            let ft1 = (ft >> ftb) + 1;
            let s = self.decode(ft1);
            self.update(s, s + 1, ft1);
            let t = (s << ftb) | self.bits(ftb);
            t.min(ft)
        } else {
            let ft = ft + 1;
            let s = self.decode(ft);
            self.update(s, s + 1, ft);
            s
        }
    }
    pub fn bits(&mut self, n: u32) -> u32 {
        let mut window = self.end_window;
        let mut avail = self.nend_bits;
        if avail < n {
            loop {
                window |= self.read_byte_from_end() << avail;
                avail += SYM_BITS;
                if avail > 32 - SYM_BITS {
                    break;
                }
            }
        }
        let ret = if n >= 32 { window } else { window & ((1u32 << n) - 1) };
        window = if n >= 32 { 0 } else { window >> n };
        avail -= n;
        self.end_window = window;
        self.nend_bits = avail;
        self.nbits_total += n as i32;
        ret
    }
    pub fn tell(&self) -> i32 {
        self.nbits_total - ilog(self.rng) as i32
    }
    pub fn tell_frac(&self) -> i32 {
        tell_frac(self.nbits_total, self.rng)
    }
    /// Make the decoder report that every bit was consumed (a silence frame).
    pub fn exhaust(&mut self) {
        let t = self.buf.len() as i32 * 8 - self.tell();
        self.nbits_total += t;
    }
}

fn tell_frac(nbits_total: i32, rng: u32) -> i32 {
    const CORRECTION: [u32; 8] = [35733, 38967, 42495, 46340, 50535, 55109, 60097, 65535];
    let nbits = nbits_total << BITRES;
    let mut l = ilog(rng);
    let r = rng >> (l - 16);
    let mut b = ((r >> 12) - 8) as usize;
    if r > CORRECTION[b] {
        b += 1;
    }
    l = (l << 3) + b as u32;
    nbits - l as i32
}

#[derive(Clone)]
pub struct Enc {
    buf: Vec<u8>,
    end_offs: usize,
    end_window: u32,
    nend_bits: u32,
    nbits_total: i32,
    offs: usize,
    pub rng: u32,
    low: u32,
    rem: i32,
    ext: u32,
    pub error: bool,
}

impl Enc {
    pub fn new(size: usize) -> Self {
        Self {
            buf: vec![0; size],
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: (CODE_BITS + 1) as i32,
            offs: 0,
            rng: CODE_TOP,
            low: 0,
            rem: -1,
            ext: 0,
            error: false,
        }
    }
    pub fn storage(&self) -> usize {
        self.buf.len()
    }
    fn write_byte(&mut self, v: u32) {
        if self.offs + self.end_offs >= self.buf.len() {
            self.error = true;
        } else {
            self.buf[self.offs] = v as u8;
            self.offs += 1;
        }
    }
    fn write_byte_at_end(&mut self, v: u32) {
        if self.offs + self.end_offs >= self.buf.len() {
            self.error = true;
        } else {
            self.end_offs += 1;
            let n = self.buf.len();
            self.buf[n - self.end_offs] = v as u8;
        }
    }
    fn carry_out(&mut self, c: i32) {
        if c != SYM_MAX as i32 {
            let carry = c >> SYM_BITS;
            if self.rem >= 0 {
                self.write_byte((self.rem + carry) as u32);
            }
            if self.ext > 0 {
                let sym = (SYM_MAX as i32 + carry) as u32 & SYM_MAX;
                while self.ext > 0 {
                    self.write_byte(sym);
                    self.ext -= 1;
                }
            }
            self.rem = c & SYM_MAX as i32;
        } else {
            self.ext += 1;
        }
    }
    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.carry_out((self.low >> CODE_SHIFT) as i32);
            self.low = (self.low << SYM_BITS) & (CODE_TOP - 1);
            self.rng <<= SYM_BITS;
            self.nbits_total += SYM_BITS as i32;
        }
    }
    pub fn encode(&mut self, fl: u32, fh: u32, ft: u32) {
        let r = self.rng / ft;
        if fl > 0 {
            self.low = self.low.wrapping_add(self.rng - r * (ft - fl));
            self.rng = r * (fh - fl);
        } else {
            self.rng -= r * (ft - fh);
        }
        self.normalize();
    }
    pub fn encode_bin(&mut self, fl: u32, fh: u32, bits: u32) {
        let r = self.rng >> bits;
        let ft = 1u32 << bits;
        if fl > 0 {
            self.low = self.low.wrapping_add(self.rng - r * (ft - fl));
            self.rng = r * (fh - fl);
        } else {
            self.rng -= r * (ft - fh);
        }
        self.normalize();
    }
    pub fn bit_logp(&mut self, val: bool, logp: u32) {
        let mut r = self.rng;
        let l = self.low;
        let s = r >> logp;
        r -= s;
        if val {
            self.low = l.wrapping_add(r);
        }
        self.rng = if val { s } else { r };
        self.normalize();
    }
    pub fn icdf(&mut self, s: usize, icdf: &[u8], ftb: u32) {
        let r = self.rng >> ftb;
        if s > 0 {
            self.low = self.low.wrapping_add(self.rng - r * u32::from(icdf[s - 1]));
            self.rng = r * u32::from(icdf[s - 1] - icdf[s]);
        } else {
            self.rng -= r * u32::from(icdf[s]);
        }
        self.normalize();
    }
    pub fn uint(&mut self, fl: u32, ft: u32) {
        let ft = ft - 1;
        let mut ftb = ilog(ft);
        if ftb > UINT_BITS {
            ftb -= UINT_BITS;
            let ft1 = (ft >> ftb) + 1;
            let fl1 = fl >> ftb;
            self.encode(fl1, fl1 + 1, ft1);
            self.bits(fl & ((1 << ftb) - 1), ftb);
        } else {
            self.encode(fl, fl + 1, ft + 1);
        }
    }
    pub fn bits(&mut self, fl: u32, bits: u32) {
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        if used + bits > 32 {
            loop {
                self.write_byte_at_end(window & SYM_MAX);
                window >>= SYM_BITS;
                used -= SYM_BITS;
                if used < SYM_BITS {
                    break;
                }
            }
        }
        window |= fl << used;
        used += bits;
        self.end_window = window;
        self.nend_bits = used;
        self.nbits_total += bits as i32;
    }
    pub fn tell(&self) -> i32 {
        self.nbits_total - ilog(self.rng) as i32
    }
    pub fn tell_frac(&self) -> i32 {
        tell_frac(self.nbits_total, self.rng)
    }
    /// Flush and return the finished buffer (its length is the size given to `new`).
    pub fn finish(mut self) -> (Vec<u8>, bool) {
        let mut l = CODE_BITS as i32 - ilog(self.rng) as i32;
        let mut msk = (CODE_TOP - 1) >> l;
        let mut end = self.low.wrapping_add(msk) & !msk;
        if (end | msk) >= self.low.wrapping_add(self.rng) {
            l += 1;
            msk >>= 1;
            end = self.low.wrapping_add(msk) & !msk;
        }
        while l > 0 {
            self.carry_out((end >> CODE_SHIFT) as i32);
            end = (end << SYM_BITS) & (CODE_TOP - 1);
            l -= SYM_BITS as i32;
        }
        if self.rem >= 0 || self.ext > 0 {
            self.carry_out(0);
        }
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        while used >= SYM_BITS {
            self.write_byte_at_end(window & SYM_MAX);
            window >>= SYM_BITS;
            used -= SYM_BITS;
        }
        if !self.error {
            let (a, b) = (self.offs, self.buf.len() - self.end_offs);
            for x in &mut self.buf[a..b] {
                *x = 0;
            }
            if used > 0 {
                if self.end_offs >= self.buf.len() {
                    self.error = true;
                } else {
                    let l = (-l) as u32;
                    if self.offs + self.end_offs >= self.buf.len() && l < used {
                        window &= (1 << l) - 1;
                        self.error = true;
                    }
                    let at = self.buf.len() - self.end_offs - 1;
                    self.buf[at] |= window as u8;
                }
            }
        }
        (self.buf, self.error)
    }
}

/// One range coder in either direction.
pub enum Rc<'a> {
    Dec(Dec<'a>),
    Enc(Enc),
}

impl Rc<'_> {
    pub fn encoding(&self) -> bool {
        matches!(self, Rc::Enc(_))
    }
    pub fn storage(&self) -> usize {
        match self {
            Rc::Dec(d) => d.storage(),
            Rc::Enc(e) => e.storage(),
        }
    }
    pub fn tell(&self) -> i32 {
        match self {
            Rc::Dec(d) => d.tell(),
            Rc::Enc(e) => e.tell(),
        }
    }
    pub fn tell_frac(&self) -> i32 {
        match self {
            Rc::Dec(d) => d.tell_frac(),
            Rc::Enc(e) => e.tell_frac(),
        }
    }
    pub fn rng(&self) -> u32 {
        match self {
            Rc::Dec(d) => d.rng,
            Rc::Enc(e) => e.rng,
        }
    }
    pub fn bit_logp(&mut self, v: bool, logp: u32) -> bool {
        match self {
            Rc::Dec(d) => d.bit_logp(logp),
            Rc::Enc(e) => {
                e.bit_logp(v, logp);
                v
            }
        }
    }
    pub fn icdf(&mut self, v: usize, icdf: &[u8], ftb: u32) -> usize {
        match self {
            Rc::Dec(d) => d.icdf(icdf, ftb),
            Rc::Enc(e) => {
                e.icdf(v, icdf, ftb);
                v
            }
        }
    }
    pub fn uint(&mut self, v: u32, ft: u32) -> u32 {
        match self {
            Rc::Dec(d) => d.uint(ft),
            Rc::Enc(e) => {
                e.uint(v, ft);
                v
            }
        }
    }
    pub fn bits(&mut self, v: u32, n: u32) -> u32 {
        match self {
            Rc::Dec(d) => d.bits(n),
            Rc::Enc(e) => {
                e.bits(v, n);
                v
            }
        }
    }
    /// Custom models: when decoding, `decode` gives the cumulative frequency to map to a
    /// symbol and `update` consumes it; when encoding, `encode` writes the symbol's interval.
    pub fn decode(&mut self, ft: u32) -> u32 {
        match self {
            Rc::Dec(d) => d.decode(ft),
            Rc::Enc(_) => 0,
        }
    }
    pub fn update_or_encode(&mut self, fl: u32, fh: u32, ft: u32) {
        match self {
            Rc::Dec(d) => d.update(fl, fh, ft),
            Rc::Enc(e) => e.encode(fl, fh, ft),
        }
    }

    /// Laplace-distributed integer (RFC 6716 section 4.3.2.1).
    pub fn laplace(&mut self, value: i32, fs0: u32, decay: u32) -> i32 {
        const MINP: u32 = 1;
        const NMIN: u32 = 16;
        let freq1 = |fs0: u32| -> u32 {
            let ft = 32768 - MINP * (2 * NMIN) - fs0;
            (ft * (16384 - decay)) >> 15
        };
        let mut fs = fs0;
        match self {
            Rc::Enc(e) => {
                let mut fl = 0u32;
                let mut val = value;
                let mut out = value;
                if val != 0 {
                    let s: i32 = -i32::from(val < 0);
                    val = (val + s) ^ s;
                    fl = fs;
                    fs = freq1(fs);
                    let mut i = 1;
                    while fs > 0 && i < val {
                        fs *= 2;
                        fl += fs + 2 * MINP;
                        fs = (fs * decay) >> 15;
                        i += 1;
                    }
                    if fs == 0 {
                        let ndi_max = ((32768 - fl + MINP - 1) as i32 - s) >> 1;
                        let di = (val - i).min(ndi_max - 1);
                        fl += ((2 * di + 1 + s) as u32) * MINP;
                        fs = MINP.min(32768 - fl);
                        out = (i + di + s) ^ s;
                    } else {
                        fs += MINP;
                        fl += fs & !(s as u32);
                    }
                }
                e.encode_bin(fl, fl + fs, 15);
                out
            }
            Rc::Dec(d) => {
                let fm = d.decode_bin(15);
                let mut fl = 0u32;
                let mut val = 0i32;
                if fm >= fs {
                    val += 1;
                    fl = fs;
                    fs = freq1(fs) + MINP;
                    while fs > MINP && fm >= fl + 2 * fs {
                        fs *= 2;
                        fl += fs;
                        fs = ((fs - 2 * MINP) * decay) >> 15;
                        fs += MINP;
                        val += 1;
                    }
                    if fs <= MINP {
                        let di = (fm - fl) >> 1;
                        val += di as i32;
                        fl += 2 * di * MINP;
                    }
                    if fm < fl + fs {
                        val = -val;
                    } else {
                        fl += fs;
                    }
                }
                d.update(fl, (fl + fs).min(32768), 32768);
                val
            }
        }
    }
}
