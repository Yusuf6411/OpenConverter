//! Opus encoder (placeholder until written).
use crate::Audio;
use oc_core::{Notes, Result};

#[derive(Debug, Clone)]
pub struct Options {
    pub bitrate: u32,
}
impl Default for Options {
    fn default() -> Self {
        Self { bitrate: 96_000 }
    }
}
pub fn write(_audio: &Audio, _opts: &Options, _notes: &mut Notes) -> Result<Vec<u8>> {
    Err(oc_core::Error::new(oc_core::ErrorCode::UnsupportedVariant, "not yet"))
}
