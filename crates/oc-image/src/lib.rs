//! Raster image codecs and operations.
//!
//! Decoders always yield an [`Image`]; encoders take one plus validated options and
//! report every lossy step as a note. Currently: Netpbm, QOI (more are added per
//! the plan in docs/MASTER_PROMPT.md §11).
#![forbid(unsafe_code)]

pub mod anim;
mod bc6h;
mod bc6h_layout;
pub mod bc7;
mod bc7_tables;
pub mod bmp;
pub mod ccitt;
mod ccitt_tables;
pub mod dds;
pub mod exr;
pub mod gif;
pub mod hdr;
pub mod icc;
pub mod icns;
pub mod ico;
pub mod image;
pub mod jpeg;
pub mod netpbm;
pub mod ops;
pub mod png;
pub mod psd;
pub mod qoi;
pub mod quant;
pub mod tga;
pub mod tiff;
pub mod webp;

pub use image::{Depth, Image, Layout, Metadata};
