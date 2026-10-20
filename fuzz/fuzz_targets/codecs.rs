#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_alloc: 1 << 26, max_output: 1 << 25, max_entries: 256, ..Limits::default() };
    let p = &NoProgress;
    let _ = oc_compress::bzip2::decompress(data, &limits, p);
    let _ = oc_compress::xz::decompress(data, &limits, p);
    let _ = oc_compress::lz4::decompress(data, &limits, p);
    let _ = oc_compress::zstd::decompress(data, &limits, p);
    let _ = oc_compress::compress_z::decompress(data, &limits, p);
    let _ = oc_compress::lzma::decode_alone(data, &limits, p);
    let _ = oc_compress::lzma::decode_lzma2(data, 0, &limits, p);
    let _ = oc_compress::inflate::inflate64(data, &limits, p);
});
