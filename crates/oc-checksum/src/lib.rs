//! Integrity checksums used inside formats: CRC-32, CRC-32C, CRC-64/XZ, Adler-32, xxHash64.
//! Scalar reference implementations; SIMD paths will live in `oc-simd`.
#![forbid(unsafe_code)]

pub mod md5;
pub use md5::{md5, Md5};

#[allow(clippy::indexing_slicing)] // i < 256 by loop bound, compile-time only
const fn crc32_table(poly: u32) -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ poly } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

#[allow(clippy::indexing_slicing)] // i < 256 by loop bound, compile-time only
const fn crc64_table(poly: u64) -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u64;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ poly } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC32_TABLE: [u32; 256] = crc32_table(0xEDB8_8320);
static CRC32C_TABLE: [u32; 256] = crc32_table(0x82F6_3B78);
static CRC64_TABLE: [u64; 256] = crc64_table(0xC96C_5795_D787_0F42);

fn crc32_update(table: &[u32; 256], state: u32, data: &[u8]) -> u32 {
    data.iter().fold(state, |c, &b| {
        let idx = usize::from((c as u8) ^ b);
        table.get(idx).copied().unwrap_or(0) ^ (c >> 8)
    })
}

/// Incremental CRC-32 (IEEE 802.3, as used by ZIP, PNG, gzip).
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);
impl Default for Crc32 {
    fn default() -> Self {
        Self(!0)
    }
}
impl Crc32 {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0 = crc32_update(&CRC32_TABLE, self.0, data);
    }
    pub fn finish(&self) -> u32 {
        !self.0
    }
}
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

/// Incremental CRC-32C (Castagnoli).
#[derive(Clone, Copy, Debug)]
pub struct Crc32c(u32);
impl Default for Crc32c {
    fn default() -> Self {
        Self(!0)
    }
}
impl Crc32c {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0 = crc32_update(&CRC32C_TABLE, self.0, data);
    }
    pub fn finish(&self) -> u32 {
        !self.0
    }
}
pub fn crc32c(data: &[u8]) -> u32 {
    let mut c = Crc32c::new();
    c.update(data);
    c.finish()
}

/// Incremental CRC-64/XZ (ECMA-182 reflected, as used by xz).
#[derive(Clone, Copy, Debug)]
pub struct Crc64(u64);
impl Default for Crc64 {
    fn default() -> Self {
        Self(!0)
    }
}
impl Crc64 {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0 = data.iter().fold(self.0, |c, &b| {
            let idx = usize::from((c as u8) ^ b);
            CRC64_TABLE.get(idx).copied().unwrap_or(0) ^ (c >> 8)
        });
    }
    pub fn finish(&self) -> u64 {
        !self.0
    }
}
pub fn crc64_xz(data: &[u8]) -> u64 {
    let mut c = Crc64::new();
    c.update(data);
    c.finish()
}

/// Incremental Adler-32 (zlib).
#[derive(Clone, Copy, Debug)]
pub struct Adler32 {
    a: u32,
    b: u32,
}
impl Default for Adler32 {
    fn default() -> Self {
        Self { a: 1, b: 0 }
    }
}
impl Adler32 {
    const MOD: u32 = 65521;
    /// Largest n such that 255*n*(n+1)/2 + (n+1)*(MOD-1) fits in u32.
    const NMAX: usize = 5552;
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(&mut self, data: &[u8]) {
        for chunk in data.chunks(Self::NMAX) {
            for &x in chunk {
                self.a += u32::from(x);
                self.b += self.a;
            }
            self.a %= Self::MOD;
            self.b %= Self::MOD;
        }
    }
    pub fn finish(&self) -> u32 {
        (self.b << 16) | self.a
    }
}
pub fn adler32(data: &[u8]) -> u32 {
    let mut c = Adler32::new();
    c.update(data);
    c.finish()
}

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

fn le64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    for (d, s) in a.iter_mut().zip(b) {
        *d = *s;
    }
    u64::from_le_bytes(a)
}
fn le32(b: &[u8]) -> u32 {
    let mut a = [0u8; 4];
    for (d, s) in a.iter_mut().zip(b) {
        *d = *s;
    }
    u32::from_le_bytes(a)
}
fn xround(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}
fn xmerge(acc: u64, val: u64) -> u64 {
    (acc ^ xround(0, val)).wrapping_mul(P1).wrapping_add(P4)
}

