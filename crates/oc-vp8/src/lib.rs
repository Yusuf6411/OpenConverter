//! VP8 key-frame codec (RFC 6386), the intra half of VP8 used by lossy WebP.
#![forbid(unsafe_code)]
pub mod bool;
pub mod dec;
pub mod enc;
pub mod filter;
pub mod predict;
pub mod recon;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use dec::{decode, dimensions, Frame};
pub use enc::{encode, Params};
