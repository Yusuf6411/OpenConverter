//! Container formats shared by audio and video: ISO base media (MP4, M4A, MOV) and, later,
//! Matroska, Ogg and MPEG-TS. Containers know nothing about codecs.
#![forbid(unsafe_code)]
#![allow(clippy::indexing_slicing, clippy::chunks_exact_to_as_chunks)]
// every offset is bounds-checked against the input before use

pub mod isobmff;
pub mod matroska;
