//! LZMA (Igor Pavlov's algorithm; the format is documented in the LZMA SDK) and LZMA2.
//! Everything is decoded into memory; the output buffer doubles as the dictionary.
#![allow(clippy::indexing_slicing)] // table indices are bounded by the bit widths that form them

mod enc;
pub use enc::{encode_lzma, encode_lzma2, Level};

use oc_core::{Error, ErrorCode, Limits, Progress, Result};

const PROB_INIT: u16 = 1024;
const MATCH_MIN: usize = 2;
const STATES: usize = 12;
const POS_STATES_MAX: usize = 16;
const END_POS_MODEL: usize = 14;
const FULL_DISTANCES: usize = 128;
const ALIGN_BITS: u32 = 4;

/// Probability model shared by the decoder and the encoder.
#[derive(Clone)]
pub(crate) struct Model {
    pub lc: u32,
    pub lp: u32,
    pub pb: u32,
    pub is_match: [[u16; POS_STATES_MAX]; STATES],
    pub is_rep: [u16; STATES],
    pub is_rep_g0: [u16; STATES],
    pub is_rep_g1: [u16; STATES],
    pub is_rep_g2: [u16; STATES],
    pub is_rep0_long: [[u16; POS_STATES_MAX]; STATES],
    pub pos_slot: [[u16; 64]; 4],
    pub pos_special: [u16; FULL_DISTANCES - END_POS_MODEL + 1],
    pub align: [u16; 1 << ALIGN_BITS],
    pub len: LenModel,
    pub rep_len: LenModel,
    pub literal: Vec<u16>,
}

#[derive(Clone)]
pub(crate) struct LenModel {
    pub choice: u16,
    pub choice2: u16,
    pub low: [[u16; 8]; POS_STATES_MAX],
    pub mid: [[u16; 8]; POS_STATES_MAX],
    pub high: [u16; 256],
}

impl LenModel {
    fn new() -> Self {
        Self {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; POS_STATES_MAX],
            mid: [[PROB_INIT; 8]; POS_STATES_MAX],
            high: [PROB_INIT; 256],
        }
    }
}

impl Model {
    pub fn new(lc: u32, lp: u32, pb: u32) -> Self {
        Self {
            lc,
            lp,
            pb,
            is_match: [[PROB_INIT; POS_STATES_MAX]; STATES],
            is_rep: [PROB_INIT; STATES],
            is_rep_g0: [PROB_INIT; STATES],
            is_rep_g1: [PROB_INIT; STATES],
            is_rep_g2: [PROB_INIT; STATES],
            is_rep0_long: [[PROB_INIT; POS_STATES_MAX]; STATES],
            pos_slot: [[PROB_INIT; 64]; 4],
            pos_special: [PROB_INIT; FULL_DISTANCES - END_POS_MODEL + 1],
            align: [PROB_INIT; 1 << ALIGN_BITS],
            len: LenModel::new(),
            rep_len: LenModel::new(),
            literal: vec![PROB_INIT; 0x300 << (lc + lp)],
        }
    }
}

/// Decodes the lc/lp/pb byte.
pub fn parse_props(b: u8) -> Result<(u32, u32, u32)> {
    if b >= 9 * 5 * 5 {
        return Err(Error::corrupt("LZMA: bad properties byte"));
    }
    let b = u32::from(b);
    Ok((b % 9, (b / 9) % 5, b / 45))
}

struct Rc<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Rc<'a> {
    fn new(data: &'a [u8]) -> Result<Self> {
        if data.len() < 5 {
            return Err(Error::truncated("LZMA: range coder header missing"));
        }
        if data[0] != 0 {
            return Err(Error::corrupt("LZMA: bad range coder start"));
        }
        let code = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        Ok(Self {
            data,
            pos: 5,
            range: 0xFFFF_FFFF,
            code,
        })
    }

    #[inline]
    fn norm(&mut self) -> Result<()> {
        if self.range < (1 << 24) {
            let b = *self
                .data
                .get(self.pos)
                .ok_or_else(|| Error::truncated("LZMA stream ended").at(self.pos as u64))?;
            self.pos += 1;
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(b);
        }
        Ok(())
    }

    #[inline]
    fn bit(&mut self, p: &mut u16) -> Result<u32> {
        self.norm()?;
        let bound = (self.range >> 11) * u32::from(*p);
        if self.code < bound {
            self.range = bound;
            *p += (2048 - *p) >> 5;
            Ok(0)
        } else {
            self.range -= bound;
            self.code -= bound;
            *p -= *p >> 5;
            Ok(1)
        }
    }

    fn direct(&mut self, n: u32) -> Result<u32> {
        let mut r = 0;
        for _ in 0..n {
            self.norm()?;
            self.range >>= 1;
            let t = (self.code.wrapping_sub(self.range) >> 31).wrapping_sub(1); // all ones if code >= range
            self.code = self.code.wrapping_sub(self.range & t);
            r = (r << 1) | (t & 1);
        }
        Ok(r)
    }

    fn tree(&mut self, probs: &mut [u16], bits: u32) -> Result<u32> {
        let mut m = 1usize;
        for _ in 0..bits {
            m = (m << 1) + self.bit(&mut probs[m])? as usize;
        }
        Ok(m as u32 - (1 << bits))
    }

    fn rev_tree(&mut self, probs: &mut [u16], bits: u32) -> Result<u32> {
        let mut m = 1usize;
        let mut r = 0;
        for i in 0..bits {
            let b = self.bit(&mut probs[m])?;
            m = (m << 1) + b as usize;
            r |= b << i;
        }
        Ok(r)
    }

    /// True when the stream ended cleanly (code zero after the last symbol).
    fn finished_ok(&self) -> bool {
        self.code == 0
    }
}

