//! Embeds the shared `ui/` folder so the server is one file.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let ui = manifest.join("../../ui");
    println!("cargo:rerun-if-changed={}", ui.display());
    let mut files = Vec::new();
    walk(&ui, &mut files);
    let mut src = String::from("pub static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for f in files {
        let rel = f
            .strip_prefix(&ui)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let _ = writeln!(
            src,
            "    ({:?}, {:?}, include_bytes!({:?})),",
            rel,
            mime(&rel),
            f.canonicalize().unwrap_or(f.clone()).display().to_string()
        );
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("assets.rs");
    let _ = std::fs::write(out, src);
}
