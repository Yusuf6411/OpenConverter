#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;

// A real key frame and inter frame first, so inter prediction has references to read.
static STREAM: &[u8] = include_bytes!("../../crates/oc-vp9/tests/data/inter.ivf");

fuzz_target!(|data: &[u8]| {
    let limits = Limits {
        max_pixels: 1 << 22,
        ..Limits::default()
    };
    let mut dec = oc_vp9::Decoder::new(&limits);
    // IVF: 32 byte header, then 12 byte frame headers.
    if let Some(rest) = STREAM.get(32..) {
        let size = rest
            .get(..4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .unwrap_or(0);
        if let Some(frame) = rest.get(12..12 + size) {
            let _ = dec.decode(frame);
        }
    }
    let _ = dec.decode(data);
    let _ = oc_vp9::info(data);
});
