//! PDF: parsing, text extraction, rendering and writing.
#![forbid(unsafe_code)]
#![allow(clippy::indexing_slicing, clippy::chunks_exact_to_as_chunks, clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop)]

pub mod crypt;
pub mod doc;
pub mod filter;
pub mod object;

pub use doc::{PageInfo, Pdf};
pub use object::{Dict, Obj, Stream};
pub mod cmap;
pub mod color;
pub mod function;
pub mod tables;
pub mod font;
pub mod image;
pub mod ops;
pub mod shading;
pub mod interp;
pub mod render;
pub mod text;
pub mod writer;
pub mod edit;
