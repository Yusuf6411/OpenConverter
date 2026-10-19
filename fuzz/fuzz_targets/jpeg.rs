#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 20, max_alloc: 1 << 27, max_output: 1 << 26, ..Limits::default() };
    // Whatever decodes must also re-encode, and our own output must decode.
    if let Ok((img, _)) = oc_image::jpeg::decode(data, &limits, &NoProgress) {
        let (bytes, _) = oc_image::jpeg::encode(&img, &Default::default()).expect("encodes");
        oc_image::jpeg::decode(&bytes, &limits, &NoProgress).expect("own output decodes");
    }
});
