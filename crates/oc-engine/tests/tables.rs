//! CSV, TSV and JSON conversions, ported from the 1.x table tests.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::{ErrorCode, NoProgress};
use oc_engine::{ConvertReport, ConvertRequest, Engine};
use std::path::PathBuf;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("oc-tables-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Converts `input` (as `from`) to `to`, returning the output bytes and the report.
fn conv(
    tag: &str,
    input: &[u8],
    from: &str,
    to: &str,
    opts: &[(&str, &str)],
) -> oc_core::Result<(Vec<u8>, ConvertReport)> {
    let d = dir(tag);
    let i = d.join(format!("in.{from}"));
    let o = d.join(format!("out.{to}"));
    std::fs::write(&i, input).unwrap();
    let e = Engine::new();
    let raw: Vec<(String, String)> = opts
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let options = e.parse_options(&o, &raw)?;
    let report = e.convert(
        &ConvertRequest {
            input: i,
            output: o.clone(),
            overwrite: true,
            options,
        },
        &NoProgress,
    )?;
    let out = std::fs::read(&o).unwrap();
    let _ = std::fs::remove_dir_all(d);
    Ok((out, report))
}

fn out(tag: &str, input: &[u8], from: &str, to: &str, opts: &[(&str, &str)]) -> Vec<u8> {
    conv(tag, input, from, to, opts).unwrap().0
}

fn has_note(r: &ConvertReport, code: &str) -> bool {
    r.notes.iter().any(|n| n.code == code)
}

const CSV: &[u8] =
    b"name,notes\r\nAda,\"likes, commas\nand newlines\"\r\nBob,\"say \"\"hi\"\"\"\r\n";

#[test]
fn all_six_pairs_round_trip() {
    let tsv = out("a1", CSV, "csv", "tsv", &[]);
    let js = out("a2", CSV, "csv", "json", &[]);
    assert_eq!(
        String::from_utf8(js.clone()).unwrap(),
        "[\n  {\n    \"name\": \"Ada\",\n    \"notes\": \"likes, commas\\nand newlines\"\n  },\n  {\n    \"name\": \"Bob\",\n    \"notes\": \"say \\\"hi\\\"\"\n  }\n]\n"
    );
    assert_eq!(out("a3", &tsv, "tsv", "csv", &[]), CSV);
    assert_eq!(out("a4", &js, "json", "csv", &[]), CSV);
    assert_eq!(out("a5", &tsv, "tsv", "json", &[]), js);
    assert_eq!(out("a6", &js, "json", "tsv", &[]), tsv);
}

#[test]
fn csv_options() {
    assert_eq!(
        out(
            "b1",
            b"a;b\n1;2\n",
            "csv",
            "csv",
            &[("delimiter", "pipe"), ("newline", "lf"), ("bom", "true")]
        ),
        b"\xef\xbb\xbfa|b\n1|2\n"
    );
    assert_eq!(
        out(
            "b2",
            b"a,b\n1,2\n",
            "csv",
            "json",
            &[("has_header", "false"), ("indent", "0")]
        ),
        b"[[\"a\",\"b\"],[\"1\",\"2\"]]\n"
    );
    assert_eq!(
        out(
            "b3",
            b"a;b\n1;2\n",
            "csv",
            "json",
            &[("source_delimiter", "comma"), ("indent", "0")]
        ),
        b"[{\"a;b\":\"1;2\"}]\n"
    );
}

#[test]
fn detected_delimiter_is_reported() {
    let (o, r) = conv("c1", b"a;b\n1;2\n", "csv", "tsv", &[("newline", "lf")]).unwrap();
    assert_eq!(o, b"a\tb\n1\t2\n");
    assert!(has_note(&r, "delimiter_detected"));
}

#[test]
fn ragged_rows_are_padded_and_noted() {
    let (o, r) = conv("d1", b"a,b,c\n1\n", "csv", "csv", &[("newline", "lf")]).unwrap();
    assert_eq!(o, b"a,b,c\n1,,\n");
    assert!(has_note(&r, "rows_padded"));
}

#[test]
fn bom_and_windows_1252_input() {
    assert_eq!(
        out(
            "e1",
            b"\xef\xbb\xbfa,b\n1,2\n",
            "csv",
            "json",
            &[("indent", "0")]
        ),
        b"[{\"a\":\"1\",\"b\":\"2\"}]\n"
    );
    let (o, r) = conv(
        "e2",
        b"x\ncaf\xe9 \x96 \x80\n",
        "csv",
        "json",
        &[("indent", "0")],
    )
    .unwrap();
    assert_eq!(String::from_utf8(o).unwrap(), "[{\"x\":\"café – €\"}]\n");
    assert!(has_note(&r, "source_encoding"));
    // The fallback encoding is configurable, and undefined cp1252 bytes still decode.
    let o = out(
        "e3",
        b"x\ncaf\xe9\n",
        "csv",
        "json",
        &[("indent", "0"), ("source_encoding", "iso-8859-1")],
    );
    assert_eq!(String::from_utf8(o).unwrap(), "[{\"x\":\"café\"}]\n");
    let o = out("e4", b"x\n\x81\x8d\n", "csv", "json", &[("indent", "0")]);
    assert_eq!(
        String::from_utf8(o).unwrap(),
        "[{\"x\":\"\u{81}\u{8d}\"}]\n"
    );
}

#[test]
fn utf16_input_is_read() {
    let mut data = vec![0xff, 0xfe];
    for u in "a,b\n1,é\n".encode_utf16() {
        data.extend_from_slice(&u.to_le_bytes());
    }
    assert_eq!(
        out("f1", &data, "csv", "csv", &[("newline", "lf")]),
        "a,b\n1,é\n".as_bytes()
    );
}

#[test]
fn json_scalars_are_noted() {
    let (o, r) = conv(
        "g1",
        br#"[{"n": 1, "ok": true, "z": null}]"#,
        "json",
        "csv",
        &[],
    )
    .unwrap();
    assert_eq!(o, b"n,ok,z\r\n1,true,\r\n");
    assert!(has_note(&r, "json_scalars_as_text"));
    let (_, r) = conv("g2", br#"[{"n": "1"}]"#, "json", "csv", &[]).unwrap();
    assert!(!has_note(&r, "json_scalars_as_text"));
}

#[test]
fn json_structure_errors() {
    let code = |t: &str, d: &[u8]| conv(t, d, "json", "csv", &[]).err().map(|e| e.code);
    assert_eq!(
        code("h1", br#"{"a":1}"#),
        Some(ErrorCode::UnsupportedVariant)
    );
    assert_eq!(
        code("h2", br#"[{"a":{"b":1}}]"#),
        Some(ErrorCode::UnsupportedVariant)
    );
    assert_eq!(code("h3", br#"[{"a":1"#), Some(ErrorCode::CorruptFile));
}

#[test]
fn json_output_escapes_and_renames() {
    let o = out(
        "i1",
        "a,a,\né,2,3\n".as_bytes(),
        "csv",
        "json",
        &[("indent", "0"), ("ensure_ascii", "true")],
    );
    assert_eq!(
        String::from_utf8(o).unwrap(),
        "[{\"a\":\"\\u00e9\",\"a_2\":\"2\",\"column_3\":\"3\"}]\n"
    );
}

#[test]
fn identical_output_is_reported() {
    let (_, r) = conv("j1", CSV, "csv", "csv", &[]).unwrap();
    assert!(has_note(&r, "no_change"));
}

#[test]
fn bad_options_are_rejected() {
    let code =
        |t: &str, to: &str, o: &[(&str, &str)]| conv(t, CSV, "csv", to, o).err().map(|e| e.code);
    assert_eq!(
        code("k1", "csv", &[("delimiter", "colon")]),
        Some(ErrorCode::InvalidOptions)
    );
    assert_eq!(
        code("k2", "csv", &[("source_encoding", "klingon")]),
        Some(ErrorCode::InvalidOptions)
    );
    // Writer-only options of another table format are not accepted.
    assert_eq!(
        code("k3", "tsv", &[("delimiter", "pipe")]),
        Some(ErrorCode::InvalidOptions)
    );
}

#[test]
fn tables_do_not_become_images_or_text() {
    assert!(conv("l1", CSV, "csv", "png", &[]).is_err());
    assert!(conv("l2", CSV, "csv", "txt", &[]).is_err());
}
