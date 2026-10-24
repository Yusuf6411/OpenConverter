//! Probability tables (a "frame context"), the counters that adapt them, and the adaptation
//! itself (spec 8.3, 8.4).
use crate::tables::*;

pub type Coef = [[[[[[u8; 3]; 6]; 6]; 2]; 2]; 4];

#[derive(Clone)]
pub struct Probs {
    /// `[max tx size][context][node]`; index 0 is unused.
    pub tx: [[[u8; 3]; 2]; 4],
    pub skip: [u8; 3],
    pub coef: Box<Coef>,
    pub inter_mode: [[u8; 3]; 7],
    pub interp_filter: [[u8; 2]; 4],
    pub is_inter: [u8; 4],
    pub comp_mode: [u8; 5],
    pub single_ref: [[u8; 2]; 5],
    pub comp_ref: [u8; 5],
    pub y_mode: [[u8; 9]; 4],
    pub uv_mode: [[u8; 9]; 10],
    pub partition: [[u8; 3]; 16],
    pub mv_joint: [u8; 3],
    pub mv_sign: [u8; 2],
    pub mv_class: [[u8; 10]; 2],
    pub mv_class0_bit: [u8; 2],
    pub mv_bits: [[u8; 10]; 2],
    pub mv_class0_fr: [[[u8; 3]; 2]; 2],
    pub mv_fr: [[u8; 3]; 2],
    pub mv_class0_hp: [u8; 2],
    pub mv_hp: [u8; 2],
}

impl Default for Probs {
    fn default() -> Self {
        Self {
            tx: DEFAULT_TX_PROBS,
            skip: DEFAULT_SKIP_PROB,
            coef: Box::new(DEFAULT_COEF_PROBS),
            inter_mode: DEFAULT_INTER_MODE_PROBS,
            interp_filter: DEFAULT_INTERP_FILTER_PROBS,
            is_inter: DEFAULT_IS_INTER_PROB,
            comp_mode: DEFAULT_COMP_MODE_PROB,
            single_ref: DEFAULT_SINGLE_REF_PROB,
            comp_ref: DEFAULT_COMP_REF_PROB,
            y_mode: DEFAULT_Y_MODE_PROBS,
            uv_mode: DEFAULT_UV_MODE_PROBS,
            partition: DEFAULT_PARTITION_PROBS,
            mv_joint: DEFAULT_MV_JOINT_PROBS,
            mv_sign: DEFAULT_MV_SIGN_PROB,
            mv_class: DEFAULT_MV_CLASS_PROBS,
            mv_class0_bit: DEFAULT_MV_CLASS0_BIT_PROB,
            mv_bits: DEFAULT_MV_BITS_PROB,
            mv_class0_fr: DEFAULT_MV_CLASS0_FR_PROBS,
            mv_fr: DEFAULT_MV_FR_PROBS,
            mv_class0_hp: DEFAULT_MV_CLASS0_HP_PROB,
            mv_hp: DEFAULT_MV_HP_PROB,
        }
    }
}

impl Probs {
    /// `load_probs`: everything except the transform-size and skip tables.
    pub fn load(&mut self, saved: &Probs) {
        let (tx, skip) = (self.tx, self.skip);
        *self = saved.clone();
        self.tx = tx;
        self.skip = skip;
    }

    /// `load_probs2`: the transform-size and skip tables.
    pub fn load2(&mut self, saved: &Probs) {
        self.tx = saved.tx;
        self.skip = saved.skip;
    }
}

#[derive(Clone)]
pub struct Counts {
    pub intra_mode: [[u32; 10]; 4],
    pub uv_mode: [[u32; 10]; 10],
    pub partition: [[u32; 4]; 16],
    pub interp_filter: [[u32; 3]; 4],
    pub inter_mode: [[u32; 4]; 7],
    pub tx_size: [[[u32; 4]; 2]; 4],
    pub is_inter: [[u32; 2]; 4],
    pub comp_mode: [[u32; 2]; 5],
    pub single_ref: [[[u32; 2]; 2]; 5],
    pub comp_ref: [[u32; 2]; 5],
    pub skip: [[u32; 2]; 3],
    pub mv_joint: [u32; 4],
    pub mv_sign: [[u32; 2]; 2],
    pub mv_class: [[u32; 11]; 2],
    pub mv_class0_bit: [[u32; 2]; 2],
    pub mv_class0_fr: [[[u32; 4]; 2]; 2],
    pub mv_class0_hp: [[u32; 2]; 2],
    pub mv_bits: [[[u32; 2]; 10]; 2],
    pub mv_fr: [[u32; 4]; 2],
    pub mv_hp: [[u32; 2]; 2],
    pub token: Box<[[[[[[u32; 3]; 6]; 6]; 2]; 2]; 4]>,
    pub more_coefs: Box<[[[[[[u32; 2]; 6]; 6]; 2]; 2]; 4]>,
}

