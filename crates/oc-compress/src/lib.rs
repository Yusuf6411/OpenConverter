//! Compression codecs written for OpenConverter: DEFLATE, zlib, gzip, LZW, PackBits.
#![forbid(unsafe_code)]

pub mod deflate;
pub mod gzip;
pub mod inflate;
pub mod lzw;
pub mod packbits;
pub mod zlib;
