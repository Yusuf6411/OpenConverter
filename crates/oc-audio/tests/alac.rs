//! ALAC against ffmpeg's encoder and decoder.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks,
    clippy::approx_constant
)]
use oc_audio::{m4a, Audio, Samples};
use oc_core::{Limits, Notes};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn expected(name: &str, bits: u8) -> Vec<i32> {
    data(&format!("{name}.pcm"))
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> (32 - u32::from(bits)))
        .collect()
}

fn ints(a: &Audio) -> &[i32] {
    match &a.samples {
        Samples::Int { data, .. } => data,
        Samples::Float(_) => panic!("float"),
    }
}

#[test]
fn ffmpeg_files_decode_bit_exact() {
    for (name, ch, bits) in [
        ("alac_stereo_16.m4a", 2, 16),
        ("alac_mono_16.m4a", 1, 16),
        ("alac_stereo_24.m4a", 2, 24),
        ("alac_51_16.m4a", 6, 16),
    ] {
        let f = data(name);
        assert!(m4a::sniff(&f), "{name}");
        let (a, _) = m4a::read(&f, &Limits::default()).unwrap();
        assert_eq!(a.channels, ch, "{name}");
        assert_eq!(a.bits(), bits, "{name}");
        assert_eq!(ints(&a), &expected(name, bits)[..], "{name}");
    }
}

fn signal(frames: usize, ch: usize, bits: u8) -> Audio {
    let mut state = 12345u32;
    let amp = f64::from(1u32 << (bits - 2));
    let mut d = Vec::new();
    for i in 0..frames {
        for c in 0..ch {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = f64::from(state >> 20) / 4096.0 - 0.5;
            let t = i as f64 / 44100.0;
            let v = amp * ((t * 440.0 * (c + 1) as f64 * 6.2831853).sin() * 0.6 + noise * 0.05);
            d.push(v as i32);
        }
    }
    Audio::new(44100, ch as u16, Samples::Int { bits, data: d })
}

#[test]
fn encoder_round_trips() {
    for (frames, ch, bits) in [
        (10_000, 2, 16),
        (4096, 1, 16),
        (4097, 2, 24),
        (9000, 6, 16),
        (3000, 3, 20),
        (5000, 8, 16),
        (7000, 2, 32),
        (1, 2, 16),
        (0, 2, 16),
    ] {
        let a = signal(frames, ch, bits);
        let mut notes = Notes::new();
        let f = m4a::write(&a, &mut notes).unwrap();
        let (b, _) = m4a::read(&f, &Limits::default()).unwrap();
        assert_eq!(b.channels as usize, ch);
        assert_eq!(ints(&b), ints(&a), "{frames}x{ch}x{bits}");
    }
}

#[test]
fn encoder_handles_extremes() {
    let mut a = signal(8192, 2, 16);
    if let Samples::Int { data, .. } = &mut a.samples {
        for (i, v) in data.iter_mut().enumerate() {
            *v = match (i / 2) % 4000 {
                0..=999 => 0,
                1000..=1999 => {
                    if i % 4 < 2 {
                        32767
                    } else {
                        -32768
                    }
                }
                _ => *v,
            };
        }
    }
    let f = m4a::write(&a, &mut Notes::new()).unwrap();
    let (b, _) = m4a::read(&f, &Limits::default()).unwrap();
    assert_eq!(ints(&b), ints(&a));
}

#[test]
fn damaged_streams_never_panic() {
    let good = data("alac_stereo_16.m4a");
    let mut state = 99u32;
    for _ in 0..80 {
        let mut f = good.clone();
        for _ in 0..4 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let at = (state as usize >> 4) % f.len();
            f[at] ^= (state >> 24) as u8 | 1;
        }
        let cut = (state as usize >> 8) % f.len();
        let _ = m4a::read(&f, &Limits::default());
        let _ = m4a::read(&f[..cut], &Limits::default());
    }
}

#[test]
fn ffmpeg_decodes_our_files() {
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_err()
    {
        return;
    }
    let a = signal(20_000, 2, 16);
    let f = m4a::write(&a, &mut Notes::new()).unwrap();
    let dir = std::env::temp_dir().join(format!("oc_alac_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("o.m4a");
    std::fs::write(&p, &f).unwrap();
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&p)
        .args(["-f", "s16le", "-"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: Vec<i32> = out
        .stdout
        .chunks_exact(2)
        .map(|c| i32::from(i16::from_le_bytes([c[0], c[1]])))
        .collect();
    assert_eq!(got, ints(&a));
}

#[test]
fn caf_alac_matches_ffmpeg_and_round_trips() {
    use oc_audio::caf;
    let f = data("alac_stereo_16.caf");
    let (a, _) = caf::read(&f, &Limits::default()).unwrap();
    assert_eq!(ints(&a), &expected("alac_stereo_16.caf", 16)[..]);
    let g = caf::write_alac(&a, &mut Notes::new()).unwrap();
    let (b, _) = caf::read(&g, &Limits::default()).unwrap();
    assert_eq!(ints(&b), ints(&a));
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok()
    {
        let dir = std::env::temp_dir().join(format!("oc_alac_caf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o.caf");
        std::fs::write(&p, &g).unwrap();
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&p)
            .args(["-f", "s16le", "-"])
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let got: Vec<i32> = out
            .stdout
            .chunks_exact(2)
            .map(|c| i32::from(i16::from_le_bytes([c[0], c[1]])))
            .collect();
        assert_eq!(got, ints(&a));
    }
}

#[test]
fn tags_and_cover_survive() {
    use oc_audio::Picture;
    let mut a = signal(5000, 2, 16);
    for (k, v) in [
        ("TITLE", "Tëst Title"),
        ("ARTIST", "Some Artist"),
        ("ALBUM", "An Album"),
        ("TRACKNUMBER", "3/12"),
        ("DISCNUMBER", "1"),
        ("DATE", "2020"),
        ("GENRE", "Rock"),
        ("MOOD", "calm"),
    ] {
        a.tags.add(k, v);
    }
    a.pictures.push(Picture {
        mime: "image/png".into(),
        kind: 3,
        description: String::new(),
        data: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
    });
    let f = m4a::write(&a, &mut Notes::new()).unwrap();
    let (b, _) = m4a::read(&f, &Limits::default()).unwrap();
    assert_eq!(b.tags.get("TITLE"), Some("Tëst Title"));
    assert_eq!(b.tags.get("TRACKNUMBER"), Some("3"));
    assert_eq!(b.tags.get("TRACKTOTAL"), Some("12"));
    assert_eq!(b.tags.get("DISCNUMBER"), Some("1"));
    assert_eq!(b.tags.get("MOOD"), Some("calm"));
    assert_eq!(b.pictures.len(), 1);
    assert_eq!(b.pictures[0].mime, "image/png");
    assert_eq!(b.pictures[0].data, a.pictures[0].data);
    if std::process::Command::new("ffprobe")
        .arg("-version")
        .output()
        .is_ok()
    {
        let dir = std::env::temp_dir().join(format!("oc_alac_tag_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o.m4a");
        std::fs::write(&p, &f).unwrap();
        let out = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format_tags=title,artist,track,genre",
                "-of",
                "default=nw=1",
            ])
            .arg(&p)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("title=Tëst Title") && text.contains("artist=Some Artist"),
            "{text}"
        );
        assert!(text.contains("track=3/12"), "{text}");
    }
}
