//! Bounds-checked big-endian reads over a byte slice.

#[derive(Clone, Copy)]
pub struct R<'a> {
    pub d: &'a [u8],
}

impl<'a> R<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Self { d }
    }
    pub fn u8(&self, o: usize) -> Option<u8> {
        self.d.get(o).copied()
    }
    pub fn i8(&self, o: usize) -> Option<i8> {
        self.u8(o).map(|v| v as i8)
    }
    pub fn u16(&self, o: usize) -> Option<u16> {
        let b = self.d.get(o..o.checked_add(2)?)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }
    pub fn i16(&self, o: usize) -> Option<i16> {
        self.u16(o).map(|v| v as i16)
    }
    pub fn u24(&self, o: usize) -> Option<u32> {
        let b = self.d.get(o..o.checked_add(3)?)?;
        Some(u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]))
    }
    pub fn u32(&self, o: usize) -> Option<u32> {
        let b = self.d.get(o..o.checked_add(4)?)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn i32(&self, o: usize) -> Option<i32> {
        self.u32(o).map(|v| v as i32)
    }
    pub fn slice(&self, o: usize, len: usize) -> Option<&'a [u8]> {
        self.d.get(o..o.checked_add(len)?)
    }
    pub fn tag(&self, o: usize) -> Option<[u8; 4]> {
        let b = self.slice(o, 4)?;
        Some([b[0], b[1], b[2], b[3]])
    }
}

pub fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
pub fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}
