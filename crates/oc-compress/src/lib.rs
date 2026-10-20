//! Compression codecs written for OpenConverter: DEFLATE, zlib, gzip, LZW, PackBits.
#![forbid(unsafe_code)]

pub mod bcj;
pub mod bzip2;
pub mod compress_z;
pub mod deflate;
pub mod gzip;
pub mod inflate;
pub mod lz4;
pub mod lzma;
pub mod lzw;
pub mod lzx;
pub(crate) mod msb;
pub mod packbits;
pub mod ppmd7;
pub(crate) mod sais;
pub mod xz;
pub mod zlib;
pub mod zstd;
