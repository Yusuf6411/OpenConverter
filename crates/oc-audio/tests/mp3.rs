#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks
)]
use oc_audio::{dsp, mp3, Samples};
use oc_core::Limits;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn matches_reference(name: &str, channels: u16, rate: u32) {
    let bytes = data(name);
    assert!(mp3::sniff(&bytes), "{name} not recognised");
    let (a, _) = mp3::read(&bytes, &Limits::default()).unwrap();
    assert_eq!((a.channels, a.sample_rate), (channels, rate), "{name}");
    let ours = dsp::to_int(&a.samples, 16);
    let reference: Vec<i32> = data(&format!("{name}.pcm"))
        .chunks_exact(2)
        .map(|c| i32::from(i16::from_le_bytes([c[0], c[1]])))
        .collect();
    assert_eq!(ours.len(), reference.len(), "{name}: length");
    let worst = ours
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .max()
        .unwrap_or(0);
    assert!(worst <= 1, "{name}: worst difference {worst} LSB");
}

#[test]
fn lame_files_decode_like_ffmpeg() {
    matches_reference("mp3_stereo_44k.mp3", 2, 44100);
    matches_reference("mp3_mono_22k.mp3", 1, 22050);
    matches_reference("mp3_mono_8k.mp3", 1, 8000);
    matches_reference("mp3_tagged_48k.mp3", 2, 48000);
}

#[test]
fn tags_are_read() {
    let (a, _) = mp3::read(&data("mp3_tagged_48k.mp3"), &Limits::default()).unwrap();
    assert_eq!(a.tags.get("TITLE"), Some("Test Title"));
    assert_eq!(a.tags.get("ARTIST"), Some("Some Artist"));
    assert_eq!(a.tags.get("ALBUM"), Some("An Album"));
    assert_eq!(a.tags.get("TRACKNUMBER"), Some("3/12"));
    assert_eq!(a.tags.get("DATE"), Some("2020"));
    assert_eq!(a.tags.get("GENRE"), Some("Rock"));
    assert!(matches!(a.samples, Samples::Float(_)));
}

