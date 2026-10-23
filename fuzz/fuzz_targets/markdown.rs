#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let doc = oc_document::markdown::read(text, &oc_document::markdown::ReadOptions { github: true });
    let _ = oc_document::html_out::fragment(&doc);
    let id = |s: &str| s.to_string();
    let _ = oc_document::md_out::write(&doc, &oc_document::md_out::WriteOptions { image_src: &id });
    let _ = oc_document::text_io::write(&doc);
});
