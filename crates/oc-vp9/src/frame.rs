//! Decoded picture storage.

/// A decoded frame. Planes are allocated up to a multiple of 64 luma samples so that blocks
/// hanging over the edge can be predicted without special cases.
pub struct FrameBuf {
    /// Visible size of the luma plane.
    pub w: usize,
    pub h: usize,
    pub ss_x: u8,
    pub ss_y: u8,
    pub bit_depth: u8,
    pub planes: [Vec<u16>; 3],
    pub stride: [usize; 3],
}

impl FrameBuf {
    pub fn new(w: usize, h: usize, ss_x: u8, ss_y: u8, bit_depth: u8) -> Self {
        let aw = (w + 63) & !63;
        let ah = (h + 63) & !63;
        let mid = 1u16 << (bit_depth - 1);
        let cw = aw >> ss_x;
        let ch = ah >> ss_y;
        Self {
            w,
            h,
            ss_x,
            ss_y,
            bit_depth,
            planes: [vec![mid; aw * ah], vec![mid; cw * ch], vec![mid; cw * ch]],
            stride: [aw, cw, cw],
        }
    }

    /// Visible size of plane `p`.
    pub fn plane_size(&self, p: usize) -> (usize, usize) {
        if p == 0 {
            (self.w, self.h)
        } else {
            (
                (self.w + usize::from(self.ss_x)) >> self.ss_x,
                (self.h + usize::from(self.ss_y)) >> self.ss_y,
            )
        }
    }

    /// Copy the visible part out as three tightly packed planes.
    pub fn to_picture(&self) -> Picture {
        let mut planes: [Vec<u16>; 3] = Default::default();
        for (p, out) in planes.iter_mut().enumerate() {
            let (w, h) = self.plane_size(p);
            out.reserve(w * h);
            for y in 0..h {
                let row = y * self.stride[p];
                out.extend_from_slice(&self.planes[p][row..row + w]);
            }
        }
        Picture {
            width: self.w,
            height: self.h,
            ss_x: self.ss_x,
            ss_y: self.ss_y,
            bit_depth: self.bit_depth,
            planes,
        }
    }
}

/// A shown frame: tightly packed planes of 16-bit samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub ss_x: u8,
    pub ss_y: u8,
    pub bit_depth: u8,
    pub planes: [Vec<u16>; 3],
}

impl Picture {
    /// Width and height of chroma planes.
    pub fn chroma_size(&self) -> (usize, usize) {
        (
            (self.width + usize::from(self.ss_x)) >> self.ss_x,
            (self.height + usize::from(self.ss_y)) >> self.ss_y,
        )
    }
}
