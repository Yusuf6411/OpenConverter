//! Vorbis I decoder: headers, codebooks, floor type 1, residues 0-2, channel coupling, IMDCT.
use super::mdct::Imdct;
use oc_core::{Error, ErrorCode, Result};

pub(crate) fn ilog(mut x: u32) -> u32 {
    let mut n = 0;
    while x > 0 {
        n += 1;
        x >>= 1;
    }
    n
}

/// LSB-first bit reader; reads past the end return zeros and set `over`.
pub(crate) struct Br<'a> {
    data: &'a [u8],
    pos: usize,
    pub over: bool,
}

impl<'a> Br<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            over: false,
        }
    }
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let end = self.pos + n as usize;
        if end > self.data.len() * 8 {
            self.over = true;
            self.pos = self.data.len() * 8;
            return 0;
        }
        let byte = self.pos >> 3;
        let mut w = [0u8; 8];
        let take = (self.data.len() - byte).min(8);
        w[..take].copy_from_slice(&self.data[byte..byte + take]);
        let v = u64::from_le_bytes(w) >> (self.pos & 7);
        self.pos = end;
        (v & ((1u64 << n) - 1)) as u32
    }
    pub fn bit(&mut self) -> bool {
        self.read(1) == 1
    }
    fn peek8(&self) -> (u32, u32) {
        // Up to 8 bits without consuming; also the number actually available.
        let left = (self.data.len() * 8).saturating_sub(self.pos);
        let n = left.min(8) as u32;
        if n == 0 {
            return (0, 0);
        }
        let byte = self.pos >> 3;
        let mut w = [0u8; 2];
        let take = (self.data.len() - byte).min(2);
        w[..take].copy_from_slice(&self.data[byte..byte + take]);
        let v = u16::from_le_bytes(w) >> (self.pos & 7);
        (u32::from(v) & ((1u32 << n) - 1), n)
    }
    pub fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }
}

fn corrupt(msg: &str) -> Error {
    Error::corrupt(format!("Vorbis: {msg}"))
}

fn float32_unpack(x: u32) -> f32 {
    let mant = f64::from(x & 0x1F_FFFF);
    let exp = ((x & 0x7FE0_0000) >> 21) as i32;
    let m = if x & 0x8000_0000 != 0 { -mant } else { mant };
    (m * 2f64.powi(exp - 788)) as f32
}

#[allow(dead_code)] // `entries` documents the book; the tree makes it redundant
pub(crate) struct Codebook {
    pub dims: usize,
    pub entries: usize,
    tree: Vec<[i32; 2]>, // leaves are -(entry + 1)
    lut: Vec<i32>,       // 256 entries: (entry << 4 | len), -1 for none
    pub vq: Vec<f32>,    // entries * dims, empty without lookup
}

pub(crate) fn assign_codewords(lens: &[u8]) -> Result<Vec<u32>> {
    let mut marker = [0u32; 33];
    let mut words = vec![0u32; lens.len()];
    let mut count = 0usize;
    for (i, &l) in lens.iter().enumerate() {
        let length = usize::from(l);
        if length == 0 {
            continue;
        }
        let mut entry = marker[length];
        if length < 32 && (entry >> length) != 0 {
            return Err(corrupt("overpopulated codebook"));
        }
        words[i] = entry;
        count += 1;
        let mut j = length;
        while j > 0 {
            if marker[j] & 1 != 0 {
                if j == 1 {
                    marker[1] += 1;
                } else {
                    marker[j] = marker[j - 1] << 1;
                }
                break;
            }
            marker[j] += 1;
            j -= 1;
        }
        for j in length + 1..33 {
            if (marker[j] >> 1) == entry {
                entry = marker[j];
                marker[j] = marker[j - 1] << 1;
            } else {
                break;
            }
        }
    }
    if count != 1 {
        for (i, m) in marker.iter().enumerate().skip(1) {
            if m & (0xFFFF_FFFFu32 >> (32 - i)) != 0 {
                return Err(corrupt("underpopulated codebook"));
            }
        }
    }
    Ok(words)
}

