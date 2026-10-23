//! Decode a JPEG XL file and write the samples as raw interleaved 8/16-bit data.
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read input");
    match oc_jxl::decode(&data, &oc_core::Limits::default()) {
        Ok((pic, notes)) => {
            eprintln!("{}x{} ch={} sixteen={}", pic.width, pic.height, pic.channels, pic.sixteen);
            for n in notes.iter() {
                eprintln!("note: {} {}", n.code, n.message);
            }
            let mut out = Vec::new();
            for &s in &pic.samples {
                if pic.sixteen {
                    out.extend_from_slice(&s.to_le_bytes());
                } else {
                    out.push(s as u8);
                }
            }
            std::fs::File::create(&args[2]).unwrap().write_all(&out).unwrap();
        }
        Err(e) => {
            eprintln!("error: {e:?}");
            std::process::exit(1);
        }
    }
}
