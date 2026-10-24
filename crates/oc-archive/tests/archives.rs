#![allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
use oc_archive::{format, safety, zip, Archive, Context, Entry, EntryKind, Format, WriteOpts};
use oc_checksum::crc32;
use oc_core::{ErrorCode, Limits, NoProgress};
use std::path::PathBuf;

fn cx(limits: &Limits) -> Context<'_> {
    Context {
        limits,
        progress: &NoProgress,
        password: None,
        list_only: false,
    }
}

fn data(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/archives")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn sample() -> Archive {
    let mut a = Archive::default();
    a.entries.push(Entry::dir("docs"));
    let mut f = Entry::file("docs/readme.txt", b"hello archive\n".repeat(200));
    f.mtime = Some(1_700_000_000);
    f.mode = 0o640;
    a.entries.push(f);
    a.entries.push(Entry::file("empty", vec![]));
    a.entries.push(Entry::file(
        "bin/data.bin",
        (0..=255u8).cycle().take(10_000).collect(),
    ));
    a.entries.push(Entry::symlink("docs/link", "readme.txt"));
    a.entries
        .push(Entry::file("ünï/cödé-日本.txt", b"unicode".to_vec()));
    a
}

fn same(a: &Archive, b: &Archive) {
    assert_eq!(a.entries.len(), b.entries.len());
    for (x, y) in a.entries.iter().zip(&b.entries) {
        assert_eq!(x.path, y.path);
        assert_eq!(x.kind, y.kind, "{}", x.path);
        assert_eq!(x.data, y.data, "{}", x.path);
        assert_eq!(x.link, y.link, "{}", x.path);
    }
}

#[test]
fn cab_variants_match_expected_content() {
    let expected = String::from_utf8(data("cab_expected.txt")).unwrap();
    let want: Vec<(String, usize, u32)> = expected
        .lines()
        .map(|l| {
            let p: Vec<&str> = l.split(' ').collect();
            (
                p[0].to_string(),
                p[1].parse().unwrap(),
                p[2].parse().unwrap(),
            )
        })
        .collect();
    let limits = Limits::default();
    for name in [
        "stored.cab",
        "mszip.cab",
        "lzx_verbatim.cab",
        "lzx_aligned.cab",
        "lzx_stored.cab",
    ] {
        let a = format::read(Format::Cab, &data(name), &cx(&limits), None)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let got: Vec<(String, usize, u32)> = a
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.data.len(), crc32(&e.data)))
            .collect();
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn cab_damage_is_reported_not_panicking() {
    let limits = Limits::default();
    let good = data("lzx_verbatim.cab");
    assert_eq!(Format::sniff(&good), Some(Format::Cab));
    let mut seed = 1u32;
    for i in 0..300usize {
        let mut bad = good.clone();
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let at = (seed as usize) % bad.len();
        bad[at] ^= 1 << (i % 8);
        let _ = format::read(Format::Cab, &bad, &cx(&limits), None);
    }
    for cut in [0usize, 10, 40, 100, 5000, good.len() - 1] {
        assert!(format::read(Format::Cab, &good[..cut], &cx(&limits), None).is_err());
    }
}

#[test]
fn in_process_roundtrips() {
    let limits = Limits::default();
    let arc = sample();
    for f in [
        Format::Tar,
        Format::TarCompressed(oc_archive::Codec::Gzip),
        Format::TarCompressed(oc_archive::Codec::Zstd),
        Format::Zip,
        Format::SevenZ,
        Format::Cpio,
    ] {
        let bytes = format::write(f, &arc, &WriteOpts::default(), &cx(&limits))
            .unwrap_or_else(|e| panic!("{f:?}: {e:?}"));
        let back =
            format::read(f, &bytes, &cx(&limits), None).unwrap_or_else(|e| panic!("{f:?}: {e:?}"));
        // CPIO and 7z store directories implicitly or explicitly; compare files and links only.
        let strip = |a: &Archive| Archive {
            entries: a
                .entries
                .iter()
                .filter(|e| e.kind != EntryKind::Dir)
                .cloned()
                .collect(),
            comment: None,
        };
        same(&strip(&arc), &strip(&back));
        assert_eq!(
            Format::sniff(&bytes)
                .map(|s| s.id())
                .filter(|_| f == Format::Zip || f == Format::SevenZ || f == Format::Tar),
            match f {
                Format::Zip => Some("zip".into()),
                Format::SevenZ => Some("7z".into()),
                Format::Tar => Some("tar".into()),
                _ => None,
            }
        );
    }
}