impl Codebook {
    fn parse(br: &mut Br) -> Result<Self> {
        if br.read(24) != 0x56_4342 {
            return Err(corrupt("bad codebook sync"));
        }
        let dims = br.read(16) as usize;
        let entries = br.read(24) as usize;
        if dims == 0 || entries == 0 {
            return Err(corrupt("empty codebook"));
        }
        let mut lens = vec![0u8; entries];
        let ordered = br.bit();
        if ordered {
            let mut cur = br.read(5) + 1;
            let mut e = 0usize;
            while e < entries {
                let n = br.read(ilog((entries - e) as u32)) as usize;
                if e + n > entries || br.over {
                    return Err(corrupt("bad ordered codebook"));
                }
                for l in &mut lens[e..e + n] {
                    *l = cur as u8;
                }
                e += n;
                cur += 1;
                if cur > 32 {
                    return Err(corrupt("codeword too long"));
                }
            }
        } else {
            let sparse = br.bit();
            for l in &mut lens {
                if sparse {
                    if br.bit() {
                        *l = br.read(5) as u8 + 1;
                    }
                } else {
                    *l = br.read(5) as u8 + 1;
                }
            }
        }
        if br.over {
            return Err(Error::truncated("Vorbis setup header ends early"));
        }
        let words = assign_codewords(&lens)?;
        // Decoding tree and an 8-bit lookup for the common short codes.
        let mut tree: Vec<[i32; 2]> = vec![[0, 0]];
        let mut lut = vec![-1i32; 256];
        for (e, &l) in lens.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let len = u32::from(l);
            let w = words[e];
            let mut node = 0usize;
            for b in (0..len).rev() {
                let bit = ((w >> b) & 1) as usize;
                if b == 0 {
                    tree[node][bit] = -(e as i32 + 1);
                } else {
                    let next = tree[node][bit];
                    if next > 0 {
                        node = next as usize;
                    } else if next == 0 {
                        tree.push([0, 0]);
                        let id = tree.len() - 1;
                        tree[node][bit] = id as i32;
                        node = id;
                    } else {
                        return Err(corrupt("codeword is a prefix of another"));
                    }
                }
            }
            if len <= 8 {
                // First bit read is the most significant bit of the codeword.
                let mut rev = 0u32;
                for b in 0..len {
                    rev |= ((w >> (len - 1 - b)) & 1) << b;
                }
                let mut k = 0u32;
                while (k << len) | rev < 256 {
                    lut[((k << len) | rev) as usize] = ((e as i32) << 4) | len as i32;
                    k += 1;
                }
            }
        }
        let lookup = br.read(4);
        let mut vq = Vec::new();
        if lookup == 1 || lookup == 2 {
            let min = float32_unpack(br.read(32));
            let delta = float32_unpack(br.read(32));
            let value_bits = br.read(4) + 1;
            let sequence = br.bit();
            let values = if lookup == 1 {
                let mut v = 0usize;
                loop {
                    let next = v + 1;
                    let mut p = 1usize;
                    let mut over = false;
                    for _ in 0..dims {
                        p = match p.checked_mul(next) {
                            Some(x) if x <= entries => x,
                            _ => {
                                over = true;
                                break;
                            }
                        };
                    }
                    if over {
                        break;
                    }
                    v = next;
                }
                v
            } else {
                entries
                    .checked_mul(dims)
                    .filter(|&n| n <= 1 << 26)
                    .ok_or_else(|| corrupt("codebook too large"))?
            };
            let mut mult = Vec::with_capacity(values);
            for _ in 0..values {
                mult.push(br.read(value_bits) as f32);
            }
            if br.over {
                return Err(Error::truncated("Vorbis setup header ends early"));
            }
            if entries.saturating_mul(dims) > 1 << 24 {
                return Err(Error::new(
                    ErrorCode::LimitExceeded,
                    "Vorbis codebook is too large",
                ));
            }
            vq = vec![0.0f32; entries * dims];
            for e in 0..entries {
                let mut last = 0.0f32;
                let mut div = 1usize;
                for d in 0..dims {
                    let idx = if lookup == 1 {
                        (e / div) % values
                    } else {
                        e * dims + d
                    };
                    let v = mult.get(idx).copied().unwrap_or(0.0) * delta + min + last;
                    vq[e * dims + d] = v;
                    if sequence {
                        last = v;
                    }
                    if lookup == 1 {
                        div = div.saturating_mul(values.max(1));
                    }
                }
            }
        } else if lookup != 0 {
            return Err(corrupt("reserved codebook lookup type"));
        }
        Ok(Self {
            dims,
            entries,
            tree,
            lut,
            vq,
        })
    }

    /// Next entry number, or `None` at the end of the packet or on an unassigned code.
    pub fn decode(&self, br: &mut Br) -> Option<usize> {
        let (bits, avail) = br.peek8();
        let hit = self.lut[bits as usize];
        if hit >= 0 {
            let len = (hit & 15) as u32;
            if len <= avail {
                br.skip(len);
                return Some((hit >> 4) as usize);
            }
            br.over = true;
            return None;
        }
        let mut node = 0usize;
        loop {
            if br.over {
                return None;
            }
            let b = br.read(1) as usize;
            if br.over {
                return None;
            }
            let next = self.tree[node][b];
            if next < 0 {
                return Some((-next - 1) as usize);
            }
            if next == 0 {
                return None;
            }
            node = next as usize;
        }
    }

    pub fn vector(&self, entry: usize) -> &[f32] {
        &self.vq[entry * self.dims..(entry + 1) * self.dims]
    }
}

