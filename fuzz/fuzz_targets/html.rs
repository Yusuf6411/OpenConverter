#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let mut notes = oc_core::Notes::new();
    let doc = oc_document::html_in::read(text, &mut notes, 128);
    let _ = oc_document::html_out::fragment(&doc);
    let id = |s: &str| s.to_string();
    let _ = oc_document::md_out::write(&doc, &oc_document::md_out::WriteOptions { image_src: &id });
});
