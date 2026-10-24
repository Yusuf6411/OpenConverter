//! Bounded byte readers, bit readers and writers. Everything is checked: malformed
//! input yields an `Error`, never a panic (MASTER_PROMPT §9.2).
#![forbid(unsafe_code)]

mod bits;
mod reader;

pub use bits::{BitOrder, BitReader, BitWriter};
pub use reader::ByteReader;
