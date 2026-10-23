#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_document::{html_in, html_out, markdown, md_out, text_io, Block, Inline};

#[test]
fn markdown_round_trip_keeps_blocks() {
    let src = "# T\n\npara *e* **s** ~~x~~\n\n1. a\n2. b\n\n```rust\nfn main() {}\n```\n";
    let doc = markdown::read(src, &markdown::ReadOptions { github: true });
    let id = |s: &str| s.to_string();
    let out = md_out::write(&doc, &md_out::WriteOptions { image_src: &id });
    let again = markdown::read(&out, &markdown::ReadOptions { github: true });
    assert_eq!(doc.blocks, again.blocks, "{out}");
}

#[test]
fn html_reader_handles_untidy_markup() {
    let mut notes = oc_core::Notes::new();
    let doc = html_in::read(
        "<ul><li>one<li>two</ul><p>a &amp; b<p>second &nbsp;para <b>bold<i>both</b>x",
        &mut notes,
        100,
    );
    assert!(matches!(&doc.blocks[0], Block::List { items, .. } if items.len() == 2));
    assert!(matches!(&doc.blocks[1], Block::Paragraph(c) if c[0] == Inline::Text("a & b".into())));
    assert_eq!(doc.blocks.len(), 3);
}

#[test]
fn html_entities_and_legacy_names() {
    assert_eq!(
        html_in::decode_entities("&copy 2024 &amp &lt;b&gt; &#x41;&#66; &hellip;"),
        "© 2024 & <b> AB …"
    );
}

#[test]
fn hostile_nesting_does_not_overflow() {
    let deep = "<div>".repeat(5000) + "x";
    let mut notes = oc_core::Notes::new();
    let doc = html_in::read(&deep, &mut notes, 100);
    assert!(!doc.blocks.is_empty());
    let q = "> ".repeat(5000) + "x";
    let d = markdown::read(&q, &markdown::ReadOptions { github: true });
    let _ = html_out::fragment(&d);
    for src in [
        format!("{}a{}", "*".repeat(5000), "*".repeat(5000)),
        format!("{}a{}", "[".repeat(3000), "](x)".repeat(3000)),
        format!("{}a{}", "![".repeat(3000), "](x)".repeat(3000)),
        "- ".repeat(5000) + "x",
        "<b>".repeat(5000) + "x",
    ] {
        let d = markdown::read(&src, &markdown::ReadOptions { github: true });
        let _ = html_out::fragment(&d);
        let id = |s: &str| s.to_string();
        let _ = md_out::write(&d, &md_out::WriteOptions { image_src: &id });
        let mut n = oc_core::Notes::new();
        let d = html_in::read(&src, &mut n, 100);
        let _ = html_out::fragment(&d);
    }
}

#[test]
fn text_blocks() {
    let d = text_io::read("a\nb\n\nc\n");
    assert_eq!(d.blocks.len(), 2);
    assert_eq!(text_io::write(&d), "a\nb\n\nc\n");
}
