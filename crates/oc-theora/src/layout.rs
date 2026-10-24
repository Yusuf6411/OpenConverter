//! Block, super block and macro block numbering in coded order (spec 2.3, 2.4).

/// Position of each block of a super block along the Hilbert curve of figure 2.4, as (x, y)
/// with y counted up from the bottom.
const HILBERT: [(usize, usize); 16] = [
    (0, 0),
    (1, 0),
    (1, 1),
    (0, 1),
    (0, 2),
    (0, 3),
    (1, 3),
    (1, 2),
    (2, 2),
    (2, 3),
    (3, 3),
    (3, 2),
    (3, 1),
    (2, 1),
    (2, 0),
    (3, 0),
];

/// The same for the four macro blocks of a super block (figure 2.6).
const MB_HILBERT: [(usize, usize); 4] = [(0, 0), (0, 1), (1, 1), (1, 0)];

pub(crate) struct Layout {
    pub pf: u8,
    pub nbs: usize,
    pub nmbs: usize,
    /// Blocks per row and per column of each plane.
    pub bw: [usize; 3],
    pub bh: [usize; 3],
    /// Plane sizes in pixels.
    pub pw: [usize; 3],
    pub ph: [usize; 3],
    /// Plane, column and row of each block in coded order.
    pub order: Vec<(u8, u32, u32)>,
    /// Coded-order index of the block at `[plane][row * bw + col]`.
    pub index: [Vec<u32>; 3],
    /// First coded-order block and block count of each super block.
    pub sbs: Vec<(u32, u32)>,
    /// Macro block of each block.
    pub block_mb: Vec<u32>,
    /// Lower-left, lower-right, upper-left and upper-right luma block of each macro block.
    pub mb_luma: Vec<[u32; 4]>,
    /// Chroma blocks of each macro block (2, 4 or 8 of them by pixel format).
    pub mb_chroma: Vec<[u32; 8]>,
}

impl Layout {
    pub fn new(pf: u8, fmbw: usize, fmbh: usize) -> Self {
        let (cbw, cbh) = match pf {
            0 => (fmbw, fmbh),
            2 => (fmbw, fmbh * 2),
            _ => (fmbw * 2, fmbh * 2),
        };
        let bw = [fmbw * 2, cbw, cbw];
        let bh = [fmbh * 2, cbh, cbh];
        let pw = [bw[0] * 8, bw[1] * 8, bw[2] * 8];
        let ph = [bh[0] * 8, bh[1] * 8, bh[2] * 8];
        let nbs = (0..3).map(|p| bw[p] * bh[p]).sum();
        let nmbs = fmbw * fmbh;

        let mut order = Vec::with_capacity(nbs);
        let mut index: [Vec<u32>; 3] = [
            vec![0; bw[0] * bh[0]],
            vec![0; bw[1] * bh[1]],
            vec![0; bw[2] * bh[2]],
        ];
        let mut sbs = Vec::new();
        for p in 0..3 {
            for sby in 0..bh[p].div_ceil(4) {
                for sbx in 0..bw[p].div_ceil(4) {
                    let first = order.len() as u32;
                    for (hx, hy) in HILBERT {
                        let (x, y) = (sbx * 4 + hx, sby * 4 + hy);
                        if x < bw[p] && y < bh[p] {
                            index[p][y * bw[p] + x] = order.len() as u32;
                            order.push((p as u8, x as u32, y as u32));
                        }
                    }
                    sbs.push((first, order.len() as u32 - first));
                }
            }
        }

        let mut block_mb = vec![0u32; nbs];
        let mut mb_luma = Vec::with_capacity(nmbs);
        let mut mb_chroma = Vec::with_capacity(nmbs);
        for sby in 0..(fmbh * 2).div_ceil(4) {
            for sbx in 0..(fmbw * 2).div_ceil(4) {
                for (dx, dy) in MB_HILBERT {
                    let (mx, my) = (sbx * 2 + dx, sby * 2 + dy);
                    if mx >= fmbw || my >= fmbh {
                        continue;
                    }
                    let mbi = mb_luma.len() as u32;
                    let l = |x: usize, y: usize| index[0][y * bw[0] + x];
                    let luma = [
                        l(2 * mx, 2 * my),
                        l(2 * mx + 1, 2 * my),
                        l(2 * mx, 2 * my + 1),
                        l(2 * mx + 1, 2 * my + 1),
                    ];
                    let c = |p: usize, x: usize, y: usize| index[p][y * bw[p] + x];
                    let mut chroma = [0u32; 8];
                    match pf {
                        0 => {
                            chroma[0] = c(1, mx, my);
                            chroma[1] = c(2, mx, my);
                        }
                        2 => {
                            chroma[0] = c(1, mx, 2 * my);
                            chroma[1] = c(1, mx, 2 * my + 1);
                            chroma[2] = c(2, mx, 2 * my);
                            chroma[3] = c(2, mx, 2 * my + 1);
                        }
                        _ => {
                            for (k, (x, y)) in [
                                (2 * mx, 2 * my),
                                (2 * mx + 1, 2 * my),
                                (2 * mx, 2 * my + 1),
                                (2 * mx + 1, 2 * my + 1),
                            ]
                            .into_iter()
                            .enumerate()
                            {
                                chroma[k] = c(1, x, y);
                                chroma[4 + k] = c(2, x, y);
                            }
                        }
                    }
                    let nchroma = match pf {
                        0 => 2,
                        2 => 4,
                        _ => 8,
                    };
                    for &b in luma.iter().chain(&chroma[..nchroma]) {
                        block_mb[b as usize] = mbi;
                    }
                    mb_luma.push(luma);
                    mb_chroma.push(chroma);
                }
            }
        }
        Self {
            pf,
            nbs,
            nmbs,
            bw,
            bh,
            pw,
            ph,
            order,
            index,
            sbs,
            block_mb,
            mb_luma,
            mb_chroma,
        }
    }

    pub fn chroma_per_mb(&self) -> usize {
        match self.pf {
            0 => 2,
            2 => 4,
            _ => 8,
        }
    }
}
