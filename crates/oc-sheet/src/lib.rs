//! Tabular data: a table model plus RFC 4180 CSV/TSV, flat JSON, XLSX and ODS readers and writers.
//!
//! Every cell is text. JSON numbers keep their written form, `true`/`false` become
//! `"true"`/`"false"` and `null` becomes an empty cell; [`Table::coerced_scalars`] records
//! that this happened so callers can say so.
#![forbid(unsafe_code)]

pub mod book;
pub mod delimited;
pub mod json;
pub mod ods;
pub mod sqlite;
mod table;
pub mod xls;
pub mod xlsx;

pub use book::{Sheet, Workbook};
pub use table::{Table, TableLimits};
