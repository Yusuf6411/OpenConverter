//! Animated GIF, APNG and WebP against frames decoded by Pillow (tools/make_anim_fixtures.py).
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use oc_core::Limits;
use oc_image::anim::Animation;
use std::path::Path;

fn fixtures(ext: &str) -> Vec<std::path::PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/anim");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some(ext))
        .collect();
    v.sort();
    v
}

/// Frames must match; the colour of fully transparent pixels does not matter.
fn check(anim: &Animation, path: &Path, delay_slack: u32) {
    let want = std::fs::read(path.with_extension("rgba")).unwrap();
    let meta = std::fs::read_to_string(path.with_extension("txt")).unwrap();
    let mut lines = meta.lines();
    let head: Vec<u32> = lines
        .next()
        .unwrap()
        .split(' ')
        .map(|x| x.parse().unwrap())
        .collect();
    let delays: Vec<f64> = lines.map(|l| l.parse().unwrap()).collect();
    let name = path.display().to_string();
    assert_eq!((anim.width, anim.height), (head[0], head[1]), "{name}");
    assert_eq!(anim.frames.len() as u32, head[2], "{name}");
    let size = (head[0] * head[1] * 4) as usize;
    for (i, f) in anim.frames.iter().enumerate() {
        let w = &want[i * size..(i + 1) * size];
        let g = &f.image.data;
        for (k, (a, b)) in g
            .as_chunks::<4>()
            .0
            .iter()
            .zip(w.as_chunks::<4>().0)
            .enumerate()
        {
            let same = if a[3] == 0 && b[3] == 0 { true } else { a == b };
            assert!(same, "{name} frame {i} pixel {k}: got {a:?} want {b:?}");
        }
        let d = delays[i] as i64;
        assert!(
            (i64::from(f.delay_ms) - d).abs() <= i64::from(delay_slack),
            "{name} frame {i} delay {} vs {d}",
            f.delay_ms
        );
    }
}

#[test]
fn gif_frames_match_pillow() {
    let files = fixtures("gif");
    assert!(files.len() >= 8);
    for p in files {
        let (anim, _) =
            oc_image::gif::decode_animation(&std::fs::read(&p).unwrap(), &Limits::default())
                .unwrap();
        check(&anim, &p, 0);
        let loops: u32 = std::fs::read_to_string(p.with_extension("txt"))
            .unwrap()
            .split(' ')
            .nth(3)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        // Pillow reports the raw Netscape count (0 = forever, no extension = absent).
        let want = if loops == 0 {
            0
        } else if loops == 1 {
            1
        } else {
            loops + 1
        };
        assert_eq!(anim.loops, want, "{}", p.display());
    }
}

#[test]
fn apng_frames_match_pillow() {
    let files = fixtures("png");
    assert!(files.len() >= 7);
    for p in files {
        let data = std::fs::read(&p).unwrap();
        let (anim, _) =
            oc_image::png::decode_animation(&data, &Limits::default(), &oc_core::NoProgress)
                .unwrap();
        // Pillow rounds delays to whole milliseconds the same way.
        check(&anim, &p, 1);
    }
}

#[test]
fn webp_frames_match_pillow() {
    let files = fixtures("webp");
    assert!(files.len() >= 4);
    for p in files {
        let data = std::fs::read(&p).unwrap();
        let (anim, _) =
            oc_image::webp::decode_animation(&data, &Limits::default(), &oc_core::NoProgress)
                .unwrap();
        check(&anim, &p, 0);
        let (first, notes) =
            oc_image::webp::decode(&data, &Limits::default(), &oc_core::NoProgress).unwrap();
        assert!(notes.iter().any(|n| n.code == "webp.animation_first_frame"));
        assert_eq!((first.width, first.height), (anim.width, anim.height));
    }
}

mod roundtrip {
    use super::*;
    use oc_image::anim::Frame;
    use oc_image::{Depth, Image, Layout};

    fn scene(i: u32, clear_bg: bool) -> Image {
        let mut d = Vec::new();
        for y in 0..24u32 {
            for x in 0..32u32 {
                let in_box = x >= 3 + i * 5 && x < 11 + i * 5 && (5..14).contains(&y);
                let px = if in_box {
                    [200, 30 + 40 * i as u8, 60, 255]
                } else if clear_bg {
                    [0, 0, 0, 0]
                } else {
                    [255, 255, 255, 255]
                };
                d.extend_from_slice(&px);
            }
        }
        Image::new(32, 24, Layout::Rgba, Depth::Eight, d).unwrap()
    }

