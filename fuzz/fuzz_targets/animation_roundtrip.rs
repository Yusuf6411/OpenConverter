#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};
use oc_image::anim::{Animation, Frame};
use oc_image::{Depth, Image, Layout};

// Frames of a few flat colours (so GIF is exact too), drawn from the input bytes.
fuzz_target!(|data: &[u8]| {
    let [w, h, n, loops, rest @ ..] = data else { return };
    let (w, h, n) = (u32::from(*w % 24) + 1, u32::from(*h % 24) + 1, usize::from(*n % 6) + 1);
    let palette: [[u8; 4]; 4] = [[0, 0, 0, 0], [255, 0, 0, 255], [0, 255, 0, 255], [10, 20, 200, 255]];
    let mut anim = Animation::new(w, h);
    anim.loops = u32::from(*loops % 5);
    let mut bytes = rest.iter().cycle();
    for i in 0..n {
        let mut px = Vec::new();
        for _ in 0..w * h {
            let k = usize::from(*bytes.next().unwrap_or(&0) & 3);
            px.extend_from_slice(&palette[k]);
        }
        let image = Image::new(w, h, Layout::Rgba, Depth::Eight, px).unwrap();
        anim.frames.push(Frame { image, delay_ms: 10 * (i as u32 + 1) });
    }
    let limits = Limits::default();
    let check = |back: &Animation| {
        assert_eq!(back.frames.len(), anim.frames.len());
        for (a, b) in back.frames.iter().zip(&anim.frames) {
            for (p, q) in a.image.data.chunks(4).zip(b.image.data.chunks(4)) {
                assert!(p == q || (p[3] == 0 && q[3] == 0));
            }
        }
    };
    let (g, _) = oc_image::gif::encode_animation(&anim, &Default::default()).unwrap();
    check(&oc_image::gif::decode_animation(&g, &limits).unwrap().0);
    let (p, _) = oc_image::png::encode_animation(&anim, &Default::default()).unwrap();
    check(&oc_image::png::decode_animation(&p, &limits, &NoProgress).unwrap().0);
    let (wp, _) = oc_image::webp::encode_animation(&anim, &Default::default()).unwrap();
    check(&oc_image::webp::decode_animation(&wp, &limits, &NoProgress).unwrap().0);
});
