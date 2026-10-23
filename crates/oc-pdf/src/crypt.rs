//! The standard security handler (revisions 2 to 6): RC4 and AES decryption with the empty
//! user password or one the caller supplies. The primitives come from RustCrypto.
use crate::doc::Pdf;
use crate::object::{Dict, Obj};
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use md5::{Digest, Md5};
use oc_core::{Error, ErrorCode, Result};
use rc4::{consts::U16, StreamCipher};
use sha2::{Sha256, Sha384, Sha512};
use std::sync::Arc;

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80,
    0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Method {
    None,
    Rc4,
    AesV2,
    AesV3,
}

#[derive(Debug, Clone)]
pub struct Crypt {
    v: i64,
    r: i64,
    o: Vec<u8>,
    u: Vec<u8>,
    oe: Vec<u8>,
    ue: Vec<u8>,
    p: i32,
    id0: Vec<u8>,
    key_len: usize,
    encrypt_metadata: bool,
    stm: Method,
    strm: Method,
    key: Vec<u8>,
    pub authenticated: bool,
    pub encrypt_obj: Option<u32>,
}

fn md5(parts: &[&[u8]]) -> [u8; 16] {
    let mut h = Md5::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    macro_rules! go {
        ($n:ty) => {{
            let k = GenericArray::from_slice(key);
            let mut c = rc4::Rc4::<$n>::new(k);
            c.apply_keystream(&mut out);
        }};
    }
    match key.len() {
        5 => go!(rc4::consts::U5),
        6 => go!(rc4::consts::U6),
        7 => go!(rc4::consts::U7),
        8 => go!(rc4::consts::U8),
        9 => go!(rc4::consts::U9),
        10 => go!(rc4::consts::U10),
        11 => go!(rc4::consts::U11),
        12 => go!(rc4::consts::U12),
        13 => go!(rc4::consts::U13),
        14 => go!(rc4::consts::U14),
        15 => go!(rc4::consts::U15),
        16 => go!(U16),
        _ => {}
    }
    out
}

fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8], unpad: bool) -> Option<Vec<u8>> {
    if iv.len() != 16 || data.len() % 16 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    macro_rules! run {
        ($c:expr) => {{
            let c = $c;
            for blk in data.chunks_exact(16) {
                let mut b = GenericArray::clone_from_slice(blk);
                c.decrypt_block(&mut b);
                for (x, p) in b.iter_mut().zip(prev.iter()) {
                    *x ^= *p;
                }
                out.extend_from_slice(&b);
                prev.copy_from_slice(blk);
            }
        }};
    }
    match key.len() {
        16 => run!(aes::Aes128::new_from_slice(key).ok()?),
        32 => run!(aes::Aes256::new_from_slice(key).ok()?),
        _ => return None,
    }
    if unpad {
        let n = *out.last()? as usize;
        if n == 0 || n > 16 || n > out.len() {
            return None;
        }
        out.truncate(out.len() - n);
    }
    Some(out)
}

fn aes128_cbc_encrypt_nopad(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let c = aes::Aes128::new_from_slice(key).ok()?;
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv.get(..16)?);
    let mut out = Vec::with_capacity(data.len());
    for blk in data.chunks_exact(16) {
        let mut b = GenericArray::clone_from_slice(blk);
        for (x, p) in b.iter_mut().zip(prev.iter()) {
            *x ^= *p;
        }
        c.encrypt_block(&mut b);
        prev.copy_from_slice(&b);
        out.extend_from_slice(&b);
    }
    Some(out)
}

/// PDF 2.0 hash (algorithm 2.B) for revision 6.
fn hash_r6(password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut k: Vec<u8> = {
        let mut h = Sha256::new();
        h.update(password);
        h.update(salt);
        h.update(udata);
        h.finalize().to_vec()
    };
    let mut i = 0;
    loop {
        let mut k1 = Vec::new();
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let e = aes128_cbc_encrypt_nopad(k.get(..16).unwrap_or_default(), k.get(16..32).unwrap_or_default(), &k1).unwrap_or_default();
        let sum: u32 = e.iter().take(16).map(|b| u32::from(*b)).sum();
        k = match sum % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        i += 1;
        if i >= 64 && e.last().is_some_and(|l| usize::from(*l) <= i - 32) {
            break;
        }
        if i > 600 {
            break;
        }
    }
    k.truncate(32);
    k
}