pub(crate) struct Floor1 {
    pub(crate) partition_class: Vec<usize>,
    pub(crate) class_dims: Vec<usize>,
    pub(crate) class_subs: Vec<u32>,
    pub(crate) class_master: Vec<usize>,
    pub(crate) class_books: Vec<Vec<i32>>,
    pub(crate) multiplier: u32,
    pub(crate) xlist: Vec<u32>,
    pub(crate) sorted: Vec<usize>, // indices of xlist in ascending x
    pub(crate) low: Vec<usize>, // per point i>=2: index of the greatest smaller x among earlier points
    pub(crate) high: Vec<usize>,
}

impl Floor1 {
    pub(crate) fn new(
        partition_class: Vec<usize>,
        class_dims: Vec<usize>,
        class_subs: Vec<u32>,
        class_master: Vec<usize>,
        class_books: Vec<Vec<i32>>,
        multiplier: u32,
        xlist: Vec<u32>,
    ) -> Result<Self> {
        let mut sorted: Vec<usize> = (0..xlist.len()).collect();
        sorted.sort_by_key(|&i| xlist[i]);
        if sorted.windows(2).any(|w| xlist[w[0]] == xlist[w[1]]) {
            return Err(corrupt("duplicate floor 1 point"));
        }
        let mut low = vec![0; xlist.len()];
        let mut high = vec![1; xlist.len()];
        for i in 2..xlist.len() {
            let mut lo = 0usize;
            let mut hi = 1usize;
            for j in 0..i {
                if xlist[j] < xlist[i] && xlist[j] >= xlist[lo] {
                    lo = j;
                }
                if xlist[j] > xlist[i] && xlist[j] <= xlist[hi] {
                    hi = j;
                }
            }
            low[i] = lo;
            high[i] = hi;
        }
        Ok(Self {
            partition_class,
            class_dims,
            class_subs,
            class_master,
            class_books,
            multiplier,
            xlist,
            sorted,
            low,
            high,
        })
    }

    pub(crate) fn range(&self) -> i32 {
        [256, 128, 64, 64][self.multiplier as usize - 1]
    }

    /// Turn the coded values into the curve's (x, table index) pairs, ascending in x.
    pub(crate) fn points(&self, y: &[i32]) -> Vec<i32> {
        let range = self.range();
        let n = self.xlist.len();
        let mut fy = vec![0i32; n];
        let mut used = vec![false; n];
        fy[0] = y[0];
        fy[1] = y[1];
        used[0] = true;
        used[1] = true;
        for i in 2..n {
            let (lo, hi) = (self.low[i], self.high[i]);
            let predicted = render_point(
                self.xlist[lo] as i32,
                fy[lo],
                self.xlist[hi] as i32,
                fy[hi],
                self.xlist[i] as i32,
            );
            let val = y[i];
            let highroom = range - predicted;
            let lowroom = predicted;
            let room = if highroom < lowroom {
                highroom * 2
            } else {
                lowroom * 2
            };
            if val != 0 {
                used[lo] = true;
                used[hi] = true;
                used[i] = true;
                fy[i] = if val >= room {
                    if highroom > lowroom {
                        val - lowroom + predicted
                    } else {
                        predicted - val + highroom - 1
                    }
                } else if val & 1 == 1 {
                    predicted - ((val + 1) >> 1)
                } else {
                    predicted + (val >> 1)
                };
            } else {
                fy[i] = predicted;
            }
        }
        let mult = self.multiplier as i32;
        let mut pts = Vec::new();
        for &i in &self.sorted {
            if used[i] {
                pts.push(self.xlist[i] as i32);
                pts.push((fy[i] * mult).clamp(0, 255));
            }
        }
        pts
    }
}

