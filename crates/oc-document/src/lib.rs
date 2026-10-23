//! Documents: the model, Markdown (CommonMark and GitHub flavour), HTML, and the office and
//! e-book formats built on it.
#![forbid(unsafe_code)]
#![allow(
    clippy::indexing_slicing,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::collapsible_else_if,
    clippy::collapsible_if,
    clippy::needless_range_loop,
    clippy::manual_strip,
    clippy::manual_range_contains,
    clippy::while_let_loop,
    clippy::question_mark
)]

pub mod base64;
pub mod build;
pub mod docx_in;
pub mod docx_out;
pub mod entities;
pub mod html_in;
pub mod odt_in;
pub mod odt_out;
pub mod imgsize;
pub mod html_out;
pub mod markdown;
pub mod md_block;
pub mod md_inline;
pub mod md_out;
pub mod model;
pub mod text_io;
pub mod xml_tree;
pub mod zipio;

pub use model::{Align, Block, Cell, Document, Image, Inline, ListItem, Meta};
