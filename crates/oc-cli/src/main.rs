//! `openconverter` command line.
//!
//! Usage:
//!   openconverter convert <input> <output> [--overwrite] [--no-isolation]
//!                         [--timeout SECONDS] [--max-memory MB] [--set key=value]...
//!   openconverter formats [--json]
//!   openconverter --version
//!
//! `__worker` and `__selftest` are internal: the first runs one conversion for the
//! isolating parent, the second misbehaves on purpose so the isolation tests can prove
//! that crashes, hangs and runaway allocations are contained.

use oc_core::{CancelToken, ErrorCode, NoProgress, Severity};
use oc_engine::isolated::{convert_isolated, encode_report, Isolation};
use oc_engine::{ConvertRequest, Engine};
use std::process::ExitCode;
use std::time::Duration;

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  openconverter convert <input> <output> [--overwrite] [--no-isolation] [--timeout SECONDS] [--max-memory MB] [--set key=value]...\n  openconverter formats [--json]\n  openconverter --version"
    );
    ExitCode::from(2)
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

struct ConvertArgs {
    input: String,
    output: String,
    overwrite: bool,
    isolate: bool,
    timeout: Duration,
    max_memory_mb: u64,
    raw_options: Vec<(String, String)>,
}

fn parse_convert(args: &[String]) -> Option<ConvertArgs> {
    let mut positional = Vec::new();
    let mut a = ConvertArgs {
        input: String::new(),
        output: String::new(),
        overwrite: false,
        isolate: true,
        timeout: Duration::from_secs(600),
        max_memory_mb: 4096,
        raw_options: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--overwrite" => a.overwrite = true,
            "--no-isolation" => a.isolate = false,
            "--timeout" => a.timeout = Duration::from_secs(it.next()?.parse().ok()?),
            "--max-memory" => a.max_memory_mb = it.next()?.parse().ok()?,
            "--set" => {
                let (k, v) = it.next()?.split_once('=')?;
                a.raw_options.push((k.to_string(), v.to_string()));
            }
            s if s.starts_with("--") => return None,
            _ => positional.push(arg.clone()),
        }
    }
    let [input, output] = positional.as_slice() else {
        return None;
    };
    a.input = input.clone();
    a.output = output.clone();
    Some(a)
}

fn fail(e: &oc_core::Error) -> ExitCode {
    eprintln!("error: {e}");
    if let Some(d) = &e.detail {
        eprintln!("  detail: {d}");
    }
    ExitCode::from(e.code.exit_code())
}

fn convert(engine: &Engine, args: &[String], worker: bool) -> ExitCode {
    let Some(a) = parse_convert(args) else {
        return usage();
    };
    let options = match engine.parse_options(a.output.as_ref(), &a.raw_options) {
        Ok(o) => o,
        Err(e) => return fail(&e),
    };
    let req = ConvertRequest {
        input: a.input.into(),
        output: a.output.into(),
        overwrite: a.overwrite,
        options,
    };
    let result = if a.isolate && !worker {
        match std::env::current_exe() {
            Ok(exe) => {
                let mut iso = Isolation::new(exe);
                iso.timeout = a.timeout;
                iso.max_memory = Some(a.max_memory_mb << 20);
                convert_isolated(engine, &iso, &req, &CancelToken::new())
            }
            Err(e) => Err(e.into()),
        }
    } else {
        engine.convert(&req, &NoProgress)
    };
    match result {
        Ok(rep) => {
            if worker {
                print!("{}", encode_report(&rep));
                return ExitCode::SUCCESS;
            }
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
        Err(e) => fail(&e),
    }
}

/// Deliberately misbehave; used only by the isolation tests.
fn selftest(what: &str) -> ExitCode {
    match what {
        "panic" => panic!("selftest panic"),
        "loop" => loop {
            std::thread::sleep(Duration::from_millis(10));
        },
        "alloc" => {
            // Allocate and touch far more than any sane cap.
            // Bounded so a platform without a memory cap cannot take the machine down.
            let mut hog: Vec<Vec<u8>> = Vec::new();
            for _ in 0..16 {
                hog.push(vec![1u8; 256 << 20]);
            }
            ExitCode::from(ErrorCode::InternalError.exit_code())
        }
        "error" => {
            eprintln!("error: corrupt_file: selftest corrupt");
            ExitCode::from(ErrorCode::CorruptFile.exit_code())
        }
        _ => ExitCode::from(2),
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
        Some("convert") => convert(&engine, args.get(1..).unwrap_or(&[]), false),
        Some("__worker") if args.get(1).map(String::as_str) == Some("convert") => {
            convert(&engine, args.get(2..).unwrap_or(&[]), true)
        }
        Some("__selftest") => selftest(args.get(1).map_or("", String::as_str)),
        _ => usage(),
    }
}
