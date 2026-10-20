//! MPEG-1/2/2.5 audio layer III decoder (ISO/IEC 11172-3, 13818-3).
//!
//! The decoder follows the standard's own pipeline: side information, scalefactors, Huffman
//! decoding, requantisation, stereo processing, reordering, alias reduction, IMDCT with window
//! switching, frequency inversion and the polyphase synthesis filter bank.

use super::tables::*;
use std::sync::OnceLock;

// ---------------------------------------------------------------- frame header

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// 1 for MPEG-1, 2 for MPEG-2 (LSF), 3 for MPEG-2.5.
    pub version: u8,
    pub crc: bool,
    pub bitrate: u32,
    pub rate: u32,
    pub padding: bool,
    /// 0 stereo, 1 joint stereo, 2 dual channel, 3 mono.
    pub mode: u8,
    pub mode_ext: u8,
}

const BITRATES_V1: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const BITRATES_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

impl Header {
    pub fn parse(b: &[u8]) -> Option<Header> {
        let h = u32::from_be_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?]);
        if h >> 21 != 0x7FF {
            return None;
        }
        let version = match (h >> 19) & 3 {
            3 => 1,
            2 => 2,
            0 => 3,
            _ => return None,
        };
        if (h >> 17) & 3 != 1 {
            return None; // not layer III
        }
        let crc = (h >> 16) & 1 == 0;
        let br = (h >> 12) & 15;
        if br == 0 || br == 15 {
            return None; // free format and invalid are not supported
        }
        let sr = (h >> 10) & 3;
        if sr == 3 {
            return None;
        }
        let rate = match (version, sr) {
            (1, 0) => 44100,
            (1, 1) => 48000,
            (1, _) => 32000,
            (2, 0) => 22050,
            (2, 1) => 24000,
            (2, _) => 16000,
            (_, 0) => 11025,
            (_, 1) => 12000,
            _ => 8000,
        };
        let bitrate = if version == 1 {
            BITRATES_V1[br as usize]
        } else {
            BITRATES_V2[br as usize]
        };
        Some(Header {
            version,
            crc,
            bitrate,
            rate,
            padding: (h >> 9) & 1 == 1,
            mode: ((h >> 6) & 3) as u8,
            mode_ext: ((h >> 4) & 3) as u8,
        })
    }
    pub fn channels(&self) -> usize {
        if self.mode == 3 {
            1
        } else {
            2
        }
    }
    pub fn granules(&self) -> usize {
        if self.version == 1 {
            2
        } else {
            1
        }
    }
    pub fn samples_per_frame(&self) -> usize {
        576 * self.granules()
    }
    pub fn frame_len(&self) -> usize {
        let base = if self.version == 1 { 144 } else { 72 };
        (base * self.bitrate as usize * 1000) / self.rate as usize + usize::from(self.padding)
    }
    pub fn side_info_len(&self) -> usize {
        match (self.version == 1, self.channels()) {
            (true, 1) => 17,
            (true, _) => 32,
            (false, 1) => 9,
            (false, _) => 17,
        }
    }
    /// Index into the band tables (0..9: 44.1, 48, 32, 22.05, 24, 16, 11.025, 12, 8 kHz).
    fn rate_index(&self) -> usize {
        match self.rate {
            44100 => 0,
            48000 => 1,
            32000 => 2,
            22050 => 3,
            24000 => 4,
            16000 => 5,
            11025 => 6,
            12000 => 7,
            _ => 8,
        }
    }
}

// ---------------------------------------------------------------- band tables

const SFB_LONG: [[u16; 23]; 9] = [
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 52, 62, 74, 90, 110, 134, 162, 196, 238, 288, 342,
        418, 576,
    ],
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 42, 50, 60, 72, 88, 106, 128, 156, 190, 230, 276, 330,
        384, 576,
    ],
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 54, 66, 82, 102, 126, 156, 194, 240, 296, 364, 448,
        550, 576,
    ],
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464,
        522, 576,
    ],
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 114, 136, 162, 194, 232, 278, 332, 394, 464,
        540, 576,
    ],
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464,
        522, 576,
    ],
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464,
        522, 576,
    ],
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200, 238, 284, 336, 396, 464,
        522, 576,
    ],
    [
        0, 12, 24, 36, 48, 60, 72, 88, 108, 132, 160, 192, 232, 280, 336, 400, 476, 566, 568, 570,
        572, 574, 576,
    ],
];

const SFB_SHORT: [[u16; 14]; 9] = [
    [0, 4, 8, 12, 16, 22, 30, 40, 52, 66, 84, 106, 136, 192],
    [0, 4, 8, 12, 16, 22, 28, 38, 50, 64, 80, 100, 126, 192],
    [0, 4, 8, 12, 16, 22, 30, 42, 58, 78, 104, 138, 180, 192],
    [0, 4, 8, 12, 18, 24, 32, 42, 56, 74, 100, 132, 174, 192],
    [0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 136, 180, 192],
    [0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192],
    [0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192],
    [0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192],
    [0, 8, 16, 24, 36, 52, 72, 96, 124, 160, 162, 164, 166, 192],
];

pub(super) fn long_bands(rate: u32) -> &'static [u16; 23] {
    // bands of the long scalefactor partition for a sample rate
    let h = Header {
        version: 1,
        crc: false,
        bitrate: 0,
        rate,
        padding: false,
        mode: 0,
        mode_ext: 0,
    };
    &SFB_LONG[h.rate_index()]
}

