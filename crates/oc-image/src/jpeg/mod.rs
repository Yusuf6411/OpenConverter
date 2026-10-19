//! JPEG (ITU T.81): decoder and baseline encoder.
mod arith_table;
mod dct;
pub mod decode;
pub mod encode;
mod tables;
mod transform;
mod trellis;

pub use decode::{decode, decode_tiff, sniff};
pub use encode::{encode, EncodeOptions, Subsampling};
pub use transform::{apply as transform, Op, Transform};
