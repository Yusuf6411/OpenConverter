//! Document to EPUB 3 (with an EPUB 2 navigation file for older readers).
use crate::html_out::{self, Ctx};
use crate::model::{plain, Block, Document};
use crate::zipio;
use oc_core::{Limits, Result};
use oc_xml::{escape_attr, escape_text};

const CSS: &str = "body{font-family:serif;line-height:1.45;margin:5%}h1,h2,h3,h4,h5,h6{font-family:sans-serif;line-height:1.2}pre,code{font-family:monospace}pre{white-space:pre-wrap;background:#f4f4f4;padding:.6em}blockquote{margin-left:1.2em;padding-left:.8em;border-left:3px solid #ccc;color:#444}table{border-collapse:collapse}td,th{border:1px solid #888;padding:.25em .5em}img{max-width:100%}\n";

fn hash(doc: &Document) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |s: &str| {
        for b in s.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    feed(doc.meta.title.as_deref().unwrap_or(""));
    for b in &doc.blocks {
        match b {
            Block::Heading { content, .. } | Block::Paragraph(content) => feed(&plain(content)),
            Block::Code { text, .. } => feed(text),
            _ => feed("."),
        }
    }
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (h >> 32) as u32,
        (h >> 16) as u16,
        h & 0xfff,
        (h >> 4) & 0xfff,
        h & 0xffff_ffff_ffff
    )
}

struct Chapter {
    title: String,
    blocks: Vec<Block>,
}

fn split(doc: &Document) -> Vec<Chapter> {
    let title = doc
        .meta
        .title
        .clone()
        .unwrap_or_else(|| "Document".to_string());
    let min = doc
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Heading { level, .. } => Some(*level),
            _ => None,
        })
        .min();
    let level = match min {
        Some(l) if l <= 2 => l,
        _ => {
            return vec![Chapter {
                title,
                blocks: doc.blocks.clone(),
            }]
        }
    };
    let mut out: Vec<Chapter> = Vec::new();
    for b in &doc.blocks {
        match b {
            Block::Heading { level: l, content } if *l == level => {
                out.push(Chapter {
                    title: plain(content),
                    blocks: vec![b.clone()],
                });
            }
            Block::PageBreak => {}
            _ => match out.last_mut() {
                Some(c) => c.blocks.push(b.clone()),
                None => out.push(Chapter {
                    title: title.clone(),
                    blocks: vec![b.clone()],
                }),
            },
        }
    }
    if out.is_empty() {
        out.push(Chapter {
            title,
            blocks: vec![],
        });
    }
    out
}

fn xhtml(title: &str, lang: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" lang=\"{l}\" xml:lang=\"{l}\">\n<head>\n<meta charset=\"utf-8\" />\n<title>{t}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\" />\n</head>\n<body>\n{body}</body>\n</html>\n",
        l = escape_attr(lang),
        t = escape_text(title)
    )
}

