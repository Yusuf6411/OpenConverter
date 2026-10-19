//! Compression codecs written for OpenConverter: DEFLATE, zlib, gzip, LZW, PackBits.
#![forbid(unsafe_code)]

pub mod bcj;
pub mod compress_z;
pub mod bzip2;
pub mod deflate;
pub mod gzip;
pub mod inflate;
pub mod lz4;
pub mod lzma;
pub mod lzx;
pub mod lzw;
pub(crate) mod sais;
pub(crate) mod msb;
pub mod packbits;
pub mod ppmd7;
pub mod xz;
pub mod zstd;
pub mod zlib;
