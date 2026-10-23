//! Document routes: Markdown, HTML and text through the document model.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::NoProgress;
use oc_engine::{ConvertRequest, Engine};
use std::path::PathBuf;

fn run(tag: &str, input: &[u8], name: &str, to: &str) -> Vec<u8> {
    let d: PathBuf = std::env::temp_dir().join(format!("oc-doc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let i = d.join(name);
    let o = d.join(format!("out.{to}"));
    std::fs::write(&i, input).unwrap();
    let e = Engine::new();
    let options = e.parse_options_for(&i, &o, &[]).unwrap();
    e.convert(
        &ConvertRequest {
            input: i,
            output: o.clone(),
            overwrite: true,
            display_name: None,
            options,
        },
        &NoProgress,
    )
    .unwrap();
    let out = std::fs::read(&o).unwrap();
    let _ = std::fs::remove_dir_all(d);
    out
}

const MD: &str = "# Title\n\nSome *emphasis*, **strong** and `code`.\n\n- one\n- two\n\n1. first\n2. second\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quote\n";

#[test]
fn markdown_to_html_keeps_structure() {
    let h = String::from_utf8(run("a", MD.as_bytes(), "in.md", "html")).unwrap();
    for needle in [
        "<h1>Title</h1>",
        "<em>emphasis</em>",
        "<strong>strong</strong>",
        "<li>one</li>",
        "<ol>",
        "<th>a</th>",
        "<blockquote>",
    ] {
        assert!(h.contains(needle), "missing {needle} in {h}");
    }
}

#[test]
fn html_round_trips_through_markdown() {
    let h = run("b", MD.as_bytes(), "in.md", "html");
    let md = String::from_utf8(run("c", &h, "in.html", "md")).unwrap();
    assert!(md.contains("# Title"), "{md}");
    assert!(
        md.contains("*emphasis*") && md.contains("**strong**"),
        "{md}"
    );
    assert!(md.contains("| a | b |"), "{md}");
}

#[test]
fn markdown_to_text_drops_markup() {
    let t = String::from_utf8(run("d", MD.as_bytes(), "in.md", "txt")).unwrap();
    assert!(t.contains("Some emphasis, strong and code."), "{t}");
    assert!(!t.contains('*') && !t.contains('#'), "{t}");
}

#[test]
fn text_becomes_paragraphs() {
    let h = String::from_utf8(run("e", b"first\n\nsecond\nline\n", "in.txt", "html")).unwrap();
    assert_eq!(h.matches("<p>").count(), 2, "{h}");
}

#[test]
fn embedded_pictures_survive_html_to_markdown() {
    let html = "<p>x <img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"dot\"></p>";
    let md = String::from_utf8(run("f", html.as_bytes(), "in.html", "md")).unwrap();
    assert!(
        md.contains("![dot](data:image/png;base64,iVBORw0KGgo=)"),
        "{md}"
    );
}
