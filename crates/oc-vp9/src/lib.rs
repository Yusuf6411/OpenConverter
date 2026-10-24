//! A VP9 video decoder written from the VP9 Bitstream & Decoding Process Specification v0.6.
//!
//! Handles every profile (8, 10 and 12 bit; 4:2:0, 4:2:2, 4:4:0 and 4:4:4), inter prediction
//! with scaled references, tiles, segmentation, superframes and hidden frames. Encoding is not
//! provided.
#![forbid(unsafe_code)]
#![allow(clippy::indexing_slicing)] // table and plane indices come from the checked frame layout
#![allow(clippy::unwrap_used)]
#![allow(clippy::needless_range_loop)] // loops index several parallel arrays by block number
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)] // the coefficient tables are 6-deep arrays by the specification's own layout

mod boolcoder;
mod frame;
mod hdr;
mod lf;
mod mi;
mod pred;
mod probs;
mod tables;
mod tile;
mod transform;

pub use frame::Picture;

use frame::FrameBuf;
use hdr::{parse_compressed, parse_uncompressed, Persist};
use mi::Mi;
use oc_core::{Error, Limits, Result};
use std::sync::Arc;
use tile::{FrameState, Seg};

/// What the first frame header of a stream says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub width: usize,
    pub height: usize,
    pub bit_depth: u8,
    pub ss_x: u8,
    pub ss_y: u8,
    pub key_frame: bool,
}

/// Read the size and format from a key frame without decoding it.
pub fn info(data: &[u8]) -> Result<Info> {
    let frame = split_superframe(data)?
        .into_iter()
        .next()
        .ok_or_else(|| Error::truncated("empty VP9 packet"))?;
    let mut p = Persist::default();
    let h = parse_uncompressed(frame, &mut p)?;
    if h.show_existing {
        return Err(Error::corrupt("VP9: no frame header in packet"));
    }
    Ok(Info {
        width: h.width,
        height: h.height,
        bit_depth: h.bit_depth,
        ss_x: h.ss_x,
        ss_y: h.ss_y,
        key_frame: h.frame_type == hdr::KEY_FRAME,
    })
}

/// Whether `data` begins like a VP9 key frame.
pub fn is_key_frame(data: &[u8]) -> bool {
    info(data).map(|i| i.key_frame).unwrap_or(false)
}

/// Split a packet into its frames (Annex B).
fn split_superframe(data: &[u8]) -> Result<Vec<&[u8]>> {
    let Some(&marker) = data.last() else {
        return Err(Error::truncated("empty VP9 packet"));
    };
    if marker & 0xe0 == 0xc0 {
        let frames = usize::from(marker & 7) + 1;
        let mag = usize::from((marker >> 3) & 3) + 1;
        let index = 2 + mag * frames;
        if data.len() >= index && data[data.len() - index] == marker {
            let mut out = Vec::with_capacity(frames);
            let mut at = data.len() - index + 1;
            let mut pos = 0usize;
            for _ in 0..frames {
                let mut size = 0usize;
                for k in 0..mag {
                    size |= usize::from(data[at + k]) << (8 * k);
                }
                at += mag;
                let end = pos.checked_add(size).filter(|&e| e <= data.len() - index);
                let end =
                    end.ok_or_else(|| Error::corrupt("VP9: superframe index is out of range"))?;
                if size > 0 {
                    out.push(&data[pos..end]);
                }
                pos = end;
            }
            return Ok(out);
        }
    }
    Ok(vec![data])
}

/// A VP9 decoder: feed it packets in decode order.
pub struct Decoder {
    p: Persist,
    prev_mi: Vec<Mi>,
    prev_seg: Vec<u8>,
    limits: Limits,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(&Limits::default())
    }
}

impl Decoder {
    pub fn new(limits: &Limits) -> Self {
        Self {
            p: Persist::default(),
            prev_mi: Vec::new(),
            prev_seg: Vec::new(),
            limits: limits.clone(),
        }
    }

    /// Decode one packet (a frame or a superframe) and return the pictures it shows.
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<Picture>> {
        let mut out = Vec::new();
        for frame in split_superframe(data)? {
            if let Some(pic) = self.decode_frame(frame)? {
                out.push(pic);
            }
        }
        Ok(out)
    }

