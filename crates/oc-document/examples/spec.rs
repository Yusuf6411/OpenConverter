//! Run CommonMark / GFM spec examples: `spec <spec.txt> [gfm] [-v] [number...]`.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::print_stdout)]
use oc_document::{html_out, markdown};

fn norm(s: &str) -> String {
    // whitespace between block tags does not matter
    let mut out = String::new();
    let mut last_close = false;
    for line in s.lines() {
        let t = line.trim_end();
        if !t.is_empty() {
            out.push_str(t);
            out.push('\n');
        }
        let _ = &mut last_close;
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let gfm = args.iter().any(|a| a == "gfm");
    let verbose = args.iter().any(|a| a == "-v");
    let only: Vec<usize> = args.iter().skip(2).filter_map(|a| a.parse().ok()).collect();
    let mut n = 0;
    let (mut pass, mut fail) = (0, 0);
    let mut section = String::new();
    let mut lines = text.lines();
    let mut by_section: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
    while let Some(l) = lines.next() {
        if l.starts_with('#') {
            section = l.trim_start_matches('#').trim().to_string();
        }
        if l.starts_with("```````````````````````````````` example") {
            let ext = l.contains("table") || l.contains("strikethrough") || l.contains("autolink") || l.contains("tasklist") || l.contains("disabled");
            let mut md = String::new();
            let mut html = String::new();
            let mut in_html = false;
            for l in lines.by_ref() {
                if l.starts_with("````````````````````````````````") {
                    break;
                }
                if l == "." {
                    in_html = true;
                } else if in_html {
                    html.push_str(l);
                    html.push('\n');
                } else {
                    md.push_str(l);
                    md.push('\n');
                }
            }
            n += 1;
            if !only.is_empty() && !only.contains(&n) {
                continue;
            }
            let md = md.replace('→', "\t");
            let html = html.replace('→', "\t");
            let doc = markdown::read(&md, &markdown::ReadOptions { github: gfm || ext });
            let got = html_out::fragment(&doc);
            let e = by_section.entry(section.clone()).or_default();
            if norm(&got) == norm(&html) {
                pass += 1;
                e.0 += 1;
            } else {
                fail += 1;
                e.1 += 1;
                if verbose || !only.is_empty() {
                    println!("--- #{n} [{section}]\n{md:?}\nexpected:\n{html}got:\n{got}");
                } else {
                    println!("FAIL #{n} [{section}]");
                }
            }
        }
    }
    println!("pass {pass} fail {fail}");
    for (s, (p, f)) in by_section {
        if f > 0 {
            println!("  {s}: {f} failing of {}", p + f);
        }
    }
}