fn decode_len(rc: &mut Rc<'_>, m: &mut LenModel, pos_state: usize) -> Result<usize> {
    if rc.bit(&mut m.choice)? == 0 {
        return Ok(MATCH_MIN + rc.tree(&mut m.low[pos_state], 3)? as usize);
    }
    if rc.bit(&mut m.choice2)? == 0 {
        return Ok(MATCH_MIN + 8 + rc.tree(&mut m.mid[pos_state], 3)? as usize);
    }
    Ok(MATCH_MIN + 16 + rc.tree(&mut m.high, 8)? as usize)
}

/// The decoder state that LZMA2 carries from chunk to chunk.
pub(crate) struct Decoder {
    pub model: Model,
    state: usize,
    reps: [usize; 4],
}

impl Decoder {
    pub fn new(lc: u32, lp: u32, pb: u32) -> Self {
        Self {
            model: Model::new(lc, lp, pb),
            state: 0,
            reps: [0; 4],
        }
    }

    pub fn reset(&mut self, lc: u32, lp: u32, pb: u32) {
        *self = Self::new(lc, lp, pb);
    }

    /// Decodes until `out` holds `limit` bytes (when given) or an end marker is read.
    /// `base` is where the current dictionary starts in `out`. Returns true if an end
    /// marker was seen.
    pub fn run(
        &mut self,
        data: &[u8],
        out: &mut Vec<u8>,
        base: usize,
        limit: Option<usize>,
        limits: &Limits,
        progress: &dyn Progress,
    ) -> Result<(bool, usize)> {
        let mut rc = Rc::new(data)?;
        let pb_mask = (1usize << self.model.pb) - 1;
        let lp_mask = (1usize << self.model.lp) - 1;
        let lc = self.model.lc;
        let mut eos = false;
        let mut since_check = 0usize;
        loop {
            if let Some(l) = limit {
                if out.len() >= l {
                    break;
                }
            }
            since_check += 1;
            if since_check >= 1 << 16 {
                since_check = 0;
                progress.check_cancelled()?;
                limits.check_expansion(data.len() as u64, out.len() as u64)?;
            }
            let pos = out.len() - base;
            let pos_state = pos & pb_mask;
            let st = self.state;
            if rc.bit(&mut self.model.is_match[st][pos_state])? == 0 {
                let prev = if pos > 0 { out[out.len() - 1] } else { 0 };
                let lit_state = ((pos & lp_mask) << lc) + (usize::from(prev) >> (8 - lc));
                let probs = &mut self.model.literal[0x300 * lit_state..0x300 * (lit_state + 1)];
                let mut sym = 1usize;
                if st >= 7 {
                    let mut mb = usize::from(out[out.len() - self.reps[0] - 1]);
                    let mut offs = 0x100usize;
                    while sym < 0x100 {
                        mb <<= 1;
                        let mbit = mb & offs;
                        let b = rc.bit(&mut probs[offs + mbit + sym])? as usize;
                        sym = (sym << 1) | b;
                        if b == 0 {
                            offs &= !mbit;
                        } else {
                            offs &= mbit;
                        }
                    }
                } else {
                    while sym < 0x100 {
                        sym = (sym << 1) | rc.bit(&mut probs[sym])? as usize;
                    }
                }
                out.push(sym as u8);
                self.state = if st < 4 {
                    0
                } else if st < 10 {
                    st - 3
                } else {
                    st - 6
                };
                continue;
            }
            let len;
            if rc.bit(&mut self.model.is_rep[st])? == 0 {
                // A new match: length, then distance.
                self.reps[3] = self.reps[2];
                self.reps[2] = self.reps[1];
                self.reps[1] = self.reps[0];
                len = decode_len(&mut rc, &mut self.model.len, pos_state)?;
                self.state = if st < 7 { 7 } else { 10 };
                let len_state = (len - MATCH_MIN).min(3);
                let slot = rc.tree(&mut self.model.pos_slot[len_state], 6)? as usize;
                let dist = if slot < 4 {
                    slot
                } else {
                    let n = (slot >> 1) as u32 - 1;
                    let mut d = (2 | (slot & 1)) << n;
                    if slot < END_POS_MODEL {
                        let base_idx = d - slot;
                        let probs = &mut self.model.pos_special[base_idx..];
                        d += rc.rev_tree(probs, n)? as usize;
                    } else {
                        d += (rc.direct(n - ALIGN_BITS)? as usize) << ALIGN_BITS;
                        d += rc.rev_tree(&mut self.model.align, ALIGN_BITS)? as usize;
                    }
                    d
                };
                if dist == 0xFFFF_FFFF {
                    eos = true;
                    break;
                }
                self.reps[0] = dist;
            } else {
                if pos == 0 {
                    return Err(Error::corrupt("LZMA: repeat match at the start"));
                }
                if rc.bit(&mut self.model.is_rep_g0[st])? == 0 {
                    if rc.bit(&mut self.model.is_rep0_long[st][pos_state])? == 0 {
                        // Short rep: one byte from rep0.
                        let d = self.reps[0];
                        if d >= pos {
                            return Err(Error::corrupt("LZMA: distance beyond the dictionary"));
                        }
                        let b = out[out.len() - d - 1];
                        out.push(b);
                        self.state = if st < 7 { 9 } else { 11 };
                        continue;
                    }
                } else {
                    let dist;
                    if rc.bit(&mut self.model.is_rep_g1[st])? == 0 {
                        dist = self.reps[1];
                    } else {
                        if rc.bit(&mut self.model.is_rep_g2[st])? == 0 {
                            dist = self.reps[2];
                        } else {
                            dist = self.reps[3];
                            self.reps[3] = self.reps[2];
                        }
                        self.reps[2] = self.reps[1];
                    }
                    self.reps[1] = self.reps[0];
                    self.reps[0] = dist;
                }
                len = decode_len(&mut rc, &mut self.model.rep_len, pos_state)?;
                self.state = if st < 7 { 8 } else { 11 };
            }
            let d = self.reps[0];
            if d >= pos {
                return Err(Error::corrupt("LZMA: distance beyond the dictionary"));
            }
            if let Some(l) = limit {
                if out.len() + len > l {
                    return Err(Error::corrupt("LZMA: match runs past the end of the data"));
                }
            }
            let new_len = out.len() + len;
            limits.check_alloc(new_len as u64)?;
            let start = out.len() - d - 1;
            if d + 1 >= len {
                out.extend_from_within(start..start + len);
            } else {
                out.reserve(len);
                for i in 0..len {
                    let b = out[start + i];
                    out.push(b);
                }
            }
        }
        if eos && !rc.finished_ok() {
            return Err(Error::corrupt("LZMA: stream does not end cleanly"));
        }
        Ok((eos, rc.pos))
    }
}

