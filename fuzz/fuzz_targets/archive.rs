#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_archive::{format, Context, Format};
use oc_core::{Limits, NoProgress};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_alloc: 1 << 26, max_output: 1 << 25, max_entries: 256, ..Limits::default() };
    for fmt in Format::all() {
        let cx = Context { limits: &limits, progress: &NoProgress, password: Some("pw"), list_only: false };
        let _ = format::read(fmt, data, &cx, Some("fuzz"));
        let cx = Context { limits: &limits, progress: &NoProgress, password: None, list_only: true };
        let _ = format::read(fmt, data, &cx, Some("fuzz"));
    }
});
