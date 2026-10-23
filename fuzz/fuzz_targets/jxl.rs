#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;

// Real files first, so mutations land in headers and bitstreams that parse: one lossless, one
// VarDCT and one recompressed JPEG with chroma subsampling.
static FILES: [&[u8]; 3] = [
    include_bytes!("../../crates/oc-image/tests/data/jxl/feat_squeeze.jxl"),
    include_bytes!("../../crates/oc-image/tests/data/jxl/feat_progressive_dc.jxl"),
    include_bytes!("../../crates/oc-image/tests/data/jxl/jpeg_small_420.jxl"),
];

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 20, max_alloc: 1 << 27, max_output: 1 << 26, ..Limits::default() };
    let _ = oc_image::jxl::decode(data, &limits);
    for file in FILES {
        let mut spliced = file.to_vec();
        let at = (data.first().copied().unwrap_or(0) as usize * file.len() / 256).min(file.len().saturating_sub(1));
        let n = data.len().saturating_sub(1).min(spliced.len() - at);
        spliced[at..at + n].copy_from_slice(&data[1.min(data.len())..1.min(data.len()) + n]);
        let _ = oc_image::jxl::decode(&spliced, &limits);
    }
});
