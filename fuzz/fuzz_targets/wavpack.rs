#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;

// A real file first, so mutations land in block headers and bitstreams that parse.
static FILE: &[u8] = include_bytes!("../../crates/oc-audio/tests/data/wavpack/mono16.wv");

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 20, max_alloc: 1 << 27, max_output: 1 << 26, ..Limits::default() };
    let _ = oc_audio::wavpack::read(data, &limits);
    let mut spliced = FILE.to_vec();
    let at = FILE.len() / 2;
    let n = data.len().min(spliced.len() - at);
    spliced[at..at + n].copy_from_slice(&data[..n]);
    let _ = oc_audio::wavpack::read(&spliced, &limits);
});