struct Residue {
    kind: u32,
    begin: usize,
    end: usize,
    part_size: usize,
    classes: usize,
    classbook: usize,
    books: Vec<[i32; 8]>,
}

struct Mapping {
    submaps: usize,
    mux: Vec<usize>,
    floors: Vec<usize>,
    residues: Vec<usize>,
    coupling: Vec<(usize, usize)>,
}

struct Mode {
    blockflag: bool,
    mapping: usize,
}

#[allow(dead_code)] // kept for callers that report stream facts
pub struct Decoder {
    pub channels: usize,
    pub sample_rate: u32,
    pub bitrate_nominal: i32,
    bs: [usize; 2],
    books: Vec<Codebook>,
    floors: Vec<Floor1>,
    residues: Vec<Residue>,
    mappings: Vec<Mapping>,
    modes: Vec<Mode>,
    imdct: [Imdct; 2],
    slopes: [Vec<f32>; 2],
    prev: Vec<Vec<f32>>,
    prev_n: usize,
    started: bool,
}

pub struct Identification {
    pub channels: usize,
    pub sample_rate: u32,
    pub bitrate_nominal: i32,
    pub bs: [usize; 2],
}

pub fn parse_identification(p: &[u8]) -> Result<Identification> {
    if p.len() < 30 || p[0] != 1 || &p[1..7] != b"vorbis" {
        return Err(corrupt("bad identification header"));
    }
    let mut br = Br::new(&p[7..]);
    if br.read(32) != 0 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "unknown Vorbis version",
        ));
    }
    let channels = br.read(8) as usize;
    let sample_rate = br.read(32);
    let _max = br.read(32);
    let nominal = br.read(32) as i32;
    let _min = br.read(32);
    let b0 = br.read(4);
    let b1 = br.read(4);
    if !br.bit()
        || channels == 0
        || sample_rate == 0
        || !(6..=13).contains(&b0)
        || !(6..=13).contains(&b1)
        || b0 > b1
    {
        return Err(corrupt("impossible identification header"));
    }
    Ok(Identification {
        channels,
        sample_rate,
        bitrate_nominal: nominal,
        bs: [1 << b0, 1 << b1],
    })
}

fn slope(len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| {
            let x = (i as f64 + 0.5) / len as f64 * std::f64::consts::FRAC_PI_2;
            (std::f64::consts::FRAC_PI_2 * x.sin().powi(2)).sin() as f32
        })
        .collect()
}

