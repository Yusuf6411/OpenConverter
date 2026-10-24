//! Frame buffers shared by the decoder and the encoder, so both predict from exactly the
//! same samples.
//!
//! Each plane carries a one-sample border above and to the left, filled with 127 above the
//! frame and 129 to its left, which is what intra prediction reads at the frame edges.
#![allow(clippy::indexing_slicing, clippy::needless_range_loop)]

use crate::transform::idct_add;

pub struct Planes {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub ys: usize,
    pub uvs: usize,
    pub mb_w: usize,
    pub mb_h: usize,
}

impl Planes {
    pub fn new(mb_w: usize, mb_h: usize) -> Self {
        let ys = mb_w * 16 + 1;
        let uvs = mb_w * 8 + 1;
        let mut p = Self {
            y: vec![0; ys * (mb_h * 16 + 1)],
            u: vec![0; uvs * (mb_h * 8 + 1)],
            v: vec![0; uvs * (mb_h * 8 + 1)],
            ys,
            uvs,
            mb_w,
            mb_h,
        };
        for (buf, stride) in [(&mut p.y, ys), (&mut p.u, uvs), (&mut p.v, uvs)] {
            buf[..stride].fill(127);
            for row in buf.chunks_exact_mut(stride).skip(1) {
                row[0] = 129;
            }
        }
        p
    }

    pub fn y_origin(&self, mb_x: usize, mb_y: usize) -> usize {
        (mb_y * 16 + 1) * self.ys + mb_x * 16 + 1
    }

    pub fn uv_origin(&self, mb_x: usize, mb_y: usize) -> usize {
        (mb_y * 8 + 1) * self.uvs + mb_x * 8 + 1
    }

    /// Samples above, to the left and above-left of a 16x16 luma block.
    pub fn luma_edges(&self, mb_x: usize, mb_y: usize) -> ([u8; 16], [u8; 16], u8) {
        let o = self.y_origin(mb_x, mb_y);
        let s = self.ys;
        let mut top = [0u8; 16];
        top.copy_from_slice(&self.y[o - s..o - s + 16]);
        let mut left = [0u8; 16];
        for (r, l) in left.iter_mut().enumerate() {
            *l = self.y[o + r * s - 1];
        }
        (top, left, self.y[o - s - 1])
    }

    pub fn chroma_edges(&self, ch: usize, mb_x: usize, mb_y: usize) -> ([u8; 8], [u8; 8], u8) {
        let plane = if ch == 0 { &self.u } else { &self.v };
        let o = self.uv_origin(mb_x, mb_y);
        let s = self.uvs;
        let mut top = [0u8; 8];
        top.copy_from_slice(&plane[o - s..o - s + 8]);
        let mut left = [0u8; 8];
        for (r, l) in left.iter_mut().enumerate() {
            *l = plane[o + r * s - 1];
        }
        (top, left, plane[o - s - 1])
    }

    /// The four samples above and to the right of a macroblock, used by the right-hand
    /// column of 4x4 blocks.
    pub fn top_right(&self, mb_x: usize, mb_y: usize) -> [u8; 4] {
        let o = self.y_origin(mb_x, mb_y);
        let s = self.ys;
        if mb_y == 0 {
            [127; 4]
        } else if mb_x == self.mb_w - 1 {
            [self.y[o - s + 15]; 4]
        } else {
            let t = o - s + 16;
            [self.y[t], self.y[t + 1], self.y[t + 2], self.y[t + 3]]
        }
    }

    fn block_pos(&self, mb_x: usize, mb_y: usize, b: usize) -> usize {
        self.y_origin(mb_x, mb_y) + (b / 4) * 4 * self.ys + (b % 4) * 4
    }

    /// Edge samples of luma sub-block `b`: eight above, four to the left, and the corner.
    pub fn i4_edges(
        &self,
        mb_x: usize,
        mb_y: usize,
        b: usize,
        tr: &[u8; 4],
    ) -> ([u8; 8], [u8; 4], u8) {
        let s = self.ys;
        let pos = self.block_pos(mb_x, mb_y, b);
        let mut top = [0u8; 8];
        top[..4].copy_from_slice(&self.y[pos - s..pos - s + 4]);
        if b % 4 < 3 {
            top[4..].copy_from_slice(&self.y[pos - s + 4..pos - s + 8]);
        } else {
            top[4..].copy_from_slice(tr);
        }
        let left = [
            self.y[pos - 1],
            self.y[pos + s - 1],
            self.y[pos + 2 * s - 1],
            self.y[pos + 3 * s - 1],
        ];
        (top, left, self.y[pos - s - 1])
    }

    pub fn put_luma(&mut self, mb_x: usize, mb_y: usize, pred: &[u8; 256]) {
        let o = self.y_origin(mb_x, mb_y);
        for r in 0..16 {
            self.y[o + r * self.ys..o + r * self.ys + 16]
                .copy_from_slice(&pred[r * 16..r * 16 + 16]);
        }
    }

    pub fn put_chroma(&mut self, ch: usize, mb_x: usize, mb_y: usize, pred: &[u8; 64]) {
        let o = self.uv_origin(mb_x, mb_y);
        let s = self.uvs;
        let plane = if ch == 0 { &mut self.u } else { &mut self.v };
        for r in 0..8 {
            plane[o + r * s..o + r * s + 8].copy_from_slice(&pred[r * 8..r * 8 + 8]);
        }
    }

    pub fn put_4x4(&mut self, mb_x: usize, mb_y: usize, b: usize, pred: &[u8; 16]) {
        let pos = self.block_pos(mb_x, mb_y, b);
        let s = self.ys;
        for r in 0..4 {
            self.y[pos + r * s..pos + r * s + 4].copy_from_slice(&pred[r * 4..r * 4 + 4]);
        }
    }

    /// Adds the inverse DCT of `coeffs` to luma block `b` of the macroblock.
    pub fn add_luma_block(&mut self, mb_x: usize, mb_y: usize, b: usize, coeffs: &[i16]) {
        let pos = self.block_pos(mb_x, mb_y, b);
        idct_add(coeffs, &mut self.y, pos, self.ys);
    }

    /// Adds the inverse DCT of `coeffs` to chroma block `b` (0..4) of plane `ch`.
    pub fn add_chroma_block(
        &mut self,
        ch: usize,
        mb_x: usize,
        mb_y: usize,
        b: usize,
        coeffs: &[i16],
    ) {
        let s = self.uvs;
        let pos = self.uv_origin(mb_x, mb_y) + (b / 2) * 4 * s + (b % 2) * 4;
        let plane = if ch == 0 { &mut self.u } else { &mut self.v };
        idct_add(coeffs, plane, pos, s);
    }
}
