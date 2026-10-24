#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let n = usize::from(data[0]) % 4 + 1;
    let (a, rest) = data[1..].split_at((data.len() - 1) / 4 * n.min(3));
    let (b, c) = rest.split_at(rest.len() / 2);
    let _ = oc_compress::bcj::bcj2_decode(a, b, c, rest, usize::from(data[0]) * 64);
});
