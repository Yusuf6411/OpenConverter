#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;
use oc_xml::{Event, Reader};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let mut r = Reader::new(text, &Limits::default());
    // Every call must advance or end; bound the loop so a bug shows up as a failure.
    for _ in 0..(text.len() * 2 + 16) {
        match r.read_event() {
            Ok(Event::Eof) | Err(_) => return,
            Ok(_) => {}
        }
    }
    panic!("XML reader did not terminate");
});
