#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let limits = Limits { max_alloc: 1 << 26, max_output: 1 << 26, max_pixels: 1 << 20, ..Limits::default() };
    let opts = oc_svg::Options { width: Some(96), ..Default::default() };
    let _ = oc_svg::render(text, &opts, &limits);
});