    fn animation(clear_bg: bool) -> Animation {
        let mut a = Animation::new(32, 24);
        a.loops = 4;
        for i in 0..5 {
            a.frames.push(Frame {
                image: scene(i, clear_bg),
                delay_ms: 30 + 20 * i,
            });
        }
        a
    }

    fn same(a: &Animation, b: &Animation) {
        assert_eq!(a.frames.len(), b.frames.len());
        assert_eq!(a.loops, b.loops);
        for (x, y) in a.frames.iter().zip(&b.frames) {
            assert_eq!(x.image.data, y.image.data);
            assert_eq!(x.delay_ms, y.delay_ms);
        }
    }

    #[test]
    fn apng_roundtrips_exactly() {
        for clear in [false, true] {
            let a = animation(clear);
            let (bytes, _) =
                oc_image::png::encode_animation(&a, &oc_image::png::EncodeOptions::default())
                    .unwrap();
            assert!(oc_image::png::is_animated(&bytes));
            let (back, _) =
                oc_image::png::decode_animation(&bytes, &Limits::default(), &oc_core::NoProgress)
                    .unwrap();
            same(&a, &back);
            // A viewer that only knows still PNG sees the first frame.
            let (still, notes) =
                oc_image::png::decode(&bytes, &Limits::default(), &oc_core::NoProgress).unwrap();
            assert!(notes.iter().any(|n| n.code == "animation_first_frame_only"));
            assert_eq!((still.width, still.height), (32, 24));
        }
    }

    #[test]
    fn webp_lossless_roundtrips_exactly() {
        for clear in [false, true] {
            let a = animation(clear);
            let (bytes, _) =
                oc_image::webp::encode_animation(&a, &oc_image::webp::EncodeOptions::default())
                    .unwrap();
            let (back, _) =
                oc_image::webp::decode_animation(&bytes, &Limits::default(), &oc_core::NoProgress)
                    .unwrap();
            same(&a, &back);
        }
    }

    #[test]
    fn webp_lossy_is_close() {
        let a = animation(false);
        let opts = oc_image::webp::EncodeOptions {
            quality: Some(90.0),
            ..Default::default()
        };
        let (bytes, _) = oc_image::webp::encode_animation(&a, &opts).unwrap();
        let (back, _) =
            oc_image::webp::decode_animation(&bytes, &Limits::default(), &oc_core::NoProgress)
                .unwrap();
        assert_eq!(back.frames.len(), 5);
        for (x, y) in back.frames.iter().zip(&a.frames) {
            let err: u64 = x
                .image
                .data
                .iter()
                .zip(&y.image.data)
                .map(|(p, q)| u64::from(p.abs_diff(*q)))
                .sum();
            assert!(
                err / (32 * 24 * 4) < 12,
                "mean error {}",
                err / (32 * 24 * 4)
            );
        }
    }

    #[test]
    fn gif_roundtrips_exactly() {
        for clear in [false, true] {
            let a = animation(clear);
            let (bytes, _) =
                oc_image::gif::encode_animation(&a, &oc_image::gif::EncodeOptions::default())
                    .unwrap();
            let (back, _) = oc_image::gif::decode_animation(&bytes, &Limits::default()).unwrap();
            // GIF stores delays in hundredths of a second.
            for (x, y) in back.frames.iter().zip(&a.frames) {
                assert_eq!(x.image.data, y.image.data);
                assert!(x.delay_ms.abs_diff(y.delay_ms) <= 5);
            }
        }
    }

    #[test]
    fn damaged_animations_fail_without_panicking() {
        let a = animation(true);
        let gif = oc_image::gif::encode_animation(&a, &oc_image::gif::EncodeOptions::default())
            .unwrap()
            .0;
        let png = oc_image::png::encode_animation(&a, &oc_image::png::EncodeOptions::default())
            .unwrap()
            .0;
        let webp = oc_image::webp::encode_animation(&a, &oc_image::webp::EncodeOptions::default())
            .unwrap()
            .0;
        for cut in (0..gif.len()).step_by(7) {
            let _ = oc_image::gif::decode_animation(&gif[..cut], &Limits::default());
        }
        for cut in (0..png.len()).step_by(7) {
            let _ = oc_image::png::decode_animation(
                &png[..cut],
                &Limits::default(),
                &oc_core::NoProgress,
            );
        }
        for cut in (0..webp.len()).step_by(7) {
            let _ = oc_image::webp::decode_animation(
                &webp[..cut],
                &Limits::default(),
                &oc_core::NoProgress,
            );
        }
    }
}
