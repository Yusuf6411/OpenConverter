//! The CommonMark specification examples (all must pass) and the GitHub flavoured ones (a
//! recorded number of known differences, because that spec is based on CommonMark 0.29).
//! The spec files are fetched by `tools/fetch_commonmark_spec.sh`; without them the test
//! checks nothing and says so.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::print_stdout)]
use oc_document::{html_out, markdown};
use std::path::PathBuf;

fn norm(s: &str) -> String {
    s.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn run(path: &str, github: bool) -> Option<(usize, usize)> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/spec")
        .join(path);
    let text = std::fs::read_to_string(p).ok()?;
    let (mut pass, mut fail) = (0, 0);
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if !l.starts_with("```````````````````````````````` example") {
            continue;
        }
        let ext = l.contains("table")
            || l.contains("strikethrough")
            || l.contains("autolink")
            || l.contains("tasklist");
        let (mut md, mut html, mut in_html) = (String::new(), String::new(), false);
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
        let md = md.replace('→', "\t");
        let html = html.replace('→', "\t");
        let doc = markdown::read(
            &md,
            &markdown::ReadOptions {
                github: github || ext,
            },
        );
        if norm(&html_out::fragment(&doc)) == norm(&html) {
            pass += 1;
        } else {
            fail += 1;
        }
    }
    Some((pass, fail))
}

#[test]
fn commonmark_spec_examples() {
    match run("commonmark.txt", false) {
        Some((pass, fail)) => {
            assert_eq!(
                fail,
                0,
                "{fail} of {} CommonMark examples fail",
                pass + fail
            );
            assert!(pass >= 650);
        }
        None => println!("spec file not present; run tools/fetch_commonmark_spec.sh"),
    }
}

#[test]
fn github_spec_examples_stay_within_the_known_differences() {
    if let Some((pass, fail)) = run("gfm.txt", true) {
        // Emphasis rules of CommonMark 0.29 vs 0.31, the task list markup, one disallowed-HTML case.
        assert!(fail <= 16, "{fail} GFM examples fail (of {})", pass + fail);
    }
}
