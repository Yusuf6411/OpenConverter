//! Arithmetic-coded JPEG scans (ITU T.81 Annex D and F.1.4, F.2.4): sequential and
//! progressive, with the QM-coder.
//!
//! The decoder follows the structure of the standard's flow charts. As in the standard, a
//! marker inside the entropy-coded data is not an error: zero bits are supplied until the
//! scan is complete.
// Context indices are bounded by the coding process: at most 35 for DC and 245 for AC.
#![allow(clippy::indexing_slicing)]
use super::{clamp16, Frame, Scan, ScanComp};
use crate::jpeg::arith_table::ARITAB;
use crate::jpeg::tables::ZIGZAG;
use oc_core::{Error, Progress, Result};

/// Conditioning values from DAC segments (or their defaults).
#[derive(Clone, Copy)]
pub(super) struct Cond {
    /// DC lower and upper bounds as powers of two, per table.
    pub dc_l: [u8; 4],
    pub dc_u: [u8; 4],
    /// AC band split, per table.
    pub ac_k: [u8; 4],
}

impl Default for Cond {
    fn default() -> Self {
        Self {
            dc_l: [0; 4],
            dc_u: [1; 4],
            ac_k: [5; 4],
        }
    }
}

pub(super) fn parse_dac(mut seg: &[u8], cond: &mut Cond) -> Result<()> {
    while let [tc_tb, cs, rest @ ..] = seg {
        let tb = usize::from(tc_tb & 15);
        if tb > 3 {
            return Err(Error::corrupt("JPEG DAC names a bad table"));
        }
        if tc_tb >> 4 == 0 {
            let (l, u) = (cs & 15, cs >> 4);
            if l > u {
                return Err(Error::corrupt("JPEG DAC has lower bound above upper bound"));
            }
            cond.dc_l[tb] = l;
            cond.dc_u[tb] = u;
        } else {
            if *cs == 0 || *cs > 63 {
                return Err(Error::corrupt("JPEG DAC has a bad AC band"));
            }
            cond.ac_k[tb] = *cs;
        }
        seg = rest;
    }
    Ok(())
}

struct Dec<'a> {
    data: &'a [u8],
    pos: usize,
    /// A marker (or the end of the data) was reached; zeros are supplied from here on.
    marker: bool,
    c: i64,
    a: i64,
    ct: i32,
}

impl Dec<'_> {
    fn reset(&mut self) {
        self.c = 0;
        self.a = 0;
        self.ct = -16;
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.data.get(self.pos).copied()?;
        self.pos += 1;
        Some(b)
    }

    fn byte(&mut self) -> u8 {
        if self.marker {
            return 0;
        }
        match self.next() {
            None => {
                self.marker = true;
                0
            }
            Some(0xFF) => {
                let code = loop {
                    match self.next() {
                        Some(0xFF) => continue,
                        other => break other,
                    }
                };
                match code {
                    Some(0) => 0xFF,
                    _ => {
                        // Leave the marker where the segment parser will find it.
                        self.marker = true;
                        self.pos = self.pos.saturating_sub(if code.is_some() { 2 } else { 1 });
                        0
                    }
                }
            }
            Some(b) => b,
        }
    }

    fn decode(&mut self, st: &mut u8) -> bool {
        while self.a < 0x8000 {
            self.ct -= 1;
            if self.ct < 0 {
                let b = self.byte();
                self.c = (self.c << 8) | i64::from(b);
                self.ct += 8;
                if self.ct < 0 {
                    self.ct += 1;
                    if self.ct == 0 {
                        self.a = 0x8000;
                    }
                }
            }
            self.a <<= 1;
        }
        let mut sv = *st;
        let entry = ARITAB.get(usize::from(sv & 0x7F)).copied().unwrap_or(0);
        let qe = i64::from(entry >> 16);
        let nm = ((entry >> 8) & 0xFF) as u8;
        let nl = (entry & 0xFF) as u8;
        let mut temp = self.a - qe;
        self.a = temp;
        temp <<= self.ct;
        if self.c >= temp {
            self.c -= temp;
            // Conditional LPS exchange.
            if self.a < qe {
                self.a = qe;
                *st = (sv & 0x80) ^ nm;
            } else {
                self.a = qe;
                *st = (sv & 0x80) ^ nl;
                sv ^= 0x80;
            }
        } else if self.a < 0x8000 {
            // Conditional MPS exchange.
            if self.a < qe {
                *st = (sv & 0x80) ^ nl;
                sv ^= 0x80;
            } else {
                *st = (sv & 0x80) ^ nm;
            }
        }
        sv >> 7 != 0
    }

    /// Moves to just after the next RSTn marker and restarts the coder.
    fn restart(&mut self) {
        self.marker = false;
        self.reset();
        while self.pos + 1 < self.data.len() {
            let (a, b) = (self.data[self.pos], self.data[self.pos + 1]);
            if a == 0xFF && (0xD0..=0xD7).contains(&b) {
                self.pos += 2;
                return;
            }
            if a == 0xFF && b != 0 && b != 0xFF {
                self.marker = true;
                return;
            }
            self.pos += 1;
        }
        self.marker = true;
    }
}

