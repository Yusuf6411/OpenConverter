//! JPEG (ITU T.81): decoder and baseline encoder.
mod dct;
pub mod decode;
pub mod encode;
mod tables;

pub use decode::{decode, sniff};
pub use encode::{encode, EncodeOptions, Subsampling};
