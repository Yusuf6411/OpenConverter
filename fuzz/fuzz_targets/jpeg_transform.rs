#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};
use oc_image::jpeg::{self, Transform};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 20, max_alloc: 1 << 27, max_output: 1 << 26, ..Limits::default() };
    let (&sel, jpeg_data) = match data.split_first() {
        Some(v) => v,
        None => return,
    };
    let mut t = Transform::rotate_flip(u32::from(sel & 3) * 90, sel & 4 != 0, sel & 8 != 0).unwrap();
    if sel & 16 != 0 {
        t.crop = Some([usize::from(sel) % 40, usize::from(sel) % 24, 40, 40]);
    }
    // Whatever transforms must decode afterwards, with the dimensions the transform implies.
    if let Ok((bytes, _)) = jpeg::transform(jpeg_data, &t, &limits, &NoProgress) {
        jpeg::decode(&bytes, &limits, &NoProgress).expect("own output decodes");
    }
});
