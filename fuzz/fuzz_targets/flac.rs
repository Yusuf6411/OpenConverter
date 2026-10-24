#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 20, max_alloc: 1 << 27, max_output: 1 << 26, ..Limits::default() };
    if let Ok((a, _)) = oc_audio::flac::read(data, &limits) {
        // What was read must write and read back to the same samples.
        let mut notes = oc_core::Notes::new();
        if let Ok(bytes) = oc_audio::flac::write(&a, &oc_audio::flac::Options::default(), &mut notes) {
            let (b, _) = oc_audio::flac::read(&bytes, &limits).expect("our own output must decode");
            assert_eq!(a.samples, b.samples);
        }
    }
});
