//! Streams encoded by libtheora must decode to exactly the frames ffmpeg's decoder produces
//! (`tools/make_theora_fixtures.py` makes both; the `.md5` file has one MD5 per frame).
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;
use std::path::Path;

fn hex(d: [u8; 16]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn run(name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let data = std::fs::read(dir.join(format!("{name}.ogv"))).unwrap();
    let want: Vec<String> = std::fs::read_to_string(dir.join(format!("{name}.md5")))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let (streams, bad) = oc_audio::ogg::read(&data).unwrap();
    assert_eq!(bad, 0);
    let packets = &streams[0].packets;
    let h = oc_theora::parse_headers(&packets[0].data, &packets[1].data, &packets[2].data).unwrap();
    let mut dec = oc_theora::decoder(h, &Limits::default()).unwrap();
    let mut got = Vec::new();
    for p in &packets[3..] {
        let f = dec.decode(&p.data).unwrap();
        let mut raw = f.y.clone();
        raw.extend_from_slice(&f.cb);
        raw.extend_from_slice(&f.cr);
        got.push(hex(oc_checksum::md5::md5(&raw)));
    }
    eprintln!("{name}: {} decoded, {} reference", got.len(), want.len());
    assert!(
        got.len() >= want.len(),
        "{name}: {} frames, wanted {}",
        got.len(),
        want.len()
    );
    for (i, w) in want.iter().enumerate() {
        assert_eq!(&got[i], w, "{name}: frame {i} differs");
    }
}

macro_rules! cases {
    ($($t:ident => $n:expr),* $(,)?) => { $( #[test] fn $t() { run($n) } )* };
}

cases! {
    yuv420 => "420_testsrc",
    yuv420_odd_size => "420_odd",
    yuv420_high_quality => "420_hq",
    yuv420_low_quality => "420_lq",
    yuv422 => "422_testsrc",
    yuv444 => "444_testsrc",
    motion_mandelbrot => "420_mandel",
    motion_life => "420_life",
    bitrate_mode => "420_rgbtest",
}
