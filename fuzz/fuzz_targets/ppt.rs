#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = oc_core::Limits::default();
    let mut notes = oc_core::Notes::new();
    if let Ok(d) = oc_document::ppt_in::read(data, &limits, &mut notes) { let _ = oc_document::html_out::fragment(&d); }
});