/// Long scalefactor bands that make up the two lowest subbands of a mixed block.
fn mixed_long_bands(ri: usize) -> usize {
    SFB_LONG[ri].iter().position(|&b| b >= 36).unwrap_or(8)
}

const PRETAB: [u8; 22] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 3, 2, 0,
];

const SLEN: [(u8, u8); 16] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (3, 0),
    (1, 1),
    (1, 2),
    (1, 3),
    (2, 1),
    (2, 2),
    (2, 3),
    (3, 1),
    (3, 2),
    (3, 3),
    (4, 2),
    (4, 3),
];

/// Scalefactor band counts of the three partitions of MPEG-2 scalefactors:
/// `[table][long, short, mixed][partition]`.
const LSF_NSF: [[[u8; 4]; 3]; 6] = [
    [[6, 5, 5, 5], [9, 9, 9, 9], [6, 9, 9, 9]],
    [[6, 5, 7, 3], [9, 9, 12, 6], [6, 9, 12, 6]],
    [[11, 10, 0, 0], [18, 18, 0, 0], [15, 18, 0, 0]],
    [[7, 7, 7, 0], [12, 12, 12, 0], [6, 15, 12, 0]],
    [[6, 6, 6, 3], [12, 9, 9, 6], [6, 12, 9, 6]],
    [[8, 8, 5, 0], [15, 12, 9, 0], [6, 18, 9, 0]],
];

// ---------------------------------------------------------------- Huffman decoding

/// A binary decoding tree: `nodes[n][bit]` is the next node, or `-(symbol + 1)` for a leaf.
pub(super) struct Tree {
    nodes: Vec<[i32; 2]>,
}

impl Tree {
    fn build(entries: impl Iterator<Item = (u8, u32, u8)>) -> Tree {
        let mut nodes: Vec<[i32; 2]> = vec![[0, 0]];
        for (len, code, sym) in entries {
            let mut n = 0usize;
            for i in (0..len).rev() {
                let bit = ((code >> i) & 1) as usize;
                if i == 0 {
                    nodes[n][bit] = -(i32::from(sym) + 1);
                } else {
                    if nodes[n][bit] <= 0 {
                        nodes.push([0, 0]);
                        nodes[n][bit] = (nodes.len() - 1) as i32;
                    }
                    n = nodes[n][bit] as usize;
                }
            }
        }
        Tree { nodes }
    }
    fn decode(&self, br: &mut Bits) -> Option<u8> {
        let mut n = 0usize;
        loop {
            let next = self.nodes.get(n)?[br.bit() as usize];
            if next < 0 {
                return Some((-next - 1) as u8);
            }
            if next == 0 {
                return None;
            }
            n = next as usize;
        }
    }
}

pub(super) struct BigTable {
    pub tree: Tree,
    pub linbits: u8,
}

pub(super) const COUNT1_A_CODES: [u8; 16] = [1, 5, 4, 5, 6, 5, 4, 4, 7, 3, 6, 0, 7, 2, 3, 1];
pub(super) const COUNT1_A_BITS: [u8; 16] = [1, 4, 4, 5, 4, 6, 5, 6, 4, 5, 5, 6, 5, 6, 6, 6];

pub(super) struct Huffman {
    /// Indexed by table select 0..=31 (None for the all-zero and unused tables).
    pub big: Vec<Option<BigTable>>,
    pub count1: [Tree; 2],
}

/// (table number in the standard, entries)
pub(super) fn big_entries(n: usize) -> Option<Vec<(u8, u32, u8)>> {
    Some(match n {
        1 => HUFF_1.to_vec(),
        2 => HUFF_2.to_vec(),
        3 => HUFF_3.to_vec(),
        5 => HUFF_5.to_vec(),
        6 => HUFF_6.to_vec(),
        7 => HUFF_7.to_vec(),
        8 => HUFF_8.to_vec(),
        9 => HUFF_9.to_vec(),
        10 => HUFF_10.to_vec(),
        11 => HUFF_11.to_vec(),
        12 => HUFF_12.to_vec(),
        13 => HUFF_13.to_vec(),
        15 => HUFF_15.to_vec(),
        16 => HUFF_16.to_vec(),
        24 => HUFF_24.to_vec(),
        _ => return None,
    })
}

/// Which standard table and how many escape bits a table select value stands for.
pub(super) fn table_of(select: usize) -> Option<(usize, u8)> {
    const LIN16: [u8; 8] = [1, 2, 3, 4, 6, 8, 10, 13];
    const LIN24: [u8; 8] = [4, 5, 6, 7, 8, 9, 11, 13];
    match select {
        0 | 4 | 14 => None,
        1..=3 | 5..=13 | 15 => Some((select, 0)),
        16..=23 => Some((16, LIN16[select - 16])),
        24..=31 => Some((24, LIN24[select - 24])),
        _ => None,
    }
}

pub(super) fn huffman() -> &'static Huffman {
    static H: OnceLock<Huffman> = OnceLock::new();
    H.get_or_init(|| {
        let mut big = Vec::new();
        for s in 0..32 {
            big.push(table_of(s).and_then(|(n, lin)| {
                big_entries(n).map(|e| BigTable {
                    tree: Tree::build(e.into_iter()),
                    linbits: lin,
                })
            }));
        }
        let a =
            Tree::build((0..16).map(|i| (COUNT1_A_BITS[i], u32::from(COUNT1_A_CODES[i]), i as u8)));
        let b = Tree::build((0..16).map(|i| (4, 15 - i as u32, i as u8)));
        Huffman {
            big,
            count1: [a, b],
        }
    })
}