impl Crypt {
    pub fn new(pdf: &Pdf, d: &Dict, id0: &[u8], enc_num: Option<u32>) -> Result<Crypt> {
        if pdf.dget_name(d, "Filter").as_deref() != Some("Standard") {
            return Err(Error::new(ErrorCode::UnsupportedVariant, "the PDF uses a security handler other than the standard one"));
        }
        let v = pdf.dget_i64(d, "V").unwrap_or(0);
        let r = pdf.dget_i64(d, "R").unwrap_or(2);
        let bytes = |k: &str| pdf.dget(d, k).as_str().map(<[u8]>::to_vec).unwrap_or_default();
        let mut key_len = (pdf.dget_i64(d, "Length").unwrap_or(40) / 8).clamp(5, 32) as usize;
        let (mut stm, mut strm) = (Method::Rc4, Method::Rc4);
        if v >= 4 {
            let cf = pdf.dget_dict(d, "CF").unwrap_or_default();
            let method = |name: Option<String>| -> Method {
                let Some(n) = name else { return Method::None };
                if n == "Identity" {
                    return Method::None;
                }
                let f = pdf.dget_dict(&cf, &n).unwrap_or_default();
                match pdf.dget_name(&f, "CFM").as_deref() {
                    Some("V2") => Method::Rc4,
                    Some("AESV2") => Method::AesV2,
                    Some("AESV3") => Method::AesV3,
                    _ => Method::None,
                }
            };
            stm = method(pdf.dget_name(d, "StmF"));
            strm = method(pdf.dget_name(d, "StrF"));
            if v == 4 {
                key_len = 16;
            }
        }
        if v == 5 {
            key_len = 32;
        }
        if v == 1 {
            key_len = 5;
        }
        let mut c = Crypt {
            v,
            r,
            o: bytes("O"),
            u: bytes("U"),
            oe: bytes("OE"),
            ue: bytes("UE"),
            p: pdf.dget_i64(d, "P").unwrap_or(-1) as i32,
            id0: id0.to_vec(),
            key_len,
            encrypt_metadata: pdf.dget(d, "EncryptMetadata").as_bool().unwrap_or(true),
            stm,
            strm,
            key: Vec::new(),
            authenticated: false,
            encrypt_obj: enc_num,
        };
        if !(1..=6).contains(&r) || !(1..=5).contains(&v) {
            return Err(Error::new(ErrorCode::UnsupportedVariant, format!("encryption revision {r} (V {v}) is not supported")));
        }
        c.authenticate(b"");
        Ok(c)
    }

    fn pad(pw: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        let n = pw.len().min(32);
        out[..n].copy_from_slice(&pw[..n]);
        out[n..].copy_from_slice(&PAD[..32 - n]);
        out
    }

    fn file_key_r234(&self, pw: &[u8]) -> Vec<u8> {
        let padded = Self::pad(pw);
        let p = (self.p as u32).to_le_bytes();
        let extra: &[u8] = if self.r >= 4 && !self.encrypt_metadata { &[0xFF; 4] } else { &[] };
        let mut h = md5(&[&padded, &self.o, &p, &self.id0, extra]);
        let n = if self.r == 2 { 5 } else { self.key_len };
        if self.r >= 3 {
            for _ in 0..50 {
                h = md5(&[&h[..n]]);
            }
        }
        h[..n].to_vec()
    }

    fn check_user_r234(&self, key: &[u8]) -> bool {
        if self.r == 2 {
            return rc4(key, &PAD) == self.u;
        }
        let mut x = rc4(key, &md5(&[&PAD, &self.id0]));
        for i in 1..=19u8 {
            let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            x = rc4(&k, &x);
        }
        self.u.get(..16) == x.get(..16)
    }

    /// Try `pw` as the user password and then as the owner password.
    pub fn authenticate(&mut self, pw: &[u8]) -> bool {
        if self.r >= 5 {
            return self.authenticate_r56(pw);
        }
        let k = self.file_key_r234(pw);
        if self.check_user_r234(&k) {
            self.key = k;
            self.authenticated = true;
            return true;
        }
        // owner password: recover the user password from /O
        let mut h = md5(&[&Self::pad(pw)]);
        let n = if self.r == 2 { 5 } else { self.key_len };
        if self.r >= 3 {
            for _ in 0..50 {
                h = md5(&[&h]);
            }
        }
        let okey = &h[..n];
        let mut user_pw = self.o.clone();
        if self.r == 2 {
            user_pw = rc4(okey, &user_pw);
        } else {
            for i in (0..=19u8).rev() {
                let k: Vec<u8> = okey.iter().map(|b| b ^ i).collect();
                user_pw = rc4(&k, &user_pw);
            }
        }
        let k = self.file_key_r234(&user_pw);
        if self.check_user_r234(&k) {
            self.key = k;
            self.authenticated = true;
            return true;
        }
        self.authenticated = false;
        false
    }

