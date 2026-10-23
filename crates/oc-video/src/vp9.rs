//! VP9 video: decoded to 8-bit 4:2:0 pictures (high bit depths are rounded down, other chroma
//! layouts are averaged down to 4:2:0).
use crate::pic::Picture;
use crate::Res;
use oc_core::Limits;

pub struct Vp9Decoder {
    inner: oc_vp9::Decoder,
}

impl Vp9Decoder {
    pub fn new(limits: &Limits) -> Self {
        Self {
            inner: oc_vp9::Decoder::new(limits),
        }
    }

    /// One packet in, at most one picture out (a hidden frame gives none; if a superframe shows
    /// several frames the last one wins).
    pub fn decode(&mut self, data: &[u8]) -> Res<Option<Picture>> {
        let pics = self.inner.decode(data)?;
        Ok(pics.last().map(to_picture))
    }
}

fn to_picture(f: &oc_vp9::Picture) -> Picture {
    let mut p = Picture::black(f.width, f.height);
    let down = u32::from(f.bit_depth) - 8;
    let to8 = |v: u16| -> u8 {
        if down == 0 {
            v as u8
        } else {
            ((u32::from(v) + (1 << (down - 1))) >> down).min(255) as u8
        }
    };
    for (d, &s) in p.y.iter_mut().zip(&f.planes[0]) {
        *d = to8(s);
    }
    let (cw, ch) = (p.cw(), p.ch());
    let (src_w, src_h) = f.chroma_size();
    let (xs, ys) = (
        if f.ss_x == 0 { 2 } else { 1 },
        if f.ss_y == 0 { 2 } else { 1 },
    );
    for (plane, out) in [(&f.planes[1], &mut p.u), (&f.planes[2], &mut p.v)] {
        for cy in 0..ch {
            for cx in 0..cw {
                let (mut sum, mut n) = (0u32, 0u32);
                for dy in 0..ys {
                    for dx in 0..xs {
                        let (x, y) = (cx * xs + dx, cy * ys + dy);
                        if x < src_w && y < src_h {
                            sum += u32::from(to8(plane[y * src_w + x]));
                            n += 1;
                        }
                    }
                }
                out[cy * cw + cx] = (sum + n / 2).checked_div(n).map_or(128, |v| v as u8);
            }
        }
    }
    p
}