// ---------------------------------------------------------------- bit reader

pub(super) struct Bits<'a> {
    data: &'a [u8],
    pub pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }
    pub fn bit(&mut self) -> u32 {
        let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
        let v = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        u32::from(v)
    }
    pub fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit();
        }
        v
    }
    fn over(&self) -> bool {
        self.pos > self.data.len() * 8
    }
}

// ---------------------------------------------------------------- granule data

#[derive(Clone, Default)]
struct Granule {
    part2_3: usize,
    big_values: usize,
    gain: i32,
    scalefac_compress: usize,
    window_switching: bool,
    block_type: u8,
    mixed: bool,
    table_select: [usize; 3],
    subblock_gain: [i32; 3],
    region0: usize,
    region1: usize,
    preflag: bool,
    scalefac_scale: bool,
    count1_table: usize,
    // decoded
    sf_long: [i32; 22],
    sf_short: [[i32; 3]; 13],
    /// Number of lines that carry data (up to the last non-zero).
    nonzero: usize,
}

#[derive(Default)]
struct SideInfo {
    main_data_begin: usize,
    scfsi: [[bool; 4]; 2],
    gr: [[Granule; 2]; 2],
}

fn parse_side_info(h: &Header, d: &[u8]) -> Option<SideInfo> {
    let mut br = Bits::new(d, 0);
    let nch = h.channels();
    let mut si = SideInfo::default();
    if h.version == 1 {
        si.main_data_begin = br.bits(9) as usize;
        br.bits(if nch == 1 { 5 } else { 3 });
        for ch in 0..nch {
            for g in 0..4 {
                si.scfsi[ch][g] = br.bit() == 1;
            }
        }
    } else {
        si.main_data_begin = br.bits(8) as usize;
        br.bits(if nch == 1 { 1 } else { 2 });
    }
    for gr in 0..h.granules() {
        for ch in 0..nch {
            let g = &mut si.gr[gr][ch];
            g.part2_3 = br.bits(12) as usize;
            g.big_values = br.bits(9) as usize;
            if g.big_values > 288 {
                return None;
            }
            g.gain = br.bits(8) as i32;
            g.scalefac_compress = br.bits(if h.version == 1 { 4 } else { 9 }) as usize;
            g.window_switching = br.bit() == 1;
            if g.window_switching {
                g.block_type = br.bits(2) as u8;
                if g.block_type == 0 {
                    return None;
                }
                g.mixed = br.bit() == 1;
                g.table_select[0] = br.bits(5) as usize;
                g.table_select[1] = br.bits(5) as usize;
                for w in 0..3 {
                    g.subblock_gain[w] = br.bits(3) as i32;
                }
                // The standard fixes the regions of switched blocks.
                if g.block_type == 2 && !g.mixed {
                    g.region0 = 8;
                } else {
                    g.region0 = 7;
                }
                g.region1 = 36;
            } else {
                for t in 0..3 {
                    g.table_select[t] = br.bits(5) as usize;
                }
                g.region0 = br.bits(4) as usize;
                g.region1 = br.bits(3) as usize;
            }
            if h.version == 1 {
                g.preflag = br.bit() == 1;
            }
            g.scalefac_scale = br.bit() == 1;
            g.count1_table = br.bit() as usize;
        }
    }
    Some(si)
}

// ---------------------------------------------------------------- scalefactors

fn read_scalefactors_v1(br: &mut Bits, g: &mut Granule, prev: Option<&Granule>, scfsi: &[bool; 4]) {
    let (s1, s2) = SLEN[g.scalefac_compress & 15];
    let (s1, s2) = (u32::from(s1), u32::from(s2));
    if g.window_switching && g.block_type == 2 {
        let long_sfbs = if g.mixed { 8 } else { 0 };
        for i in 0..long_sfbs {
            g.sf_long[i] = br.bits(s1) as i32;
        }
        let start = if g.mixed { 3 } else { 0 };
        for i in start..12 {
            let n = if i < 6 { s1 } else { s2 };
            for w in 0..3 {
                g.sf_short[i][w] = br.bits(n) as i32;
            }
        }
    } else {
        let groups = [
            (0usize, 6usize, s1),
            (6, 11, s1),
            (11, 16, s2),
            (16, 21, s2),
        ];
        for (gi, &(a, b, n)) in groups.iter().enumerate() {
            if scfsi[gi] {
                if let Some(p) = prev {
                    for i in a..b {
                        g.sf_long[i] = p.sf_long[i];
                    }
                    continue;
                }
            }
            for i in a..b {
                g.sf_long[i] = br.bits(n) as i32;
            }
        }
    }
}