    fn authenticate_r56(&mut self, pw: &[u8]) -> bool {
        let pw = &pw[..pw.len().min(127)];
        let hash = |p: &[u8], salt: &[u8], u: &[u8]| -> Vec<u8> {
            if self.r == 5 {
                let mut h = Sha256::new();
                h.update(p);
                h.update(salt);
                h.update(u);
                h.finalize().to_vec()
            } else {
                hash_r6(p, salt, u)
            }
        };
        let (Some(uh), Some(uvs), Some(uks)) = (self.u.get(..32), self.u.get(32..40), self.u.get(40..48)) else {
            return false;
        };
        if hash(pw, uvs, &[]) == uh {
            let ik = hash(pw, uks, &[]);
            if let Some(k) = aes_cbc_decrypt(&ik, &[0u8; 16], self.ue.get(..32).unwrap_or_default(), false) {
                self.key = k;
                self.authenticated = true;
                return true;
            }
        }
        if let (Some(oh), Some(ovs), Some(oks)) = (self.o.get(..32), self.o.get(32..40), self.o.get(40..48)) {
            let u48 = self.u.get(..48).unwrap_or_default();
            if hash(pw, ovs, u48) == oh {
                let ik = hash(pw, oks, u48);
                if let Some(k) = aes_cbc_decrypt(&ik, &[0u8; 16], self.oe.get(..32).unwrap_or_default(), false) {
                    self.key = k;
                    self.authenticated = true;
                    return true;
                }
            }
        }
        self.authenticated = false;
        false
    }

    fn object_key(&self, num: u32, gen: u16, aes: bool) -> Vec<u8> {
        if self.v >= 5 {
            return self.key.clone();
        }
        let n = num.to_le_bytes();
        let g = gen.to_le_bytes();
        let salt: &[u8] = if aes { b"sAlT" } else { &[] };
        let h = md5(&[&self.key, &n[..3], &g[..2], salt]);
        h[..(self.key.len() + 5).min(16)].to_vec()
    }

    fn decrypt_bytes(&self, method: Method, data: &[u8], num: u32, gen: u16) -> Vec<u8> {
        match method {
            Method::None => data.to_vec(),
            Method::Rc4 => rc4(&self.object_key(num, gen, false), data),
            Method::AesV2 | Method::AesV3 => {
                if data.len() < 32 {
                    return Vec::new();
                }
                let key = self.object_key(num, gen, method == Method::AesV2);
                let (iv, body) = data.split_at(16);
                let body = &body[..body.len() / 16 * 16];
                aes_cbc_decrypt(&key, iv, body, true).unwrap_or_default()
            }
        }
    }

    pub fn decrypt_stream(&self, raw: &[u8], num: u32, gen: u16, dict: &Dict) -> Vec<u8> {
        if dict.get("Type").and_then(Obj::as_name) == Some("XRef") {
            return raw.to_vec();
        }
        if !self.encrypt_metadata && dict.get("Type").and_then(Obj::as_name) == Some("Metadata") {
            return raw.to_vec();
        }
        self.decrypt_bytes(self.stm, raw, num, gen)
    }

    pub fn decrypt_strings(&self, o: Obj, num: u32, gen: u16) -> Obj {
        match o {
            Obj::Str(s) => Obj::Str(self.decrypt_bytes(self.strm, &s, num, gen)),
            Obj::Array(a) => Obj::Array(a.into_iter().map(|x| self.decrypt_strings(x, num, gen)).collect()),
            Obj::Dict(d) => Obj::Dict(d.into_iter().map(|(k, v)| (k, self.decrypt_strings(v, num, gen))).collect()),
            Obj::Stream(s) => {
                let mut st = (*s).clone();
                st.dict = st.dict.into_iter().map(|(k, v)| (k, self.decrypt_strings(v, num, gen))).collect();
                Obj::Stream(Arc::new(st))
            }
            o => o,
        }
    }
}