impl Default for Counts {
    fn default() -> Self {
        Self {
            intra_mode: [[0; 10]; 4],
            uv_mode: [[0; 10]; 10],
            partition: [[0; 4]; 16],
            interp_filter: [[0; 3]; 4],
            inter_mode: [[0; 4]; 7],
            tx_size: [[[0; 4]; 2]; 4],
            is_inter: [[0; 2]; 4],
            comp_mode: [[0; 2]; 5],
            single_ref: [[[0; 2]; 2]; 5],
            comp_ref: [[0; 2]; 5],
            skip: [[0; 2]; 3],
            mv_joint: [0; 4],
            mv_sign: [[0; 2]; 2],
            mv_class: [[0; 11]; 2],
            mv_class0_bit: [[0; 2]; 2],
            mv_class0_fr: [[[0; 4]; 2]; 2],
            mv_class0_hp: [[0; 2]; 2],
            mv_bits: [[[0; 2]; 10]; 2],
            mv_fr: [[0; 4]; 2],
            mv_hp: [[0; 2]; 2],
            token: Box::new([[[[[[0; 3]; 6]; 6]; 2]; 2]; 4]),
            more_coefs: Box::new([[[[[[0; 2]; 6]; 6]; 2]; 2]; 4]),
        }
    }
}

// Decode trees (spec 9.3.1), with leaves stored negated.
pub const PARTITION_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub const INTRA_MODE_TREE: [i8; 18] = [
    0, 2, -9, 4, -1, 6, 8, 12, -2, 10, -4, -5, -3, 14, -8, 16, -6, -7,
];
pub const SEGMENT_TREE: [i8; 14] = [2, 4, 6, 8, 10, 12, 0, -1, -2, -3, -4, -5, -6, -7];
pub const TX_SIZE_32_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub const TX_SIZE_16_TREE: [i8; 4] = [0, 2, -1, -2];
pub const TX_SIZE_8_TREE: [i8; 2] = [0, -1];
/// Leaves are `inter_mode` values: ZEROMV-NEARESTMV = 2, NEAREST = 0, NEAR = 1, NEW = 3.
pub const INTER_MODE_TREE: [i8; 6] = [-2, 2, 0, 4, -1, -3];
pub const INTERP_FILTER_TREE: [i8; 4] = [0, 2, -1, -2];
pub const MV_JOINT_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub const MV_CLASS_TREE: [i8; 20] = [
    0, 2, -1, 4, 6, 8, -2, -3, 10, 12, -4, -5, -6, 14, 16, 18, -7, -8, -9, -10,
];
pub const MV_FR_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub const TOKEN_TREE: [i8; 20] = [
    0, 2, -1, 4, 6, 10, -2, 8, -3, -4, 12, 14, -5, -6, 16, 18, -7, -8, -9, -10,
];

fn merge_prob(pre: u8, ct0: u32, ct1: u32, count_sat: u32, max_update: u32) -> u8 {
    let den = ct0 + ct1;
    let prob = if den == 0 {
        128
    } else {
        ((u64::from(ct0) * 256 + u64::from(den >> 1)) / u64::from(den)).clamp(1, 255) as u32
    };
    let count = den.min(count_sat);
    let factor = max_update * count / count_sat;
    ((u32::from(pre) * (256 - factor) + prob * factor + 128) >> 8) as u8
}

fn merge_probs(tree: &[i8], i: usize, probs: &mut [u8], counts: &[u32], sat: u32, max: u32) -> u32 {
    let s = tree[i];
    let left = if s <= 0 {
        counts[(-s) as usize]
    } else {
        merge_probs(tree, s as usize, probs, counts, sat, max)
    };
    let r = tree[i + 1];
    let right = if r <= 0 {
        counts[(-r) as usize]
    } else {
        merge_probs(tree, r as usize, probs, counts, sat, max)
    };
    probs[i >> 1] = merge_prob(probs[i >> 1], left, right, sat, max);
    left + right
}

const COUNT_SAT: u32 = 20;
const MAX_UPDATE: u32 = 128;

fn adapt_probs(tree: &[i8], probs: &mut [u8], counts: &[u32]) {
    merge_probs(tree, 0, probs, counts, COUNT_SAT, MAX_UPDATE);
}

