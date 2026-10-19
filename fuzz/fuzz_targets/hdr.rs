#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 20, max_alloc: 1 << 27, max_output: 1 << 26, ..Limits::default() };
    if let Ok((img, _)) = oc_image::hdr::decode(data, &limits) {
        // Whatever was read must write and read back to the same size.
        if let Ok((bytes, _)) = oc_image::hdr::encode(&img) {
            let (back, _) = oc_image::hdr::decode(&bytes, &limits).expect("our own output must decode");
            assert_eq!((back.width, back.height), (img.width, img.height));
        }
    }
});
