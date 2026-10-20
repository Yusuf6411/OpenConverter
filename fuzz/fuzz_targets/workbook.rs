#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress, Notes};
use oc_sheet::{ods, xlsx};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_alloc: 1 << 24, max_entries: 1 << 14, ..Limits::default() };
    let mut notes = Notes::new();
    if let Ok(book) = xlsx::read(data, &limits, &NoProgress, true, &mut notes) {
        let opts = xlsx::WriteOptions { detect_types: true, detect_dates: true, style_header: true };
        if let Ok(out) = xlsx::write(&book, &opts, &limits, &NoProgress, &mut notes) {
            let _ = xlsx::read(&out, &limits, &NoProgress, true, &mut notes);
        }
    }
    if let Ok(book) = ods::read(data, &limits, &NoProgress, true, &mut notes) {
        let opts = ods::WriteOptions { detect_types: true, detect_dates: true, style_header: true };
        if let Ok(out) = ods::write(&book, &opts, &limits, &NoProgress, &mut notes) {
            let _ = ods::read(&out, &limits, &NoProgress, true, &mut notes);
        }
    }
});
