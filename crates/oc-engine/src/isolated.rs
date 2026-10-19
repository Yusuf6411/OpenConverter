//! Running conversions in a separate worker process so a hostile file can crash, hang
//! or exhaust memory without taking the host down (MASTER_PROMPT §16).
//!
//! The worker is `<exe> __worker convert ...` and talks over stdout/exit status:
//! `OK<TAB>in<TAB>out<TAB>bytes_in<TAB>bytes_out<TAB>route` and
//! `NOTE<TAB>severity<TAB>code<TAB>message` lines on success, an `ErrorCode` exit
//! status plus a message on stderr on failure. Death by signal, a panic exit (101) or
//! any unknown status is reported as `worker_crashed`.
//!
//! Limits: wall-clock timeout and cancellation everywhere; a virtual-memory cap on Unix
//! via `ulimit -v`. A Windows job-object memory cap is not implemented yet.

use crate::{ConvertReport, ConvertRequest, Engine};
use oc_core::{CancelToken, Error, ErrorCode, FormatId, Note, Notes, Result, Severity};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Isolation {
    /// The executable that understands `__worker` (normally the CLI itself).
    pub exe: PathBuf,
    pub timeout: Duration,
    /// Address-space cap in bytes (Unix only).
    pub max_memory: Option<u64>,
}

impl Isolation {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            timeout: Duration::from_secs(600),
            max_memory: Some(4 << 30),
        }
    }
}

/// Option keys whose values are secrets: they travel to the worker in its environment
/// (`OC_OPT_<KEY>`), never on a command line where other users could read them.
pub const SECRET_KEYS: [&str; 2] = ["source_password", "password"];

pub fn secret_env_name(key: &str) -> String {
    format!("OC_OPT_{}", key.to_ascii_uppercase())
}

/// Run the worker with `args`, enforcing the limits. Returns its stdout on success.
pub fn run_worker(iso: &Isolation, args: &[String], cancel: &CancelToken) -> Result<String> {
    run_worker_env(iso, args, &[], cancel, 1 << 20)
}

/// [`run_worker`] with extra environment variables and a stdout size cap.
pub fn run_worker_env(
    iso: &Isolation,
    args: &[String],
    envs: &[(String, String)],
    cancel: &CancelToken,
    stdout_cap: u64,
) -> Result<String> {
    let mut cmd;
    #[cfg(unix)]
    {
        // `ulimit -v` takes KiB. A failed ulimit (already lower) is not fatal.
        cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("ulimit -v \"$OC_MEM_KB\" 2>/dev/null; exec \"$@\"")
            .arg("sh");
        cmd.arg(&iso.exe);
        cmd.env(
            "OC_MEM_KB",
            iso.max_memory
                .map_or("unlimited".to_string(), |b| (b / 1024).to_string()),
        );
    }
    #[cfg(not(unix))]
    {
        cmd = Command::new(&iso.exe);
    }
    cmd.envs(envs.iter().map(|(k, v)| (k, v)));
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        Error::new(
            ErrorCode::WorkerCrashed,
            format!("cannot start worker: {e}"),
        )
    })?;

    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = o.take(stdout_cap).read_to_string(&mut s);
        }
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.take(1 << 16).read_to_string(&mut s);
        }
        s
    });

    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::cancelled());
        }
        if start.elapsed() > iso.timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                Error::new(ErrorCode::Timeout, "worker exceeded the time limit")
                    .with_detail(format!("{}s", iso.timeout.as_secs())),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();

    match status.code() {
        Some(0) => Ok(stdout),
        Some(n) => match ErrorCode::from_exit_code(n) {
            Some(code) => {
                let msg = stderr.lines().next().unwrap_or("worker failed").to_string();
                let msg = msg.trim_start_matches("error: ");
                // The worker prints "<code>: <message>"; the code is added again on display.
                let msg = msg
                    .strip_prefix(&format!("{}: ", code.as_str()))
                    .unwrap_or(msg);
                Err(Error::new(code, msg.to_string()))
            }
            None => Err(Error::new(
                ErrorCode::WorkerCrashed,
                format!("worker exited with status {n}"),
            )),
        },
        None => Err(Error::new(
            ErrorCode::WorkerCrashed,
            "worker was terminated by a signal",
        )),
    }
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Worker side: format a successful result for the parent.
pub fn encode_report(r: &ConvertReport) -> String {
    let mut s = format!(
        "OK\t{}\t{}\t{}\t{}\t{}\n",
        r.input_format.0,
        r.output_format.0,
        r.bytes_in,
        r.bytes_out,
        clean(&r.route)
    );
    for n in r.notes.iter() {
        let sev = match n.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Loss => "loss",
        };
        s.push_str(&format!(
            "NOTE\t{sev}\t{}\t{}\n",
            clean(&n.code),
            clean(&n.message)
        ));
    }
    s
}