/// A raw LZMA stream with known properties (7z, ZIP, XZ-less uses). With `size` the stream
/// stops after that many bytes; without it an end marker is required.
pub fn decode_raw(
    props: u8,
    data: &[u8],
    size: Option<u64>,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Vec<u8>> {
    let (lc, lp, pb) = parse_props(props)?;
    let mut dec = Decoder::new(lc, lp, pb);
    let mut out = Vec::new();
    if let Some(s) = size {
        limits.check_alloc(s)?;
        out.reserve(usize::try_from(s).unwrap_or(0).min(1 << 28));
    }
    let limit = size.map(|s| s as usize);
    let (eos, _) = dec.run(data, &mut out, 0, limit, limits, progress)?;
    if size.is_none() && !eos {
        return Err(Error::truncated("LZMA: no end marker"));
    }
    Ok(out)
}

/// The legacy `.lzma` container: properties, dictionary size, 64-bit size, then the stream.
pub fn decode_alone(data: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    if data.len() < 13 {
        return Err(Error::truncated("LZMA: header missing"));
    }
    let size = u64::from_le_bytes(data[5..13].try_into().unwrap_or([0; 8]));
    let known = if size == u64::MAX { None } else { Some(size) };
    decode_raw(data[0], &data[13..], known, limits, progress)
}

/// Writes the `.lzma` container.
pub fn encode_alone(data: &[u8], level: Level) -> Vec<u8> {
    let (props, dict, stream) = encode_lzma(data, level, true);
    let mut out = vec![props];
    out.extend_from_slice(&dict.to_le_bytes());
    out.extend_from_slice(&u64::MAX.to_le_bytes());
    out.extend(stream);
    out
}

/// LZMA2 chunk stream as stored in XZ and 7z. `dict_size` limits match distances.
pub fn decode_lzma2(
    data: &[u8],
    dict_size: u32,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<Vec<u8>> {
    decode_lzma2_counted(data, dict_size, limits, progress).map(|(v, _)| v)
}

/// Like [`decode_lzma2`], also returning how many input bytes the chunks (and their end
/// marker) took.
pub fn decode_lzma2_counted(
    data: &[u8],
    dict_size: u32,
    limits: &Limits,
    progress: &dyn Progress,
) -> Result<(Vec<u8>, usize)> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut dec: Option<Decoder> = None;
    let mut base = 0usize;
    let mut need_dict_reset = true;
    let mut need_props = true;
    let _ = dict_size;
    loop {
        progress.check_cancelled()?;
        let ctl = *data
            .get(pos)
            .ok_or_else(|| Error::truncated("LZMA2: missing end marker").at(pos as u64))?;
        pos += 1;
        if ctl == 0 {
            return Ok((out, pos));
        }
        if ctl < 0x80 {
            if ctl > 2 {
                return Err(Error::corrupt("LZMA2: bad control byte").at(pos as u64 - 1));
            }
            let sz = data
                .get(pos..pos + 2)
                .ok_or_else(|| Error::truncated("LZMA2: chunk header"))?;
            let n = usize::from(u16::from_be_bytes([sz[0], sz[1]])) + 1;
            pos += 2;
            if ctl == 1 {
                base = out.len();
                need_dict_reset = false;
            } else if need_dict_reset {
                return Err(Error::corrupt("LZMA2: first chunk must reset the dictionary"));
            }
            let body = data
                .get(pos..pos + n)
                .ok_or_else(|| Error::truncated("LZMA2: stored chunk"))?;
            out.extend_from_slice(body);
            pos += n;
        } else {
            let hdr = data
                .get(pos..pos + 4)
                .ok_or_else(|| Error::truncated("LZMA2: chunk header"))?;
            let usize_ = ((usize::from(ctl & 0x1F) << 16) | (usize::from(hdr[0]) << 8) | usize::from(hdr[1])) + 1;
            let csize = (usize::from(hdr[2]) << 8 | usize::from(hdr[3])) + 1;
            pos += 4;
            let reset = (ctl >> 5) & 3;
            if reset == 3 {
                base = out.len();
                need_dict_reset = false;
            } else if need_dict_reset {
                return Err(Error::corrupt("LZMA2: first chunk must reset the dictionary"));
            }
            if reset >= 2 {
                let b = *data
                    .get(pos)
                    .ok_or_else(|| Error::truncated("LZMA2: properties"))?;
                pos += 1;
                let (lc, lp, pb) = parse_props(b)?;
                if lc + lp > 4 {
                    return Err(Error::corrupt("LZMA2: lc + lp above 4"));
                }
                dec = Some(Decoder::new(lc, lp, pb));
                need_props = false;
            } else if need_props {
                return Err(Error::corrupt("LZMA2: properties missing"));
            } else if reset == 1 {
                if let Some(d) = dec.as_mut() {
                    let (lc, lp, pb) = (d.model.lc, d.model.lp, d.model.pb);
                    d.reset(lc, lp, pb);
                }
            }
            let body = data
                .get(pos..pos + csize)
                .ok_or_else(|| Error::truncated("LZMA2: compressed chunk"))?;
            let d = dec
                .as_mut()
                .ok_or_else(|| Error::corrupt("LZMA2: properties missing"))?;
            let target = out.len() + usize_;
            limits.check_alloc(target as u64)?;
            let (eos, _) = d.run(body, &mut out, base, Some(target), limits, progress)?;
            if eos || out.len() != target {
                return Err(Error::corrupt("LZMA2: chunk size mismatch"));
            }
            pos += csize;
        }
        limits.check_expansion(data.len() as u64, out.len() as u64)?;
    }
}

pub(crate) fn unsupported(msg: &str) -> Error {
    Error::new(ErrorCode::UnsupportedVariant, msg.to_string())
}
