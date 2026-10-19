//! `openconverter` command line.
//!
//! Usage:
//!   openconverter convert <input> <output> [--overwrite] [--set key=value]...
//!   openconverter formats [--json]
//!   openconverter --version

use oc_core::options::OptionValue;
use oc_core::{ErrorCode, NoProgress, Severity};
use oc_engine::{ConvertRequest, Engine};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  openconverter convert <input> <output> [--overwrite] [--set key=value]...\n  openconverter formats [--json]\n  openconverter --version"
    );
    ExitCode::from(2)
}

fn parse_value(v: &str) -> OptionValue {
    if let Ok(i) = v.parse::<i64>() {
        OptionValue::Int(i)
    } else if v == "true" || v == "false" {
        OptionValue::Bool(v == "true")
    } else {
        OptionValue::Text(v.to_string())
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn formats(engine: &Engine, json: bool) -> ExitCode {
    let list = engine.formats();
    if json {
        let items: Vec<String> = list
            .iter()
            .map(|f| {
                let exts: Vec<String> = f.extensions.iter().map(|e| json_str(e)).collect();
                format!(
                    "{{\"id\":{},\"name\":{},\"extensions\":[{}],\"ownership\":\"{:?}\"}}",
                    json_str(f.id.0),
                    json_str(f.name),
                    exts.join(","),
                    f.ownership
                )
            })
            .collect();
        println!("[{}]", items.join(","));
    } else {
        for f in list {
            println!(
                "{:<8} {:<20} .{}  [{:?}]",
                f.id.0,
                f.name,
                f.extensions.join(" ."),
                f.ownership
            );
        }
    }
    ExitCode::SUCCESS
}

fn convert(engine: &Engine, args: &[String]) -> ExitCode {
    let mut positional = Vec::new();
    let mut overwrite = false;
    let mut options = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--overwrite" => overwrite = true,
            "--set" => match it.next().and_then(|kv| kv.split_once('=')) {
                Some((k, v)) => options.push((k.to_string(), parse_value(v))),
                None => return usage(),
            },
            s if s.starts_with("--") => return usage(),
            _ => positional.push(a.clone()),
        }
    }
    let [input, output] = positional.as_slice() else {
        return usage();
    };
    let req = ConvertRequest {
        input: input.into(),
        output: output.into(),
        overwrite,
        options,
    };
    match engine.convert(&req, &NoProgress) {
        Ok(rep) => {
            println!(
                "{} -> {}: {} bytes written",
                rep.input_format.0, rep.output_format.0, rep.bytes_out
            );
            println!("route: {}", rep.route);
            for n in rep.notes.iter() {
                let tag = match n.severity {
                    Severity::Info => "info",
                    Severity::Warning => "warning",
                    Severity::Loss => "loss",
                };
                eprintln!("{tag}: {} ({})", n.message, n.code);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            if let Some(d) = &e.detail {
                eprintln!("  detail: {d}");
            }
            ExitCode::from(if e.code == ErrorCode::InternalError {
                70
            } else {
                e.code.exit_code()
            })
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let engine = Engine::new();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("openconverter {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("formats") => formats(&engine, args.iter().any(|a| a == "--json")),
        Some("convert") => convert(&engine, args.get(1..).unwrap_or(&[])),
        _ => usage(),
    }
}
