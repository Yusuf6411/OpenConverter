//! Resource limits enforced by every decoder (MASTER_PROMPT §16).
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Maximum decoded pixels for one image (default 64 MP stays in-memory; above this tiles).
    pub max_pixels: u64,
    /// Maximum bytes a decoder may allocate for one buffer.
    pub max_alloc: u64,
    /// Maximum total output size of an expansion (decompression bomb guard).
    pub max_output: u64,
    /// Maximum expansion ratio output/input once `ratio_floor` bytes were produced.
    pub max_ratio: u64,
    pub ratio_floor: u64,
    /// Maximum nesting depth for recursive structures.
    pub max_depth: u32,
    /// Maximum number of entries (archive members, XML nodes, PDF objects ...).
    pub max_entries: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pixels: 1 << 30,
            max_alloc: 1 << 31,
            max_output: 1 << 36,
            max_ratio: 1000,
            ratio_floor: 1 << 20,
            max_depth: 128,
            max_entries: 10_000_000,
        }
    }
}

impl Limits {
    /// Check an image size and return the pixel count.
    pub fn check_pixels(&self, width: u32, height: u32) -> Result<u64> {
        let px = u64::from(width) * u64::from(height);
        if px > self.max_pixels {
            return Err(Error::limit(
                "max_pixels",
                format!("{width}x{height} exceeds pixel limit"),
            ));
        }
        Ok(px)
    }

    /// Check that an allocation of `bytes` is allowed.
    pub fn check_alloc(&self, bytes: u64) -> Result<()> {
        if bytes > self.max_alloc {
            return Err(Error::limit(
                "max_alloc",
                format!("allocation of {bytes} bytes refused"),
            ));
        }
        Ok(())
    }

    /// Check running decompression totals against the size and ratio limits.
    pub fn check_expansion(&self, input_bytes: u64, output_bytes: u64) -> Result<()> {
        if output_bytes > self.max_output {
            return Err(Error::limit(
                "max_output",
                "decompressed size exceeds limit",
            ));
        }
        if output_bytes > self.ratio_floor && output_bytes / input_bytes.max(1) > self.max_ratio {
            return Err(Error::limit(
                "max_ratio",
                "compression ratio looks like a bomb",
            ));
        }
        Ok(())
    }

    pub fn check_depth(&self, depth: u32) -> Result<()> {
        if depth > self.max_depth {
            return Err(Error::limit("max_depth", "nesting too deep"));
        }
        Ok(())
    }

    pub fn check_entries(&self, count: u64) -> Result<()> {
        if count > self.max_entries {
            return Err(Error::limit("max_entries", "too many entries"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    #[test]
    fn pixel_limit() {
        let l = Limits {
            max_pixels: 100,
            ..Limits::default()
        };
        assert_eq!(l.check_pixels(10, 10).ok(), Some(100));
        let e = l.check_pixels(11, 10).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::LimitExceeded));
    }

    #[test]
    fn bomb_detected_only_above_floor() {
        let l = Limits::default();
        assert!(l.check_expansion(10, 1000).is_ok());
        assert!(l.check_expansion(1000, 10_000_000_000).is_err());
        assert!(l.check_expansion(1 << 20, 1 << 26).is_ok());
    }

    #[test]
    fn huge_alloc_refused() {
        assert!(Limits::default().check_alloc(u64::MAX).is_err());
    }
}
