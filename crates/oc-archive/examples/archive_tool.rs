//! Developer tool: `list <file>`, `convert <in> <out> [level] [password]`,
//! `extract <in> <dir>`, `pack <dir> <out>`. Used by tools/archive_interop.py.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout
)]
use oc_archive::{format, safety, Context, EntryKind, Format, WriteOpts};
use oc_core::{Limits, NoProgress};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let limits = Limits::default();
    let pw = std::env::var("OC_PASSWORD").ok();
    let cx = Context {
        limits: &limits,
        progress: &NoProgress,
        password: pw.as_deref(),
        list_only: std::env::var("OC_LIST_ONLY").is_ok(),
    };
    let load = |p: &str| -> oc_archive::Archive {
        let data = std::fs::read(p).unwrap();
        let f = Format::from_name(p)
            .or_else(|| Format::sniff(&data))
            .expect("unknown format");
        match format::read(f, &data, &cx, p.rsplit('/').next()) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("error: {} ({})", e.message, e.code.as_str());
                std::process::exit(1)
            }
        }
    };
    match a[1].as_str() {
        "list" => {
            for e in &load(&a[2]).entries {
                let k = match e.kind {
                    EntryKind::File => "f",
                    EntryKind::Dir => "d",
                    EntryKind::Symlink => "l",
                };
                println!(
                    "{k} {:o} {} {} {}",
                    e.mode,
                    e.size(),
                    oc_checksum::crc32(&e.data),
                    e.path
                );
            }
        }
        "convert" => {
            let arc = load(&a[2]);
            let f = Format::from_name(&a[3]).expect("unknown output format");
            let opts = WriteOpts {
                level: a.get(4).and_then(|s| s.parse().ok()),
                password: std::env::var("OC_OUT_PASSWORD").ok(),
                zip_method: std::env::var("OC_ZIP_METHOD")
                    .ok()
                    .map(|m| match m.as_str() {
                        "stored" => oc_archive::zip::Method::Stored,
                        "deflate" => oc_archive::zip::Method::Deflate,
                        "bzip2" => oc_archive::zip::Method::Bzip2,
                        "lzma" => oc_archive::zip::Method::Lzma,
                        "zstd" => oc_archive::zip::Method::Zstd,
                        "xz" => oc_archive::zip::Method::Xz,
                        _ => oc_archive::zip::Method::Auto,
                    }),
                sevenz_method: std::env::var("OC_7Z_METHOD")
                    .ok()
                    .map(|m| match m.as_str() {
                        "lzma" => oc_archive::sevenz::Method::Lzma,
                        "bzip2" => oc_archive::sevenz::Method::Bzip2,
                        "deflate" => oc_archive::sevenz::Method::Deflate,
                        "zstd" => oc_archive::sevenz::Method::Zstd,
                        "copy" => oc_archive::sevenz::Method::Copy,
                        _ => oc_archive::sevenz::Method::Lzma2,
                    }),
                zip_encryption: std::env::var("OC_ZIP_ENC").ok().map(|m| match m.as_str() {
                    "zipcrypto" => oc_archive::zip::Encryption::ZipCrypto,
                    "aes128" => oc_archive::zip::Encryption::Aes128,
                    _ => oc_archive::zip::Encryption::Aes256,
                }),
                ..WriteOpts::default()
            };
            let out = format::write(f, &arc, &opts, &cx).unwrap();
            std::fs::write(&a[3], out).unwrap();
        }
        "extract" => {
            let rep = safety::extract(
                &load(&a[2]),
                std::path::Path::new(&a[3]),
                &safety::ExtractOptions {
                    overwrite: true,
                    ..Default::default()
                },
            )
            .unwrap();
            println!(
                "{} files, {} dirs, {} links, skipped {:?}",
                rep.files, rep.dirs, rep.symlinks, rep.skipped
            );
        }
        "pack" => {
            let arc = safety::collect_dir(std::path::Path::new(&a[2])).unwrap();
            let f = Format::from_name(&a[3]).expect("unknown output format");
            std::fs::write(
                &a[3],
                format::write(f, &arc, &WriteOpts::default(), &cx).unwrap(),
            )
            .unwrap();
        }
        _ => eprintln!("usage"),
    }
}
