//! AV1 video: decoded to 8-bit 4:2:0 pictures (high bit depths are rounded down, other chroma
//! layouts are averaged down to 4:2:0, monochrome gets neutral chroma).
use crate::pic::Picture;
use crate::Res;

pub struct Av1Decoder {
    inner: oc_av1::Decoder,
}

impl Av1Decoder {
    /// `private` is the `av1C` box payload (or the Matroska CodecPrivate), whose tail carries the
    /// sequence header OBU for streams that do not repeat it in every key frame.
    pub fn new(private: &[u8]) -> Self {
        let mut inner = oc_av1::Decoder::new();
        if private.len() > 4 && private[0] == 0x81 {
            let _ = inner.decode(&private[4..]);
        }
        Self { inner }
    }

    /// One temporal unit in, at most one picture out (the last shown frame wins).
    pub fn decode(&mut self, data: &[u8]) -> Res<Option<Picture>> {
        let pics = self.inner.decode(data)?;
        Ok(pics.last().map(to_picture))
    }
}

fn to_picture(f: &oc_av1::Picture) -> Picture {
    let mut p = Picture::black(f.width, f.height);
    let down = f.bit_depth - 8;
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
    if f.mono || f.planes.len() < 3 {
        return p;
    }
    let (cw, ch) = (p.cw(), p.ch());
    let (src_w, src_h) = (f.plane_widths[1], f.plane_heights[1]);
    let (xs, ys) = (
        1 + usize::from(f.subsampling_x == 0),
        1 + usize::from(f.subsampling_y == 0),
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
