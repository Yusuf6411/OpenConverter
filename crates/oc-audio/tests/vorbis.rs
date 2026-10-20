#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
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
    assert!(ours.len() <= reference.len() && reference.len() - ours.len() < 4096 * usize::from(channels), "{name}: length {} vs {}", ours.len(), reference.len());
    let worst = ours.iter().zip(&reference).map(|(a, b)| (a - b).abs()).max().unwrap_or(0);
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