impl Decoder {
    pub fn new(id: &Identification, setup: &[u8]) -> Result<Self> {
        if setup.len() < 8 || setup[0] != 5 || &setup[1..7] != b"vorbis" {
            return Err(corrupt("bad setup header"));
        }
        let mut br = Br::new(&setup[7..]);
        let nbooks = br.read(8) as usize + 1;
        let mut books = Vec::with_capacity(nbooks);
        for _ in 0..nbooks {
            books.push(Codebook::parse(&mut br)?);
        }
        // Time-domain transforms: placeholders that must be zero.
        for _ in 0..br.read(6) + 1 {
            if br.read(16) != 0 {
                return Err(corrupt("time transform type is not zero"));
            }
        }
        let nfloors = br.read(6) as usize + 1;
        let mut floors = Vec::with_capacity(nfloors);
        for _ in 0..nfloors {
            let kind = br.read(16);
            if kind != 1 {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "Vorbis floor type 0 is not supported",
                ));
            }
            let nparts = br.read(5) as usize;
            let partition_class: Vec<usize> = (0..nparts).map(|_| br.read(4) as usize).collect();
            let maxc = partition_class.iter().copied().max().map_or(0, |m| m + 1);
            let mut class_dims = vec![0usize; maxc];
            let mut class_subs = vec![0u32; maxc];
            let mut class_master = vec![0usize; maxc];
            let mut class_books = vec![Vec::new(); maxc];
            for c in 0..maxc {
                class_dims[c] = br.read(3) as usize + 1;
                class_subs[c] = br.read(2);
                if class_subs[c] != 0 {
                    class_master[c] = br.read(8) as usize;
                    if class_master[c] >= books.len() {
                        return Err(corrupt("floor master book out of range"));
                    }
                }
                for _ in 0..(1u32 << class_subs[c]) {
                    let b = br.read(8) as i32 - 1;
                    if b >= books.len() as i32 {
                        return Err(corrupt("floor book out of range"));
                    }
                    class_books[c].push(b);
                }
            }
            let multiplier = br.read(2) + 1;
            let rangebits = br.read(4);
            let mut xlist = vec![0u32, 1 << rangebits];
            for &c in &partition_class {
                for _ in 0..class_dims[c] {
                    xlist.push(br.read(rangebits));
                }
            }
            if xlist.len() > 65 || br.over {
                return Err(corrupt("bad floor 1 point list"));
            }
            let floor = Floor1::new(
                partition_class,
                class_dims,
                class_subs,
                class_master,
                class_books,
                multiplier,
                xlist,
            )?;
            floors.push(floor);
        }
        let nres = br.read(6) as usize + 1;
        let mut residues = Vec::with_capacity(nres);
        for _ in 0..nres {
            let kind = br.read(16);
            if kind > 2 {
                return Err(corrupt("bad residue type"));
            }
            let begin = br.read(24) as usize;
            let end = br.read(24) as usize;
            let part_size = br.read(24) as usize + 1;
            let classes = br.read(6) as usize + 1;
            let classbook = br.read(8) as usize;
            if classbook >= books.len() || end < begin {
                return Err(corrupt("bad residue"));
            }
            let mut cascade = Vec::with_capacity(classes);
            for _ in 0..classes {
                let low = br.read(3);
                let high = if br.bit() { br.read(5) } else { 0 };
                cascade.push(high * 8 + low);
            }
            let mut rb = Vec::with_capacity(classes);
            for &c in &cascade {
                let mut a = [-1i32; 8];
                for (j, slot) in a.iter_mut().enumerate() {
                    if c & (1 << j) != 0 {
                        let b = br.read(8) as i32;
                        if b as usize >= books.len() {
                            return Err(corrupt("residue book out of range"));
                        }
                        *slot = b;
                    }
                }
                rb.push(a);
            }
            residues.push(Residue {
                kind,
                begin,
                end,
                part_size,
                classes,
                classbook,
                books: rb,
            });
        }
        let nmaps = br.read(6) as usize + 1;
        let mut mappings = Vec::with_capacity(nmaps);
        for _ in 0..nmaps {
            if br.read(16) != 0 {
                return Err(corrupt("bad mapping type"));
            }
            let submaps = if br.bit() { br.read(4) as usize + 1 } else { 1 };
            let mut coupling = Vec::new();
            if br.bit() {
                let steps = br.read(8) as usize + 1;
                let bits = ilog(id.channels as u32 - 1);
                for _ in 0..steps {
                    let m = br.read(bits) as usize;
                    let a = br.read(bits) as usize;
                    if m == a || m >= id.channels || a >= id.channels {
                        return Err(corrupt("bad channel coupling"));
                    }
                    coupling.push((m, a));
                }
            }
            if br.read(2) != 0 {
                return Err(corrupt("reserved mapping bits"));
            }
            let mut mux = vec![0usize; id.channels];
            if submaps > 1 {
                for m in &mut mux {
                    *m = br.read(4) as usize;
                    if *m >= submaps {
                        return Err(corrupt("bad submap number"));
                    }
                }
            }
            let mut fl = Vec::new();
            let mut rs = Vec::new();
            for _ in 0..submaps {
                br.read(8);
                let f = br.read(8) as usize;
                let r = br.read(8) as usize;
                if f >= floors.len() || r >= residues.len() {
                    return Err(corrupt("mapping refers to a missing floor or residue"));
                }
                fl.push(f);
                rs.push(r);
            }
            mappings.push(Mapping {
                submaps,
                mux,
                floors: fl,
                residues: rs,
                coupling,
            });
        }
        let nmodes = br.read(6) as usize + 1;
        let mut modes = Vec::with_capacity(nmodes);
        for _ in 0..nmodes {
            let blockflag = br.bit();
            if br.read(16) != 0 || br.read(16) != 0 {
                return Err(corrupt("bad mode window or transform type"));
            }
            let mapping = br.read(8) as usize;
            if mapping >= mappings.len() {
                return Err(corrupt("mode refers to a missing mapping"));
            }
            modes.push(Mode { blockflag, mapping });
        }
        if !br.bit() || br.over {
            return Err(corrupt("setup header framing error"));
        }
        let bs = id.bs;
        Ok(Self {
            channels: id.channels,
            sample_rate: id.sample_rate,
            bitrate_nominal: id.bitrate_nominal,
            bs,
            books,
            floors,
            residues,
            mappings,
            modes,
            imdct: [Imdct::new(bs[0]), Imdct::new(bs[1])],
            slopes: [slope(bs[0] / 2), slope(bs[1] / 2)],
            prev: vec![Vec::new(); id.channels],
            prev_n: 0,
            started: false,
        })
    }

    fn floor1_decode(&self, f: &Floor1, br: &mut Br) -> Option<Vec<i32>> {
        if !br.bit() {
            return None;
        }
        let range = f.range();
        let bits = ilog(range as u32 - 1);
        let n = f.xlist.len();
        let mut y = vec![0i32; n];
        y[0] = br.read(bits) as i32;
        y[1] = br.read(bits) as i32;
        let mut offset = 2;
        for &c in &f.partition_class {
            let cdim = f.class_dims[c];
            let cbits = f.class_subs[c];
            let csub = (1u32 << cbits) - 1;
            let mut cval = 0u32;
            if cbits > 0 {
                cval = self.books[f.class_master[c]].decode(br)? as u32;
            }
            for j in 0..cdim {
                let book = f.class_books[c][(cval & csub) as usize];
                cval >>= cbits;
                y[offset + j] = if book >= 0 {
                    self.books[book as usize].decode(br)? as i32
                } else {
                    0
                };
            }
            offset += cdim;
        }
        if br.over {
            return None;
        }
        Some(f.points(&y))
    }

    fn residue_decode(
        &self,
        r: &Residue,
        br: &mut Br,
        nbuf: usize,
        skip: &[bool],
        bufs: &mut [Vec<f32>],
    ) {
        let ch = bufs.len();
        let (vectors, size) = if r.kind == 2 {
            (1, nbuf * ch)
        } else {
            (ch, nbuf)
        };
        if r.kind == 2 && skip.iter().all(|&s| s) {
            return;
        }
        let mut inter: Vec<f32> = if r.kind == 2 {
            vec![0.0; size]
        } else {
            Vec::new()
        };
        let begin = r.begin.min(size);
        let end = r.end.min(size);
        if end <= begin {
            return;
        }
        let book = &self.books[r.classbook];
        let cw = book.dims;
        let nparts = (end - begin) / r.part_size;
        if nparts == 0 {
            return;
        }
        let mut classes = vec![vec![0usize; nparts + cw]; vectors];
        for pass in 0..8 {
            let mut p = 0;
            while p < nparts {
                if pass == 0 {
                    for v in 0..vectors {
                        if r.kind != 2 && skip[v] {
                            continue;
                        }
                        let Some(mut temp) = book.decode(br) else {
                            return;
                        };
                        for i in (0..cw).rev() {
                            classes[v][p + i] = temp % r.classes;
                            temp /= r.classes;
                        }
                    }
                }
                let mut i = 0;
                while i < cw && p < nparts {
                    for v in 0..vectors {
                        if r.kind != 2 && skip[v] {
                            continue;
                        }
                        let cls = classes[v][p];
                        let b = r.books[cls][pass];
                        if b >= 0 {
                            let cb = &self.books[b as usize];
                            if cb.vq.is_empty() {
                                continue;
                            }
                            let offset = begin + p * r.part_size;
                            let target: &mut [f32] = if r.kind == 2 {
                                &mut inter
                            } else {
                                &mut bufs[v]
                            };
                            if r.kind == 0 {
                                let step = r.part_size / cb.dims;
                                for s in 0..step {
                                    let Some(e) = cb.decode(br) else { return };
                                    let vec = cb.vector(e);
                                    for (j, x) in vec.iter().enumerate() {
                                        if let Some(t) = target.get_mut(offset + s + j * step) {
                                            *t += x;
                                        }
                                    }
                                }
                            } else {
                                let mut s = 0;
                                while s < r.part_size {
                                    let Some(e) = cb.decode(br) else { return };
                                    let vec = cb.vector(e);
                                    for (j, x) in vec.iter().enumerate() {
                                        if let Some(t) = target.get_mut(offset + s + j) {
                                            *t += x;
                                        }
                                    }
                                    s += cb.dims;
                                }
                            }
                        }
                    }
                    i += 1;
                    p += 1;
                }
            }
        }
        if r.kind == 2 {
            for (c, buf) in bufs.iter_mut().enumerate() {
                for k in 0..nbuf {
                    buf[k] = inter[k * ch + c];
                }
            }
        }
    }

    /// Decode one audio packet, appending interleaved samples to `out`.
    pub fn decode(&mut self, pkt: &[u8], out: &mut Vec<f32>) -> Result<()> {
        let mut br = Br::new(pkt);
        if br.bit() {
            return Ok(()); // not an audio packet
        }
        let mode_bits = ilog(self.modes.len() as u32 - 1);
        let mode_no = br.read(mode_bits) as usize;
        let Some(mode) = self.modes.get(mode_no) else {
            return Err(corrupt("bad mode number"));
        };
        let long = mode.blockflag;
        let (prev_long, next_long) = if long {
            (br.bit(), br.bit())
        } else {
            (false, false)
        };
        if br.over {
            return Ok(()); // an empty packet decodes to nothing but still counts as a block
        }
        let n = self.bs[usize::from(long)];
        let half = n / 2;
        let map = &self.mappings[mode.mapping];
        let ch = self.channels;
        // Floors.
        let mut curves: Vec<Option<Vec<f32>>> = Vec::with_capacity(ch);
        for c in 0..ch {
            let f = &self.floors[map.floors[map.mux[c]]];
            curves.push(
                self.floor1_decode(f, &mut br)
                    .map(|pts| render_floor(&pts, half)),
            );
        }
        let mut skip: Vec<bool> = curves.iter().map(Option::is_none).collect();
        // A coupled pair is decoded whenever either of its channels carries data.
        for &(m, a) in &map.coupling {
            if !skip[m] || !skip[a] {
                skip[m] = false;
                skip[a] = false;
            }
        }
        // Residues, per submap.
        let mut res = vec![vec![0.0f32; half]; ch];
        for s in 0..map.submaps {
            let members: Vec<usize> = (0..ch).filter(|&c| map.mux[c] == s).collect();
            if members.is_empty() {
                continue;
            }
            let flags: Vec<bool> = members.iter().map(|&c| skip[c]).collect();
            let mut bufs: Vec<Vec<f32>> = vec![vec![0.0; half]; members.len()];
            let r = &self.residues[map.residues[s]];
            self.residue_decode(r, &mut br, half, &flags, &mut bufs);
            for (b, &c) in bufs.into_iter().zip(&members) {
                res[c] = b;
            }
        }
        // Undo channel coupling, last step first.
        for &(m, a) in map.coupling.iter().rev() {
            for i in 0..half {
                let (mag, ang) = (res[m][i], res[a][i]);
                let (nm, na) = if mag > 0.0 {
                    if ang > 0.0 {
                        (mag, mag - ang)
                    } else {
                        (mag + ang, mag)
                    }
                } else if ang > 0.0 {
                    (mag, mag + ang)
                } else {
                    (mag - ang, mag)
                };
                res[m][i] = nm;
                res[a][i] = na;
            }
        }
        // Window shape.
        let ln = if long && !prev_long {
            self.bs[0] / 2
        } else {
            half
        };
        let rn = if long && !next_long {
            self.bs[0] / 2
        } else {
            half
        };
        let slope_for = |len: usize| {
            if len == self.bs[0] / 2 {
                &self.slopes[0]
            } else {
                &self.slopes[1]
            }
        };
        let (left_slope, right_slope) = (slope_for(ln), slope_for(rn));
        let mut window = vec![0.0f32; n];
        let ls = n / 4 - ln / 2;
        for (i, w) in window.iter_mut().enumerate().take(half) {
            *w = if i < ls {
                0.0
            } else if i < ls + ln {
                left_slope[i - ls]
            } else {
                1.0
            };
        }
        let rs = 3 * n / 4 - rn / 2;
        for i in half..n {
            window[i] = if i < rs {
                1.0
            } else if i < rs + rn {
                right_slope[rn - 1 - (i - rs)]
            } else {
                0.0
            };
        }
        // Inverse transform each channel with floor times residue.
        let imdct = &self.imdct[usize::from(long)];
        let mut cur: Vec<Vec<f32>> = Vec::with_capacity(ch);
        for c in 0..ch {
            let mut spec = vec![0.0f32; half];
            if let Some(curve) = &curves[c] {
                for i in 0..half {
                    spec[i] = res[c][i] * curve[i];
                }
            }
            let mut t = vec![0.0f32; n];
            imdct.inverse(&spec, &mut t);
            for (v, w) in t.iter_mut().zip(&window) {
                *v *= w;
            }
            cur.push(t);
        }
        // Overlap-add against the previous window's right half.
        if self.started {
            let len = self.prev_n / 4 + n / 4;
            for k in 0..len {
                for c in 0..ch {
                    let p = self.prev[c].get(k).copied().unwrap_or(0.0);
                    // index into the current window's left half
                    let ci = (k + n / 4) as isize - (self.prev_n / 4) as isize;
                    let q = if ci >= 0 && (ci as usize) < n {
                        cur[c][ci as usize]
                    } else {
                        0.0
                    };
                    out.push(p + q);
                }
            }
        }
        for c in 0..ch {
            self.prev[c] = cur[c][half..].to_vec();
        }
        self.prev_n = n;
        self.started = true;
        Ok(())
    }
}