/// MPEG-2 scalefactors. Returns the intensity scale bit when this is an intensity channel.
fn read_scalefactors_v2(br: &mut Bits, g: &mut Granule, intensity_channel: bool) {
    let mut sc = g.scalefac_compress;
    let (slen, tab);
    if intensity_channel {
        sc >>= 1;
        if sc < 180 {
            slen = [sc / 36, (sc % 36) / 6, (sc % 36) % 6, 0];
            tab = 3;
        } else if sc < 244 {
            sc -= 180;
            slen = [(sc & 63) >> 4, (sc & 15) >> 2, sc & 3, 0];
            tab = 4;
        } else {
            sc -= 244;
            slen = [sc / 3, sc % 3, 0, 0];
            tab = 5;
        }
    } else if sc < 400 {
        slen = [(sc >> 4) / 5, (sc >> 4) % 5, (sc & 15) >> 2, sc & 3];
        tab = 0;
    } else if sc < 500 {
        sc -= 400;
        slen = [(sc >> 2) / 5, (sc >> 2) % 5, sc & 3, 0];
        tab = 1;
    } else {
        sc -= 500;
        slen = [sc / 3, sc % 3, 0, 0];
        tab = 2;
        g.preflag = true;
    }
    let kind = if g.window_switching && g.block_type == 2 {
        if g.mixed {
            2
        } else {
            1
        }
    } else {
        0
    };
    let counts = LSF_NSF[tab][kind];
    let mut vals: Vec<i32> = Vec::with_capacity(40);
    for (p, &n) in counts.iter().enumerate() {
        for _ in 0..n {
            vals.push(br.bits(slen[p] as u32) as i32);
        }
    }
    vals.resize(vals.len().max(40), 0);
    let mut it = vals.into_iter();
    if kind == 0 {
        for i in 0..21 {
            g.sf_long[i] = it.next().unwrap_or(0);
        }
    } else {
        let long_n = if kind == 2 { 6 } else { 0 };
        for i in 0..long_n {
            g.sf_long[i] = it.next().unwrap_or(0);
        }
        let start = if kind == 2 { 3 } else { 0 };
        for i in start..12 {
            for w in 0..3 {
                g.sf_short[i][w] = it.next().unwrap_or(0);
            }
        }
    }
}

// ---------------------------------------------------------------- Huffman spectrum

fn decode_spectrum(br: &mut Bits, g: &mut Granule, h: &Header, end: usize, is: &mut [i32; 576]) {
    let hf = huffman();
    let ri = h.rate_index();
    let bands = &SFB_LONG[ri];
    let (b0, b1) = if g.window_switching {
        let r0 = if g.block_type == 2 {
            // Three short bands: 36 lines except at 8 kHz (72).
            if g.mixed {
                36
            } else {
                3 * usize::from(SFB_SHORT[ri][3])
            }
        } else if ri <= 2 {
            36
        } else if ri != 8 {
            54
        } else {
            108
        };
        (r0, 576)
    } else {
        let i0 = (g.region0 + 1).min(22);
        let i1 = (g.region0 + g.region1 + 2).min(22);
        (usize::from(bands[i0]), usize::from(bands[i1]))
    };
    let big_end = (g.big_values * 2).min(576);
    let mut k = 0usize;
    while k < big_end {
        let region = if k < b0 {
            0
        } else if k < b1 {
            1
        } else {
            2
        };
        let sel = g.table_select[region];
        let region_end = match region {
            0 => b0,
            1 => b1,
            _ => 576,
        }
        .min(big_end);
        match &hf.big[sel] {
            None => {
                k = region_end.max(k + 1).min(big_end);
            }
            Some(t) => {
                while k < region_end {
                    let Some(sym) = t.tree.decode(br) else {
                        br.pos = usize::MAX / 2;
                        return;
                    };
                    let (mut x, mut y) = (i32::from(sym >> 4), i32::from(sym & 15));
                    if t.linbits > 0 && x == 15 {
                        x += br.bits(u32::from(t.linbits)) as i32;
                    }
                    if x != 0 && br.bit() == 1 {
                        x = -x;
                    }
                    if t.linbits > 0 && y == 15 {
                        y += br.bits(u32::from(t.linbits)) as i32;
                    }
                    if y != 0 && br.bit() == 1 {
                        y = -y;
                    }
                    is[k] = x;
                    is[k + 1] = y;
                    k += 2;
                }
            }
        }
        if br.over() {
            return;
        }
    }
    // Quadruples.
    let tree = &hf.count1[g.count1_table & 1];
    while k + 4 <= 576 && br.pos < end {
        let Some(code) = tree.decode(br) else { break };
        let mut v = [
            i32::from((code >> 3) & 1),
            i32::from((code >> 2) & 1),
            i32::from((code >> 1) & 1),
            i32::from(code & 1),
        ];
        for x in v.iter_mut() {
            if *x != 0 && br.bit() == 1 {
                *x = -*x;
            }
        }
        if br.pos > end {
            break; // the last quadruple ran past the granule: it is padding
        }
        is[k..k + 4].copy_from_slice(&v);
        k += 4;
    }
    g.nonzero = is.iter().rposition(|&v| v != 0).map_or(0, |p| p + 1);
}

// ---------------------------------------------------------------- tables for the filter bank

struct Dsp {
    pow43: Vec<f32>,
    cs: [f32; 8],
    ca: [f32; 8],
    imdct36: Vec<f32>, // 36 x 18
    imdct12: Vec<f32>, // 12 x 6
    win: [Vec<f32>; 4],
    win_short: [f32; 12],
    synth_cos: Vec<f32>, // 64 x 32
    d: Vec<f32>,         // 512
    is_ratio: [f32; 7],
}

