//! `openconverter serve`: the shared web UI and a JSON API on a local port.
//!
//! Every conversion, probe and preview runs in a sandboxed worker process, exactly as in
//! the CLI. The server itself never parses an upload. See `docs/API.md`.
#![forbid(unsafe_code)]
// `Value["key"]` yields Null for a missing key and never panics; the few slices are length-checked.
#![allow(clippy::indexing_slicing)]

mod api;
mod guard;
mod multipart;
pub mod state;

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use axum::Router;
use state::App;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

pub mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// Required, in addition to a non-loopback host, to listen beyond this computer.
    pub allow_remote: bool,
    /// Uploads, outputs, history and settings live here.
    pub data_dir: PathBuf,
    /// The executable that understands `__worker` (normally the CLI itself).
    pub worker_exe: PathBuf,
    /// Conversions that may run at once.
    pub workers: usize,
    pub open_browser: bool,
}

impl Config {
    pub fn new(worker_exe: PathBuf) -> Self {
        let data_dir = std::env::var_os("OPENCONVERTER_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(default_data_dir);
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            allow_remote: false,
            data_dir,
            worker_exe,
            workers: std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 8)),
            open_browser: true,
        }
    }
}

fn default_data_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.unwrap_or_else(std::env::temp_dir)
        .join("openconverter")
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|a| a.is_loopback())
}

/// The router, for tests and for `run`.
pub fn router(app: Arc<App>) -> Router {
    let files = Router::new()
        .route("/api/files", post(api::upload).get(api::lookup))
        .layer(DefaultBodyLimit::disable());
    Router::new()
        .route("/api/health", get(api::health))
        .route("/api/formats", get(api::formats))
        .route(
            "/api/settings",
            get(api::get_settings).put(api::put_settings),
        )
        .route("/api/files/{id}", get(api::file))
        .route("/api/files/{id}/preview", get(api::file_preview))
        .route("/api/files/{id}/table", get(api::table_preview))
        .route("/api/files/{id}/entries", post(api::archive_entries))
        .route("/api/files/{id}/extract", post(api::archive_extract))
        .route("/api/archives", post(api::create_archive))
        .route("/api/jobs", post(api::create_job).get(api::list_jobs))
        .route("/api/jobs/{id}", get(api::job))
        .route("/api/jobs/{id}/events", get(api::job_events))
        .route("/api/jobs/{id}/cancel", post(api::cancel_job))
        .route("/api/outputs/{id}/download", get(api::download))
        .route("/api/outputs/{id}/preview", get(api::output_preview))
        .route("/api/history", get(api::history).delete(api::clear_history))
        .route("/api/history/{id}", delete(api::delete_history))
        .merge(files)
        .fallback(get(guard::static_file))
        .layer(DefaultBodyLimit::max(1 << 20))
        .layer(axum::middleware::from_fn_with_state(
            app.clone(),
            guard::guard,
        ))
        .with_state(app)
}

fn check_host(cfg: &Config) -> Result<(), String> {
    if !is_loopback(&cfg.host) && !cfg.allow_remote {
        return Err(format!(
            "refusing to listen on {}: anyone who can reach this computer could use the server. Add --allow-remote to do it anyway (an access token is then required).",
            cfg.host
        ));
    }
    Ok(())
}

async fn bind(cfg: &Config) -> Result<(tokio::net::TcpListener, Arc<App>, String), String> {
    let remote = !is_loopback(&cfg.host);
    let app = App::new(cfg.clone(), remote).map_err(|e| e.to_string())?;
    let addr: SocketAddr = format!(
        "{}:{}",
        if cfg.host.contains(':') && !cfg.host.starts_with('[') {
            format!("[{}]", cfg.host)
        } else {
            cfg.host.clone()
        },
        cfg.port
    )
    .parse()
    .or_else(|_| {
        // A host name such as "localhost".
        use std::net::ToSocketAddrs;
        (cfg.host.as_str(), cfg.port)
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or_else(|| "cannot resolve the host".to_string())
    })?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    app.set_port(port);
    let url = match &app.token {
        Some(t) => format!("http://{}:{port}/?token={t}", cfg.host),
        None => format!("http://{}:{port}/", cfg.host),
    };
    Ok((listener, app, url))
}

/// Starts the server and blocks until it is stopped with Ctrl+C.
pub fn run(cfg: Config) -> Result<(), String> {
    check_host(&cfg)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let (listener, app, url) = bind(&cfg).await?;
        if app.token.is_some() {
            eprintln!(
                "WARNING: listening beyond this computer. Keep the access token in the address private."
            );
        }
        println!("OpenConverter is running at {url}");
        println!("Press Ctrl+C to stop.");
        if cfg.open_browser {
            open_browser(&url);
        }
        app.start_cleanup();
        axum::serve(listener, router(app))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|e| e.to_string())
    })
}

/// A server running on its own thread, for the desktop app.
pub struct Running {
    /// Where the UI is (with the access token when one is needed).
    pub url: String,
    pub port: u16,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

/// Starts the server on a background thread and returns once it is listening.
pub fn start(cfg: Config) -> Result<Running, String> {
    check_host(&cfg)?;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(String, u16), String>>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
                return;
            }
        };
        rt.block_on(async move {
            let (listener, app, url) = match bind(&cfg).await {
                Ok(v) => v,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let port = listener.local_addr().map_or(0, |a| a.port());
            app.start_cleanup();
            let _ = ready_tx.send(Ok((url, port)));
            let _ = axum::serve(listener, router(app))
                .with_graceful_shutdown(async {
                    let _ = stop_rx.await;
                })
                .await;
        });
    });
    let (url, port) = ready_rx.recv().map_err(|e| e.to_string())??;
    Ok(Running {
        url,
        port,
        stop: Some(stop_tx),
    })
}

fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let cmd = ("xdg-open", vec![url.to_string()]);
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url.to_string()]);
    #[cfg(windows)]
    let cmd = (
        "rundll32",
        vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()],
    );
    let _ = std::process::Command::new(cmd.0)
        .args(&cmd.1)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
