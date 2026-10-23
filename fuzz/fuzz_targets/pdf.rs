#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_pdf::interp::{interpret_page, Settings};
use oc_pdf::render::{render, RenderOptions};

fuzz_target!(|data: &[u8]| {
    let Ok(pdf) = oc_pdf::Pdf::open(data.to_vec()) else { return };
    let limits = oc_core::Limits { max_alloc: 1 << 26, max_output: 1 << 26, max_pixels: 1 << 20, ..oc_core::Limits::default() };
    for info in pdf.pages().iter().take(3) {
        let page = interpret_page(&pdf, info, &Settings { fonts: None, annotations: true });
        let _ = oc_pdf::text::extract_page(&page);
        let _ = render(&page, &RenderOptions { scale: 0.5, background: Some([1.0; 3]) }, &limits);
    }
});
