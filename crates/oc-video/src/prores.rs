//! Apple ProRes video: every packet is one intra-coded frame, decoded to an 8-bit 4:2:0 picture
//! (alpha is dropped).
use crate::pic::Picture;
use crate::{corrupt, Res};
use oc_core::Limits;

/// The QuickTime sample-entry codes ProRes uses: 422 Proxy, LT, standard, HQ, 4444, 4444 XQ.
pub const FOURCCS: [&[u8; 4]; 6] = [b"apco", b"apcs", b"apcn", b"apch", b"ap4h", b"ap4x"];

pub struct ProResDecoder {
    limits: Limits,
}

impl ProResDecoder {
    pub fn new(limits: &Limits) -> Self {
        Self {
            limits: limits.clone(),
        }
    }

    pub fn decode(&mut self, data: &[u8]) -> Res<Option<Picture>> {
        let f = oc_prores::decode(data, &self.limits)?;
        if f.width == 0 || f.height == 0 {
            return Err(corrupt("ProRes picture is empty"));
        }
        Ok(Some(to_picture(&f)))
    }
}

fn to8(v: u16) -> u8 {
    ((u32::from(v) + 2) >> 2).min(255) as u8
}

fn to_picture(f: &oc_prores::Frame) -> Picture {
    let mut p = Picture::black(f.width, f.height);
    for (d, &s) in p.y.iter_mut().zip(&f.y) {
        *d = to8(s);
    }
    let (cw, ch) = (p.cw(), p.ch());
    let sx = if f.chroma_format == 3 { 2 } else { 1 };
    let avg = |src: &[u16], out: &mut [u8]| {
        for cy in 0..ch {
            for cx in 0..cw {
                let (mut sum, mut n) = (0u32, 0u32);
                for dy in 0..2 {
                    for dx in 0..sx {
                        let (x, y) = (cx * sx + dx, cy * 2 + dy);
                        if x < f.chroma_width && y < f.height {
                            sum += u32::from(src[y * f.chroma_width + x]);
                            n += 1;
                        }
                    }
                }
                out[cy * cw + cx] = to8(((sum + n / 2).checked_div(n)).map_or(512, |v| v as u16));
            }
        }
    };
    avg(&f.cb, &mut p.u);
    avg(&f.cr, &mut p.v);
    p
}
