//! PDF: parsing, text extraction, rendering and writing.
#![forbid(unsafe_code)]
#![allow(
    clippy::arc_with_non_send_sync,
    clippy::collapsible_match,
    clippy::should_implement_trait,
    clippy::single_match,
    clippy::while_let_loop,
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop
)]

pub mod crypt;
pub mod doc;
pub mod filter;
pub mod object;

pub use doc::{PageInfo, Pdf};
pub use object::{Dict, Obj, Stream};
pub mod cmap;
pub mod color;
pub mod edit;
pub mod font;
pub mod function;
pub mod image;
pub mod interp;
pub mod ops;
pub mod render;
pub mod shading;
pub mod tables;
pub mod text;
pub mod writer;
