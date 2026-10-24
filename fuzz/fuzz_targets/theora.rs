#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;

// Headers of a real stream, so the fuzzer spends its time in the frame decoder.
static STREAM: &[u8] = include_bytes!("../../crates/oc-theora/tests/data/420_testsrc.ogv");

fuzz_target!(|data: &[u8]| {
    let limits = Limits {
        max_pixels: 1 << 22,
        ..Limits::default()
    };
    if let Ok((streams, _)) = oc_audio::ogg::read(STREAM) {
        let pk = &streams[0].packets;
        if let Ok(h) = oc_theora::parse_headers(&pk[0].data, &pk[1].data, &pk[2].data) {
            if let Ok(mut dec) = oc_theora::decoder(h, &limits) {
                // A key frame first so inter frames have references, then the fuzz input.
                let _ = dec.decode(&pk[3].data);
                let _ = dec.decode(data);
                let _ = dec.decode(data.get(1..).unwrap_or_default());
            }
        }
    }
    // Arbitrary headers and arbitrary Ogg files.
    let third = data.len() / 3;
    let _ = oc_theora::parse_headers(&data[..third], &data[third..2 * third], &data[2 * third..]);
    let _ = oc_video::ogv::read(data, &limits);
});
