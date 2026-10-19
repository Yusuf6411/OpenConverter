#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 18, max_alloc: 1 << 26, max_output: 1 << 25, max_entries: 64, ..Limits::default() };
    let _ = oc_image::gif::decode_animation(data, &limits);
    let _ = oc_image::png::decode_animation(data, &limits, &NoProgress);
    let _ = oc_image::webp::decode_animation(data, &limits, &NoProgress);
});
