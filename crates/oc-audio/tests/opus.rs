#![allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]
use oc_audio::{opus, Samples};
use oc_core::Limits;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn reference(name: &str) -> Vec<f32> {
    data(&format!("{name}.pcm"))
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn decode(name: &str) -> Vec<f32> {
    let (a, _) = opus::read(&data(name), &Limits::default()).unwrap();
    match a.samples {
        Samples::Float(v) => v,
        Samples::Int { .. } => panic!("expected float"),
    }
}

#[test]
fn libopus_celt_streams_decode_to_the_same_audio() {
    for name in [
        "opus_celt_mono_20ms.opus",
        "opus_celt_stereo_20ms.opus",
        "opus_celt_stereo_10ms.opus",
        "opus_celt_stereo_5ms.opus",
        "opus_celt_mono_2_5ms.opus",
        "opus_celt_stereo_60ms.opus",
        "opus_celt_stereo_vbr_wb.opus",
    ] {
        let want = reference(name);
        let got = decode(name);
        let n = want.len().min(got.len());
        let (mut err, mut sig) = (0f64, 0f64);
        let mut worst = 0f32;
        for i in 0..n {
            let d = f64::from(got[i] - want[i]);
            err += d * d;
            sig += f64::from(want[i]).powi(2);
            worst = worst.max((got[i] - want[i]).abs());
        }
        let snr = 10.0 * (sig / err.max(1e-30)).log10();
        eprintln!("{name}: len {} vs {}, snr {snr:.1} dB, worst {worst}", got.len(), want.len());
        assert_eq!(got.len(), want.len(), "{name} length");
        assert!(snr > 40.0, "{name}: snr {snr}");
    }
}

#[test]
#[ignore]
fn dump() {
    let name = std::env::var("OPUS_DUMP").unwrap();
    let got = decode(&name);
    let bytes: Vec<u8> = got.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(std::env::var("OPUS_OUT").unwrap(), bytes).unwrap();
}

#[test]
#[ignore]
fn ranges() {
    let name = std::env::var("OPUS_DUMP").unwrap();
    for r in opus::final_ranges(&data(&name)).unwrap() {
        println!("R {r}");
    }
}
