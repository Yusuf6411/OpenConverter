//! The CMM against Little CMS (tools/make_icc_fixtures.py).
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use oc_image::icc::{convert_samples, Intent, Transform};
use oc_image::Depth;

#[test]
fn icc_matches_little_cms() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/icc");
    let mut n = 0;
    let mut report = Vec::new();
    let mut failed = false;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("icc") {
            continue;
        }
        let name = p.file_stem().unwrap().to_str().unwrap().to_string();
        let profile = std::fs::read(&p).unwrap();
        let input = std::fs::read(p.with_extension("in")).unwrap();
        for (intent, bpc, tag) in [
            (Intent::Perceptual, false, "i0"),
            (Intent::RelativeColorimetric, false, "i1"),
            (Intent::RelativeColorimetric, true, "i1b"),
        ] {
            let want = std::fs::read(p.with_extension(format!("{tag}.rgb"))).unwrap();
            let t = Transform::new(&profile, intent, bpc).unwrap_or_else(|e| panic!("{name}: {e}"));
            let got = convert_samples(&t, &input, Depth::Eight);
            assert_eq!(got.len(), want.len(), "{name}");
            let diffs: Vec<u8> = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).collect();
            let worst = *diffs.iter().max().unwrap();
            let bad = diffs.iter().filter(|&&d| d > 2).count();
            report.push(format!(
                "{name} {tag}: worst {worst}, {bad} of {} samples off by more than 2",
                diffs.len()
            ));
            // Little CMS finds the black point of CMYK output profiles by a round trip through
            // B2A0, which we do not read; with compensation on the two disagree a little near black.
            let allowed = if name.contains("cmyk") && tag != "i1" {
                10
            } else {
                3
            };
            if worst > allowed {
                failed = true;
            }
            n += 1;
        }
    }
    report.sort();
    assert!(!failed, "{}", report.join("\n"));
    eprintln!("{}", report.join("\n"));
    assert!(n >= 30, "ran {n} comparisons");
}

fn fixture(name: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/icc");
    (
        std::fs::read(dir.join(format!("{name}.icc"))).unwrap(),
        std::fs::read(dir.join(format!("{name}.in"))).unwrap(),
        std::fs::read(dir.join(format!("{name}.i1b.rgb"))).unwrap(),
    )
}

#[test]
fn images_keep_alpha_and_depth_when_converted() {
    use oc_image::{icc, Image, Layout};
    let (profile, input, want) = fixture("m_gamma22");
    let n = input.len() / 3;
    // RGBA 8 bit: colour converted, alpha untouched.
    let rgba: Vec<u8> = input
        .chunks(3)
        .enumerate()
        .flat_map(|(i, p)| [p[0], p[1], p[2], (i % 251) as u8])
        .collect();
    let mut img = Image::new(n as u32, 1, Layout::Rgba, Depth::Eight, rgba).unwrap();
    img.metadata.icc_profile = Some(profile.clone());
    let out = icc::to_srgb(&img, Intent::RelativeColorimetric, true).unwrap();
    assert!(out.metadata.icc_profile.is_none());
    for (i, (o, w)) in out.data.chunks(4).zip(want.chunks(3)).enumerate() {
        assert_eq!(o[3], (i % 251) as u8);
        for k in 0..3 {
            assert!(o[k].abs_diff(w[k]) <= 2, "pixel {i}");
        }
    }
    // 16 bit: the high byte agrees with the 8-bit result.
    let wide: Vec<u8> = input.iter().flat_map(|&v| [v, v]).collect();
    let mut img = Image::new(n as u32, 1, Layout::Rgb, Depth::Sixteen, wide).unwrap();
    img.metadata.icc_profile = Some(profile);
    let out = icc::to_srgb(&img, Intent::RelativeColorimetric, true).unwrap();
    for (i, (o, w)) in out.data.chunks(2).zip(&want).enumerate() {
        let v = u16::from_be_bytes([o[0], o[1]]) as f64 / 257.0;
        assert!((v - f64::from(*w)).abs() <= 2.5, "sample {i}: {v} vs {w}");
    }
    // Gray stays gray.
    let (profile, input, want) = fixture("g_gamma18");
    let mut img = Image::new(input.len() as u32, 1, Layout::Gray, Depth::Eight, input).unwrap();
    img.metadata.icc_profile = Some(profile.clone());
    let out = icc::to_srgb(&img, Intent::RelativeColorimetric, true).unwrap();
    assert_eq!(out.layout, Layout::Gray);
    for (i, (o, w)) in out.data.iter().zip(want.chunks(3)).enumerate() {
        assert!(o.abs_diff(w[1]) <= 2, "gray {i}");
    }
    // A profile that does not fit the image is refused, not applied.
    let mut img = Image::new(2, 1, Layout::Rgb, Depth::Eight, vec![0; 6]).unwrap();
    img.metadata.icc_profile = Some(profile);
    assert!(icc::to_srgb(&img, Intent::RelativeColorimetric, true).is_err());
}

#[test]
fn srgb_profiles_are_left_alone() {
    use oc_image::{icc, Image, Layout};
    let (profile, input, _) = fixture("lcms_srgb");
    let mut img = Image::new(
        (input.len() / 3) as u32,
        1,
        Layout::Rgb,
        Depth::Eight,
        input.clone(),
    )
    .unwrap();
    img.metadata.icc_profile = Some(profile);
    let out = icc::to_srgb(&img, Intent::RelativeColorimetric, true).unwrap();
    assert_eq!(out.data, input);
}

#[test]
fn cmyk_jpeg_uses_its_profile() {
    use oc_core::{Limits, NoProgress};
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/icc");
    let (img, notes) = oc_image::jpeg::decode(
        &std::fs::read(dir.join("cmyk_profile.jpg")).unwrap(),
        &Limits::default(),
        &NoProgress,
    )
    .unwrap();
    let want = std::fs::read(dir.join("cmyk_profile.rgb")).unwrap();
    assert_eq!(img.data.len(), want.len());
    assert!(
        img.metadata.icc_profile.is_none(),
        "the pixels are sRGB now"
    );
    assert!(notes.iter().any(|n| n.code == "jpeg.cmyk_profile"));
    // Little CMS reads the same JPEG samples; allow its black point handling and our DCT rounding.
    let worst = img
        .data
        .iter()
        .zip(&want)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(worst <= 10, "off by {worst}");
}
