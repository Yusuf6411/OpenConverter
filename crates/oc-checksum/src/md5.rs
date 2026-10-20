//! MD5 (RFC 1321). Used only as an integrity check inside formats (FLAC's audio digest), never
//! for security.

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const fn k_table() -> [u32; 64] {
    // floor(2^32 * abs(sin(i + 1))), written out because const floating point is unavailable.
    [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ]
}
static K: [u32; 64] = k_table();

/// Incremental MD5.
#[derive(Clone)]
pub struct Md5 {
    state: [u32; 4],
    buf: [u8; 64],
    fill: usize,
    len: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(clippy::indexing_slicing)] // all indices are bounded by the fixed 64-entry tables and 16-word block
impl Md5 {
    pub fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buf: [0; 64],
            fill: 0,
            len: 0,
        }
    }

    fn block(&mut self, b: &[u8]) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        let [mut a, mut bb, mut c, mut d] = self.state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((bb & c) | (!bb & d), i),
                1 => ((d & bb) | (!d & c), (5 * i + 1) % 16),
                2 => (bb ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (bb | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = bb;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            bb = bb.wrapping_add(sum.rotate_left(S[i]));
            a = tmp;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(bb);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        if self.fill > 0 {
            let take = (64 - self.fill).min(data.len());
            self.buf[self.fill..self.fill + take].copy_from_slice(&data[..take]);
            self.fill += take;
            data = &data[take..];
            if self.fill == 64 {
                let b = self.buf;
                self.block(&b);
                self.fill = 0;
            }
        }
        while data.len() >= 64 {
            self.block(&data[..64]);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.fill = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 16] {
        let bits = self.len.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        let used = (self.fill + 1) % 64;
        pad.resize(1 + if used <= 56 { 56 - used } else { 120 - used }, 0);
        pad.extend_from_slice(&bits.to_le_bytes());
        let len = self.len;
        self.update(&pad);
        self.len = len;
        let mut out = [0u8; 16];
        for (i, w) in self.state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

pub fn md5(data: &[u8]) -> [u8; 16] {
    let mut m = Md5::new();
    m.update(data);
    m.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(d: [u8; 16]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rfc1321_vectors() {
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(md5(b"message digest")),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            hex(md5(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            )),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn incremental_matches() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 13 + 5) as u8).collect();
        let mut m = Md5::new();
        for p in data.chunks(37) {
            m.update(p);
        }
        assert_eq!(m.finish(), md5(&data));
    }
}
