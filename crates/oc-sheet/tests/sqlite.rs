#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Notes;
use oc_sheet::{sqlite, Sheet, Table, TableLimits, Workbook};

fn book(tables: usize, rows: usize) -> Workbook {
    Workbook {
        sheets: (0..tables)
            .map(|t| Sheet {
                name: format!("t{t}"),
                table: Table::new(
                    Some(vec!["id".into(), "name".into(), "x".into()]),
                    (0..rows)
                        .map(|r| {
                            vec![
                                r.to_string(),
                                format!("name {r} {}", "é".repeat(r % 7)),
                                format!("{}.5", r),
                            ]
                        })
                        .collect(),
                ),
                ..Sheet::default()
            })
            .collect(),
        date1904: false,
    }
}

#[test]
fn round_trip_with_many_tables_and_rows() {
    let b = book(300, 3);
    let bytes = sqlite::write(&b, true, &mut Notes::new()).unwrap();
    let back = sqlite::read(&bytes, &TableLimits::default(), true, &mut Notes::new()).unwrap();
    assert_eq!(back.sheets.len(), 300);
    assert_eq!(back.sheets[299].table.rows, b.sheets[299].table.rows);
    let b = book(2, 6000);
    let bytes = sqlite::write(&b, true, &mut Notes::new()).unwrap();
    let back = sqlite::read(&bytes, &TableLimits::default(), true, &mut Notes::new()).unwrap();
    assert_eq!(back.sheets[1].table.rows, b.sheets[1].table.rows);
}

#[test]
fn damaged_files_do_not_panic() {
    let bytes = sqlite::write(&book(3, 500), true, &mut Notes::new()).unwrap();
    let mut s = 12345u64;
    for _ in 0..300 {
        let mut m = bytes.clone();
        for _ in 0..6 {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let i = (s >> 33) as usize % m.len();
            m[i] ^= (s >> 20) as u8 | 1;
        }
        let _ = sqlite::read(&m, &TableLimits::default(), true, &mut Notes::new());
    }
    let _ = sqlite::read(
        &bytes[..5000],
        &TableLimits::default(),
        true,
        &mut Notes::new(),
    );
}
