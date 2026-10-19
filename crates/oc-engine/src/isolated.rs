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
use oc_core::{CancelToken, Error, ErrorCode, Note, Notes, Result, Severity};
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

/// Run the worker with `args`, enforcing the limits. Returns its stdout on success.
pub fn run_worker(iso: &Isolation, args: &[String], cancel: &CancelToken) -> Result<String> {
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
            let _ = o.take(1 << 20).read_to_string(&mut s);
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
                Err(Error::new(
                    code,
                    msg.trim_start_matches("error: ").to_string(),
                ))
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
    for (k, v) in &req.options {
        args.push("--set".into());
        args.push(format!("{k}={}", v.to_arg()));
    }
    let stdout = run_worker(iso, &args, cancel)?;
    decode_report(engine, &stdout)
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
