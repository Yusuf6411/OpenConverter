#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let c = oc_compress::brotli::compress(data, 5);
    // What we write must read back; arbitrary input must never panic.
    assert_eq!(
        oc_compress::brotli::decompress(&c, &Limits::default(), &NoProgress).ok().as_deref(),
        Some(data)
    );
    let _ = oc_compress::brotli::decompress(data, &Limits::default(), &NoProgress);
    let _ = oc_font::woff2::woff2_to_sfnt(data, 0, &Limits::default());
});
