#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_output: 1 << 26, ..Limits::default() };
    let _ = oc_compress::gzip::decompress(data, &limits, &NoProgress);
});