/// One-shot xxHash64.
pub fn xxhash64(data: &[u8], seed: u64) -> u64 {
    let mut rest = data;
    let mut h = if data.len() >= 32 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while rest.len() >= 32 {
            for (i, vi) in v.iter_mut().enumerate() {
                *vi = xround(*vi, le64(rest.get(i * 8..).unwrap_or(&[])));
            }
            rest = rest.get(32..).unwrap_or(&[]);
        }
        let mut h = v[0]
            .rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18));
        for x in v {
            h = xmerge(h, x);
        }
        h
    } else {
        seed.wrapping_add(P5)
    };
    h = h.wrapping_add(data.len() as u64);
    while rest.len() >= 8 {
        h = (h ^ xround(0, le64(rest)))
            .rotate_left(27)
            .wrapping_mul(P1)
            .wrapping_add(P4);
        rest = rest.get(8..).unwrap_or(&[]);
    }
    if rest.len() >= 4 {
        h = (h ^ u64::from(le32(rest)).wrapping_mul(P1))
            .rotate_left(23)
            .wrapping_mul(P2)
            .wrapping_add(P3);
        rest = rest.get(4..).unwrap_or(&[]);
    }
    for &b in rest {
        h = (h ^ u64::from(b).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^ (h >> 32)
}

const Q1: u32 = 2_654_435_761;
const Q2: u32 = 2_246_822_519;
const Q3: u32 = 3_266_489_917;
const Q4: u32 = 668_265_263;
const Q5: u32 = 374_761_393;

/// One-shot xxHash32.
pub fn xxhash32(data: &[u8], seed: u32) -> u32 {
    let mut rest = data;
    let mut h = if data.len() >= 16 {
        let mut v = [
            seed.wrapping_add(Q1).wrapping_add(Q2),
            seed.wrapping_add(Q2),
            seed,
            seed.wrapping_sub(Q1),
        ];
        while rest.len() >= 16 {
            for (i, vi) in v.iter_mut().enumerate() {
                *vi = vi
                    .wrapping_add(le32(rest.get(i * 4..).unwrap_or(&[])).wrapping_mul(Q2))
                    .rotate_left(13)
                    .wrapping_mul(Q1);
            }
            rest = rest.get(16..).unwrap_or(&[]);
        }
        v[0].rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18))
    } else {
        seed.wrapping_add(Q5)
    };
    h = h.wrapping_add(data.len() as u32);
    while rest.len() >= 4 {
        h = h
            .wrapping_add(le32(rest).wrapping_mul(Q3))
            .rotate_left(17)
            .wrapping_mul(Q4);
        rest = rest.get(4..).unwrap_or(&[]);
    }
    for &b in rest {
        h = h
            .wrapping_add(u32::from(b).wrapping_mul(Q5))
            .rotate_left(11)
            .wrapping_mul(Q1);
    }
    h ^= h >> 15;
    h = h.wrapping_mul(Q2);
    h ^= h >> 13;
    h = h.wrapping_mul(Q3);
    h ^ (h >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECK: &[u8] = b"123456789";

    #[test]
    fn xxhash32_known_values() {
        assert_eq!(xxhash32(b"", 0), 0x02CC_5D05);
        assert_eq!(xxhash32(CHECK, 0), 0x937B_AD67);
    }

    #[test]
    fn crc_check_values() {
        assert_eq!(crc32(CHECK), 0xCBF4_3926);
        assert_eq!(crc32c(CHECK), 0xE306_9283);
        assert_eq!(crc64_xz(CHECK), 0x995D_C9BB_DF19_39FA);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn adler_known_values() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
    }

    #[test]
    fn adler_handles_long_runs_of_ff() {
        // Reference computed with the unoptimised definition.
        let data = vec![0xFFu8; 100_000];
        let (mut a, mut b) = (1u64, 0u64);
        for &x in &data {
            a = (a + u64::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(adler32(&data), ((b << 16) | a) as u32);
    }

    #[test]
    fn xxhash_known_values() {
        assert_eq!(xxhash64(b"", 0), 0xEF46_DB37_51D8_E999);
        assert_eq!(xxhash64(b"abc", 0), 0x44BC_2CF5_AD77_0999);
        // 39 bytes: exercises the 32-byte stripe loop.
        assert_eq!(
            xxhash64(b"Nobody inspects the spammish repetition", 0),
            0xFBCE_A83C_8A37_8BF1
        );
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut c = Crc32::new();
        let mut c2 = Crc32c::new();
        let mut c3 = Crc64::new();
        let mut a = Adler32::new();
        for part in data.chunks(37) {
            c.update(part);
            c2.update(part);
            c3.update(part);
            a.update(part);
        }
        assert_eq!(c.finish(), crc32(&data));
        assert_eq!(c2.finish(), crc32c(&data));
        assert_eq!(c3.finish(), crc64_xz(&data));
        assert_eq!(a.finish(), adler32(&data));
    }
}
