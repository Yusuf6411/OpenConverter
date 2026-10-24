#![allow(clippy::unwrap_used)]
//! The UI may not build HTML from strings, run inline scripts, evaluate code or
//! mention remote addresses (other than the SVG namespace). Content-Security-Policy
//! backs this up at run time; this keeps the source honest.
use std::path::{Path, PathBuf};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn ui_source_has_no_unsafe_patterns() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    let mut all = Vec::new();
    files(&root, &mut all);
    assert!(!all.is_empty(), "ui/ not found");
    let mut bad = Vec::new();
    for p in all {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let is_html = p.extension().is_some_and(|e| e == "html");
        let is_js = p.extension().is_some_and(|e| e == "js");
        let mut hit = |what: &str| bad.push(format!("{}: {what}", p.display()));
        if is_js {
            for pat in [
                "innerHTML",
                "outerHTML",
                "insertAdjacentHTML",
                "document.write",
                "eval(",
                "new Function",
            ] {
                if text.contains(pat) {
                    hit(pat);
                }
            }
        }
        if is_html {
            if text.contains("<script>") || text.contains("javascript:") {
                hit("inline script");
            }
            for attr in [" onclick=", " onload=", " onerror=", " style="] {
                if text.contains(attr) {
                    hit(attr);
                }
            }
        }
        for line in text.lines() {
            let mut rest = line;
            while let Some(i) = rest.find("http") {
                let tail = &rest[i..];
                if (tail.starts_with("http://") || tail.starts_with("https://"))
                    && !tail.contains("www.w3.org/")
                {
                    hit(&format!(
                        "remote address: {}",
                        tail.chars().take(40).collect::<String>()
                    ));
                }
                rest = &rest[i + 4..];
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn translation_catalogs_match_the_page() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    let html = std::fs::read_to_string(root.join("index.html")).unwrap();
    let keys = |text: &str| -> std::collections::BTreeSet<String> {
        // flat string-to-string JSON written by tools/i18n.py: one `"key": "value"` per line
        text.lines()
            .filter_map(|l| l.trim().strip_prefix('"'))
            .filter_map(|l| l.split_once("\": "))
            .map(|(k, _)| k.to_string())
            .collect()
    };
    let en = keys(&std::fs::read_to_string(root.join("locales/en.json")).unwrap());
    let qps = keys(&std::fs::read_to_string(root.join("locales/qps.json")).unwrap());
    assert!(!en.is_empty());
    assert_eq!(
        en, qps,
        "pseudo-locale out of date: run tools/i18n.py pseudo"
    );
    for part in html.split("data-i18n=\"").skip(1) {
        let key = part.split('"').next().unwrap();
        assert!(
            en.contains(key),
            "index.html uses {key}, missing from en.json"
        );
    }
}
