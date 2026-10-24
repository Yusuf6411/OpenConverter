#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
use oc_core::{Limits, NoProgress};
use std::time::Instant;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[2]).unwrap();
    let level: u32 = a[3].parse().unwrap();
    let t = Instant::now();
    let z = match a[1].as_str() {
        "bzip2" => oc_compress::bzip2::compress(&data, level),
        "xz" => oc_compress::xz::compress(&data, level, oc_compress::xz::Check::Crc64),
        "zstd" => oc_compress::zstd::compress(&data, level as i32, true),
        _ => panic!(),
    };
    let tc = t.elapsed();
    let t = Instant::now();
    let back = match a[1].as_str() {
        "bzip2" => oc_compress::bzip2::decompress(&z, &Limits::default(), &NoProgress).unwrap(),
        "xz" => oc_compress::xz::decompress(&z, &Limits::default(), &NoProgress).unwrap(),
        _ => oc_compress::zstd::decompress(&z, &Limits::default(), &NoProgress).unwrap(),
    };
    let td = t.elapsed();
    let mb = data.len() as f64 / 1e6;
    println!(
        "{} level {level}: {} -> {} ({:.1}%)  enc {:.1} MB/s  dec {:.1} MB/s  ok {}",
        a[1],
        data.len(),
        z.len(),
        100.0 * z.len() as f64 / data.len() as f64,
        mb / tc.as_secs_f64(),
        mb / td.as_secs_f64(),
        back == data
    );
}
