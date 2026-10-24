#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::{Limits, NoProgress, Notes};
use oc_sheet::{delimited, json, Table, TableLimits};

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else { return };
    let Ok(text) = std::str::from_utf8(rest) else { return };
    let limits = TableLimits::from(&Limits { max_alloc: 1 << 24, max_entries: 1 << 16, ..Limits::default() });
    let padded = |t: &Table| -> Vec<Vec<String>> { t.rows.iter().map(|r| t.padded(r)).collect() };
    if mode & 1 == 0 {
        let delim = b",;\t|"[usize::from(mode >> 1) & 3];
        let Ok(rows) = delimited::parse(text, delim, &limits, &NoProgress) else { return };
        let table = Table::new(None, rows);
        if table.check_padded_size(&limits).is_err() {
            return;
        }
        let out = delimited::write(&table, delim, mode & 0x80 != 0, &NoProgress).unwrap();
        let again = delimited::parse(&out, delim, &limits, &NoProgress).expect("our own output must parse");
        assert_eq!(again, padded(&table));
        let _ = delimited::detect_delimiter(text, b',');
    } else {
        let Ok(table) = json::read(text, &limits) else { return };
        if table.check_padded_size(&limits).is_err() {
            return;
        }
        let indent = usize::from(mode >> 1) & 3;
        let out = json::write(&table, indent * 2, mode & 0x80 != 0, &mut Notes::new());
        let again = json::read(&out, &limits).expect("our own output must parse");
        assert_eq!(again.rows, padded(&table));
    }
});