struct State {
    dc: [[u8; 64]; 4],
    ac: [[u8; 256]; 4],
    fixed: u8,
    dc_context: [usize; 4],
    last_dc: [i32; 4],
}

impl State {
    fn new() -> Self {
        Self {
            dc: [[0; 64]; 4],
            ac: [[0; 256]; 4],
            fixed: 113,
            dc_context: [0; 4],
            last_dc: [0; 4],
        }
    }
}

/// Decodes the magnitude bits that follow a category, shared by DC and AC: `m` is the
/// category's leading value, `st` the first of the bit-pattern contexts.
fn magnitude_bits(d: &mut Dec<'_>, stats: &mut [u8], mut st: usize, mut m: i32, sign: bool) -> i32 {
    let mut v = m;
    st += 14;
    loop {
        m >>= 1;
        if m == 0 {
            break;
        }
        if stats.get_mut(st).is_some_and(|s| d.decode(s)) {
            v |= m;
        }
    }
    v += 1;
    if sign {
        -v
    } else {
        v
    }
}

/// Decodes one scan starting at `pos`; returns where it ended and whether the data was bad.
pub(super) fn decode_scan(
    frame: &mut Frame,
    scan: &Scan,
    data: &[u8],
    pos: usize,
    cond: &Cond,
    restart_interval: usize,
    progress: &dyn Progress,
) -> Result<(usize, bool)> {
    let interleaved = scan.comps.len() > 1;
    let total = if interleaved {
        frame.mcus_x * frame.mcus_y
    } else {
        let c = scan
            .comps
            .first()
            .and_then(|s| frame.comps.get(s.index))
            .ok_or_else(|| Error::corrupt("JPEG scan has no components"))?;
        c.real_bw * c.real_bh
    };
    let mut d = Dec {
        data,
        pos,
        marker: false,
        c: 0,
        a: 0,
        ct: -16,
    };
    let mut st = State::new();
    let mut bad = false;
    let mcus_x = frame.mcus_x;
    for mcu in 0..total {
        if restart_interval > 0 && mcu > 0 && mcu % restart_interval == 0 {
            d.restart();
            st = State::new();
        }
        if mcu % 256 == 0 {
            progress.check_cancelled()?;
        }
        if interleaved {
            let (mx, my) = (mcu % mcus_x, mcu / mcus_x);
            for sc in &scan.comps {
                let (h, v) = frame
                    .comps
                    .get(sc.index)
                    .map(|c| (c.h, c.v))
                    .ok_or_else(|| Error::corrupt("JPEG scan names a missing component"))?;
                for by in 0..v {
                    for bx in 0..h {
                        bad |= !block(
                            frame,
                            scan,
                            sc,
                            my * v + by,
                            mx * h + bx,
                            &mut d,
                            &mut st,
                            cond,
                        )?;
                    }
                }
            }
        } else if let Some(sc) = scan.comps.first() {
            let real_bw = frame.comps.get(sc.index).map_or(1, |c| c.real_bw.max(1));
            bad |= !block(
                frame,
                scan,
                sc,
                mcu / real_bw,
                mcu % real_bw,
                &mut d,
                &mut st,
                cond,
            )?;
        }
        if bad {
            break;
        }
    }
    Ok((d.pos, bad))
}

