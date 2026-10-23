//! VP8 codec (RFC 6386): key-frame and inter-frame decoding, and the key-frame encoder used by lossy WebP.
#![forbid(unsafe_code)]
pub mod bool;
pub mod dec;
pub mod enc;
pub mod filter;
pub mod inter;
pub mod predict;
pub mod recon;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use dec::{decode, dimensions, Decoder, Frame};
pub use enc::{encode, Params};