/// Parent side: parse what [`encode_report`] produced.
pub fn decode_report(engine: &Engine, text: &str) -> Result<ConvertReport> {
    let bad = || {
        Error::new(
            ErrorCode::WorkerCrashed,
            "worker produced an unreadable result",
        )
    };
    let mut report = None;
    let mut notes = Notes::new();
    for line in text.lines() {
        let f: Vec<&str> = line.splitn(6, '\t').collect();
        match f.as_slice() {
            ["OK", a, b, bin, bout, route] => {
                report = Some(ConvertReport {
                    input_format: engine.format_by_name(a).ok_or_else(bad)?,
                    output_format: engine.format_by_name(b).ok_or_else(bad)?,
                    route: (*route).to_string(),
                    bytes_in: bin.parse().map_err(|_| bad())?,
                    bytes_out: bout.parse().map_err(|_| bad())?,
                    notes: Notes::new(),
                });
            }
            ["NOTE", sev, code, msg] => {
                let severity = match *sev {
                    "loss" => Severity::Loss,
                    "warning" => Severity::Warning,
                    _ => Severity::Info,
                };
                notes.push(Note::new(severity, *code, *msg));
            }
            _ => {}
        }
    }
    let mut r = report.ok_or_else(bad)?;
    r.notes = notes;
    Ok(r)
}

/// Convert in a worker process.
pub fn convert_isolated(
    engine: &Engine,
    iso: &Isolation,
    req: &ConvertRequest,
    cancel: &CancelToken,
) -> Result<ConvertReport> {
    let mut args = vec![
        "__worker".to_string(),
        "convert".to_string(),
        req.input.display().to_string(),
        req.output.display().to_string(),
    ];
    if req.overwrite {
        args.push("--overwrite".into());
    }
    if let Some(n) = &req.display_name {
        args.push("--name".into());
        args.push(n.clone());
    }
    let mut envs = Vec::new();
    for (k, v) in &req.options {
        if SECRET_KEYS.contains(&k.as_str()) {
            envs.push((secret_env_name(k), v.to_arg()));
            continue;
        }
        args.push("--set".into());
        args.push(format!("{k}={}", v.to_arg()));
    }
    let stdout = run_worker_env(iso, &args, &envs, cancel, 1 << 20)?;
    decode_report(engine, &stdout)
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn unesc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// Worker side of a listing.
pub fn encode_list(format: FormatId, entries: &[crate::archive::EntryInfo]) -> String {
    let mut s = format!("FORMAT\t{}\n", format.0);
    for e in entries {
        s.push_str(&format!(
            "ENTRY\t{}\t{:o}\t{}\t{}\t{}\t{}\t{}\n",
            e.kind,
            e.mode,
            e.size,
            e.mtime.map_or(String::new(), |t| t.to_string()),
            u8::from(e.encrypted),
            esc(&e.path),
            esc(e.link.as_deref().unwrap_or(""))
        ));
    }
    s
}

pub fn decode_list(engine: &Engine, text: &str) -> Result<(FormatId, Vec<crate::archive::EntryInfo>)> {
    let bad = || Error::new(ErrorCode::WorkerCrashed, "worker produced an unreadable result");
    let mut format = None;
    let mut entries = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.splitn(8, '\t').collect();
        match f.as_slice() {
            ["FORMAT", id] => format = Some(engine.format_by_name(id).ok_or_else(bad)?),
            ["ENTRY", kind, mode, size, mtime, enc, path, link] => entries.push(crate::archive::EntryInfo {
                kind: kind.chars().next().ok_or_else(bad)?,
                mode: u32::from_str_radix(mode, 8).map_err(|_| bad())?,
                size: size.parse().map_err(|_| bad())?,
                mtime: mtime.parse().ok(),
                encrypted: *enc == "1",
                path: unesc(path),
                link: Some(unesc(link)).filter(|l| !l.is_empty()),
            }),
            _ => {}
        }
    }
    Ok((format.ok_or_else(bad)?, entries))
}

/// List an archive in a worker process.
pub fn list_isolated(
    engine: &Engine,
    iso: &Isolation,
    path: &std::path::Path,
    password: Option<&str>,
    cancel: &CancelToken,
) -> Result<(FormatId, Vec<crate::archive::EntryInfo>)> {
    let args = vec!["__worker".to_string(), "list".to_string(), path.display().to_string()];
    let envs: Vec<(String, String)> = password
        .map(|p| vec![(secret_env_name("source_password"), p.to_string())])
        .unwrap_or_default();
    let out = run_worker_env(iso, &args, &envs, cancel, 256 << 20)?;
    decode_list(engine, &out)
}

