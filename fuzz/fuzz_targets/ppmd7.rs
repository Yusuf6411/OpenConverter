#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let Some((head, body)) = data.split_first_chunk::<2>() else { return };
    // Small models keep each run fast while still reaching the restart and gluing paths.
    let order = 2 + head[0] % 31;
    let mem = 2048u32 << (head[1] % 6);
    let mut props = vec![order];
    props.extend_from_slice(&mem.to_le_bytes());
    let limits = Limits { max_alloc: 1 << 26, max_output: 1 << 24, ..Limits::default() };
    let _ = oc_compress::ppmd7::decode(&props, body, 1 << 14, &limits, &NoProgress);
});