#[test]
fn zip_keeps_mtime_mode_and_comment() {
    let limits = Limits::default();
    let mut arc = sample();
    arc.comment = Some("a comment".into());
    let bytes = zip::write(&arc, &zip::WriteOptions::default(), &cx(&limits)).unwrap();
    let back = zip::read(&bytes, &cx(&limits)).unwrap();
    let f = back
        .entries
        .iter()
        .find(|e| e.path == "docs/readme.txt")
        .unwrap();
    assert_eq!(f.mtime, Some(1_700_000_000));
    assert_eq!(f.mode, 0o640);
    assert_eq!(back.comment.as_deref(), Some("a comment"));
}

#[test]
fn format_ids_and_names() {
    assert_eq!(
        Format::from_name("a/b/archive.tar.gz").map(|f| f.id()),
        Some("tar.gz".into())
    );
    assert_eq!(
        Format::from_name("x.TGZ").map(|f| f.id()),
        Some("tar.gz".into())
    );
    assert_eq!(
        Format::from_name("x.txz").map(|f| f.id()),
        Some("tar.xz".into())
    );
    assert_eq!(
        Format::from_name("notes.gz").map(|f| f.id()),
        Some("gz".into())
    );
    assert_eq!(Format::from_name("x.7z").map(|f| f.id()), Some("7z".into()));
    assert_eq!(Format::from_name("plain.txt"), None);
    for f in Format::all() {
        assert_eq!(Format::from_id(&f.id()), Some(f));
    }
}

// ---- safety ----

#[test]
fn zip_slip_and_absolute_paths_are_refused() {
    let limits = Limits::default();
    let mut arc = Archive::default();
    for p in [
        "../evil.txt",
        "a/../../evil2.txt",
        "/abs/evil3.txt",
        "C:/win/evil4.txt",
        "ok/fine.txt",
        "back\\..\\evil5.txt",
    ] {
        arc.entries.push(Entry::file(p, b"x".to_vec()));
    }
    // Writers refuse to *create* such names; build the hostile archive by hand-patching a ZIP.
    let mut safe = Archive::default();
    safe.entries
        .push(Entry::file("aaa/evil.txt", b"x".to_vec()));
    let mut bytes = zip::write(
        &safe,
        &zip::WriteOptions {
            method: zip::Method::Stored,
            ..Default::default()
        },
        &cx(&limits),
    )
    .unwrap();
    let find = |b: &[u8], needle: &[u8]| b.windows(needle.len()).position(|w| w == needle).unwrap();
    // Replace "aaa/evil.txt" (12 bytes) with "../evil.txt!!" shaped names of equal length in both headers.
    let hostile = b"../../evil.t";
    for _ in 0..2 {
        let at = find(&bytes, b"aaa/evil.txt");
        bytes[at..at + 12].copy_from_slice(hostile);
    }
    let parsed = zip::read(&bytes, &cx(&limits)).unwrap();
    assert_eq!(parsed.entries[0].path, "../../evil.t");
    let dir = tempdir("slip");
    let rep = safety::extract(&parsed, &dir, &safety::ExtractOptions::default()).unwrap();
    assert_eq!(rep.files, 0);
    assert_eq!(rep.skipped.len(), 1);
    assert!(!dir.parent().unwrap().join("evil.t").exists());
    for p in ["/abs/x", "C:/x", "a/../../x", "x\\..\\..\\y"] {
        assert!(safety::components(p).is_err(), "{p}");
    }
    assert_eq!(
        safety::components("./a//b/./c").unwrap(),
        vec!["a", "b", "c"]
    );
    assert!(oc_archive::safety::clean_for_write("../x").is_err());
    assert_eq!(
        oc_archive::safety::clean_for_write("/a/./b").unwrap(),
        "a/b"
    );
}

