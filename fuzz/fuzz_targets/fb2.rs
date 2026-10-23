#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let limits = oc_core::Limits::default();
    let mut notes = oc_core::Notes::new();
    if let Ok(doc) = oc_document::fb2_in::read(text, &limits, &mut notes) {
        let _ = oc_document::html_out::fragment(&doc);
    }
});