fn adapt_prob(prob: u8, counts: &[u32; 2]) -> u8 {
    merge_prob(prob, counts[0], counts[1], COUNT_SAT, MAX_UPDATE)
}

impl Probs {
    /// 8.4.3
    pub fn adapt_coef(&mut self, c: &Counts, update_factor: u32) {
        const SMALL_TOKEN_TREE: [i8; 6] = [0, 0, 0, 4, -1, -2];
        const BINARY_TREE: [i8; 2] = [0, -1];
        for t in 0..4 {
            for i in 0..2 {
                for j in 0..2 {
                    for k in 0..6 {
                        let max_l = if k == 0 { 3 } else { 6 };
                        for l in 0..max_l {
                            let p = &mut self.coef[t][i][j][k][l];
                            merge_probs(
                                &SMALL_TOKEN_TREE,
                                2,
                                p,
                                &c.token[t][i][j][k][l],
                                24,
                                update_factor,
                            );
                            merge_probs(
                                &BINARY_TREE,
                                0,
                                p,
                                &c.more_coefs[t][i][j][k][l],
                                24,
                                update_factor,
                            );
                        }
                    }
                }
            }
        }
    }

    /// 8.4.4
    pub fn adapt_noncoef(
        &mut self,
        c: &Counts,
        switchable_filter: bool,
        tx_select: bool,
        allow_hp: bool,
    ) {
        for i in 0..4 {
            self.is_inter[i] = adapt_prob(self.is_inter[i], &c.is_inter[i]);
        }
        for i in 0..5 {
            self.comp_mode[i] = adapt_prob(self.comp_mode[i], &c.comp_mode[i]);
            self.comp_ref[i] = adapt_prob(self.comp_ref[i], &c.comp_ref[i]);
            for j in 0..2 {
                self.single_ref[i][j] = adapt_prob(self.single_ref[i][j], &c.single_ref[i][j]);
            }
        }
        for i in 0..7 {
            adapt_probs(&INTER_MODE_TREE, &mut self.inter_mode[i], &c.inter_mode[i]);
        }
        for i in 0..4 {
            adapt_probs(&INTRA_MODE_TREE, &mut self.y_mode[i], &c.intra_mode[i]);
        }
        for i in 0..10 {
            adapt_probs(&INTRA_MODE_TREE, &mut self.uv_mode[i], &c.uv_mode[i]);
        }
        for i in 0..16 {
            adapt_probs(&PARTITION_TREE, &mut self.partition[i], &c.partition[i]);
        }
        for i in 0..3 {
            self.skip[i] = adapt_prob(self.skip[i], &c.skip[i]);
        }
        if switchable_filter {
            for i in 0..4 {
                adapt_probs(
                    &INTERP_FILTER_TREE,
                    &mut self.interp_filter[i],
                    &c.interp_filter[i],
                );
            }
        }
        if tx_select {
            for i in 0..2 {
                adapt_probs(&TX_SIZE_8_TREE, &mut self.tx[1][i], &c.tx_size[1][i]);
                adapt_probs(&TX_SIZE_16_TREE, &mut self.tx[2][i], &c.tx_size[2][i]);
                adapt_probs(&TX_SIZE_32_TREE, &mut self.tx[3][i], &c.tx_size[3][i]);
            }
        }
        adapt_probs(&MV_JOINT_TREE, &mut self.mv_joint, &c.mv_joint);
        for i in 0..2 {
            self.mv_sign[i] = adapt_prob(self.mv_sign[i], &c.mv_sign[i]);
            adapt_probs(&MV_CLASS_TREE, &mut self.mv_class[i], &c.mv_class[i]);
            self.mv_class0_bit[i] = adapt_prob(self.mv_class0_bit[i], &c.mv_class0_bit[i]);
            for j in 0..10 {
                self.mv_bits[i][j] = adapt_prob(self.mv_bits[i][j], &c.mv_bits[i][j]);
            }
            for j in 0..2 {
                adapt_probs(
                    &MV_FR_TREE,
                    &mut self.mv_class0_fr[i][j],
                    &c.mv_class0_fr[i][j],
                );
            }
            adapt_probs(&MV_FR_TREE, &mut self.mv_fr[i], &c.mv_fr[i]);
            if allow_hp {
                self.mv_class0_hp[i] = adapt_prob(self.mv_class0_hp[i], &c.mv_class0_hp[i]);
                self.mv_hp[i] = adapt_prob(self.mv_hp[i], &c.mv_hp[i]);
            }
        }
    }
}
