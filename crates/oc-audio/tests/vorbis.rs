#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks
)]
use oc_audio::{dsp, vorbis, Samples};
use oc_core::Limits;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Decode a fixture and compare with the s16le PCM ffmpeg's own decoder produced.
fn matches_reference(name: &str, channels: u16, rate: u32) {
    let (a, _) = vorbis::read(&data(name), &Limits::default()).unwrap();
    assert_eq!((a.channels, a.sample_rate), (channels, rate), "{name}");
    let ours = dsp::to_int(&a.samples, 16);
    let reference: Vec<i32> = data(&format!("{name}.pcm"))
        .chunks_exact(2)
        .map(|c| i32::from(i16::from_le_bytes([c[0], c[1]])))
        .collect();
    // We trim to the final granule position as the specification says; ffmpeg's command-line
    // path sometimes keeps up to one block more, so only the common part is compared.
    assert!(
        ours.len() <= reference.len()
            && reference.len() - ours.len() < 4096 * usize::from(channels),
        "{name}: length {} vs {}",
        ours.len(),
        reference.len()
    );
    let worst = ours
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .max()
        .unwrap_or(0);
    assert!(worst <= 1, "{name}: worst difference {worst} LSB");
}

#[test]
fn libvorbis_files_decode_like_ffmpeg() {
    matches_reference("vorbis_stereo_q5.ogg", 2, 44100);
    matches_reference("vorbis_mono_q2.ogg", 1, 22050);
    matches_reference("vorbis_51_q4.ogg", 6, 48000);
    matches_reference("vorbis_stereo_qm1.ogg", 2, 44100);
}

#[test]
fn tags_are_read() {
    // ffmpeg writes an ENCODER tag.
    let (a, _) = vorbis::read(&data("vorbis_stereo_q5.ogg"), &Limits::default()).unwrap();
    assert!(a.tags.get("ENCODER").is_some(), "{:?}", a.tags);
    assert!(matches!(a.samples, Samples::Float(_)));
}

#[test]
fn damaged_streams_never_panic() {
    let bytes = data("vorbis_stereo_q5.ogg");
    let mut s = 4242u64;
    for _ in 0..800 {
        let mut b = bytes.clone();
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        for k in 0..(1 + s % 5) as usize {
            let pos = ((s >> (7 * k)) as usize + k * 104_729) % b.len();
            b[pos] ^= 1 << ((s >> 5) % 8);
        }
        if s.is_multiple_of(7) {
            b.truncate(b.len() * (s as usize % 100) / 100);
        }
        let _ = vorbis::read(&b, &Limits::default());
    }
}

fn synth(channels: usize, rate: u32, secs: f32) -> Vec<f32> {
    let n = (rate as f32 * secs) as usize;
    let mut s = 99u64;
    let mut out = Vec::with_capacity(n * channels);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        for c in 0..channels {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let noise = ((s >> 40) as f32 / 16_777_216.0 - 0.5) * 0.02;
            let f = 440.0 * (1.0 + c as f32 * 0.5);
            out.push(
                0.4 * (2.0 * std::f32::consts::PI * f * t).sin()
                    + 0.15 * (2.0 * std::f32::consts::PI * 3000.0 * t).sin()
                    + noise,
            );
        }
    }
    out
}

fn snr_db(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (mut sig, mut err) = (0f64, 0f64);
    for i in 0..n {
        sig += f64::from(a[i]).powi(2);
        err += f64::from(a[i] - b[i]).powi(2);
    }
    (10.0 * (sig / err.max(1e-20)).log10()) as f32
}

#[test]
fn encoder_round_trips() {
    for (ch, rate) in [(1usize, 44100u32), (2, 44100), (2, 48000), (6, 48000)] {
        let src = synth(ch, rate, 1.5);
        let audio = oc_audio::Audio::new(rate, ch as u16, Samples::Float(src.clone()));
        let mut notes = oc_core::Notes::default();
        let ogg = vorbis::write(&audio, &vorbis::Options::default(), &mut notes).unwrap();
        let (back, _) = vorbis::read(&ogg, &Limits::default()).unwrap();
        assert_eq!((back.channels, back.sample_rate), (ch as u16, rate));
        let Samples::Float(b) = back.samples else {
            panic!()
        };
        assert_eq!(b.len(), src.len(), "length for {ch}ch");
        let snr = snr_db(&src, &b);
        eprintln!("{ch}ch {rate}: {} bytes, snr {snr:.1} dB", ogg.len());
        assert!(snr > 12.0, "{ch}ch snr {snr}");
    }
}

#[test]
fn encoder_edge_cases() {
    for (rate, ch, frames) in [
        (44100u32, 2u16, 0usize),
        (44100, 2, 1),
        (8000, 1, 5000),
        (96000, 2, 3000),
        (44100, 3, 4000),
    ] {
        let silent = oc_audio::Audio::new(
            rate,
            ch,
            Samples::Float(vec![0.0; frames * usize::from(ch)]),
        );
        let ogg = vorbis::write(
            &silent,
            &vorbis::Options::default(),
            &mut oc_core::Notes::default(),
        )
        .unwrap();
        let (back, _) = vorbis::read(&ogg, &Limits::default()).unwrap();
        assert_eq!(
            dsp::to_float(&back.samples).len(),
            frames * usize::from(ch),
            "{rate} {ch} {frames}"
        );
    }
    // Loud full-scale square wave must not overflow any book.
    let loud: Vec<f32> = (0..20000)
        .map(|i| if (i / 50) % 2 == 0 { 1.0 } else { -1.0 })
        .collect();
    for q in [-1.0f32, 5.0, 10.0] {
        let a = oc_audio::Audio::new(44100, 2, Samples::Float(loud.clone()));
        let o = vorbis::Options {
            quality: q,
            ..Default::default()
        };
        let ogg = vorbis::write(&a, &o, &mut oc_core::Notes::default()).unwrap();
        vorbis::read(&ogg, &Limits::default()).unwrap();
    }
}
