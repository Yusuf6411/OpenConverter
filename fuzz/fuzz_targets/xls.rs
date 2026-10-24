#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = oc_core::Limits::default();
    let mut notes = oc_core::Notes::new();
    let _ = oc_sheet::xls::read(data, &limits, true, &mut notes);
});
