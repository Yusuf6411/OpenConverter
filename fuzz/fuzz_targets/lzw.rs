#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_compress::lzw::{decode, Config};
use oc_core::Limits;

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_output: 1 << 26, ..Limits::default() };
    let _ = decode(data, Config::gif(8), &limits);
    let _ = decode(data, Config::gif(2), &limits);
    let _ = decode(data, Config::tiff(), &limits);
});
