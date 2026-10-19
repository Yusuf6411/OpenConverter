//! Tabular data: a table model plus RFC 4180 CSV/TSV and flat JSON readers and writers.
//!
//! Every cell is text. JSON numbers keep their written form, `true`/`false` become
//! `"true"`/`"false"` and `null` becomes an empty cell; [`Table::coerced_scalars`] records
//! that this happened so callers can say so.
#![forbid(unsafe_code)]

pub mod delimited;
pub mod json;
mod table;

pub use table::{Table, TableLimits};
