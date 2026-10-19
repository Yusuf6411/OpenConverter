//! Raster image codecs and operations.
//!
//! Decoders always yield an [`Image`]; encoders take one plus validated options and
//! report every lossy step as a note. Currently: Netpbm, QOI (more are added per
//! the plan in docs/MASTER_PROMPT.md §11).
#![forbid(unsafe_code)]

pub mod bmp;
pub mod gif;
pub mod image;
pub mod netpbm;
pub mod ops;
pub mod png;
pub mod qoi;
pub mod quant;
pub mod tga;

pub use image::{Depth, Image, Layout, Metadata};