    fn decode_frame(&mut self, data: &[u8]) -> Result<Option<Picture>> {
        let mut h = parse_uncompressed(data, &mut self.p)?;
        if h.show_existing {
            let r = self.p.refs[h.frame_to_show]
                .as_ref()
                .ok_or_else(|| Error::corrupt("VP9: shown frame does not exist"))?;
            return Ok(Some(r.to_picture()));
        }
        self.limits.check_pixels(h.width as u32, h.height as u32)?;
        let aligned = ((h.width + 63) & !63) as u64 * ((h.height + 63) & !63) as u64;
        self.limits.check_alloc(aligned * 6)?;
        self.limits
            .check_alloc((h.mi_rows * h.mi_cols * std::mem::size_of::<Mi>()) as u64)?;
        if h.header_size == 0 {
            return Err(Error::corrupt("VP9: compressed header is missing"));
        }
        let n = h.mi_rows * h.mi_cols;
        // compute_image_size effects (7.2.6)
        let first = self.p.last_size.is_none();
        let changed = self.p.last_size != Some((h.width, h.height));
        if first || changed || self.prev_seg.len() != n || h.past_independent {
            self.prev_seg = vec![0; n];
        }
        let use_prev_mvs = !first
            && !changed
            && self.p.last_show_frame
            && !h.error_res
            && !h.frame_is_intra
            && self.prev_mi.len() == n;
        self.p.last_size = Some((h.width, h.height));
        self.p.last_show_frame = h.show_frame;

        let comp_start = h.uncompressed_len;
        let comp_end = comp_start + h.header_size;
        let comp = data
            .get(comp_start..comp_end)
            .ok_or_else(|| Error::truncated("VP9 compressed header is truncated"))?;
        let mut probs = self.p.saved[h.ctx_idx].clone();
        parse_compressed(comp, &mut h, &mut probs)?;

        let mut refs: [Option<Arc<FrameBuf>>; 3] = Default::default();
        if !h.frame_is_intra {
            for i in 0..3 {
                refs[i] = self.p.refs[h.ref_idx[i]].clone();
            }
        }
        let seg = Seg {
            enabled: self.p.feature_enabled,
            data: self.p.feature_data,
            abs: self.p.seg_abs,
            tree_probs: self.p.seg_tree_probs,
            pred_probs: self.p.seg_pred_probs,
        };
        let prev_mi = if use_prev_mvs {
            std::mem::take(&mut self.prev_mi)
        } else {
            Vec::new()
        };
        let prev_seg = std::mem::take(&mut self.prev_seg);
        let mut fs = FrameState::new(h.clone(), probs, refs, prev_mi, use_prev_mvs, prev_seg, seg)?;
        let res = fs.decode_tiles(&data[comp_end..]);
        // Put the persistent arrays back whatever happened.
        self.prev_seg = std::mem::take(&mut fs.prev_seg);
        res?;

        if h.lf_level != 0 {
            let lvl = lf::level_lookup(&h, &self.p);
            lf::loop_filter(&mut fs.cur, &fs.mi, &h, &lvl);
        }

        // refresh_probs
        if !h.error_res && !h.parallel {
            let saved = &self.p.saved[h.ctx_idx];
            fs.probs.load(saved);
            let factor = if h.frame_is_intra {
                112
            } else if self.p.last_frame_type == hdr::KEY_FRAME {
                128
            } else {
                112
            };
            fs.probs.adapt_coef(&fs.counts, factor);
            if !h.frame_is_intra {
                fs.probs.load2(saved);
                fs.probs.adapt_noncoef(
                    &fs.counts,
                    h.interp_filter == hdr::SWITCHABLE,
                    h.tx_mode == hdr::TX_MODE_SELECT,
                    h.allow_hp,
                );
            }
        }
        if h.refresh_ctx {
            self.p.saved[h.ctx_idx] = fs.probs.clone();
        }

        // 8.1 step 3
        if h.seg_enabled && h.seg_update_map {
            for (dst, m) in self.prev_seg.iter_mut().zip(&fs.mi) {
                *dst = m.seg_id;
            }
        }
        let pic = h.show_frame.then(|| fs.cur.to_picture());
        // 8.10 reference update
        let mi = std::mem::take(&mut fs.mi);
        let frame = Arc::new(fs.cur);
        for i in 0..8 {
            if (h.refresh_flags >> i) & 1 == 1 {
                self.p.refs[i] = Some(frame.clone());
            }
        }
        self.prev_mi = mi;
        Ok(pic)
    }
}
