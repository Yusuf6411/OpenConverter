#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let level = u32::from(data.first().copied().unwrap_or(6) % 10);
    let body = data.get(1..).unwrap_or(&[]);
    let z = oc_compress::deflate::deflate(body, level);
    let back = oc_compress::inflate::inflate(&z, &Limits::default(), &NoProgress).expect("own output must inflate");
    assert_eq!(back.data, body);
});
