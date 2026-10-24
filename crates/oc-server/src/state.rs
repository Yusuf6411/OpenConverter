//! Shared server state: uploads, jobs, outputs, history and settings.
use crate::Config;
use oc_core::CancelToken;
use oc_engine::isolated::Isolation;
use oc_engine::Engine;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicUsize};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{watch, Semaphore};

pub const MAX_STREAMS: usize = 32;
const HISTORY_LIMIT: usize = 1000;

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// A fresh 32-character lowercase hex id.
pub fn new_id() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        // Fall back to the clock; ids only need to be unguessable in practice.
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        b.copy_from_slice(&n.to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub struct FileRec {
    pub path: PathBuf,
    /// Format id string (`png`, `csv`, ...).
    pub format: &'static str,
    pub meta: Value,
    pub created: f64,
}

pub struct JobRec {
    pub cancel: CancelToken,
    pub tx: watch::Sender<Value>,
}

pub struct OutRec {
    pub path: PathBuf,
    pub name: String,
    pub format: &'static str,
    pub created: f64,
}

pub struct App {
    pub engine: Engine,
    pub cfg: Config,
    pub token: Option<String>,
    pub port: AtomicU16,
    pub settings: Mutex<Map<String, Value>>,
    pub files: Mutex<HashMap<String, FileRec>>,
    pub jobs: Mutex<HashMap<String, JobRec>>,
    pub outputs: Mutex<HashMap<String, OutRec>>,
    pub history: Mutex<Vec<Value>>,
    pub sem: Semaphore,
    pub streams: AtomicUsize,
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn default_settings() -> Map<String, Value> {
    let Value::Object(m) = json!({
        "theme": "system",
        "default_jpeg_quality": 85,
        "default_png_level": 6,
        "alpha_background": "#FFFFFF",
        "retention_hours": 24,
        "max_upload_mb": 200
    }) else {
        return Map::new();
    };
    m
}

impl App {
    pub fn new(cfg: Config, remote: bool) -> std::io::Result<Arc<Self>> {
        for sub in ["uploads", "outputs", "previews"] {
            std::fs::create_dir_all(cfg.data_dir.join(sub))?;
        }
        // Uploads do not survive a restart; clear leftovers so nothing lingers on disk.
        for sub in ["uploads", "previews"] {
            if let Ok(rd) = std::fs::read_dir(cfg.data_dir.join(sub)) {
                for e in rd.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let mut settings = default_settings();
        if let Some(Value::Object(saved)) = std::fs::read(cfg.data_dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            for (k, v) in saved {
                if settings.contains_key(&k) {
                    settings.insert(k, v);
                }
            }
        }
        let history: Vec<Value> = std::fs::read(cfg.data_dir.join("history.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let token = remote.then(|| {
            let mut b = [0u8; 18];
            let _ = getrandom::fill(&mut b);
            b.iter().map(|x| format!("{x:02x}")).collect::<String>()
        });
        let workers = cfg.workers.max(1);
        let app = Arc::new(Self {
            engine: Engine::new(),
            token,
            port: AtomicU16::new(cfg.port),
            settings: Mutex::new(settings),
            files: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            outputs: Mutex::new(HashMap::new()),
            history: Mutex::new(history),
            sem: Semaphore::new(workers),
            streams: AtomicUsize::new(0),
            cfg,
        });
        // Outputs that are still on disk stay downloadable from the history.
        {
            let hist = lock(&app.history);
            let mut outs = lock(&app.outputs);
            for e in hist.iter() {
                let (Some(id), Some(name), Some(fmt)) = (
                    e["output_id"].as_str(),
                    e["output_name"].as_str(),
                    e["target_format"].as_str(),
                ) else {
                    continue;
                };
                let Some(f) = app.engine.format_by_name(fmt) else {
                    continue;
                };
                let ext = app
                    .engine
                    .registry()
                    .format(f)
                    .and_then(|d| d.extensions.first().copied())
                    .unwrap_or("bin");
                let path = app.cfg.data_dir.join("outputs").join(format!("{id}.{ext}"));
                if valid_id(id) && path.exists() {
                    outs.insert(
                        id.to_string(),
                        OutRec {
                            path,
                            name: name.to_string(),
                            format: f.0,
                            created: e["timestamp"].as_f64().unwrap_or_else(now),
                        },
                    );
                }
            }
        }
        Ok(app)
    }

    pub fn set_port(&self, p: u16) {
        self.port.store(p, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn host_allowed(&self, host: &str) -> bool {
        // Listening on every interface: the name clients use cannot be known, and the
        // access token is what protects the server.
        if self
            .cfg
            .host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|a| a.is_unspecified())
        {
            return true;
        }
        let port = self.port.load(std::sync::atomic::Ordering::Relaxed);
        let h = host.to_ascii_lowercase();
        let mut ok = vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ];
        let cfg_host = self.cfg.host.to_ascii_lowercase();
        ok.push(format!("{cfg_host}:{port}"));
        if cfg_host.contains(':') {
            ok.push(format!("[{cfg_host}]:{port}"));
        }
        ok.contains(&h)
    }

    pub fn isolation(&self) -> Isolation {
        let mut iso = Isolation::new(self.cfg.worker_exe.clone());
        iso.timeout = Duration::from_secs(600);
        iso.max_memory = Some(4 << 30);
        iso
    }

    pub fn setting_i64(&self, key: &str) -> i64 {
        lock(&self.settings)[key].as_i64().unwrap_or(0)
    }

    pub fn save_settings(&self) {
        let s = lock(&self.settings).clone();
        let _ = std::fs::write(
            self.cfg.data_dir.join("settings.json"),
            Value::Object(s).to_string(),
        );
    }

    pub fn save_history(&self) {
        let h = lock(&self.history).clone();
        let _ = std::fs::write(
            self.cfg.data_dir.join("history.json"),
            Value::Array(h).to_string(),
        );
    }

    pub fn push_history(&self, entry: Value) {
        {
            let mut h = lock(&self.history);
            h.insert(0, entry);
            h.truncate(HISTORY_LIMIT);
        }
        self.save_history();
    }

    /// Removes uploads and outputs older than the retention period, hourly.
    pub fn start_cleanup(self: &Arc<Self>) {
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(600)).await;
                let keep = app.setting_i64("retention_hours").max(1) as f64 * 3600.0;
                let cutoff = now() - keep;
                let mut gone = Vec::new();
                lock(&app.files).retain(|_, f| {
                    let keep = f.created >= cutoff;
                    if !keep {
                        gone.push(f.path.clone());
                    }
                    keep
                });
                lock(&app.outputs).retain(|_, o| {
                    let keep = o.created >= cutoff;
                    if !keep {
                        gone.push(o.path.clone());
                    }
                    keep
                });
                for p in gone {
                    let _ = std::fs::remove_file(p);
                }
            }
        });
    }
}
