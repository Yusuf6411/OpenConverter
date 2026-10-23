#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Notes;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let mut n = Notes::new();
    let _ = oc_subtitle::sniff(text);
    for parse in [oc_subtitle::srt::parse, oc_subtitle::vtt::parse, oc_subtitle::ass::parse, oc_subtitle::ttml::parse] {
        let mut t = parse(text, &mut n);
        t.tidy(&mut n);
        t.shift(-500, &mut n);
        let _ = oc_subtitle::srt::write(&t, &mut n);
        let _ = oc_subtitle::vtt::write(&t, &mut n);
        let _ = oc_subtitle::ass::write(&t, &mut n);
        let _ = oc_subtitle::ttml::write(&t, &mut n);
    }
});
