#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;
use oc_video::{avi, flv, mkv, mp4, ps, ts, y4m};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_pixels: 1 << 20, max_alloc: 1 << 27, max_output: 1 << 26, ..Limits::default() };
    let _ = avi::read(data, &limits);
    let _ = y4m::read(data, &limits);
    let _ = flv::read(data, &limits);
    let _ = ps::read(data, &limits);
    let _ = mkv::read(data, &limits);
    // Whatever a reader accepts, the writers must take without panicking.
    for media in [mp4::read(data, &limits), ts::read(data, &limits)].into_iter().flatten() {
        let _ = ts::write(&media);
        let _ = mkv::write(&media, false, "fuzz");
        let _ = avi::write(&media);
        let _ = mp4::write(&media, &mp4::Options { mov: false, faststart: true, fragmented: false });
    }
});
