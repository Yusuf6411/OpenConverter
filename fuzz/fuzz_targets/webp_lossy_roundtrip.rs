#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};
use oc_image::{Depth, Image, Layout};

// Our lossy encoder must produce files our decoder (and every other) accepts, at any size,
// quality and effort, with or without alpha.
fuzz_target!(|data: &[u8]| {
    let [effort, quality, w, h, rest @ ..] = data else { return };
    let (w, h) = (usize::from(*w % 70) + 1, usize::from(*h % 70) + 1);
    let layout = if rest.first().is_some_and(|b| b & 1 == 1) { Layout::Rgba } else { Layout::Rgb };
    let ch = layout.channels();
    let mut px = Vec::with_capacity(w * h * ch);
    let mut it = rest.iter().cycle();
    for _ in 0..w * h * ch {
        px.push(it.next().copied().unwrap_or(0));
    }
    let Ok(img) = Image::new(w as u32, h as u32, layout, Depth::Eight, px) else { return };
    let opts = oc_image::webp::EncodeOptions {
        effort: effort % 7,
        clean_transparent: false,
        quality: Some(f32::from(*quality % 101)),
    };
    let (bytes, _) = oc_image::webp::encode(&img, &opts).expect("encode");
    let (back, _) = oc_image::webp::decode(&bytes, &Limits::default(), &NoProgress).expect("own output decodes");
    assert_eq!((back.width, back.height), (w as u32, h as u32));
    if layout == Layout::Rgba {
        // Alpha is coded losslessly; a fully opaque picture may come back without the channel.
        let (back, _) = oc_image::ops::convert_layout(&back, Layout::Rgba, [0, 0, 0]);
        let a: Vec<u8> = img.data.chunks_exact(4).map(|c| c[3]).collect();
        let b: Vec<u8> = back.data.chunks_exact(4).map(|c| c[3]).collect();
        assert!(a == b, "alpha changed");
    }
});
