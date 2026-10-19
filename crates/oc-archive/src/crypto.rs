//! Encryption helpers for ZIP (WinZip AES, legacy ZipCrypto) and 7z (AES-256-CBC).
//! Built on RustCrypto's block cipher, HMAC and SHA (MASTER_PROMPT §5.2 exception).
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::{Aes128, Aes192, Aes256};
use hmac::{Hmac, Mac};
use oc_core::{Error, Result};
use sha1::Sha1;

/// PBKDF2 with HMAC-SHA-1 (RFC 8018) as WinZip AES specifies.
pub fn pbkdf2_sha1(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    type H = Hmac<Sha1>;
    let Ok(base) = <H as Mac>::new_from_slice(password) else {
        return;
    };
    for (i, chunk) in out.chunks_mut(20).enumerate() {
        let mut mac = base.clone();
        mac.update(salt);
        mac.update(&(i as u32 + 1).to_be_bytes());
        let mut u = mac.finalize().into_bytes();
        let mut t = u;
        for _ in 1..iterations {
            let mut mac = base.clone();
            mac.update(&u);
            u = mac.finalize().into_bytes();
            for (a, b) in t.iter_mut().zip(u.iter()) {
                *a ^= *b;
            }
        }
        chunk.copy_from_slice(t.get(..chunk.len()).unwrap_or(&[]));
    }
}

pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut out = [0u8; 20];
    if let Ok(mut mac) = <Hmac<Sha1> as Mac>::new_from_slice(key) {
        mac.update(data);
        out.copy_from_slice(&mac.finalize().into_bytes());
    }
    out
}

/// A block cipher over any of the three AES key sizes.
pub enum AesKey {
    K128(Aes128),
    K192(Aes192),
    K256(Aes256),
}

impl AesKey {
    pub fn new(key: &[u8]) -> Result<Self> {
        let bad = || Error::corrupt("invalid AES key length");
        Ok(match key.len() {
            16 => Self::K128(Aes128::new_from_slice(key).map_err(|_| bad())?),
            24 => Self::K192(Aes192::new_from_slice(key).map_err(|_| bad())?),
            32 => Self::K256(Aes256::new_from_slice(key).map_err(|_| bad())?),
            _ => return Err(bad()),
        })
    }
    pub fn encrypt_block(&self, b: &mut [u8; 16]) {
        let g = GenericArray::from_mut_slice(b);
        match self {
            Self::K128(c) => c.encrypt_block(g),
            Self::K192(c) => c.encrypt_block(g),
            Self::K256(c) => c.encrypt_block(g),
        }
    }
    pub fn decrypt_block(&self, b: &mut [u8; 16]) {
        let g = GenericArray::from_mut_slice(b);
        match self {
            Self::K128(c) => c.decrypt_block(g),
            Self::K192(c) => c.decrypt_block(g),
            Self::K256(c) => c.decrypt_block(g),
        }
    }
}

/// WinZip AES-CTR: 128-bit block, little-endian counter starting at 1. Symmetric.
pub fn aes_ctr_le(key: &AesKey, data: &mut [u8]) {
    let mut counter: u128 = 1;
    for chunk in data.chunks_mut(16) {
        let mut block = counter.to_le_bytes();
        key.encrypt_block(&mut block);
        for (d, k) in chunk.iter_mut().zip(block.iter()) {
            *d ^= *k;
        }
        counter = counter.wrapping_add(1);
    }
}

/// AES-CBC without padding (7z pads with zeros itself). `data.len()` must be a multiple of 16.
pub fn cbc_decrypt(key: &AesKey, iv: &[u8; 16], data: &mut [u8]) {
    let mut prev = *iv;
    for chunk in data.chunks_exact_mut(16) {
        let mut cur = [0u8; 16];
        cur.copy_from_slice(chunk);
        let saved = cur;
        key.decrypt_block(&mut cur);
        for (c, p) in cur.iter_mut().zip(prev.iter()) {
            *c ^= *p;
        }
        chunk.copy_from_slice(&cur);
        prev = saved;
    }
}

pub fn cbc_encrypt(key: &AesKey, iv: &[u8; 16], data: &mut [u8]) {
    let mut prev = *iv;
    for chunk in data.chunks_exact_mut(16) {
        let mut cur = [0u8; 16];
        cur.copy_from_slice(chunk);
        for (c, p) in cur.iter_mut().zip(prev.iter()) {
            *c ^= *p;
        }
        key.encrypt_block(&mut cur);
        chunk.copy_from_slice(&cur);
        prev = cur;
    }
}

/// Traditional PKWARE stream cipher.
pub struct ZipCrypto {
    k: [u32; 3],
}

fn crc_table() -> &'static [u32; 256] {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    })
}

fn crc_step(crc: u32, b: u8) -> u32 {
    let idx = usize::from((crc as u8) ^ b);
    crc_table().get(idx).copied().unwrap_or(0) ^ (crc >> 8)
}

impl ZipCrypto {
    pub fn new(password: &[u8]) -> Self {
        let mut z = Self {
            k: [0x1234_5678, 0x2345_6789, 0x3456_7890],
        };
        for &b in password {
            z.update(b);
        }
        z
    }
    fn update(&mut self, c: u8) {
        self.k[0] = crc_step(self.k[0], c);
        self.k[1] = self.k[1]
            .wrapping_add(self.k[0] & 0xFF)
            .wrapping_mul(134_775_813)
            .wrapping_add(1);
        self.k[2] = crc_step(self.k[2], (self.k[1] >> 24) as u8);
    }
    fn stream(&self) -> u8 {
        let t = (self.k[2] | 2) & 0xFFFF;
        ((t.wrapping_mul(t ^ 1)) >> 8) as u8
    }
    pub fn decrypt(&mut self, data: &mut [u8]) {
        for b in data {
            *b ^= self.stream();
            self.update(*b);
        }
    }
    pub fn encrypt(&mut self, data: &mut [u8]) {
        for b in data {
            let plain = *b;
            *b ^= self.stream();
            self.update(plain);
        }
    }
}

/// Fill with unpredictable bytes (salts, IVs) from the OS entropy pool, with a time-based
/// fallback where `/dev/urandom` is missing.
pub fn random_bytes(out: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(out).is_ok() {
            return;
        }
    }
    use sha2::{Digest, Sha256};
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_le_bytes()
        .to_vec();
    seed.extend_from_slice(&std::process::id().to_le_bytes());
    let mut counter = 0u32;
    for chunk in out.chunks_mut(32) {
        let mut h = Sha256::new();
        h.update(&seed);
        h.update(counter.to_le_bytes());
        let d = h.finalize();
        chunk.copy_from_slice(d.get(..chunk.len()).unwrap_or(&[]));
        counter += 1;
    }
}
