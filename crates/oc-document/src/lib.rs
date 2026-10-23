//! Documents: the model, Markdown (CommonMark and GitHub flavour), HTML, and the office and
//! e-book formats built on it.
#![forbid(unsafe_code)]
#![allow(
    clippy::indexing_slicing,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::collapsible_else_if,
    clippy::collapsible_if,
    clippy::needless_range_loop
)]

pub mod entities;
pub mod html_out;
pub mod markdown;
pub mod md_block;
pub mod md_inline;
pub mod model;

pub use model::{Align, Block, Cell, Document, Image, Inline, ListItem, Meta};