fn dsp() -> &'static Dsp {
    static D: OnceLock<Dsp> = OnceLock::new();
    D.get_or_init(|| {
        let pi = std::f64::consts::PI;
        let pow43 = (0..8207)
            .map(|i| (i as f64).powf(4.0 / 3.0) as f32)
            .collect();
        let c = [
            -0.6f64, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037,
        ];
        let mut cs = [0.0; 8];
        let mut ca = [0.0; 8];
        for i in 0..8 {
            let s = (1.0 + c[i] * c[i]).sqrt();
            cs[i] = (1.0 / s) as f32;
            ca[i] = (c[i] / s) as f32;
        }
        let mut imdct36 = vec![0.0f32; 36 * 18];
        for i in 0..36 {
            for k in 0..18 {
                imdct36[i * 18 + k] =
                    (pi / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64).cos() as f32;
            }
        }
        let mut imdct12 = vec![0.0f32; 12 * 6];
        for i in 0..12 {
            for k in 0..6 {
                imdct12[i * 6 + k] =
                    (pi / 24.0 * (2 * i + 1 + 6) as f64 * (2 * k + 1) as f64).cos() as f32;
            }
        }
        let sin36 = |i: usize| (pi / 36.0 * (i as f64 + 0.5)).sin() as f32;
        let sin12 = |i: usize| (pi / 12.0 * (i as f64 + 0.5)).sin() as f32;
        let mut win: [Vec<f32>; 4] = [vec![0.0; 36], vec![0.0; 36], vec![0.0; 36], vec![0.0; 36]];
        for i in 0..36 {
            win[0][i] = sin36(i);
            win[1][i] = match i {
                0..=17 => sin36(i),
                18..=23 => 1.0,
                24..=29 => sin12(i - 18),
                _ => 0.0,
            };
            win[3][i] = match i {
                0..=5 => 0.0,
                6..=11 => sin12(i - 6),
                12..=17 => 1.0,
                _ => sin36(i),
            };
        }
        let mut win_short = [0.0f32; 12];
        for (i, w) in win_short.iter_mut().enumerate() {
            *w = sin12(i);
        }
        let mut synth_cos = vec![0.0f32; 64 * 32];
        for i in 0..64 {
            for k in 0..32 {
                synth_cos[i * 32 + k] = (((16 + i) * (2 * k + 1)) as f64 * pi / 64.0).cos() as f32;
            }
        }
        let mut d = vec![0.0f32; 512];
        for i in 0..=256usize {
            let v = WINDOW[i] as f32 / 65536.0;
            d[i] = v;
            if i != 0 {
                d[512 - i] = if i % 64 != 0 { -v } else { v };
            }
        }
        let mut is_ratio = [0.0f32; 7];
        for (i, r) in is_ratio.iter_mut().enumerate() {
            *r = (i as f64 * pi / 12.0).tan() as f32;
        }
        Dsp {
            pow43,
            cs,
            ca,
            imdct36,
            imdct12,
            win,
            win_short,
            synth_cos,
            d,
            is_ratio,
        }
    })
}

/// The 512-point window of the polyphase filter banks (the synthesis window `D`; the
/// analysis window `C` is `D / 32`).
pub(super) fn synth_window() -> &'static [f32] {
    &dsp().d
}

/// Scale of the alias-reduction butterflies: (cs, ca).
pub(super) fn alias_coefficients() -> ([f32; 8], [f32; 8]) {
    let d = dsp();
    (d.cs, d.ca)
}

// ---------------------------------------------------------------- the decoder