#[test]
fn damaged_streams_never_panic() {
    for name in ["mp3_stereo_44k.mp3", "mp3_mono_8k.mp3"] {
        let bytes = data(name);
        let mut s = 777u64;
        for _ in 0..120 {
            let mut b = bytes.clone();
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            for k in 0..(1 + s % 6) as usize {
                let pos = ((s >> (7 * k)) as usize + k * 104_729) % b.len();
                b[pos] ^= 1 << ((s >> 5) % 8);
            }
            if s.is_multiple_of(5) {
                b.truncate(b.len() * (s as usize % 100) / 100);
            }
            let _ = mp3::read(&b, &Limits::default());
        }
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
            let noise = ((s >> 40) as f32 / 16_777_216.0 - 0.5) * 0.01;
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
    for (ch, rate) in [
        (1usize, 44100u32),
        (2, 44100),
        (2, 48000),
        (1, 22050),
        (2, 16000),
        (1, 8000),
    ] {
        let src = synth(ch, rate, 1.5);
        let audio = oc_audio::Audio::new(rate, ch as u16, Samples::Float(src.clone()));
        let mut notes = oc_core::Notes::default();
        let bytes = mp3::write(&audio, &mp3::Options::default(), &mut notes).unwrap();
        let (back, _) = mp3::read(&bytes, &Limits::default()).unwrap();
        assert_eq!((back.channels, back.sample_rate), (ch as u16, rate));
        let Samples::Float(b) = back.samples else {
            panic!()
        };
        eprintln!(
            "{ch}ch {rate}: {} bytes, {} vs {} samples, snr {:.1} dB",
            bytes.len(),
            b.len(),
            src.len(),
            snr_db(&src, &b)
        );
        assert_eq!(b.len(), src.len(), "length for {ch}ch {rate}");
        assert!(snr_db(&src, &b) > 15.0);
    }
}

#[test]
#[ignore]
fn encode_file() {
    let raw = std::fs::read(std::env::var("IN").unwrap()).unwrap();
    let f: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let ch: u16 = std::env::var("CH").map_or(2, |v| v.parse().unwrap());
    let rate: u32 = std::env::var("RATE").map_or(44100, |v| v.parse().unwrap());
    let audio = oc_audio::Audio::new(rate, ch, Samples::Float(f));
    let mut o = mp3::Options::default();
    if let Ok(b) = std::env::var("BR") {
        o.bitrate = b.parse().unwrap();
    }
    if let Ok(q) = std::env::var("VBR") {
        o.vbr = Some(q.parse().unwrap());
    }
    let bytes = mp3::write(&audio, &o, &mut oc_core::Notes::default()).unwrap();
    std::fs::write(std::env::var("OUT").unwrap(), bytes).unwrap();
}

#[test]
fn encoder_edge_cases() {
    let enc = |a: &oc_audio::Audio, o: &mp3::Options| {
        let mut notes = oc_core::Notes::default();
        let bytes = mp3::write(a, o, &mut notes).unwrap();
        (bytes, notes)
    };
    // Nothing, one sample, silence.
    for frames in [0usize, 1, 700, 5000] {
        let a = oc_audio::Audio::new(44100, 2, Samples::Float(vec![0.0; frames * 2]));
        let (bytes, _) = enc(&a, &mp3::Options::default());
        let (back, _) = mp3::read(&bytes, &Limits::default()).unwrap();
        assert_eq!(
            dsp::to_float(&back.samples).len(),
            frames * 2,
            "{frames} frames"
        );
    }
    // Full-scale square wave in both channel layouts and every bit rate mode.
    let loud: Vec<f32> = (0..30000)
        .map(|i| if (i / 40) % 2 == 0 { 1.0 } else { -1.0 })
        .collect();
    for ch in [1u16, 2] {
        for o in [
            mp3::Options {
                bitrate: 32,
                ..Default::default()
            },
            mp3::Options {
                bitrate: 320,
                ..Default::default()
            },
            mp3::Options {
                vbr: Some(0.0),
                ..Default::default()
            },
            mp3::Options {
                vbr: Some(9.0),
                joint_stereo: false,
                ..Default::default()
            },
        ] {
            let a = oc_audio::Audio::new(
                44100,
                ch,
                Samples::Float(loud[..30000 / usize::from(ch) * usize::from(ch)].to_vec()),
            );
            let (bytes, _) = enc(&a, &o);
            let (back, _) = mp3::read(&bytes, &Limits::default()).unwrap();
            assert_eq!(back.channels, ch);
        }
    }
    // A rate MP3 does not have is resampled (with a note); more than two channels are mixed down.
    let a = oc_audio::Audio::new(96000, 6, Samples::Float(synth(6, 96000, 0.3)));
    let (bytes, notes) = enc(&a, &mp3::Options::default());
    let (back, _) = mp3::read(&bytes, &Limits::default()).unwrap();
    assert_eq!((back.channels, back.sample_rate), (2, 48000));
    assert!(
        notes.iter().any(|n| n.code == "mp3.resampled")
            && notes.iter().any(|n| n.code == "mp3.downmix")
    );
}

#[test]
fn tags_survive_an_encode() {
    let mut a = oc_audio::Audio::new(32000, 1, Samples::Float(synth(1, 32000, 0.3)));
    a.tags.add("TITLE", "Tést ♪");
    a.tags.add("ARTIST", "Someone");
    a.tags.add("TRACKNUMBER", "2");
    a.pictures.push(oc_audio::Picture {
        mime: "image/png".into(),
        kind: 3,
        description: "cover".into(),
        data: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
    });
    let bytes = mp3::write(&a, &mp3::Options::default(), &mut oc_core::Notes::default()).unwrap();
    assert!(mp3::sniff(&bytes));
    let (back, _) = mp3::read(&bytes, &Limits::default()).unwrap();
    assert_eq!(back.tags.get("TITLE"), Some("Tést ♪"));
    assert_eq!(back.tags.get("TRACKNUMBER"), Some("2"));
    assert_eq!(back.pictures, a.pictures);
}
