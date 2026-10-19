//! Development helper: `codec_tool <codec> <d|c> <level> <in> <out>` for interop checks
//! against other implementations (see tools/interop.py).
use oc_core::{Limits, NoProgress};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (codec, mode, level) = (a[1].as_str(), a[2].as_str(), a[3].parse::<u32>().unwrap_or(6));
    let data = std::fs::read(&a[4]).unwrap();
    let l = Limits::default();
    let out = match (codec, mode) {
        ("bzip2", "d") => oc_compress::bzip2::decompress(&data, &l, &NoProgress).unwrap(),
        ("bzip2", "c") => oc_compress::bzip2::compress(&data, level),
        ("xz", "d") => oc_compress::xz::decompress(&data, &l, &NoProgress).unwrap(),
        ("xz", "c") => oc_compress::xz::compress(&data, level, oc_compress::xz::Check::Crc64),
        ("zstd", "d") => oc_compress::zstd::decompress(&data, &l, &NoProgress).unwrap(),
        ("zstd", "c") => oc_compress::zstd::compress(&data, level as i32, true),
        _ => panic!("unknown codec/mode"),
    };
    std::fs::write(&a[5], out).unwrap();
}
