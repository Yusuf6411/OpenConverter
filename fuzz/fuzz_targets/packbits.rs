#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = oc_compress::packbits::decode(data, None);
    let _ = oc_compress::packbits::decode(data, Some(4096));
});
