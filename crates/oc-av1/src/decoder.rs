//! OBU parsing and the public decoder interface (sections 5.3 - 5.12).
use crate::bits::{leb128_bytes, Bits};
use crate::dec::*;
use crate::hdr::*;
use oc_core::{Error, Result};
use std::rc::Rc;

/// One decoded picture, cropped to the frame size, with tightly packed planes.
#[derive(Clone, Debug, Default)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub bit_depth: u32,
    pub subsampling_x: usize,
    pub subsampling_y: usize,
    pub mono: bool,
    pub planes: Vec<Vec<u16>>,
    pub plane_widths: Vec<usize>,
    pub plane_heights: Vec<usize>,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub full_range: bool,
}

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_TILE_GROUP: u8 = 4;
const OBU_FRAME: u8 = 6;
const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;

pub struct Decoder {
    seq: Option<SeqHdr>,
    refh: [RefHdr; 8],
    refd: [Option<RefData>; 8],
    cur: Option<Box<Dec>>,
    seen_frame_header: bool,
    out: Vec<Picture>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            seq: None,
            refh: Default::default(),
            refd: Default::default(),
            cur: None,
            seen_frame_header: false,
            out: Vec::new(),
        }
    }

    /// The sequence header seen so far, if any.
    pub fn sequence(&self) -> Option<&SeqHdr> {
        self.seq.as_ref()
    }

    /// Decodes a chunk of OBUs in the low-overhead format (each OBU carries its size) and returns
    /// every picture that became ready for output.
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<Picture>> {
        let mut pos = 0usize;
        while pos < data.len() {
            let b0 = data[pos];
            pos += 1;
            if b0 & 0x80 != 0 {
                return Err(Error::corrupt("AV1: OBU forbidden bit set"));
            }
            let obu_type = (b0 >> 3) & 15;
            let has_ext = b0 & 4 != 0;
            let has_size = b0 & 2 != 0;
            let (mut tid, mut sid) = (0u32, 0u32);
            if has_ext {
                let e = *data
                    .get(pos)
                    .ok_or_else(|| Error::truncated("AV1: OBU extension"))?;
                pos += 1;
                tid = u32::from(e >> 5);
                sid = u32::from((e >> 3) & 3);
            }
            let size = if has_size {
                let (v, n) =
                    leb128_bytes(&data[pos..]).ok_or_else(|| Error::truncated("AV1: OBU size"))?;
                pos += n;
                v as usize
            } else {
                data.len() - pos
            };
            if pos + size > data.len() {
                return Err(Error::truncated("AV1: OBU payload"));
            }
            let payload = &data[pos..pos + size];
            pos += size;
            if obu_type != OBU_SEQUENCE_HEADER && obu_type != OBU_TEMPORAL_DELIMITER && has_ext {
                if let Some(seq) = &self.seq {
                    let idc = seq.operating_point_idc[seq.operating_point];
                    if idc != 0 {
                        let in_t = (idc >> tid) & 1;
                        let in_s = (idc >> (sid + 8)) & 1;
                        if in_t == 0 || in_s == 0 {
                            continue;
                        }
                    }
                }
            }
            match obu_type {
                OBU_SEQUENCE_HEADER => {
                    let mut b = Bits::new(payload);
                    self.seq = Some(SeqHdr::parse(&mut b)?);
                }
                OBU_TEMPORAL_DELIMITER => {
                    self.seen_frame_header = false;
                }
                OBU_FRAME_HEADER | OBU_REDUNDANT_FRAME_HEADER | OBU_FRAME => {
                    if self.seen_frame_header && obu_type != OBU_FRAME {
                        continue;
                    }
                    let seq = self
                        .seq
                        .clone()
                        .ok_or_else(|| Error::corrupt("AV1: frame before sequence header"))?;
                    let mut b = Bits::new(payload);
                    let hdr = FrameHdr::parse(&mut b, &seq, &mut self.refh, tid, sid)?;
                    b.byte_align();
                    let hdr_bytes = b.position() / 8;
                    if hdr.show_existing_frame {
                        self.show_existing(&seq, &hdr)?;
                        continue;
                    }
                    self.seen_frame_header = true;
                    let dec = Dec::new_frame(&seq, hdr, &self.refh, &self.refd)?;
                    self.cur = Some(Box::new(dec));
                    if obu_type == OBU_FRAME {
                        self.tile_group(&payload[hdr_bytes.min(payload.len())..])?;
                    }
                }
                OBU_TILE_GROUP => {
                    self.tile_group(payload)?;
                }
                _ => {}
            }
        }
        Ok(std::mem::take(&mut self.out))
    }

    fn tile_group(&mut self, data: &[u8]) -> Result<()> {
        let dec = self
            .cur
            .as_mut()
            .ok_or_else(|| Error::corrupt("AV1: tile group without frame header"))?;
        if dec.tile_group(data)? {
            let mut dec = self.cur.take().unwrap();
            self.seen_frame_header = false;
            dec.wrapup();
            let frame = Rc::new(std::mem::take(&mut dec.cur));
            let rh = dec.ref_hdr();
            let rd = dec.ref_data(frame.clone());
            let refresh = dec.hdr.refresh_frame_flags;
            for i in 0..8 {
                if (refresh >> i) & 1 == 1 {
                    self.refh[i] = rh.clone();
                    self.refd[i] = Some(rd.clone());
                }
            }
            if dec.hdr.show_frame {
                let pic = make_picture(&dec.seq, &frame, &rh, &dec.hdr.film_grain);
                self.out.push(pic);
            }
        }
        Ok(())
    }

    fn show_existing(&mut self, seq: &SeqHdr, hdr: &FrameHdr) -> Result<()> {
        let slot = hdr.frame_to_show_map_idx;
        let rd = self.refd[slot]
            .clone()
            .ok_or_else(|| Error::corrupt("AV1: show_existing_frame of an empty slot"))?;
        let rh = self.refh[slot].clone();
        let pic = make_picture(seq, &rd.frame, &rh, &hdr.film_grain);
        self.out.push(pic);
        if hdr.frame_type == KEY_FRAME {
            for i in 0..8 {
                self.refh[i] = rh.clone();
                self.refd[i] = Some(rd.clone());
            }
        }
        Ok(())
    }
}

fn make_picture(seq: &SeqHdr, frame: &Frame, rh: &RefHdr, fg: &FilmGrain) -> Picture {
    let w = rh.upscaled_width as usize;
    let h = rh.frame_height as usize;
    let sx = seq.subsampling_x as usize;
    let sy = seq.subsampling_y as usize;
    let mut pic = Picture {
        width: w,
        height: h,
        bit_depth: seq.bit_depth,
        subsampling_x: sx,
        subsampling_y: sy,
        mono: seq.mono_chrome,
        color_primaries: seq.color_primaries,
        transfer_characteristics: seq.transfer_characteristics,
        matrix_coefficients: seq.matrix_coefficients,
        full_range: seq.color_range,
        ..Default::default()
    };
    for (p, pl) in frame.planes.iter().enumerate() {
        let (pw, ph) = if p == 0 {
            (w, h)
        } else {
            ((w + sx) >> sx, (h + sy) >> sy)
        };
        let mut d = Vec::with_capacity(pw * ph);
        for y in 0..ph {
            d.extend_from_slice(&pl.d[y * pl.stride..y * pl.stride + pw]);
        }
        pic.planes.push(d);
        pic.plane_widths.push(pw);
        pic.plane_heights.push(ph);
    }
    if seq.film_grain_params_present && fg.apply_grain {
        crate::grain::apply_film_grain(&mut pic, fg);
    }
    pic
}
