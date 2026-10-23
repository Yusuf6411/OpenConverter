//! Theora I video decoder, written from the Theora specification (version 1.1, 2011).
//!
//! The decoder follows the specification step by step, including the exact integer inverse
//! DCT, so its output is bit-identical to libtheora's. Feed it the three header packets, then
//! data packets in order; each gives one [`Frame`] (an empty packet repeats the previous one).
//! Encoding is not provided.
#![forbid(unsafe_code)]
#![allow(clippy::indexing_slicing)] // block, plane and table indices come from the checked layout
#![allow(clippy::needless_range_loop)] // loops index several parallel arrays by block number

mod bits;
mod decode;
mod header;
mod idct;
mod layout;

pub use decode::{Decoder, Frame};
pub use header::{is_identification, parse_comments, parse_identification, Comments, Info, Setup};

use oc_core::{Limits, Result};

/// The three headers of a stream, parsed.
pub struct Headers {
    pub info: Info,
    pub comments: Comments,
    pub setup: Setup,
}

/// Parse the identification, comment and setup packets (in that order).
pub fn parse_headers(id: &[u8], comment: &[u8], setup: &[u8]) -> Result<Headers> {
    Ok(Headers {
        info: header::parse_identification(id)?,
        comments: header::parse_comments(comment).unwrap_or_default(),
        setup: header::parse_setup(setup)?,
    })
}

/// A decoder ready for the first data packet.
pub fn decoder(h: Headers, limits: &Limits) -> Result<Decoder> {
    Decoder::new(h.info, h.setup, limits)
}
