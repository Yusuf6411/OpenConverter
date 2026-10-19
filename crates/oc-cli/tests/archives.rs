#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
//! Command-line archive workflows through the real binary and its isolated workers.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn oc(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_openconverter"));
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("oc-cli-arc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut items: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        items.sort();
        for i in items {
            let rel = i.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if std::fs::symlink_metadata(&i).unwrap().file_type().is_symlink() {
                out.push((format!("{rel} -> {}", std::fs::read_link(&i).unwrap().display()), vec![]));
            } else if i.is_dir() {
                walk(root, &i, out);
            } else {
                out.push((rel, std::fs::read(&i).unwrap()));
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v
}

fn make_source(d: &Path) -> PathBuf {
    let src = d.join("proj");
    std::fs::create_dir_all(src.join("sub/deeper")).unwrap();
    std::fs::write(src.join("a.txt"), "hello\n".repeat(100)).unwrap();
    std::fs::write(src.join("sub/b.bin"), (0..=255u8).cycle().take(9000).collect::<Vec<_>>()).unwrap();
    std::fs::write(src.join("sub/deeper/ünï.txt"), "unicode").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", src.join("link")).unwrap();
    src
}

#[test]
fn create_convert_and_extract_roundtrip_through_many_formats() {
    let d = tmp("round");
    let src = make_source(&d);
    let want = tree(&d.join("proj"));
    for first in ["tar.gz", "zip", "7z", "tar.zst"] {
        let a1 = d.join(format!("one.{first}"));
        let o = oc(&["create", p(&a1), p(&src)], &[]);
        assert!(o.status.success(), "{first}: {}", String::from_utf8_lossy(&o.stderr));
        for second in ["zip", "7z", "tar.xz", "tar.bz2", "cpio"] {
            let a2 = d.join(format!("two-{first}.{second}"));
            let o = oc(&["convert", p(&a1), p(&a2)], &[]);
            assert!(o.status.success(), "{first}->{second}: {}", String::from_utf8_lossy(&o.stderr));
            let out = d.join(format!("out-{first}-{second}"));
            let o = oc(&["extract", p(&a2), p(&out)], &[]);
            assert!(o.status.success(), "extract {second}: {}", String::from_utf8_lossy(&o.stderr));
            assert_eq!(tree(&out.join("proj")), want, "{first} -> {second}");
        }
    }
}

#[test]
fn list_json_and_text() {
    let d = tmp("list");
    let src = make_source(&d);
    let a = d.join("x.zip");
    assert!(oc(&["create", p(&a), p(&src)], &[]).status.success());
    let o = oc(&["list", p(&a), "--json"], &[]);
    let text = String::from_utf8(o.stdout).unwrap();
    assert!(text.contains("\"format\":\"zip\""), "{text}");
    assert!(text.contains("\"path\":\"proj/sub/b.bin\""), "{text}");
    assert!(text.contains("\"size\":9000"), "{text}");
    let o = oc(&["list", p(&a)], &[]);
    assert!(String::from_utf8(o.stdout).unwrap().contains("proj/a.txt"));
}

#[test]
fn passwords_travel_in_the_environment_and_are_checked() {
    let d = tmp("pw");
    let src = make_source(&d);
    let plain = d.join("plain.tar");
    assert!(oc(&["create", p(&plain), p(&src)], &[]).status.success());
    for (ext, extra) in [("zip", "aes256"), ("zip", "zipcrypto"), ("7z", "")] {
        let enc = d.join(format!("enc-{extra}.{ext}"));
        let mut args = vec!["convert", p(&plain), p(&enc)];
        if !extra.is_empty() {
            args.extend(["--set", if extra == "aes256" { "encryption=aes256" } else { "encryption=zipcrypto" }]);
        }
        let o = oc(&args, &[("OC_OUTPUT_PASSWORD", "s3cret")]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        // The secret is not in the file in the clear.
        let bytes = std::fs::read(&enc).unwrap();
        assert!(!bytes.windows(6).any(|w| w == b"hello\n"), "{ext}/{extra}: content visible");
        let none = oc(&["extract", p(&enc), p(&d.join("x0"))], &[]);
        assert_eq!(none.status.code(), Some(16), "{ext}/{extra}");
        let wrong = oc(&["extract", p(&enc), p(&d.join("x1"))], &[("OC_PASSWORD", "nope")]);
        assert_eq!(wrong.status.code(), Some(17), "{ext}/{extra}");
        let out = d.join(format!("x2-{extra}-{ext}"));
        let right = oc(&["extract", p(&enc), p(&out)], &[("OC_PASSWORD", "s3cret")]);
        assert!(right.status.success(), "{}", String::from_utf8_lossy(&right.stderr));
        assert_eq!(tree(&out.join("proj")), tree(&src));
    }
}

#[test]
fn packing_any_file_and_unpacking_a_single_stream() {
    let d = tmp("single");
    let f = d.join("report.dat");
    std::fs::write(&f, b"binary \x00\x01 payload".repeat(500)).unwrap();
    let gz = d.join("report.dat.gz");
    let o = oc(&["convert", p(&f), p(&gz)], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = d.join("un");
    assert!(oc(&["extract", p(&gz), p(&out)], &[]).status.success());
    assert_eq!(std::fs::read(out.join("report.dat")).unwrap(), std::fs::read(&f).unwrap());
    // Several files do not fit in a single-stream format.
    let two = d.join("two.tar");
    let sub = d.join("both");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("a"), "1").unwrap();
    std::fs::write(sub.join("b"), "2").unwrap();
    assert!(oc(&["create", p(&two), p(&sub)], &[]).status.success());
    let o = oc(&["convert", p(&two), p(&d.join("two.gz"))], &[]);
    assert_eq!(o.status.code(), Some(11));
}

#[test]
fn hostile_archives_cannot_write_outside_the_folder() {
    let d = tmp("slip");
    // A tar with ../ and absolute names, a symlink out, and a normal file.
    let mut tar: Vec<u8> = Vec::new();
    let header = |name: &str, typeflag: u8, link: &str, size: usize| {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        let sz = format!("{size:011o}\0");
        h[124..136].copy_from_slice(sz.as_bytes());
        h[156] = typeflag;
        h[157..157 + link.len()].copy_from_slice(link.as_bytes());
        h[257..263].copy_from_slice(b"ustar\0");
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    };
    for (name, tf, link, body) in [
        ("../escape.txt", b'0', "", &b"pwned"[..]),
        ("/tmp/abs-escape.txt", b'0', "", &b"pwned"[..]),
        ("evil", b'2', "../../../../etc", &b""[..]),
        ("good.txt", b'0', "", &b"fine"[..]),
    ] {
        tar.extend_from_slice(&header(name, tf, link, body.len()));
        tar.extend_from_slice(body);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    let a = d.join("evil.tar");
    std::fs::write(&a, &tar).unwrap();
    let out = d.join("sandbox").join("out");
    let o = oc(&["extract", p(&a), p(&out)], &[]);
    assert_eq!(o.status.code(), Some(3), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read(out.join("good.txt")).unwrap(), b"fine");
    assert!(!d.join("sandbox").join("escape.txt").exists());
    assert!(!Path::new("/tmp/abs-escape.txt").exists());
    assert!(!out.join("evil").exists());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("escape.txt") && err.contains("evil"), "{err}");
}

#[test]
fn corrupt_and_truncated_archives_give_typed_errors() {
    let d = tmp("bad");
    let src = make_source(&d);
    let a = d.join("ok.zip");
    assert!(oc(&["create", p(&a), p(&src)], &[]).status.success());
    let bytes = std::fs::read(&a).unwrap();
    let cut = d.join("cut.zip");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
    let o = oc(&["list", p(&cut)], &[]);
    assert!(matches!(o.status.code(), Some(13 | 14)), "{:?} {}", o.status.code(), String::from_utf8_lossy(&o.stderr));
    let junk = d.join("junk.7z");
    std::fs::write(&junk, b"7z\xbc\xaf\x27\x1c garbage garbage garbage garbage garbage").unwrap();
    let o = oc(&["list", p(&junk)], &[]);
    assert!(matches!(o.status.code(), Some(13 | 14)), "{:?}", o.status.code());
    let o = oc(&["extract", p(&d.join("missing.zip")), p(&d.join("o"))], &[]);
    assert_eq!(o.status.code(), Some(22));
}

#[test]
fn zip_bomb_is_stopped_by_the_ratio_limit() {
    let d = tmp("bomb");
    // 64 MiB of zeros deflates to ~64 KB; the default ratio limit (1000:1) refuses it.
    let zeros = vec![0u8; 64 << 20];
    let f = d.join("zeros.bin");
    std::fs::write(&f, &zeros).unwrap();
    let z = d.join("zeros.zip");
    assert!(oc(&["convert", p(&f), p(&z)], &[]).status.success());
    let o = oc(&["extract", p(&z), p(&d.join("out"))], &[]);
    assert_eq!(o.status.code(), Some(15), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!d.join("out").join("zeros.bin").exists());
}