pub struct Decoder {
    reservoir: Vec<u8>,
    /// IMDCT overlap per channel: 32 subbands x 18.
    overlap: [[f32; 576]; 2],
    /// Polyphase state per channel: 1024 values and the rotating offset.
    v: [Vec<f32>; 2],
    voff: [usize; 2],
    /// Granules from the previous frame (scalefactor sharing between granules).
    pub frames: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            reservoir: Vec::new(),
            overlap: [[0.0; 576]; 2],
            v: [vec![0.0; 1024], vec![0.0; 1024]],
            voff: [0, 0],
            frames: 0,
        }
    }

    /// Decode one frame (header included) and append `samples_per_frame * channels`
    /// interleaved samples to `out`. A frame that cannot be decoded yields silence.
    pub fn frame(&mut self, h: &Header, frame: &[u8], out: &mut Vec<f32>) {
        let nch = h.channels();
        let ngr = h.granules();
        let n = ngr * 576;
        let start = out.len();
        out.resize(start + n * nch, 0.0);
        self.frames += 1;
        let hdr = 4 + if h.crc { 2 } else { 0 };
        let si_len = h.side_info_len();
        let Some(si_bytes) = frame.get(hdr..hdr + si_len) else {
            return;
        };
        let Some(mut si) = parse_side_info(h, si_bytes) else {
            return;
        };
        let main = frame.get(hdr + si_len..).unwrap_or(&[]);
        // Bit reservoir: main data may begin in earlier frames.
        let have = self.reservoir.len();
        if si.main_data_begin > have {
            // Not enough history (stream start or lost frames): play silence but keep the data.
            self.push_reservoir(main);
            return;
        }
        let mut data = self.reservoir[have - si.main_data_begin..].to_vec();
        data.extend_from_slice(main);
        self.push_reservoir(main);

        let mut pos = 0usize;
        let mut xr = [[[0.0f32; 576]; 2]; 2];
        let mut intensity_scale = [0u8; 2];
        for gr in 0..ngr {
            let mut is_all = [[0i32; 576]; 2];
            for ch in 0..nch {
                let (before, rest) = si.gr.split_at_mut(gr);
                let prev = before.first().map(|p| &p[ch]);
                let g = &mut rest[0][ch];
                let part_end = pos + g.part2_3;
                let mut br = Bits::new(&data, pos);
                if h.version == 1 {
                    read_scalefactors_v1(&mut br, g, prev, &si.scfsi[ch]);
                } else {
                    let intensity_channel = h.mode == 1 && h.mode_ext & 1 == 1 && ch == 1;
                    read_scalefactors_v2(&mut br, g, intensity_channel);
                    if intensity_channel {
                        intensity_scale[gr] = (g.scalefac_compress & 1) as u8;
                    }
                }
                decode_spectrum(&mut br, g, h, part_end, &mut is_all[ch]);
                pos = part_end;
                requantize(g, h, &is_all[ch], &mut xr[gr][ch]);
            }
            // Stereo processing.
            if nch == 2 && h.mode == 1 {
                let (a, b) = {
                    let (x0, x1) = xr[gr].split_at_mut(1);
                    (&mut x0[0], &mut x1[0])
                };
                stereo(h, &si.gr[gr], a, b, gr, intensity_scale[gr]);
            }
            for ch in 0..nch {
                let g = &si.gr[gr][ch];
                let mut x = xr[gr][ch];
                reorder(g, h, &mut x);
                alias_reduce(g, &mut x);
                let mut sb = [0.0f32; 576];
                self.imdct(ch, g, &x, &mut sb);
                // Frequency inversion.
                for s in (1..32).step_by(2) {
                    for t in (1..18).step_by(2) {
                        sb[s * 18 + t] = -sb[s * 18 + t];
                    }
                }
                // Polyphase synthesis, 18 slots of 32 samples.
                for slot in 0..18 {
                    let mut input = [0.0f32; 32];
                    for s in 0..32 {
                        input[s] = sb[s * 18 + slot];
                    }
                    let mut pcm = [0.0f32; 32];
                    self.synth(ch, &input, &mut pcm);
                    for (j, &v) in pcm.iter().enumerate() {
                        let idx = start + (gr * 576 + slot * 32 + j) * nch + ch;
                        out[idx] = v;
                    }
                }
            }
        }
        // Mono output of a stereo-looking stream needs nothing more; the caller handles mode.
        let _ = &mut si;
    }

    fn push_reservoir(&mut self, main: &[u8]) {
        self.reservoir.extend_from_slice(main);
        let keep = 511;
        if self.reservoir.len() > keep {
            let cut = self.reservoir.len() - keep;
            self.reservoir.drain(..cut);
        }
    }

    fn imdct(&mut self, ch: usize, g: &Granule, x: &[f32; 576], out: &mut [f32; 576]) {
        let d = dsp();
        for sb in 0..32 {
            let src = &x[sb * 18..sb * 18 + 18];
            let mut y = [0.0f32; 36];
            let short = g.window_switching && g.block_type == 2 && !(g.mixed && sb < 2);
            if short {
                for w in 0..3 {
                    for i in 0..12 {
                        let mut acc = 0.0;
                        for k in 0..6 {
                            acc += src[3 * k + w] * d.imdct12[i * 6 + k];
                        }
                        y[6 + 6 * w + i] += acc * d.win_short[i];
                    }
                }
            } else {
                let bt = if g.window_switching && g.mixed && sb < 2 && g.block_type == 2 {
                    0
                } else if g.window_switching {
                    usize::from(g.block_type)
                } else {
                    0
                };
                let win = &d.win[bt];
                for i in 0..36 {
                    let mut acc = 0.0;
                    for k in 0..18 {
                        acc += src[k] * d.imdct36[i * 18 + k];
                    }
                    y[i] = acc * win[i];
                }
            }
            for i in 0..18 {
                out[sb * 18 + i] = y[i] + self.overlap[ch][sb * 18 + i];
                self.overlap[ch][sb * 18 + i] = y[18 + i];
            }
        }
    }

    fn synth(&mut self, ch: usize, s: &[f32; 32], out: &mut [f32; 32]) {
        let d = dsp();
        // The V vector holds 16 groups of 64; new values enter at the front, the rest shifts.
        let off = (self.voff[ch] + 1024 - 64) % 1024;
        self.voff[ch] = off;
        let v = &mut self.v[ch];
        for i in 0..64 {
            let mut acc = 0.0;
            for k in 0..32 {
                acc += d.synth_cos[i * 32 + k] * s[k];
            }
            v[(off + i) % 1024] = acc;
        }
        for j in 0..32 {
            let mut acc = 0.0;
            for i in 0..8 {
                // U[i*64 + j] = V[i*128 + j], U[i*64 + 32 + j] = V[i*128 + 96 + j]
                acc += v[(off + i * 128 + j) % 1024] * d.d[j + 64 * i];
                acc += v[(off + i * 128 + 96 + j) % 1024] * d.d[j + 64 * i + 32];
            }
            out[j] = acc;
        }
    }
}

// ---------------------------------------------------------------- spectrum processing

