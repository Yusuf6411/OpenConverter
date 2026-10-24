#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_text::{Encoding, Policy};

fuzz_target!(|data: &[u8]| {
    let d = oc_text::detect(data);
    for enc in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Utf32Le, Encoding::Utf32Be, Encoding::Windows1252, Encoding::Ascii, Encoding::Latin1] {
        if let Ok((s, _)) = oc_text::decode(data, enc, Policy::Replace) {
            let _ = oc_text::convert_line_endings(&s, oc_text::LineEnding::CrLf);
            let _ = oc_text::encode(&s, enc, Policy::Replace);
        }
    }
    let _ = d;
});