fn tempdir(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("oc-archive-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let d = base.join("out");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[cfg(unix)]
#[test]
fn symlink_escape_and_write_through_symlink_are_refused() {
    let mut arc = Archive::default();
    arc.entries.push(Entry::symlink("up", "../../outside"));
    arc.entries.push(Entry::symlink("abs", "/etc/passwd"));
    arc.entries.push(Entry::symlink("fine", "sub/target"));
    arc.entries
        .push(Entry::symlink("a/b/deep", "../../x/../../escape"));
    arc.entries.push(Entry::symlink("dirlink", "."));
    arc.entries
        .push(Entry::file("dirlink/inside.txt", b"x".to_vec()));
    let dir = tempdir("links");
    let rep = safety::extract(&arc, &dir, &safety::ExtractOptions::default()).unwrap();
    assert_eq!(rep.symlinks, 2, "{:?}", rep.skipped); // fine + dirlink (stays inside)
    assert!(rep.skipped.iter().any(|(p, _)| p == "up"));
    assert!(rep.skipped.iter().any(|(p, _)| p == "abs"));
    assert!(rep.skipped.iter().any(|(p, _)| p == "a/b/deep"));
    // Writing *through* the symlink "dirlink" is refused even though it points inside.
    assert!(rep.skipped.iter().any(|(p, _)| p == "dirlink/inside.txt"));
    // A later entry that tries to write through an existing symlink to the outside:
    let outside = dir.parent().unwrap().join("outside-dir");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("preplanted")).unwrap();
    let mut arc2 = Archive::default();
    arc2.entries
        .push(Entry::file("preplanted/pwned.txt", b"x".to_vec()));
    let rep2 = safety::extract(&arc2, &dir, &safety::ExtractOptions::default()).unwrap();
    assert_eq!(rep2.files, 0);
    assert!(!outside.join("pwned.txt").exists());
}

#[test]
fn extraction_does_not_overwrite_unless_asked_and_drops_setuid() {
    let mut arc = Archive::default();
    let mut e = Entry::file("run.sh", b"#!/bin/sh\n".to_vec());
    e.mode = 0o4755;
    arc.entries.push(e);
    let dir = tempdir("over");
    let r1 = safety::extract(&arc, &dir, &safety::ExtractOptions::default()).unwrap();
    assert_eq!(r1.files, 1);
    let r2 = safety::extract(&arc, &dir, &safety::ExtractOptions::default()).unwrap();
    assert_eq!(r2.files, 0);
    assert_eq!(r2.skipped.len(), 1);
    let r3 = safety::extract(
        &arc,
        &dir,
        &safety::ExtractOptions {
            overwrite: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r3.files, 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(dir.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(m & 0o7000, 0, "setuid/setgid/sticky must not be restored");
        assert_eq!(m & 0o777, 0o755);
    }
}

#[test]
fn strip_components_and_selection() {
    let arc = sample();
    let dir = tempdir("sel");
    let rep = safety::extract(
        &arc,
        &dir,
        &safety::ExtractOptions {
            only: vec!["docs".into()],
            strip_components: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(dir.join("readme.txt").exists());
    assert!(!dir.join("bin").exists());
    assert!(rep.files >= 1);
}

#[test]
fn bombs_are_stopped_by_limits() {
    // A highly compressible member whose declared size is within bounds but the ratio is absurd.
    let limits = Limits::default();
    let mut arc = Archive::default();
    arc.entries.push(Entry::file("zeros", vec![0u8; 64 << 20]));
    let bytes = zip::write(&arc, &zip::WriteOptions::default(), &cx(&limits)).unwrap();
    assert!(bytes.len() < 200_000);
    let tight = Limits {
        max_ratio: 100,
        ratio_floor: 1 << 20,
        ..Limits::default()
    };
    let err = zip::read(&bytes, &cx(&tight)).unwrap_err();
    assert_eq!(err.code, ErrorCode::LimitExceeded);
    let small = Limits {
        max_output: 1 << 20,
        ..Limits::default()
    };
    assert_eq!(
        zip::read(&bytes, &cx(&small)).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    // Entry-count bomb.
    let mut many = Archive::default();
    for i in 0..500 {
        many.entries.push(Entry::file(format!("f{i}"), vec![]));
    }
    let b = zip::write(&many, &zip::WriteOptions::default(), &cx(&limits)).unwrap();
    let few = Limits {
        max_entries: 100,
        ..Limits::default()
    };
    assert_eq!(
        zip::read(&b, &cx(&few)).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
}

#[test]
fn lying_zip_sizes_cannot_force_big_allocations() {
    let limits = Limits::default();
    let mut arc = Archive::default();
    arc.entries
        .push(Entry::file("a.txt", b"hello hello hello".to_vec()));
    let mut b = zip::write(
        &arc,
        &zip::WriteOptions {
            method: zip::Method::Deflate,
            ..Default::default()
        },
        &cx(&limits),
    )
    .unwrap();
    // Patch the central directory's uncompressed size to 4 GiB - 2.
    let cd = b.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
    b[cd + 24..cd + 28].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    let small = Limits {
        max_output: 1 << 20,
        ..Limits::default()
    };
    assert!(zip::read(&b, &cx(&small)).is_err());
}

#[test]
fn garbage_never_panics() {
    let limits = Limits::default();
    let mut seed = 99u32;
    let samples: Vec<(Format, Vec<u8>)> = {
        let arc = sample();
        [Format::Tar, Format::Zip, Format::SevenZ, Format::Cpio]
            .into_iter()
            .map(|f| {
                (
                    f,
                    format::write(f, &arc, &WriteOpts::default(), &cx(&limits)).unwrap(),
                )
            })
            .collect()
    };
    for (f, good) in &samples {
        for i in 0..400usize {
            let mut bad = good.clone();
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = (seed as usize >> 4) % bad.len();
                bad[at] = (seed >> 24) as u8;
            }
            if i % 7 == 0 {
                bad.truncate((seed as usize >> 8) % bad.len());
            }
            let _ = format::read(*f, &bad, &cx(&limits), None);
        }
    }
}

#[test]
fn listing_mode_reports_the_same_names_and_sizes_without_contents() {
    let limits = Limits::default();
    let lcx = Context {
        limits: &limits,
        progress: &NoProgress,
        password: None,
        list_only: true,
    };
    let arc = sample();
    for f in [
        Format::Tar,
        Format::Zip,
        Format::SevenZ,
        Format::Cpio,
        Format::TarCompressed(oc_archive::Codec::Xz),
    ] {
        let bytes = format::write(f, &arc, &WriteOpts::default(), &cx(&limits)).unwrap();
        let full = format::read(f, &bytes, &cx(&limits), None).unwrap();
        let list = format::read(f, &bytes, &lcx, None).unwrap();
        assert_eq!(full.entries.len(), list.entries.len(), "{f:?}");
        for (a, b) in full.entries.iter().zip(&list.entries) {
            assert_eq!(a.path, b.path);
            assert_eq!(a.kind, b.kind);
            assert_eq!(a.size(), b.size(), "{f:?} {}", a.path);
            if b.kind == EntryKind::File {
                assert!(b.data.is_empty());
            }
        }
    }
    for name in ["mszip.cab", "lzx_aligned.cab"] {
        let b = format::read(Format::Cab, &data(name), &lcx, None).unwrap();
        let a = format::read(Format::Cab, &data(name), &cx(&limits), None).unwrap();
        assert_eq!(
            a.entries.iter().map(|e| e.size()).collect::<Vec<_>>(),
            b.entries.iter().map(|e| e.size()).collect::<Vec<_>>()
        );
    }
    // An encrypted ZIP lists without a password.
    let opts = zip::WriteOptions {
        password: Some("pw".into()),
        ..Default::default()
    };
    let enc = zip::write(&arc, &opts, &cx(&limits)).unwrap();
    let l = zip::read(&enc, &lcx).unwrap();
    assert!(l.entries.iter().any(|e| e.encrypted));
}

/// Every reader against randomly damaged copies of real archives: errors are fine, panics and
/// runaway allocation are not.
#[test]
fn damaged_archives_never_panic() {
    let limits = Limits {
        max_alloc: 1 << 26,
        max_output: 1 << 25,
        max_entries: 1000,
        ..Limits::default()
    };
    let mut seeds: Vec<Vec<u8>> = [
        "stored.cab",
        "mszip.cab",
        "lzx_verbatim.cab",
        "lzx_aligned.cab",
    ]
    .iter()
    .map(|n| data(n))
    .collect();
    let a = sample();
    for f in [
        Format::Tar,
        Format::Zip,
        Format::SevenZ,
        Format::Cpio,
        Format::TarCompressed(oc_archive::Codec::Gzip),
        Format::TarCompressed(oc_archive::Codec::Xz),
    ] {
        let c = cx(&limits);
        seeds.push(format::write(f, &a, &WriteOpts::default(), &c).unwrap());
    }
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for seed in &seeds {
        for _ in 0..150 {
            let mut d = seed.clone();
            for _ in 0..=(next() % 4) {
                let at = (next() as usize) % d.len();
                match next() % 3 {
                    0 => d[at] ^= 1 << (next() % 8),
                    1 => d[at] = next() as u8,
                    _ => d.truncate(at.max(1)),
                }
            }
            for f in Format::all() {
                for list_only in [false, true] {
                    let c = Context {
                        limits: &limits,
                        progress: &NoProgress,
                        password: Some("pw"),
                        list_only,
                    };
                    let _ = format::read(f, &d, &c, Some("x"));
                }
            }
        }
    }
}
