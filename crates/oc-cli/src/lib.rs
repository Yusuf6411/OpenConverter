//! `openconverter` command line.
//!
//! Usage:
//!   openconverter convert <input> <output> [--overwrite] [--no-isolation]
//!                         [--timeout SECONDS] [--max-memory MB] [--set key=value]...
//!   openconverter formats [--json]
//!   openconverter list <archive> [--json] [--no-isolation]
//!   openconverter extract <archive> <folder> [--overwrite] [--strip N] [--only PATH]...
//!   openconverter create <archive> <file-or-folder>... [--overwrite] [--set key=value]...
//!   openconverter --version
//!
//! Passwords are read from the `OC_PASSWORD` environment variable (input password of
//! `convert`, `list` and `extract`) and `OC_OUTPUT_PASSWORD` (encrypts the output of
//! `convert` and `create`); they never travel on a command line.
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
        "usage:\n  openconverter serve [--host H] [--port N] [--allow-remote] [--no-browser] [--data-dir DIR]\n  openconverter convert <input> <output> [--overwrite] [--no-isolation] [--timeout SECONDS] [--max-memory MB] [--set key=value]...\n  openconverter formats [--json]\n  openconverter list <archive> [--json] [--no-isolation]\n  openconverter extract <archive> <folder> [--overwrite] [--strip N] [--only PATH]... [--no-isolation]\n  openconverter create <archive> <file-or-folder>... [--overwrite] [--no-isolation] [--set key=value]...\n  openconverter --version"
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
    name: Option<String>,
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
        name: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--overwrite" => a.overwrite = true,
            "--no-isolation" => a.isolate = false,
            "--name" => a.name = Some(it.next()?.clone()),
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

/// Passwords given through the environment become ordinary options.
fn with_env_secrets(raw: &mut Vec<(String, String)>, worker: bool) {
    let mut add = |key: &str, vars: &[&str]| {
        if raw.iter().any(|(k, _)| k == key) {
            return;
        }
        for v in vars {
            if let Ok(val) = std::env::var(v) {
                if !val.is_empty() {
                    raw.push((key.to_string(), val));
                    return;
                }
            }
        }
    };
    if worker {
        for key in oc_engine::isolated::SECRET_KEYS {
            add(key, &[&oc_engine::isolated::secret_env_name(key)]);
        }
    } else {
        add("source_password", &["OC_PASSWORD"]);
        add("password", &["OC_OUTPUT_PASSWORD"]);
    }
}

