#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_image::icc::{convert_samples, Intent, Transform};
use oc_image::Depth;

fuzz_target!(|data: &[u8]| {
    for intent in [Intent::Perceptual, Intent::RelativeColorimetric] {
        if let Ok(t) = Transform::new(data, intent, true) {
            let n = t.channels();
            let px: Vec<u8> = (0..n * 16).map(|i| (i * 37 % 256) as u8).collect();
            let rgb = convert_samples(&t, &px, Depth::Eight);
            assert_eq!(rgb.len(), 16 * 3);
        }
    }
});