pub(crate) fn render_point(x0: i32, y0: i32, x1: i32, y1: i32, x: i32) -> i32 {
    let dy = y1 - y0;
    let adx = x1 - x0;
    let ady = dy.abs();
    let err = ady * (x - x0);
    let off = err / adx;
    if dy < 0 {
        y0 - off
    } else {
        y0 + off
    }
}

/// Points are flattened (x, y) pairs in ascending x. Fills `half` values.
pub(crate) fn render_floor(pts: &[i32], half: usize) -> Vec<f32> {
    let mut floor = vec![0i32; half];
    let n = pts.len() / 2;
    let (mut lx, mut ly) = (pts[0], pts[1]);
    let mut hx = 0;
    for k in 1..n {
        let (x1, y1) = (pts[2 * k], pts[2 * k + 1]);
        hx = x1;
        render_line(lx, ly, x1, y1, &mut floor);
        lx = x1;
        ly = y1;
    }
    if (hx as usize) < half {
        for v in floor.iter_mut().skip(hx.max(0) as usize) {
            *v = ly;
        }
    }
    floor.iter().map(|&v| inverse_db(v as usize)).collect()
}

fn render_line(x0: i32, y0: i32, x1: i32, y1: i32, floor: &mut [i32]) {
    let dy = y1 - y0;
    let adx = x1 - x0;
    if adx <= 0 {
        return;
    }
    let ady0 = dy.abs();
    let base = dy / adx;
    let mut x = x0;
    let mut y = y0;
    let mut err = 0;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    let ady = ady0 - base.abs() * adx;
    if let Some(v) = floor.get_mut(x as usize) {
        *v = y;
    }
    x += 1;
    while x < x1 {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        if let Some(v) = floor.get_mut(x as usize) {
            *v = y;
        }
        x += 1;
    }
}

pub(crate) fn inverse_db(i: usize) -> f32 {
    // The 256-entry table of the specification is geometric from 1.0649863e-07 to 1.0.
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let first = 1.064_986_3e-7_f64;
        (0..256)
            .map(|i| first.powf((255 - i) as f64 / 255.0) as f32)
            .collect()
    });
    t[i.min(255)]
}
