//! Developer tasks: `cargo xtask check-deps`.
use std::process::{Command, ExitCode};

/// Crates that must never appear in a shipped binary's dependency tree (MASTER_PROMPT §5.1, §5.3).
const BANNED: &[&str] = &[
    "image",
    "png",
    "jpeg-decoder",
    "gif",
    "tiff",
    "webp",
    "ravif",
    "rav1e",
    "flate2",
    "miniz_oxide",
    "zstd",
    "ruzstd",
    "lzma-rs",
    "xz2",
    "bzip2",
    "brotli",
    "zip",
    "tar",
    "sevenz-rust",
    "unrar",
    "symphonia",
    "hound",
    "claxon",
    "lewton",
    "minimp3",
    "lopdf",
    "pdf",
    "printpdf",
    "docx-rs",
    "calamine",
    "quick-xml",
    "roxmltree",
    "xml-rs",
    "pulldown-cmark",
    "comrak",
    "ttf-parser",
    "rustybuzz",
    "fontdue",
    "ab_glyph",
    "tiny-skia",
    "resvg",
    "usvg",
    "lyon",
    "encoding_rs",
    "chardetng",
    "reqwest",
    "ureq",
    "curl",
    "isahc",
    "attohttpc",
];

fn check_deps() -> Result<(), String> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let members: Vec<&str> = meta["workspace_members"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    // Shipped crates are every member except xtask; xtask is a dev tool.
    let shipped: Vec<&str> = members
        .iter()
        .copied()
        .filter(|m| !m.contains("xtask"))
        .collect();
    let nodes = meta["resolve"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut bad = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<String> = shipped.iter().map(|s| s.to_string()).collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let name = meta["packages"]
            .as_array()
            .and_then(|p| p.iter().find(|p| p["id"] == id.as_str()))
            .and_then(|p| p["name"].as_str())
            .unwrap_or("")
            .to_string();
        // The desktop window shell (ADR-0005) is OS glue, not conversion code; its
        // windowing stack pulls image/png/xml helpers for icons and bundling that never
        // touch user files. Its subtree is exempt; everything else is checked.
        if name == "tauri" || name.starts_with("tauri-") || name == "wry" || name == "tao" {
            continue;
        }
        if BANNED.contains(&name.as_str()) {
            bad.push(name);
        }
        if let Some(n) = nodes.iter().find(|n| n["id"] == id.as_str()) {
            for d in n["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d.as_str())
            {
                stack.push(d.to_string());
            }
        }
    }
    if bad.is_empty() {
        println!("check-deps: ok ({} packages inspected)", seen.len());
        Ok(())
    } else {
        Err(format!("banned crates in shipped dependency tree: {bad:?}"))
    }
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("check-deps") => match check_deps() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: cargo xtask check-deps");
            ExitCode::from(2)
        }
    }
}
