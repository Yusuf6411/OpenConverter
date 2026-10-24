#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 20, max_alloc: 1 << 27, max_output: 1 << 26, ..Limits::default() };
    let Ok((img, _)) = oc_image::png::decode(data, &limits, &NoProgress) else { return };
    let (bytes, _) = oc_image::png::encode(&img, &Default::default());
    let (back, _) = oc_image::png::decode(&bytes, &limits, &NoProgress).expect("own output decodes");
    assert_eq!(back.data, img.data);
    let (tga, _) = oc_image::tga::encode(&img, &Default::default());
    let (back, _) = oc_image::tga::decode(&tga, &limits).expect("own TGA decodes");
    let (a, _) = oc_image::ops::convert_layout(&back, oc_image::Layout::Rgba, [0, 0, 0]);
    let (b, _) = oc_image::ops::convert_depth(&img, oc_image::Depth::Eight);
    let (b, _) = oc_image::ops::convert_layout(&b, oc_image::Layout::Rgba, [255, 255, 255]);
    assert_eq!(a.data, b.data, "TGA must be lossless at 8 bits");
});
