#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = oc_core::Limits::default();
    let mut notes = oc_core::Notes::new();
    if let Ok(b) = oc_sheet::sqlite::read(data, &oc_sheet::TableLimits::default(), true, &mut notes) { let _ = oc_sheet::sqlite::write(&b, true, &mut notes); }
});
