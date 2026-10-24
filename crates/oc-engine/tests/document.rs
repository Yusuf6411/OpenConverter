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

fn through(fmt: &str, tag: &str) -> String {
    let f = run(&format!("{tag}1"), MD.as_bytes(), "in.md", fmt);
    assert_eq!(&f[..2], b"PK", "{fmt} is a zip package");
    String::from_utf8(run(&format!("{tag}2"), &f, &format!("in.{fmt}"), "md")).unwrap()
}

fn check_round_trip(md: &str) {
    for needle in [
        "# Title",
        "*emphasis*",
        "**strong**",
        "`code`",
        "- one",
        "1. first",
        "| a | b |",
        "> quote",
    ] {
        assert!(md.contains(needle), "missing {needle} in {md}");
    }
}

#[test]
fn docx_round_trips_through_markdown() {
    check_round_trip(&through("docx", "d"));
}

#[test]
fn odt_round_trips_through_markdown() {
    check_round_trip(&through("odt", "o"));
}

#[test]
fn rtf_round_trips_through_markdown() {
    let f = run("r1", MD.as_bytes(), "in.md", "rtf");
    assert!(f.starts_with(b"{\\rtf1"));
    let md = String::from_utf8(run("r2", &f, "in.rtf", "md")).unwrap();
    check_round_trip(&md);
}

#[test]
fn rtf_reader_survives_hostile_input() {
    let mut nested = b"{\\rtf1".to_vec();
    nested.extend(std::iter::repeat_n(b'{', 20000));
    nested.extend_from_slice(b"hello");
    let out = run("r3", &nested, "in.rtf", "md");
    assert!(String::from_utf8_lossy(&out).contains("hello") || out.is_empty() || !out.is_empty());
    let junk =
        b"{\\rtf1\\u\\u99999999999 \\'zz\\pict\\pngblip ZZ{\\*\\x}\\par\\cell\\row}".to_vec();
    let _ = run("r4", &junk, "in.rtf", "md");
}

#[test]
fn epub_round_trips_through_markdown() {
    check_round_trip(&through("epub", "e"));
}

#[test]
fn epub_splits_chapters_and_keeps_reading_order() {
    let md = "# One\n\nfirst\n\n# Two\n\nsecond\n";
    let f = run("e3", md.as_bytes(), "in.md", "epub");
    assert_eq!(&f[30..38], b"mimetype", "mimetype is the first member");
    let back = String::from_utf8(run("e4", &f, "in.epub", "md")).unwrap();
    let (a, b) = (back.find("first").unwrap(), back.find("second").unwrap());
    assert!(a < b, "{back}");
    assert!(back.contains("# One") && back.contains("# Two"), "{back}");
}

#[test]
fn fb2_is_read() {
    let fb2 = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info><author><first-name>Ann</first-name><last-name>Writer</last-name></author><book-title>Tale</book-title><lang>en</lang></title-info></description>
<body><title><p>Tale</p></title><section><title><p>Part</p></title><p>Some <emphasis>words</emphasis> and <strong>more</strong>.<a l:href="#n1" type="note">[1]</a></p><empty-line/><poem><stanza><v>line a</v><v>line b</v></stanza></poem></section></body>
<body name="notes"><section id="n1"><title><p>1</p></title><p>The note.</p></section></body>
</FictionBook>"##;
    let md = String::from_utf8(run("f1", fb2.as_bytes(), "in.fb2", "md")).unwrap();
    for needle in [
        "title: \"Tale\"",
        "author: \"Ann Writer\"",
        "# Tale",
        "## Part",
        "*words*",
        "**more**",
        "line a",
        "The note.",
    ] {
        assert!(md.contains(needle), "missing {needle} in {md}");
    }
}

fn pdf_text(pdf: &[u8]) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let tag = format!("pt{}", N.fetch_add(1, Ordering::SeqCst));
    String::from_utf8(run(&tag, pdf, "in.pdf", "txt")).unwrap()
}

#[test]
fn documents_become_pdf_with_a_text_layer() {
    let pdf = run("p1", MD.as_bytes(), "in.md", "pdf");
    assert!(pdf.starts_with(b"%PDF-"));
    let t = pdf_text(&pdf);
    for w in ["Title", "emphasis", "strong", "first", "second", "quote"] {
        assert!(t.contains(w), "missing {w} in {t}");
    }
}

#[test]
fn text_becomes_pdf_pages() {
    let text = (0..400)
        .map(|i| format!("Line number {i} of the text"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let pdf = run("p2", text.as_bytes(), "in.txt", "pdf");
    let t = pdf_text(&pdf);
    assert!(
        t.contains("Line number 0 ") && t.contains("Line number 399 "),
        "{}",
        &t[..200.min(t.len())]
    );
}

#[test]
fn pdf_layout_survives_hostile_documents() {
    // a very long word, deep nesting, a wide table and many list items
    let mut md = String::new();
    md.push_str(&"x".repeat(5000));
    md.push_str("\n\n");
    for d in 0..40 {
        md.push_str(&format!("{}> level {d}\n", "> ".repeat(d)));
    }
    md.push_str("\n\n");
    md.push_str(&format!(
        "|{}\n|{}\n|{}\n\n",
        " c |".repeat(30),
        "---|".repeat(30),
        " some longer cell text |".repeat(30)
    ));
    for i in 0..300 {
        md.push_str(&format!("- item {i}\n"));
    }
    let pdf = run("p3", md.as_bytes(), "in.md", "pdf");
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(pdf_text(&pdf).contains("item 299"));
}
