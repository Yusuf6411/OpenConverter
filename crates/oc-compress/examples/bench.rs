use std::time::Instant;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(path).unwrap();
    let t = Instant::now();
    let z = oc_compress::bzip2::compress(&data, 9);
    let tc = t.elapsed();
    let t = Instant::now();
    let back = oc_compress::bzip2::decompress(&z, &oc_core::Limits::default(), &oc_core::NoProgress).unwrap();
    println!("in {} out {} comp {:?} decomp {:?} ok {}", data.len(), z.len(), tc, t.elapsed(), back == data);
}