pub fn encode_extract(r: &oc_archive::safety::ExtractReport) -> String {
    let mut s = format!("DONE\t{}\t{}\t{}\t{}\n", r.files, r.dirs, r.symlinks, r.bytes);
    for (p, why) in &r.skipped {
        s.push_str(&format!("SKIP\t{}\t{}\n", esc(p), esc(why)));
    }
    s
}

pub fn decode_extract(text: &str) -> Result<oc_archive::safety::ExtractReport> {
    let bad = || Error::new(ErrorCode::WorkerCrashed, "worker produced an unreadable result");
    let mut rep = None;
    let mut skipped = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.splitn(5, '\t').collect();
        match f.as_slice() {
            ["DONE", a, b, c, d] => {
                rep = Some(oc_archive::safety::ExtractReport {
                    files: a.parse().map_err(|_| bad())?,
                    dirs: b.parse().map_err(|_| bad())?,
                    symlinks: c.parse().map_err(|_| bad())?,
                    bytes: d.parse().map_err(|_| bad())?,
                    skipped: Vec::new(),
                });
            }
            ["SKIP", p, why] => skipped.push((unesc(p), unesc(why))),
            _ => {}
        }
    }
    let mut r = rep.ok_or_else(bad)?;
    r.skipped = skipped;
    Ok(r)
}

/// Extract an archive in a worker process.
pub fn extract_isolated(
    iso: &Isolation,
    archive: &std::path::Path,
    dest: &std::path::Path,
    opts: &oc_archive::safety::ExtractOptions,
    password: Option<&str>,
    cancel: &CancelToken,
) -> Result<oc_archive::safety::ExtractReport> {
    let mut args = vec![
        "__worker".to_string(),
        "extract".to_string(),
        archive.display().to_string(),
        dest.display().to_string(),
    ];
    if opts.overwrite {
        args.push("--overwrite".into());
    }
    if !opts.preserve_mode {
        args.push("--no-mode".into());
    }
    if !opts.preserve_mtime {
        args.push("--no-mtime".into());
    }
    if opts.strip_components > 0 {
        args.push("--strip".into());
        args.push(opts.strip_components.to_string());
    }
    for o in &opts.only {
        args.push("--only".into());
        args.push(o.clone());
    }
    let envs: Vec<(String, String)> = password
        .map(|p| vec![(secret_env_name("source_password"), p.to_string())])
        .unwrap_or_default();
    decode_extract(&run_worker_env(iso, &args, &envs, cancel, 64 << 20)?)
}

/// Create an archive from files and folders in a worker process.
pub fn create_isolated(
    engine: &Engine,
    iso: &Isolation,
    sources: &[PathBuf],
    output: &std::path::Path,
    overwrite: bool,
    options: &[(String, oc_core::options::OptionValue)],
    cancel: &CancelToken,
) -> Result<ConvertReport> {
    let mut args = vec!["__worker".to_string(), "create".to_string(), output.display().to_string()];
    if overwrite {
        args.push("--overwrite".into());
    }
    let mut envs = Vec::new();
    for (k, v) in options {
        if SECRET_KEYS.contains(&k.as_str()) {
            envs.push((secret_env_name(k), v.to_arg()));
        } else {
            args.push("--set".into());
            args.push(format!("{k}={}", v.to_arg()));
        }
    }
    args.push("--".into());
    args.extend(sources.iter().map(|s| s.display().to_string()));
    decode_report(engine, &run_worker_env(iso, &args, &envs, cancel, 1 << 20)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_roundtrips_through_the_protocol() {
        let engine = Engine::new();
        let mut notes = Notes::new();
        notes.push(Note::new(Severity::Loss, "alpha_flattened", "a\tb\nc"));
        let r = ConvertReport {
            input_format: crate::TXT,
            output_format: crate::TXT,
            route: "x -> y".into(),
            bytes_in: 5,
            bytes_out: 7,
            notes,
        };
        let back = decode_report(&engine, &encode_report(&r)).ok();
        assert_eq!(back.as_ref().map(|r| r.bytes_out), Some(7));
        assert_eq!(back.as_ref().map(|r| r.route.as_str()), Some("x -> y"));
        assert_eq!(back.map(|r| r.notes.has_loss()), Some(true));
        assert!(decode_report(&engine, "garbage").is_err());
    }
}
