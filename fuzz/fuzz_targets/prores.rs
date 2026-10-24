#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;

// A real frame whose header the fuzzer then mangles, plus arbitrary bytes on their own.
static FRAME: &[u8] = include_bytes!("../../crates/oc-prores/tests/data/prof2.prores");

fuzz_target!(|data: &[u8]| {
    let limits = Limits {
        max_pixels: 1 << 20,
        ..Limits::default()
    };
    let _ = oc_prores::decode(data, &limits);
    let n = u32::from_be_bytes([FRAME[0], FRAME[1], FRAME[2], FRAME[3]]) as usize;
    let mut f = FRAME[..n].to_vec();
    for (i, b) in data.iter().take(64).enumerate() {
        let at = 8 + (i * 37) % (n - 8);
        f[at] ^= *b;
    }
    let _ = oc_prores::decode(&f, &limits);
    let _ = oc_prores::info(data);
});
