//! OpenConverter desktop: a Tauri window around the shared web UI.
//!
//! The UI is served by the same local server as `openconverter serve`, started on an
//! ephemeral loopback port inside this process, so the desktop app keeps all of its
//! protections (Host and CSRF checks, sandboxed worker processes, strict CSP). The binary
//! doubles as the worker executable: `openconverter-desktop __worker ...` runs one job.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;
use tauri::webview::DownloadEvent;
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// A file name that cannot collide with an existing one in `dir`.
fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let name = std::path::Path::new(name)
        .file_name()
        .map_or_else(|| "converted".into(), |n| n.to_os_string());
    let candidate = dir.join(&name);
    if !candidate.exists() {
        return candidate;
    }
    let p = std::path::Path::new(&name);
    let stem = p
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap_or(candidate)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("__worker") {
        return oc_cli::run(args);
    }
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("error: cannot find this executable");
        return ExitCode::FAILURE;
    };
    let mut cfg = oc_server::Config::new(exe);
    cfg.port = 0;
    cfg.open_browser = false;
    let server = match oc_server::start(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let url = server.url.clone();
    let port = server.port;
    let built = tauri::Builder::default()
        .setup(move |app| {
            let target: tauri::Url = url.parse()?;
            let handle = app.handle().clone();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(target))
                .title("OpenConverter")
                .inner_size(1100.0, 780.0)
                .min_inner_size(800.0, 600.0)
                // Let the page's own drag-and-drop handle dropped files.
                .disable_drag_drop_handler()
                // The window may only show the local server.
                .on_navigation(move |u| {
                    u.scheme() == "http"
                        && u.host_str() == Some("127.0.0.1")
                        && u.port() == Some(port)
                })
                // Converted files go to the Downloads folder instead of a save dialog.
                .on_download(move |_, event| match event {
                    DownloadEvent::Requested { url, destination } => {
                        let Ok(dir) = tauri::Manager::path(&handle).download_dir() else {
                            return false;
                        };
                        let name = url
                            .query_pairs()
                            .find(|(k, _)| k == "name")
                            .map(|(_, v)| v.into_owned())
                            .or_else(|| {
                                destination
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                            })
                            .unwrap_or_else(|| "converted".into());
                        *destination = unique_path(&dir, &name);
                        true
                    }
                    _ => true,
                })
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!());
    drop(server);
    match built {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