fn convert(engine: &Engine, args: &[String], worker: bool) -> ExitCode {
    let Some(mut a) = parse_convert(args) else {
        return usage();
    };
    // `password` only exists on encrypting writers; ignore the variable elsewhere.
    with_env_secrets(&mut a.raw_options, worker);
    let options = match engine.parse_options(a.output.as_ref(), &a.raw_options) {
        Ok(o) => o,
        Err(e) if e.code == ErrorCode::InvalidOptions && !worker => {
            // An unused password variable must not break conversions to formats without one.
            a.raw_options.retain(|(k, _)| k != "password" && k != "source_password");
            match engine.parse_options(a.output.as_ref(), &a.raw_options) {
                Ok(o) => o,
                Err(e) => return fail(&e),
            }
        }
        Err(e) => return fail(&e),
    };
    let req = ConvertRequest {
        input: a.input.into(),
        output: a.output.into(),
        overwrite: a.overwrite,
        options,
        display_name: a.name.clone(),
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

struct ArchiveArgs {
    positional: Vec<String>,
    overwrite: bool,
    isolate: bool,
    json: bool,
    strip: usize,
    only: Vec<String>,
    no_mode: bool,
    no_mtime: bool,
    raw_options: Vec<(String, String)>,
}

fn parse_archive_args(args: &[String]) -> Option<ArchiveArgs> {
    let mut a = ArchiveArgs {
        positional: Vec::new(),
        overwrite: false,
        isolate: true,
        json: false,
        strip: 0,
        only: Vec::new(),
        no_mode: false,
        no_mtime: false,
        raw_options: Vec::new(),
    };
    let mut it = args.iter();
    let mut rest_positional = false;
    while let Some(arg) = it.next() {
        if rest_positional {
            a.positional.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--" => rest_positional = true,
            "--overwrite" => a.overwrite = true,
            "--no-isolation" => a.isolate = false,
            "--json" => a.json = true,
            "--no-mode" => a.no_mode = true,
            "--no-mtime" => a.no_mtime = true,
            "--strip" => a.strip = it.next()?.parse().ok()?,
            "--only" => a.only.push(it.next()?.clone()),
            "--set" => {
                let (k, v) = it.next()?.split_once('=')?;
                a.raw_options.push((k.to_string(), v.to_string()));
            }
            s if s.starts_with("--") => return None,
            _ => a.positional.push(arg.clone()),
        }
    }
    Some(a)
}

fn isolation() -> Option<Isolation> {
    let exe = std::env::current_exe().ok()?;
    let mut iso = Isolation::new(exe);
    iso.timeout = Duration::from_secs(600);
    iso.max_memory = Some(4096 << 20);
    Some(iso)
}

fn password_from_env(worker: bool) -> Option<String> {
    let var = if worker {
        oc_engine::isolated::secret_env_name("source_password")
    } else {
        "OC_PASSWORD".to_string()
    };
    std::env::var(var).ok().filter(|p| !p.is_empty())
}

fn fmt_time(t: i64) -> String {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

fn list(engine: &Engine, args: &[String], worker: bool) -> ExitCode {
    let Some(a) = parse_archive_args(args) else {
        return usage();
    };
    let [path] = a.positional.as_slice() else {
        return usage();
    };
    let pw = password_from_env(worker);
    let result = if a.isolate && !worker {
        match isolation() {
            Some(iso) => oc_engine::isolated::list_isolated(engine, &iso, path.as_ref(), pw.as_deref(), &CancelToken::new()),
            None => return ExitCode::from(1),
        }
    } else {
        engine.archive_list(path.as_ref(), pw.as_deref(), &NoProgress)
    };
    let (format, entries) = match result {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    if worker {
        print!("{}", oc_engine::isolated::encode_list(format, &entries));
        return ExitCode::SUCCESS;
    }
    if a.json {
        let items: Vec<String> = entries
            .iter()
            .map(|e| {
                format!(
                    "{{\"path\":{},\"kind\":\"{}\",\"size\":{},\"mode\":{},\"mtime\":{},\"encrypted\":{},\"link\":{}}}",
                    json_str(&e.path),
                    e.kind,
                    e.size,
                    e.mode,
                    e.mtime.map_or("null".to_string(), |t| t.to_string()),
                    e.encrypted,
                    e.link.as_deref().map_or("null".to_string(), json_str)
                )
            })
            .collect();
        println!("{{\"format\":{},\"entries\":[{}]}}", json_str(format.0), items.join(","));
    } else {
        println!("{} entries ({})", entries.len(), format.0);
        for e in &entries {
            let kind = match e.kind {
                'd' => 'd',
                'l' => 'l',
                _ => '-',
            };
            println!(
                "{kind}{:04o} {:>12} {:<16} {}{}{}",
                e.mode & 0o7777,
                e.size,
                e.mtime.map_or(String::new(), fmt_time),
                e.path,
                e.link.as_ref().map_or(String::new(), |l| format!(" -> {l}")),
                if e.encrypted { " [encrypted]" } else { "" }
            );
        }
    }
    ExitCode::SUCCESS
}

fn extract(engine: &Engine, args: &[String], worker: bool) -> ExitCode {
    let Some(a) = parse_archive_args(args) else {
        return usage();
    };
    let [path, dest] = a.positional.as_slice() else {
        return usage();
    };
    let pw = password_from_env(worker);
    let opts = oc_archive::safety::ExtractOptions {
        overwrite: a.overwrite,
        strip_components: a.strip,
        only: a.only.clone(),
        preserve_mode: !a.no_mode,
        preserve_mtime: !a.no_mtime,
    };
    let result = if a.isolate && !worker {
        match isolation() {
            Some(iso) => oc_engine::isolated::extract_isolated(&iso, path.as_ref(), dest.as_ref(), &opts, pw.as_deref(), &CancelToken::new()),
            None => return ExitCode::from(1),
        }
    } else {
        engine.archive_extract(path.as_ref(), dest.as_ref(), &opts, pw.as_deref(), &NoProgress)
    };
    match result {
        Ok(r) => {
            if worker {
                print!("{}", oc_engine::isolated::encode_extract(&r));
                return ExitCode::SUCCESS;
            }
            println!("extracted {} files, {} folders, {} links ({} bytes)", r.files, r.dirs, r.symlinks, r.bytes);
            for (p, why) in &r.skipped {
                eprintln!("skipped: {p} ({why})");
            }
            if r.skipped.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(3) }
        }
        Err(e) => fail(&e),
    }
}

fn create(engine: &Engine, args: &[String], worker: bool) -> ExitCode {
    let Some(mut a) = parse_archive_args(args) else {
        return usage();
    };
    if a.positional.len() < 2 {
        return usage();
    }
    let output = a.positional.remove(0);
    let sources: Vec<std::path::PathBuf> = a.positional.iter().map(Into::into).collect();
    with_env_secrets(&mut a.raw_options, worker);
    let options = match engine.parse_options(output.as_ref(), &a.raw_options) {
        Ok(o) => o,
        Err(e) if e.code == ErrorCode::InvalidOptions && !worker => {
            a.raw_options.retain(|(k, _)| k != "password" && k != "source_password");
            match engine.parse_options(output.as_ref(), &a.raw_options) {
                Ok(o) => o,
                Err(e) => return fail(&e),
            }
        }
        Err(e) => return fail(&e),
    };
    let result = if a.isolate && !worker {
        match isolation() {
            Some(iso) => oc_engine::isolated::create_isolated(engine, &iso, &sources, output.as_ref(), a.overwrite, &options, &CancelToken::new()),
            None => return ExitCode::from(1),
        }
    } else {
        engine.archive_create(&sources, output.as_ref(), a.overwrite, &options, &NoProgress)
    };
    match result {
        Ok(rep) => {
            if worker {
                print!("{}", encode_report(&rep));
                return ExitCode::SUCCESS;
            }
            println!("{}: {} bytes written", rep.output_format.0, rep.bytes_out);
            println!("route: {}", rep.route);
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn serve(args: &[String]) -> ExitCode {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("error: cannot find this executable");
        return ExitCode::from(1);
    };
    let mut cfg = oc_server::Config::new(exe);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--host" => match it.next() {
                Some(h) => cfg.host = h.clone(),
                None => return usage(),
            },
            "--port" => match it.next().and_then(|p| p.parse().ok()) {
                Some(p) => cfg.port = p,
                None => return usage(),
            },
            "--data-dir" => match it.next() {
                Some(d) => cfg.data_dir = d.into(),
                None => return usage(),
            },
            "--allow-remote" => cfg.allow_remote = true,
            "--no-browser" => cfg.open_browser = false,
            _ => return usage(),
        }
    }
    match oc_server::run(cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

/// Worker side of the server's look at an upload: `probe <file> <format>` prints
/// `width`, `height` and `alpha` lines; `thumb <file> <format> <out.png>` writes a preview.
fn worker_inspect(engine: &Engine, args: &[String]) -> ExitCode {
    let (Some(kind), Some(path), Some(format)) = (args.first(), args.get(1), args.get(2)) else {
        return usage();
    };
    let Some(from) = engine.format_by_name(format) else {
        return ExitCode::from(ErrorCode::UnsupportedFormat.exit_code());
    };
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return ExitCode::from(ErrorCode::IoError.exit_code()),
    };
    match kind.as_str() {
        "probe" => match engine.probe(from, &data, &NoProgress) {
            Ok(p) => {
                println!("width={}", p.width.unwrap_or(0));
                println!("height={}", p.height.unwrap_or(0));
                println!("alpha={}", p.alpha);
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
        "thumb" => {
            let Some(out) = args.get(3) else {
                return usage();
            };
            match engine
                .thumbnail(from, &data, 512, &NoProgress)
                .and_then(|png| std::fs::write(out, png).map_err(Into::into))
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        _ => usage(),
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

/// Runs the command line with `args` (without the program name).
pub fn run(args: Vec<String>) -> ExitCode {
    let engine = Engine::new();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("openconverter {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("serve") => serve(args.get(1..).unwrap_or(&[])),
        Some("formats") => formats(&engine, args.iter().any(|a| a == "--json")),
        Some("convert") => convert(&engine, args.get(1..).unwrap_or(&[]), false),
        Some("list") => list(&engine, args.get(1..).unwrap_or(&[]), false),
        Some("extract") => extract(&engine, args.get(1..).unwrap_or(&[]), false),
        Some("create") => create(&engine, args.get(1..).unwrap_or(&[]), false),
        Some("__worker") if args.get(1).map(String::as_str) == Some("convert") => {
            convert(&engine, args.get(2..).unwrap_or(&[]), true)
        }
        Some("__worker") if args.get(1).map(String::as_str) == Some("list") => {
            list(&engine, args.get(2..).unwrap_or(&[]), true)
        }
        Some("__worker") if args.get(1).map(String::as_str) == Some("extract") => {
            extract(&engine, args.get(2..).unwrap_or(&[]), true)
        }
        Some("__worker") if args.get(1).map(String::as_str) == Some("create") => {
            create(&engine, args.get(2..).unwrap_or(&[]), true)
        }
        Some("__worker") if matches!(args.get(1).map(String::as_str), Some("probe" | "thumb")) => {
            worker_inspect(&engine, args.get(1..).unwrap_or(&[]))
        }
        Some("__selftest") => selftest(args.get(1).map_or("", String::as_str)),
        _ => usage(),
    }
}
