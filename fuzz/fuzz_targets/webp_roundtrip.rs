#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};
use oc_image::{Depth, Image, Layout};

// Arbitrary pixels must survive our lossless encoder and decoder unchanged.
fuzz_target!(|data: &[u8]| {
    let [effort, w, h, rest @ ..] = data else { return };
    let (w, h) = (usize::from(*w % 48) + 1, usize::from(*h % 48) + 1);
    let layout = if rest.first().is_some_and(|b| b & 1 == 1) { Layout::Rgba } else { Layout::Rgb };
    let ch = layout.channels();
    // Repeat the input so that short inputs still fill the image and produce matches.
    let mut px = Vec::with_capacity(w * h * ch);
    let mut it = rest.iter().cycle();
    for _ in 0..w * h * ch {
        px.push(it.next().copied().unwrap_or(0));
    }
    let Ok(img) = Image::new(w as u32, h as u32, layout, Depth::Eight, px) else { return };
    let opts = oc_image::webp::EncodeOptions { effort: effort % 7, clean_transparent: false, quality: None };
    let (bytes, _) = oc_image::webp::encode(&img, &opts).expect("encode");
    let (back, _) = oc_image::webp::decode(&bytes, &Limits::default(), &NoProgress).expect("own output decodes");
    let (a, _) = oc_image::ops::convert_layout(&back, Layout::Rgba, [0, 0, 0]);
    let (b, _) = oc_image::ops::convert_layout(&img, Layout::Rgba, [0, 0, 0]);
    assert_eq!(a.data, b.data);
});
