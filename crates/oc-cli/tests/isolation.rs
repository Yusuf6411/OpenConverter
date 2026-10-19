//! Proves that the worker isolation contains crashes, hangs and runaway allocations,
//! and that a normal conversion works through the worker.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oc_core::{CancelToken, ErrorCode};
use oc_engine::isolated::{run_worker, Isolation};
use std::time::{Duration, Instant};

fn exe() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_openconverter").into()
}

fn selftest(what: &str, iso: &Isolation) -> oc_core::Result<String> {
    run_worker(
        iso,
        &["__selftest".into(), what.into()],
        &CancelToken::new(),
    )
}

#[test]
fn panic_is_reported_as_worker_crashed() {
    let e = selftest("panic", &Isolation::new(exe())).unwrap_err();
    assert_eq!(e.code, ErrorCode::WorkerCrashed);
}

#[test]
fn typed_error_status_is_preserved() {
    let e = selftest("error", &Isolation::new(exe())).unwrap_err();
    assert_eq!(e.code, ErrorCode::CorruptFile);
}

#[test]
fn infinite_loop_is_killed_by_the_timeout() {
    let mut iso = Isolation::new(exe());
    iso.timeout = Duration::from_millis(500);
    let t = Instant::now();
    let e = selftest("loop", &iso).unwrap_err();
    assert_eq!(e.code, ErrorCode::Timeout);
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn cancellation_kills_the_worker() {
    let iso = Isolation::new(exe());
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c2.cancel();
    });
    let e = run_worker(&iso, &["__selftest".into(), "loop".into()], &cancel).unwrap_err();
    assert_eq!(e.code, ErrorCode::Cancelled);
}

// RLIMIT_AS is only enforced on Linux (macOS ignores it).
#[cfg(target_os = "linux")]
#[test]
fn runaway_allocation_is_stopped_by_the_memory_cap() {
    let mut iso = Isolation::new(exe());
    iso.max_memory = Some(1 << 30);
    let e = selftest("alloc", &iso).unwrap_err();
    assert_eq!(e.code, ErrorCode::WorkerCrashed);
}

#[test]
fn conversion_runs_through_the_worker() {
    let dir = std::env::temp_dir().join(format!("oc-iso-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (i, o) = (dir.join("in.txt"), dir.join("out.txt"));
    std::fs::write(&i, "a\r\nb").unwrap();
    let out = std::process::Command::new(exe())
        .args([
            "convert",
            i.to_str().unwrap(),
            o.to_str().unwrap(),
            "--set",
            "line_ending=lf",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(&o).unwrap(), b"a\nb");
    // Re-running without --overwrite reports the typed error through the worker.
    let again = std::process::Command::new(exe())
        .args(["convert", i.to_str().unwrap(), o.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        again.status.code(),
        Some(i32::from(ErrorCode::OutputExists.exit_code()))
    );
    let _ = std::fs::remove_dir_all(dir);
}