fn requantize(g: &Granule, h: &Header, is: &[i32; 576], xr: &mut [f32; 576]) {
    let d = dsp();
    let ri = h.rate_index();
    let mult = if g.scalefac_scale { 1.0f64 } else { 0.5 };
    let base = f64::from(g.gain - 210);
    let value = |q: i32, exp2: f64| -> f32 {
        let a = q.unsigned_abs() as usize;
        let m = if a < d.pow43.len() {
            d.pow43[a]
        } else {
            (a as f64).powf(4.0 / 3.0) as f32
        };
        let v = m * exp2.exp2() as f32;
        if q < 0 {
            -v
        } else {
            v
        }
    };
    let short = g.window_switching && g.block_type == 2;
    let long_end = if !short {
        22
    } else if g.mixed {
        mixed_long_bands(ri)
    } else {
        0
    };
    // Long bands.
    let bands = &SFB_LONG[ri];
    for sfb in 0..long_end {
        let pre = if g.preflag { i32::from(PRETAB[sfb]) } else { 0 };
        let e = base / 4.0 - mult * f64::from(g.sf_long[sfb.min(21)] + pre);
        // The last band (21) has no scalefactor of its own and uses zero.
        let e = if sfb == 21 {
            base / 4.0 - mult * f64::from(pre)
        } else {
            e
        };
        for k in bands[sfb] as usize..bands[sfb + 1] as usize {
            if is[k] != 0 {
                xr[k] = value(is[k], e);
            }
        }
    }
    if short {
        let sb = &SFB_SHORT[ri];
        let first = if g.mixed { 3 } else { 0 };
        // Lines of short bands are stored band by band, window by window.
        let mut k = if g.mixed { 36 } else { 0 };
        for sfb in first..13 {
            let wd = usize::from(sb[sfb + 1] - sb[sfb]);
            for w in 0..3 {
                let sg = f64::from(g.subblock_gain[w]) * 8.0;
                let e = (base - sg) / 4.0 - mult * f64::from(g.sf_short[sfb][w]);
                for _ in 0..wd {
                    if k < 576 && is[k] != 0 {
                        xr[k] = value(is[k], e);
                    }
                    k += 1;
                }
            }
        }
    }
}

fn reorder(g: &Granule, h: &Header, xr: &mut [f32; 576]) {
    if !(g.window_switching && g.block_type == 2) {
        return;
    }
    let ri = h.rate_index();
    let sb = &SFB_SHORT[ri];
    let first = if g.mixed { 3 } else { 0 };
    let mut tmp = [0.0f32; 576];
    let mut src = if g.mixed { 36 } else { 0 };
    tmp[..src].copy_from_slice(&xr[..src]);
    for sfb in first..13 {
        let s = usize::from(sb[sfb]);
        let wd = usize::from(sb[sfb + 1]) - s;
        for w in 0..3 {
            for k in 0..wd {
                let dst = 3 * s + 3 * k + w;
                if dst < 576 && src < 576 {
                    tmp[dst] = xr[src];
                }
                src += 1;
            }
        }
    }
    *xr = tmp;
}

fn alias_reduce(g: &Granule, xr: &mut [f32; 576]) {
    let d = dsp();
    let limit = if g.window_switching && g.block_type == 2 {
        if g.mixed {
            2
        } else {
            return;
        }
    } else {
        32
    };
    for sb in 1..limit {
        for i in 0..8 {
            let lo = sb * 18 - 1 - i;
            let hi = sb * 18 + i;
            let (a, b) = (xr[lo], xr[hi]);
            xr[lo] = a * d.cs[i] - b * d.ca[i];
            xr[hi] = b * d.cs[i] + a * d.ca[i];
        }
    }
}

