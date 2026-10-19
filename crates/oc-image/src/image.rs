//! The in-memory raster image: interleaved samples, 8 or 16 bits per sample.
use oc_core::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layout {
    Gray,
    GrayAlpha,
    Rgb,
    Rgba,
}

impl Layout {
    pub const fn channels(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::GrayAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::GrayAlpha | Self::Rgba)
    }
    pub const fn is_gray(self) -> bool {
        matches!(self, Self::Gray | Self::GrayAlpha)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Eight,
    Sixteen,
}

impl Depth {
    pub const fn bytes(self) -> usize {
        match self {
            Self::Eight => 1,
            Self::Sixteen => 2,
        }
    }
    pub const fn max(self) -> u32 {
        match self {
            Self::Eight => 255,
            Self::Sixteen => 65535,
        }
    }
}

/// Descriptive data that travels with the pixels.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    /// Pixels per inch (horizontal, vertical).
    pub dpi: Option<(f64, f64)>,
    pub icc_profile: Option<Vec<u8>>,
    /// Keyword/value text entries (PNG tEXt and friends).
    pub text: Vec<(String, String)>,
}

/// Straight (non-premultiplied) alpha, row-major, top-down. 16-bit samples are
/// stored big-endian.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub layout: Layout,
    pub depth: Depth,
    pub data: Vec<u8>,
    pub metadata: Metadata,
}

impl Image {
    /// Create an image, checking that `data` has exactly the expected size.
    pub fn new(
        width: u32,
        height: u32,
        layout: Layout,
        depth: Depth,
        data: Vec<u8>,
    ) -> Result<Self> {
        let want = Self::byte_len(width, height, layout, depth)?;
        if data.len() != want {
            return Err(Error::corrupt(format!(
                "pixel buffer has {} bytes, expected {want}",
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            layout,
            depth,
            data,
            metadata: Metadata::default(),
        })
    }

    /// Size in bytes of the pixel buffer, failing on overflow.
    pub fn byte_len(width: u32, height: u32, layout: Layout, depth: Depth) -> Result<usize> {
        (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(layout.channels() * depth.bytes()))
            .ok_or_else(|| Error::limit("max_pixels", "image dimensions overflow"))
    }

    pub fn samples_per_row(&self) -> usize {
        self.width as usize * self.layout.channels()
    }

    pub fn row_bytes(&self) -> usize {
        self.samples_per_row() * self.depth.bytes()
    }

    pub fn row(&self, y: u32) -> &[u8] {
        let n = self.row_bytes();
        self.data
            .get(y as usize * n..(y as usize + 1) * n)
            .unwrap_or(&[])
    }

    /// Sample `i` (interleaved index) as a value in `0..=depth.max()`.
    pub fn sample(&self, i: usize) -> u16 {
        match self.depth {
            Depth::Eight => u16::from(self.data.get(i).copied().unwrap_or(0)),
            Depth::Sixteen => {
                let hi = self.data.get(2 * i).copied().unwrap_or(0);
                let lo = self.data.get(2 * i + 1).copied().unwrap_or(0);
                u16::from_be_bytes([hi, lo])
            }
        }
    }

    /// True if any pixel has alpha below the maximum.
    pub fn has_translucency(&self) -> bool {
        if !self.layout.has_alpha() {
            return false;
        }
        let ch = self.layout.channels();
        let max = self.depth.max() as u16;
        (0..self.width as usize * self.height as usize).any(|p| self.sample(p * ch + ch - 1) != max)
    }

    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }
}

/// Collects samples into a buffer of the right depth.
pub(crate) struct SampleWriter {
    depth: Depth,
    pub data: Vec<u8>,
}

impl SampleWriter {
    pub fn new(depth: Depth, capacity_samples: usize) -> Self {
        Self {
            depth,
            data: Vec::with_capacity(capacity_samples * depth.bytes()),
        }
    }
    pub fn push(&mut self, v: u16) {
        match self.depth {
            Depth::Eight => self.data.push(v as u8),
            Depth::Sixteen => self.data.extend_from_slice(&v.to_be_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_is_checked() {
        assert!(Image::new(2, 2, Layout::Rgb, Depth::Eight, vec![0; 12]).is_ok());
        assert!(Image::new(2, 2, Layout::Rgb, Depth::Eight, vec![0; 11]).is_err());
        assert!(Image::new(u32::MAX, u32::MAX, Layout::Rgba, Depth::Sixteen, vec![]).is_err());
    }

    #[test]
    fn samples_and_translucency() {
        let img = Image::new(
            1,
            2,
            Layout::GrayAlpha,
            Depth::Sixteen,
            vec![1, 2, 255, 255, 0, 0, 0, 5],
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(img.sample(0), 0x0102);
        assert_eq!(img.sample(3), 5);
        assert!(img.has_translucency());
    }
}
