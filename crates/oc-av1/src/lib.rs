//! A from-scratch AV1 video decoder written against the AV1 Bitstream & Decoding Process Specification.
#![forbid(unsafe_code)]
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity,
    // The code follows the specification's pseudo-code closely, which these lints would reshape.
    clippy::needless_late_init,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::if_same_then_else,
    clippy::let_and_return,
    clippy::unnecessary_cast,
    clippy::manual_memcpy,
    clippy::manual_div_ceil,
    clippy::manual_clamp,
    clippy::manual_swap,
    dead_code
)]

mod bits;
mod block;
mod cdf;
mod consts;
mod dec;
mod decoder;
mod frame;
mod grain;
mod hdr;
mod inter;
mod inter_syntax;
mod intra;
mod mvpred;
mod postfilter;
mod recon;
mod resid;
mod sym;
mod tables;

pub use decoder::{Decoder, Picture};