/// Mid/side and intensity stereo on the requantised lines of one granule.
fn stereo(
    h: &Header,
    gs: &[Granule; 2],
    l: &mut [f32; 576],
    r: &mut [f32; 576],
    _gr: usize,
    iscale: u8,
) {
    let d = dsp();
    let ms = h.mode_ext & 2 != 0;
    let intensity = h.mode_ext & 1 != 0;
    let g = &gs[1];
    let ri = h.rate_index();
    let sqrt_half = std::f32::consts::FRAC_1_SQRT_2;
    let do_ms = |l: &mut [f32; 576], r: &mut [f32; 576], range: std::ops::Range<usize>| {
        for k in range {
            let (m, s) = (l[k], r[k]);
            l[k] = (m + s) * sqrt_half;
            r[k] = (m - s) * sqrt_half;
        }
    };
    if !intensity {
        if ms {
            do_ms(l, r, 0..576);
        }
        return;
    }
    // Intensity: bands above the last non-zero line of the right channel.
    let short = g.window_switching && g.block_type == 2;
    let is_val = |pos: i32| -> Option<i32> { (0..7).contains(&pos).then_some(pos) };
    let lsf = h.version != 1;
    let apply = |l: &mut [f32; 576], r: &mut [f32; 576], k: usize, pos: i32| {
        let src = l[k];
        if lsf {
            let kk = if iscale == 0 {
                2f32.powf(-0.25)
            } else {
                2f32.powf(-0.5)
            };
            if pos & 1 == 1 {
                l[k] = src * kk.powi((pos + 1) / 2);
                r[k] = src;
            } else {
                l[k] = src;
                r[k] = src * kk.powi(pos / 2);
            }
        } else {
            let ratio = d.is_ratio[pos as usize];
            l[k] = src * ratio / (1.0 + ratio);
            r[k] = src / (1.0 + ratio);
        }
    };
    let positions = |sfb_value: i32| -> Option<i32> {
        if lsf {
            // MPEG-2 positions run to 31 and are always valid.
            Some(sfb_value)
        } else {
            is_val(sfb_value)
        }
    };
    let right_last = r.iter().rposition(|&v| v != 0.0).map_or(0, |p| p + 1);
    if !short {
        let bands = &SFB_LONG[ri];
        // First intensity band: the first band entirely above the last non-zero right line.
        let mut first_is = 22;
        for sfb in 0..22 {
            if usize::from(bands[sfb]) >= right_last {
                first_is = sfb;
                break;
            }
        }
        if ms {
            do_ms(l, r, 0..usize::from(bands[first_is.min(22)]));
        }
        let mut last_pos = 0;
        for sfb in first_is..22 {
            let pos = if sfb < 21 {
                gs[1].sf_long[sfb]
            } else {
                last_pos
            };
            last_pos = pos;
            let range = usize::from(bands[sfb])..usize::from(bands[sfb + 1]);
            match positions(pos) {
                Some(p) => {
                    for k in range {
                        apply(l, r, k, p);
                    }
                }
                None => {
                    if ms {
                        do_ms(l, r, range);
                    }
                }
            }
        }
    } else {
        // Short blocks: decide per window; lines are still in transmission order here.
        let sb = &SFB_SHORT[ri];
        let long_lines = if g.mixed { 36 } else { 0 };
        let first_short = if g.mixed { 3 } else { 0 };
        // Offsets of each (band, window) in transmission order.
        let mut off = long_lines;
        let mut table = [[0usize; 3]; 13];
        for sfb in first_short..13 {
            let wd = usize::from(sb[sfb + 1] - sb[sfb]);
            for w in 0..3 {
                table[sfb][w] = off;
                off += wd;
            }
        }
        if ms {
            do_ms(l, r, 0..long_lines);
        }
        for w in 0..3 {
            let mut last_nz = None;
            for sfb in first_short..13 {
                let wd = usize::from(sb[sfb + 1] - sb[sfb]);
                let s = table[sfb][w];
                if r[s..s + wd].iter().any(|&v| v != 0.0) {
                    last_nz = Some(sfb);
                }
            }
            let first_is = last_nz.map_or(first_short, |s| s + 1);
            let mut last_pos = 0;
            for sfb in first_short..13 {
                let wd = usize::from(sb[sfb + 1] - sb[sfb]);
                let s = table[sfb][w];
                if sfb < first_is {
                    if ms {
                        do_ms(l, r, s..s + wd);
                    }
                    continue;
                }
                let pos = if sfb < 12 {
                    g.sf_short[sfb][w]
                } else {
                    last_pos
                };
                last_pos = pos;
                match positions(pos) {
                    Some(p) => {
                        for k in s..s + wd {
                            apply(l, r, k, p);
                        }
                    }
                    None => {
                        if ms {
                            do_ms(l, r, s..s + wd);
                        }
                    }
                }
            }
        }
    }
}

/// Test helper: decode the Huffman data of one granule.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn parse_spectrum_for_test(
    bytes: &[u8],
    bits: usize,
    big_values: usize,
    tables: [usize; 3],
    r0: usize,
    r1: usize,
    count1_table: usize,
    rate: u32,
) -> [i32; 576] {
    let h = Header {
        version: 1,
        crc: false,
        bitrate: 128,
        rate,
        padding: false,
        mode: 3,
        mode_ext: 0,
    };
    let g_big = big_values;
    let mut g = Granule {
        big_values: g_big,
        table_select: tables,
        region0: r0,
        region1: r1,
        count1_table,
        ..Granule::default()
    };
    let mut br = Bits::new(bytes, 0);
    let mut is = [0i32; 576];
    decode_spectrum(&mut br, &mut g, &h, bits, &mut is);
    assert_eq!(br.pos, bits, "decoder consumed a different number of bits");
    is
}

/// Test helper: run granules of long-block spectra through the back half of the decoder.
#[cfg(test)]
pub(super) fn reconstruct_long(spectra: &[[f32; 576]]) -> Vec<f32> {
    let mut d = Decoder::new();
    let g = Granule::default();
    let mut out = Vec::new();
    for xr in spectra {
        let mut x = *xr;
        alias_reduce(&g, &mut x);
        let mut sb = [0.0f32; 576];
        d.imdct(0, &g, &x, &mut sb);
        for s in (1..32).step_by(2) {
            for t in (1..18).step_by(2) {
                sb[s * 18 + t] = -sb[s * 18 + t];
            }
        }
        for slot in 0..18 {
            let mut input = [0.0f32; 32];
            for s in 0..32 {
                input[s] = sb[s * 18 + slot];
            }
            let mut pcm = [0.0f32; 32];
            d.synth(0, &input, &mut pcm);
            out.extend_from_slice(&pcm);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_decodes_to_its_symbol() {
        for n in [1usize, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 24] {
            let e = big_entries(n).unwrap();
            let tree = Tree::build(e.iter().copied());
            for &(len, code, sym) in &e {
                let mut bytes = vec![0u8; 4];
                for i in 0..len {
                    let bit = (code >> (len - 1 - i)) & 1;
                    bytes[usize::from(i) / 8] |= (bit as u8) << (7 - i % 8);
                }
                let mut br = Bits::new(&bytes, 0);
                assert_eq!(
                    tree.decode(&mut br),
                    Some(sym),
                    "table {n} code {code:b}/{len}"
                );
                assert_eq!(br.pos, usize::from(len));
            }
        }
    }
}
