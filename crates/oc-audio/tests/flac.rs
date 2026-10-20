#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::precedence
)]
use oc_audio::flac::{self, Options};
use oc_audio::{Audio, Samples};
use oc_core::{Limits, Notes};

fn signal(bits: u8, ch: usize, frames: usize, seed: u64) -> Audio {
    let mut s = seed | 1;
    let mut rnd = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33
    };
    let amp = ((1i64 << (bits - 1)) - 1) as f64;
    let mut d = Vec::with_capacity(frames * ch);
    for i in 0..frames {
        for c in 0..ch {
            let t = i as f64 / 44100.0;
            let tone = (2.0 * std::f64::consts::PI * (440.0 + 110.0 * c as f64) * t).sin() * 0.5
                + (2.0 * std::f64::consts::PI * 3000.0 * t).sin() * 0.1;
            let noise = (rnd() % 2001) as f64 / 1000.0 - 1.0;
            let v = ((tone + noise * 0.02) * amp).round() as i64;
            d.push(v.clamp(-(1 << (bits - 1)), (1 << (bits - 1)) - 1) as i32);
        }
    }
    Audio::new(44100, ch as u16, Samples::Int { bits, data: d })
}

fn round_trip(a: &Audio, level: u8) -> Vec<u8> {
    let mut notes = Notes::new();
    let bytes = flac::write(
        a,
        &Options {
            level,
            ..Options::default()
        },
        &mut notes,
    )
    .unwrap();
    let (b, n) = flac::read(&bytes, &Limits::default()).unwrap();
    assert!(
        n.iter()
            .all(|x| x.code != "flac.md5_mismatch" && x.code != "flac.damaged"),
        "{:?}",
        n.iter().map(|x| x.code.clone()).collect::<Vec<_>>()
    );
    assert_eq!(b.sample_rate, a.sample_rate);
    assert_eq!(b.channels, a.channels);
    assert_eq!(
        b.samples,
        a.samples,
        "bits {} ch {} frames {} level {level}",
        a.bits(),
        a.channels,
        a.frames()
    );
    bytes
}

#[test]
fn every_depth_and_channel_count_round_trips() {
    for bits in [4u8, 8, 12, 16, 20, 24, 32] {
        for ch in [1usize, 2, 3, 6] {
            round_trip(&signal(bits, ch, 5000, u64::from(bits) * 7 + ch as u64), 5);
        }
    }
}

#[test]
fn every_level_and_odd_length_round_trips() {
    for level in 0..=8u8 {
        for frames in [0usize, 1, 2, 15, 17, 1152, 1153, 4096, 4097, 9000] {
            round_trip(&signal(16, 2, frames, 99 + frames as u64), level);
        }
    }
}

#[test]
fn special_signals() {
    // Silence, a constant, a full-scale square wave and white noise at full range.
    let mut a = signal(16, 2, 6000, 1);
    if let Samples::Int { data, .. } = &mut a.samples {
        data.fill(0);
    }
    round_trip(&a, 8);
    if let Samples::Int { data, .. } = &mut a.samples {
        data.fill(-12345);
    }
    round_trip(&a, 8);
    if let Samples::Int { data, .. } = &mut a.samples {
        for (i, v) in data.iter_mut().enumerate() {
            *v = if (i / 200) % 2 == 0 { 32767 } else { -32768 };
        }
    }
    round_trip(&a, 8);
    let mut s = 12345u64;
    if let Samples::Int { data, .. } = &mut a.samples {
        for v in data.iter_mut() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *v = ((s >> 40) & 0xFFFF) as i32 - 32768;
        }
    }
    let bytes = round_trip(&a, 8);
    // Noise cannot shrink; the stream must still be near the raw size.
    assert!(bytes.len() < 6000 * 4 + 400, "{}", bytes.len());
    // Left-justified samples (low bits unused) exercise the wasted-bits path.
    let mut w = signal(16, 2, 4000, 5);
    if let Samples::Int { data, .. } = &mut w.samples {
        for v in data.iter_mut() {
            *v &= !0xFF;
        }
    }
    round_trip(&w, 5);
    // 32-bit extremes (the side channel needs 33 bits).
    let mut x = signal(32, 2, 3000, 6);
    if let Samples::Int { data, .. } = &mut x.samples {
        for (i, v) in data.iter_mut().enumerate() {
            *v = if i % 2 == 0 { i32::MAX } else { i32::MIN };
        }
    }
    round_trip(&x, 8);
}

#[test]
fn higher_levels_are_not_larger() {
    let a = signal(16, 2, 44100, 3);
    let sizes: Vec<usize> = [0u8, 3, 5, 8]
        .iter()
        .map(|&l| round_trip(&a, l).len())
        .collect();
    assert!(sizes[3] <= sizes[1] && sizes[1] < sizes[0], "{sizes:?}");
}

#[test]
fn damaged_streams_never_panic() {
    let a = signal(16, 2, 9000, 8);
    let bytes = round_trip(&a, 5);
    let mut s = 777u64;
    for _ in 0..600 {
        let mut b = bytes.clone();
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let n = 1 + (s % 4) as usize;
        for k in 0..n {
            let pos = ((s >> (8 * k)) as usize ^ k * 7919) % b.len();
            b[pos] ^= 1 << ((s >> 3) % 8);
        }
        if s.is_multiple_of(5) {
            b.truncate(b.len() * (s as usize % 100) / 100);
        }
        let _ = flac::read(&b, &Limits::default());
    }
}

#[test]
fn tags_and_pictures_survive() {
    let mut a = signal(16, 2, 2000, 4);
    a.tags.add("TITLE", "Tëst");
    a.tags.add("artist", "A");
    a.pictures.push(oc_audio::Picture {
        mime: "image/png".into(),
        kind: 3,
        description: "c".into(),
        data: vec![9; 100],
    });
    let mut notes = Notes::new();
    let bytes = flac::write(&a, &Options::default(), &mut notes).unwrap();
    let (b, _) = flac::read(&bytes, &Limits::default()).unwrap();
    assert_eq!(b.tags, a.tags);
    assert_eq!(b.pictures, a.pictures);
}
