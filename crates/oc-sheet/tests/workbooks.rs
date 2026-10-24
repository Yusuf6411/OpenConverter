#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used)]
use oc_core::{Limits, NoProgress, Notes};
use oc_sheet::{ods, xlsx, Sheet, Table, Workbook};

fn sample() -> Workbook {
    let s = |v: &str| v.to_string();
    let mut first = Sheet {
        name: s("Data"),
        table: Table::new(
            Some(vec![s("name"), s("qty"), s("when"), s("note")]),
            vec![
                vec![s("Alpha"), s("3"), s("2024-01-05"), s("line1\nline2")],
                vec![
                    s("Beta"),
                    s("-1.5"),
                    s("2024-02-29 13:45:10"),
                    s("  padded  "),
                ],
                vec![s("Gamma"), s("TRUE"), s(""), s("ünï 日本 & <tags>")],
                vec![s("merged"), s(""), s(""), s("")],
            ],
        ),
        ..Sheet::default()
    };
    first.formulas.insert((2, 1), "SUM(B2:B3)".into());
    first.merges.push((4, 0, 4, 2));
    let second = Sheet {
        name: s("Second"),
        table: Table::new(None, vec![vec![s("x"), s("1")], vec![s("y"), s("2")]]),
        ..Sheet::default()
    };
    Workbook {
        sheets: vec![first, second],
        date1904: false,
    }
}

fn limits() -> Limits {
    Limits {
        max_alloc: 1 << 26,
        max_output: 1 << 26,
        max_entries: 100_000,
        ..Limits::default()
    }
}

#[test]
fn xlsx_roundtrip_keeps_cells_formulas_and_merges() {
    let book = sample();
    let mut notes = Notes::new();
    let o = xlsx::WriteOptions {
        detect_types: true,
        detect_dates: true,
        style_header: true,
    };
    let bytes = xlsx::write(&book, &o, &limits(), &NoProgress, &mut notes).unwrap();
    assert_eq!(&bytes[..2], b"PK");
    let back = xlsx::read(&bytes, &limits(), &NoProgress, true, &mut notes).unwrap();
    assert_eq!(back.sheets.len(), 2);
    assert_eq!(back.sheets[0].table, book.sheets[0].table);
    assert_eq!(back.sheets[0].formulas, book.sheets[0].formulas);
    assert_eq!(back.sheets[0].merges, book.sheets[0].merges);
    // No header requested: the first row is data.
    let flat = xlsx::read(&bytes, &limits(), &NoProgress, false, &mut notes).unwrap();
    assert_eq!(flat.sheets[1].table.rows, book.sheets[1].table.rows);
}

#[test]
fn ods_roundtrip_keeps_cells_formulas_and_merges() {
    let book = sample();
    let mut notes = Notes::new();
    let o = ods::WriteOptions {
        detect_types: true,
        detect_dates: true,
        style_header: true,
    };
    let bytes = ods::write(&book, &o, &limits(), &NoProgress, &mut notes).unwrap();
    let back = ods::read(&bytes, &limits(), &NoProgress, true, &mut notes).unwrap();
    assert_eq!(back.sheets.len(), 2);
    assert_eq!(back.sheets[0].table, book.sheets[0].table);
    assert_eq!(back.sheets[0].formulas, book.sheets[0].formulas);
    assert_eq!(back.sheets[0].merges, book.sheets[0].merges);
    assert_eq!(back.sheets[1].name, "Second");
}

#[test]
fn types_off_keeps_everything_as_text() {
    let book = sample();
    let mut notes = Notes::new();
    let o = xlsx::WriteOptions {
        detect_types: false,
        detect_dates: false,
        style_header: false,
    };
    let bytes = xlsx::write(&book, &o, &limits(), &NoProgress, &mut notes).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("<v>3</v>"));
    let back = xlsx::read(&bytes, &limits(), &NoProgress, true, &mut notes).unwrap();
    assert_eq!(back.sheets[0].table.rows[0][2], "2024-01-05");
}

#[test]
fn oversized_tables_are_refused() {
    let wide = Table::new(None, vec![vec!["x".to_string(); 16_385]]);
    let mut notes = Notes::new();
    let o = xlsx::WriteOptions {
        detect_types: true,
        detect_dates: true,
        style_header: true,
    };
    let err = xlsx::write(
        &Workbook::single(wide, "w"),
        &o,
        &limits(),
        &NoProgress,
        &mut notes,
    )
    .unwrap_err();
    assert_eq!(err.code, oc_core::ErrorCode::LimitExceeded);
}

#[test]
fn damaged_workbooks_give_errors_not_panics() {
    let book = sample();
    let mut notes = Notes::new();
    let xo = xlsx::WriteOptions {
        detect_types: true,
        detect_dates: true,
        style_header: true,
    };
    let oo = ods::WriteOptions {
        detect_types: true,
        detect_dates: true,
        style_header: true,
    };
    let seeds = [
        (
            true,
            xlsx::write(&book, &xo, &limits(), &NoProgress, &mut notes).unwrap(),
        ),
        (
            false,
            ods::write(&book, &oo, &limits(), &NoProgress, &mut notes).unwrap(),
        ),
    ];
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for (is_xlsx, seed) in &seeds {
        for _ in 0..300 {
            let mut d = seed.clone();
            for _ in 0..=(next() % 3) {
                let at = (next() as usize) % d.len();
                match next() % 3 {
                    0 => d[at] ^= 1 << (next() % 8),
                    1 => d[at] = next() as u8,
                    _ => d.truncate(at.max(1)),
                }
            }
            let mut n = Notes::new();
            let _ = if *is_xlsx {
                xlsx::read(&d, &limits(), &NoProgress, true, &mut n)
            } else {
                ods::read(&d, &limits(), &NoProgress, true, &mut n)
            };
        }
    }
}

#[test]
fn xml_bombs_in_cells_are_bounded() {
    // A sheet whose rows repeat millions of times must hit the cell limit, not memory.
    let content = r#"<?xml version="1.0"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:spreadsheet><table:table table:name="s"><table:table-row table:number-rows-repeated="5000000"><table:table-cell table:number-columns-repeated="5000"><text:p>x</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#;
    let mut a = oc_archive::Archive::default();
    a.entries.push(oc_archive::Entry::file(
        "content.xml",
        content.as_bytes().to_vec(),
    ));
    let l = limits();
    let cx = oc_archive::Context {
        limits: &l,
        progress: &NoProgress,
        password: None,
        list_only: false,
    };
    let zip = oc_archive::format::write(
        oc_archive::Format::Zip,
        &a,
        &oc_archive::WriteOpts::default(),
        &cx,
    )
    .unwrap();
    let mut n = Notes::new();
    let err = ods::read(&zip, &l, &NoProgress, true, &mut n).unwrap_err();
    assert_eq!(err.code, oc_core::ErrorCode::LimitExceeded);
}
