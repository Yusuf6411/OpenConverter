#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::chunks_exact_to_as_chunks
)]
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
        eprintln!(
            "{name}: len {} vs {}, snr {snr:.1} dB, worst {worst}",
            got.len(),
            want.len()
        );
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

fn test_audio(ch: u16, secs: f32) -> oc_audio::Audio {
    let n = (48000.0 * secs) as usize;
    let mut v = Vec::with_capacity(n * usize::from(ch));
    let mut seed = 12345u32;
    for i in 0..n {
        let t = i as f32 / 48000.0;
        for c in 0..ch {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / 16_777_216.0 - 0.5;
            let tone =
                (2.0 * std::f32::consts::PI * (440.0 + 110.0 * f32::from(c)) * t).sin() * 0.4;
            let tone2 =
                (2.0 * std::f32::consts::PI * 3000.0 * t).sin() * 0.1 * (1.0 + (t * 3.0).sin());
            v.push(tone + tone2 + noise * 0.05);
        }
    }
    oc_audio::Audio::new(48000, ch, Samples::Float(v))
}

fn snr_db(a: &[f32], b: &[f32]) -> f64 {
    let (mut e, mut s) = (0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        e += f64::from(x - y).powi(2);
        s += f64::from(*x).powi(2);
    }
    10.0 * (s / e.max(1e-30)).log10()
}

#[test]
fn encoder_round_trips_through_our_decoder() {
    for (ch, ms, rate) in [
        (1u16, 20.0, 64_000u32),
        (2, 20.0, 128_000),
        (2, 10.0, 128_000),
        (2, 5.0, 192_000),
        (1, 2.5, 128_000),
    ] {
        let a = test_audio(ch, 1.0);
        let opts = opus::Options {
            bitrate: rate,
            frame_ms: ms,
            mode: opus::RateMode::Cbr,
            ..Default::default()
        };
        let bytes = opus::write(&a, &opts, &mut oc_core::Notes::new()).unwrap();
        let (b, _) = opus::read(&bytes, &Limits::default()).unwrap();
        let (Samples::Float(x), Samples::Float(y)) = (&a.samples, &b.samples) else {
            panic!()
        };
        assert_eq!(x.len(), y.len(), "length ch={ch} ms={ms}");
        let snr = snr_db(x, y);
        eprintln!(
            "ch {ch} {ms} ms {rate} bps: {} bytes, snr {snr:.1} dB",
            bytes.len()
        );
        assert!(snr > 15.0, "snr {snr}");
    }
}

fn ffmpeg_decode(bytes: &[u8], tag: &str) -> Option<Vec<f32>> {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .ok()?;
    let dir = std::env::temp_dir().join(format!("oc_opus_{}_{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("o.opus");
    std::fs::write(&p, bytes).unwrap();
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-c:a", "libopus", "-i"])
        .arg(&p)
        .args(["-f", "f32le", "-"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        out.stdout
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

#[test]
fn libopus_decodes_our_files() {
    for (ch, ms, mode) in [
        (1u16, 20.0, opus::RateMode::Cbr),
        (2, 20.0, opus::RateMode::Vbr),
        (2, 5.0, opus::RateMode::Cvbr),
        (6, 20.0, opus::RateMode::Cbr),
    ] {
        let a = test_audio(ch, 0.6);
        let opts = opus::Options {
            bitrate: 48_000 * u32::from(ch),
            frame_ms: ms,
            mode,
            ..Default::default()
        };
        let bytes = opus::write(&a, &opts, &mut oc_core::Notes::new()).unwrap();
        let Some(theirs) = ffmpeg_decode(&bytes, &format!("{ch}_{ms}")) else {
            return;
        };
        let (b, _) = opus::read(&bytes, &Limits::default()).unwrap();
        let Samples::Float(mine) = &b.samples else {
            panic!()
        };
        assert_eq!(theirs.len(), mine.len(), "length ch={ch}");
        let snr = snr_db(&theirs, mine);
        eprintln!("ch {ch} {ms} ms: libopus vs ours {snr:.1} dB");
        assert!(snr > 40.0, "ch {ch}: {snr}");
    }
}