pub fn write(doc: &Document, limits: &Limits) -> Result<Vec<u8>> {
    let mut doc = doc.clone();
    crate::model::strip_html(&mut doc.blocks);
    let lang = doc
        .meta
        .language
        .clone()
        .unwrap_or_else(|| "en".to_string());
    let title = doc
        .meta
        .title
        .clone()
        .unwrap_or_else(|| "Untitled".to_string());
    let chapters = split(&doc);
    let id = hash(&doc);

    let src = |s: &str| match doc.image(s) {
        Some(img) => format!("images/{}", img.name),
        None => s.to_string(),
    };
    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("mimetype".into(), b"application/epub+zip".to_vec()),
        (
            "META-INF/container.xml".into(),
            b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>".to_vec(),
        ),
    ];
    let mut manifest = String::new();
    let mut spine = String::new();
    let mut nav = String::new();
    let mut ncx = String::new();
    for (n, ch) in chapters.iter().enumerate() {
        let file = format!("chap{:03}.xhtml", n + 1);
        let cx = Ctx {
            doc: &doc,
            image_src: &src,
        };
        let sub = Document {
            blocks: ch.blocks.clone(),
            images: doc.images.clone(),
            meta: doc.meta.clone(),
        };
        let cx = Ctx { doc: &sub, ..cx };
        let body = html_out::fragment_with(&cx);
        entries.push((
            format!("OEBPS/{file}"),
            xhtml(&ch.title, &lang, &body).into_bytes(),
        ));
        manifest.push_str(&format!(
            "<item id=\"c{n}\" href=\"{file}\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        spine.push_str(&format!("<itemref idref=\"c{n}\"/>\n"));
        nav.push_str(&format!(
            "<li><a href=\"{file}\">{}</a></li>\n",
            escape_text(&ch.title)
        ));
        ncx.push_str(&format!(
            "<navPoint id=\"n{n}\" playOrder=\"{p}\"><navLabel><text>{}</text></navLabel><content src=\"{file}\"/></navPoint>\n",
            escape_text(&ch.title),
            p = n + 1
        ));
    }
    for (n, img) in doc.images.iter().enumerate() {
        manifest.push_str(&format!(
            "<item id=\"i{n}\" href=\"images/{}\" media-type=\"{}\"/>\n",
            escape_attr(&img.name),
            escape_attr(&img.mime)
        ));
        entries.push((format!("OEBPS/images/{}", img.name), img.data.clone()));
    }
    entries.push(("OEBPS/style.css".into(), CSS.as_bytes().to_vec()));
    let navdoc = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" lang=\"{l}\" xml:lang=\"{l}\"><head><meta charset=\"utf-8\" /><title>{t}</title></head><body><nav epub:type=\"toc\" id=\"toc\"><h1>{t}</h1><ol>\n{nav}</ol></nav></body></html>\n",
        l = escape_attr(&lang),
        t = escape_text(&title)
    );
    entries.push(("OEBPS/nav.xhtml".into(), navdoc.into_bytes()));
    let ncxdoc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\"><head><meta name=\"dtb:uid\" content=\"urn:uuid:{id}\"/><meta name=\"dtb:depth\" content=\"1\"/></head><docTitle><text>{t}</text></docTitle><navMap>\n{ncx}</navMap></ncx>\n",
        t = escape_text(&title)
    );
    entries.push(("OEBPS/toc.ncx".into(), ncxdoc.into_bytes()));
    let mut meta = format!(
        "<dc:identifier id=\"bookid\">urn:uuid:{id}</dc:identifier>\n<dc:title>{}</dc:title>\n<dc:language>{}</dc:language>\n",
        escape_text(&title),
        escape_text(&lang)
    );
    if let Some(a) = &doc.meta.author {
        meta.push_str(&format!("<dc:creator>{}</dc:creator>\n", escape_text(a)));
    }
    if let Some(s) = &doc.meta.subject {
        meta.push_str(&format!(
            "<dc:description>{}</dc:description>\n",
            escape_text(s)
        ));
    }
    if let Some(d) = &doc.meta.date {
        meta.push_str(&format!("<dc:date>{}</dc:date>\n", escape_text(d)));
    }
    let modified = doc
        .meta
        .date
        .as_deref()
        .filter(|d| d.len() == 10)
        .map(|d| format!("{d}T00:00:00Z"))
        .unwrap_or_else(|| "2000-01-01T00:00:00Z".to_string());
    meta.push_str(&format!(
        "<meta property=\"dcterms:modified\">{modified}</meta>\n"
    ));
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\" xml:lang=\"{l}\"><metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n{meta}</metadata>\n<manifest>\n<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n<item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n{manifest}</manifest>\n<spine toc=\"ncx\">\n{spine}</spine></package>\n",
        l = escape_attr(&lang)
    );
    entries.push(("OEBPS/content.opf".into(), opf.into_bytes()));
    zipio::write(entries, limits)
}
