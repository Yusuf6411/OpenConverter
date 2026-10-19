#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::NoProgress;

// A raw VP8 key frame: header sizes are validated by the caller in real use, so keep the
// claimed size small here to bound memory.
fuzz_target!(|data: &[u8]| {
    if let Ok((w, h)) = oc_vp8::dimensions(data) {
        if w * h <= 1 << 20 {
            let _ = oc_vp8::decode(data, &NoProgress);
        }
    }
});