/// Decodes one block; `Ok(false)` means the data is invalid and the scan should stop.
#[allow(clippy::too_many_arguments)]
fn block(
    frame: &mut Frame,
    scan: &Scan,
    sc: &ScanComp,
    by: usize,
    bx: usize,
    d: &mut Dec<'_>,
    st: &mut State,
    cond: &Cond,
) -> Result<bool> {
    let comp = frame
        .comps
        .get_mut(sc.index)
        .ok_or_else(|| Error::corrupt("JPEG scan names a missing component"))?;
    let off = (by * comp.bw + bx) * 64;
    let blk = comp
        .coefs
        .get_mut(off..off + 64)
        .ok_or_else(|| Error::corrupt("JPEG block outside the image"))?;
    let (td, ta, ci) = (sc.td.min(3), sc.ta.min(3), sc.index.min(3));
    let progressive = frame.progressive;
    let (ss, se, ah, al) = (scan.ss, scan.se, scan.ah, scan.al);

    // DC coefficient (sequential, or the first progressive DC pass).
    if !progressive || (ss == 0 && ah == 0) {
        let ctx = st.dc_context[ci];
        let stats = &mut st.dc[td];
        let mut diff_ctx = ctx;
        if !d.decode(&mut stats[diff_ctx]) {
            st.dc_context[ci] = 0;
        } else {
            let sign = d.decode(&mut stats[diff_ctx + 1]);
            diff_ctx += 2 + usize::from(sign);
            let mut m = i32::from(d.decode(&mut stats[diff_ctx]));
            let mut at = diff_ctx;
            if m != 0 {
                at = 20;
                while d.decode(&mut stats[at]) {
                    m <<= 1;
                    if m == 0x8000 {
                        return Ok(false);
                    }
                    at += 1;
                }
            }
            let dc_l = i32::from(cond.dc_l[td]);
            let dc_u = i32::from(cond.dc_u[td]);
            st.dc_context[ci] = if m < ((1i32 << dc_l) >> 1) {
                0
            } else if m > ((1i32 << dc_u) >> 1) {
                12 + 4 * usize::from(sign)
            } else {
                4 + 4 * usize::from(sign)
            };
            let v = magnitude_bits(d, stats, at, m, sign);
            st.last_dc[ci] = st.last_dc[ci].wrapping_add(v);
        }
        if let Some(c) = blk.first_mut() {
            *c = clamp16(st.last_dc[ci] << al);
        }
        if progressive {
            return Ok(true);
        }
    } else if ss == 0 {
        // DC refinement: one bit with a fixed probability.
        if d.decode(&mut st.fixed) {
            if let Some(c) = blk.first_mut() {
                *c |= 1 << al;
            }
        }
        return Ok(true);
    }

    // AC coefficients.
    let (first, last) = if progressive { (ss, se) } else { (1, 63) };
    let natural = |k: usize| ZIGZAG.get(k).copied().unwrap_or(0);
    let stats = &mut st.ac[ta];
    let k_split = usize::from(cond.ac_k[ta]);
    if progressive && ah != 0 {
        // AC refinement.
        let p1 = 1i16 << al;
        let m1 = -1i16 << al;
        let mut kex = se;
        while kex > 0 && blk.get(natural(kex)).copied().unwrap_or(0) == 0 {
            kex -= 1;
        }
        let mut k = ss;
        while k <= se {
            let mut at = 3 * (k - 1);
            if k > kex && d.decode(&mut stats[at]) {
                break;
            }
            loop {
                let Some(coef) = blk.get_mut(natural(k)) else {
                    return Ok(false);
                };
                if *coef != 0 {
                    if d.decode(&mut stats[at + 2]) {
                        *coef = coef.saturating_add(if *coef < 0 { m1 } else { p1 });
                    }
                    break;
                }
                if d.decode(&mut stats[at + 1]) {
                    *coef = if d.decode(&mut st.fixed) { m1 } else { p1 };
                    break;
                }
                at += 3;
                k += 1;
                if k > se {
                    return Ok(false);
                }
            }
            k += 1;
        }
        return Ok(true);
    }

    let mut k = first;
    while k <= last {
        let mut at = 3 * (k - 1);
        if d.decode(&mut stats[at]) {
            break; // end of block
        }
        while !d.decode(&mut stats[at + 1]) {
            at += 3;
            k += 1;
            if k > last {
                return Ok(false);
            }
        }
        let sign = d.decode(&mut st.fixed);
        at += 2;
        let mut m = i32::from(d.decode(&mut stats[at]));
        if m != 0 && d.decode(&mut stats[at]) {
            m <<= 1;
            at = if k <= k_split { 189 } else { 217 };
            while d.decode(&mut stats[at]) {
                m <<= 1;
                if m == 0x8000 {
                    return Ok(false);
                }
                at += 1;
            }
        }
        let v = magnitude_bits(d, stats, at, m, sign);
        if let Some(c) = blk.get_mut(natural(k)) {
            *c = clamp16(v << al);
        }
        k += 1;
    }
    Ok(true)
}
