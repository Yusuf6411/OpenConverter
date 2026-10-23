//! The JSON API (see docs/API.md).
use crate::guard::error_response;
use crate::multipart::{boundary, Multipart};
use crate::state::{lock, new_id, now, valid_id, App, FileRec, JobRec, OutRec, MAX_STREAMS};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use oc_core::options::{OptionKind, OptionSpec};
use oc_core::{CancelToken, Category, ErrorCode, FormatId};
use oc_engine::isolated::{
    convert_isolated, create_isolated, extract_isolated, list_isolated, run_worker,
};
use oc_engine::ConvertRequest;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub struct ApiErr(StatusCode, String, String);

impl ApiErr {
    fn new(status: StatusCode, code: &str, msg: impl Into<String>) -> Self {
        Self(status, code.into(), msg.into())
    }
    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "Not found.")
    }
    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        error_response(self.0, &self.1, &self.2)
    }
}

type R<T> = Result<T, ApiErr>;

fn category_id(c: Category) -> &'static str {
    match c {
        Category::Image => "image",
        Category::Text => "text",
        Category::Sheet => "data",
        Category::Audio => "audio",
        Category::Video => "video",
        Category::Document => "document",
        Category::Pdf => "pdf",
        Category::Subtitle => "subtitle",
        Category::Font => "font",
        Category::Archive => "archive",
    }
}

fn category_name(c: Category) -> &'static str {
    match c {
        Category::Image => "Images",
        Category::Text => "Text",
        Category::Sheet => "Tabular data",
        Category::Audio => "Audio",
        Category::Video => "Video",
        Category::Document => "Documents",
        Category::Pdf => "PDF",
        Category::Subtitle => "Subtitles",
        Category::Font => "Fonts",
        Category::Archive => "Archives",
    }
}

fn hex(c: [u8; 4]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn option_json(spec: &OptionSpec, app: &App) -> Value {
    let settings = lock(&app.settings);
    let mut o = Map::new();
    o.insert("label".into(), spec.label.into());
    o.insert("help".into(), spec.help.into());
    match &spec.kind {
        OptionKind::Int { min, max, default } => {
            // The user's saved defaults for the two most common knobs.
            let default = match spec.key {
                "quality" => settings["default_jpeg_quality"]
                    .as_i64()
                    .filter(|q| (min..=max).contains(&q)),
                "level" => settings["default_png_level"]
                    .as_i64()
                    .filter(|q| (min..=max).contains(&q)),
                _ => None,
            }
            .unwrap_or(*default);
            o.insert("type".into(), "int".into());
            o.insert("min".into(), (*min).into());
            o.insert("max".into(), (*max).into());
            o.insert("default".into(), default.into());
        }
        OptionKind::Float { min, max, default } => {
            o.insert("type".into(), "float".into());
            o.insert("min".into(), (*min).into());
            o.insert("max".into(), (*max).into());
            o.insert("default".into(), (*default).into());
        }
        OptionKind::Bool { default } => {
            o.insert("type".into(), "bool".into());
            o.insert("default".into(), (*default).into());
        }
        OptionKind::Enum { choices, default } => {
            o.insert("type".into(), "choice".into());
            o.insert("choices".into(), json!(choices));
            o.insert("default".into(), (*default).into());
        }
        OptionKind::Color { default } => {
            let d = if spec.key == "background" {
                settings["alpha_background"].as_str().map(str::to_string)
            } else {
                None
            }
            .unwrap_or_else(|| hex(*default));
            o.insert("type".into(), "colour".into());
            o.insert("default".into(), d.into());
        }
        OptionKind::Text { default } => {
            o.insert("type".into(), "text".into());
            o.insert("default".into(), default.clone().into());
        }
    }
    Value::Object(o)
}

pub async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

pub async fn formats(State(app): State<Arc<App>>) -> Json<Value> {
    let mut cats: Vec<Category> = Vec::new();
    let list: Vec<Value> = app
        .engine
        .formats()
        .into_iter()
        .map(|f| {
            if !cats.contains(&f.category) {
                cats.push(f.category);
            }
            // Options of the format itself first, then the ones every image output shares.
            const COMMON: [&str; 8] = [
                "background",
                "width",
                "height",
                "rotate",
                "flip",
                "frames",
                "profile",
                "intent",
            ];
            let mut specs: Vec<&OptionSpec> = f.writer_options.specs.iter().collect();
            specs.sort_by_key(|s| COMMON.iter().position(|c| *c == s.key).map_or(0, |i| i + 1));
            let opts: Map<String, Value> = specs
                .into_iter()
                .map(|s| (s.key.to_string(), option_json(s, &app)))
                .collect();
            let targets: Vec<&str> = app.engine.targets(f.id).into_iter().map(|t| t.0).collect();
            json!({
                "id": f.id.0,
                "name": f.name,
                "category": category_id(f.category),
                "extensions": f.extensions.iter().map(|e| format!(".{e}")).collect::<Vec<_>>(),
                "mime": f.mime.first().copied().unwrap_or("application/octet-stream"),
                "can_read": f.capabilities.read,
                "can_write": f.capabilities.write,
                "ownership": format!("{:?}", f.ownership).to_uppercase(),
                "ownership_note": "",
                "variants_note": f.spec,
                "read_options": {},
                "write_options": opts,
                "targets": targets,
                "identity_meaningful": !f.writer_options.specs.is_empty(),
            })
        })
        .collect();
    let categories: Vec<Value> = cats
        .into_iter()
        .map(|c| json!({"id": category_id(c), "name": category_name(c)}))
        .collect();
    Json(json!({"formats": list, "categories": categories}))
}

pub async fn get_settings(State(app): State<Arc<App>>) -> Json<Value> {
    Json(Value::Object(lock(&app.settings).clone()))
}

pub async fn put_settings(State(app): State<Arc<App>>, Json(body): Json<Value>) -> R<Json<Value>> {
    let Value::Object(given) = body else {
        return Err(ApiErr::bad("Settings must be a JSON object."));
    };
    let mut next = lock(&app.settings).clone();
    for (k, v) in given {
        let ok = match k.as_str() {
            "theme" => matches!(v.as_str(), Some("system" | "light" | "dark")),
            "default_jpeg_quality" => v.as_i64().is_some_and(|n| (1..=100).contains(&n)),
            "default_png_level" => v.as_i64().is_some_and(|n| (0..=9).contains(&n)),
            "alpha_background" => v.as_str().is_some_and(|s| {
                s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|c| c.is_ascii_hexdigit())
            }),
            "retention_hours" => v.as_i64().is_some_and(|n| (1..=720).contains(&n)),
            "max_upload_mb" => v.as_i64().is_some_and(|n| (1..=4096).contains(&n)),
            _ => return Err(ApiErr::bad(format!("Unknown setting '{k}'."))),
        };
        if !ok {
            return Err(ApiErr::bad(format!("Invalid value for '{k}'.")));
        }
        let v = if k == "alpha_background" {
            Value::String(v.as_str().unwrap_or("#FFFFFF").to_uppercase())
        } else {
            v
        };
        next.insert(k, v);
    }
    *lock(&app.settings) = next.clone();
    app.save_settings();
    Ok(Json(Value::Object(next)))
}

/// A name that is safe to show and to put in a header or file name.
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_lowercase();
    let reserved = matches!(
        stem.as_str(),
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
    );
    let cleaned: String = cleaned.chars().take(180).collect();
    if cleaned.is_empty() || reserved {
        format!("file-{cleaned}")
    } else {
        cleaned
    }
}

fn format_ext(app: &App, id: FormatId) -> &'static str {
    app.engine
        .registry()
        .format(id)
        .and_then(|d| d.extensions.first().copied())
        .unwrap_or("bin")
}

fn error_entry(name: &str, size: u64, code: &str, msg: &str) -> Value {
    json!({"original_name": name, "size": size, "error": {"code": code, "message": msg}})
}

pub async fn upload(
    State(app): State<Arc<App>>,
    headers: axum::http::HeaderMap,
    body: Body,
) -> R<Json<Value>> {
    let boundary = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(boundary)
        .ok_or_else(|| ApiErr::bad("Expected a multipart/form-data upload."))?;
    let mut mp = Multipart::new(body, &boundary);
    let max = (app.setting_i64("max_upload_mb").max(1) as u64) << 20;
    let mut entries = Vec::new();
    while let Some(part) = mp.next_part().await.map_err(|e| ApiErr::bad(e.0))? {
        if part.name != "files" || entries.len() >= 64 {
            continue;
        }
        let name = safe_name(part.filename.as_deref().unwrap_or("file"));
        let id = new_id();
        // The extension decides how the engine reads text and tables, and is replaced
        // below once the content is known; the first bytes are enough to detect images.
        let tmp = app
            .cfg
            .data_dir
            .join("uploads")
            .join(format!("{id}.upload"));
        let mut file = tokio::fs::File::create(&tmp).await.map_err(|e| {
            ApiErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                e.to_string(),
            )
        })?;
        let mut size = 0u64;
        let mut head = Vec::new();
        loop {
            match mp.chunk().await {
                Ok(Some(chunk)) => {
                    size += chunk.len() as u64;
                    if size > max {
                        drop(file);
                        let _ = tokio::fs::remove_file(&tmp).await;
                        return Err(ApiErr::new(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "limit_exceeded",
                            format!("The file is larger than the {} MB upload limit.", max >> 20),
                        ));
                    }
                    if head.len() < 65536 {
                        let take = (65536 - head.len()).min(chunk.len());
                        head.extend_from_slice(&chunk[..take]);
                    }
                    if file.write_all(&chunk).await.is_err() {
                        let _ = tokio::fs::remove_file(&tmp).await;
                        return Err(ApiErr::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "internal_error",
                            "Cannot store the upload.",
                        ));
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    drop(file);
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(ApiErr::bad(e.0));
                }
            }
        }
        let _ = file.flush().await;
        drop(file);
        entries.push(finish_upload(&app, id, name, size, head, tmp).await);
    }
    if entries.is_empty() {
        return Err(ApiErr::bad("No files in the request."));
    }
    Ok(Json(json!({"files": entries})))
}

async fn finish_upload(
    app: &Arc<App>,
    id: String,
    name: String,
    size: u64,
    head: Vec<u8>,
    tmp: std::path::PathBuf,
) -> Value {
    let name_path = std::path::Path::new(&name);
    let Some(fmt) = app.engine.detect(&head, name_path) else {
        let _ = tokio::fs::remove_file(&tmp).await;
        return error_entry(
            &name,
            size,
            "unsupported_format",
            "This file type is not supported.",
        );
    };
    let ext = format_ext(app, fmt);
    let path = app.cfg.data_dir.join("uploads").join(format!("{id}.{ext}"));
    if tokio::fs::rename(&tmp, &path).await.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return error_entry(&name, size, "internal_error", "Cannot store the upload.");
    }
    let desc = app.engine.registry().format(fmt);
    let category = desc.map_or("", |d| category_id(d.category));
    let by_ext = app.engine.format_for_path(name_path);
    let mut meta = json!({
        "file_id": id,
        "original_name": name,
        "size": size,
        "detected_format": fmt.0,
        "category": category,
        "extension_mismatch": by_ext.is_some_and(|e| e != fmt),
        "available_targets": app.engine.targets(fmt).iter().map(|t| t.0).collect::<Vec<_>>(),
        "uploaded": now(),
    });
    if category == "image" {
        let app2 = app.clone();
        let p = path.clone();
        let probe = tokio::task::spawn_blocking(move || {
            let args = vec![
                "__worker".to_string(),
                "probe".to_string(),
                p.display().to_string(),
                fmt.0.to_string(),
            ];
            let mut iso = app2.isolation();
            iso.timeout = Duration::from_secs(60);
            run_worker(&iso, &args, &CancelToken::new())
        })
        .await;
        match probe {
            Ok(Ok(out)) => {
                let kv: HashMap<&str, &str> =
                    out.lines().filter_map(|l| l.split_once('=')).collect();
                let num = |k: &str| kv.get(k).and_then(|v| v.parse::<u64>().ok());
                if let Some(o) = meta.as_object_mut() {
                    o.insert("width".into(), num("width").into());
                    o.insert("height".into(), num("height").into());
                    o.insert(
                        "may_have_alpha".into(),
                        (kv.get("alpha") == Some(&"true")).into(),
                    );
                }
            }
            Ok(Err(e)) => {
                let _ = tokio::fs::remove_file(&path).await;
                return error_entry(&name, size, e.code.as_str(), &e.message);
            }
            Err(_) => {
                let _ = tokio::fs::remove_file(&path).await;
                return error_entry(
                    &name,
                    size,
                    "internal_error",
                    "The file could not be inspected.",
                );
            }
        }
    }
    lock(&app.files).insert(
        id,
        FileRec {
            path,
            format: fmt.0,
            meta: meta.clone(),
            created: now(),
        },
    );
    meta
}

pub async fn file(State(app): State<Arc<App>>, Path(id): Path<String>) -> R<Json<Value>> {
    if !valid_id(&id) {
        return Err(ApiErr::not_found());
    }
    lock(&app.files)
        .get(&id)
        .map(|f| Json(f.meta.clone()))
        .ok_or_else(ApiErr::not_found)
}

#[derive(serde::Deserialize)]
pub struct LookupQuery {
    ids: Option<String>,
    outputs: Option<String>,
}

pub async fn lookup(State(app): State<Arc<App>>, Query(q): Query<LookupQuery>) -> Json<Value> {
    let split = |s: &Option<String>| -> Vec<String> {
        s.as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|x| !x.is_empty())
            .take(500)
            .map(str::to_string)
            .collect()
    };
    let files = lock(&app.files);
    let outs = lock(&app.outputs);
    let f: Map<String, Value> = split(&q.ids)
        .into_iter()
        .map(|id| {
            let v = if valid_id(&id) {
                files.get(&id).map_or(Value::Null, |r| r.meta.clone())
            } else {
                Value::Null
            };
            (id, v)
        })
        .collect();
    let o: Map<String, Value> = split(&q.outputs)
        .into_iter()
        .map(|id| {
            let ok = valid_id(&id) && outs.get(&id).is_some_and(|r| r.path.exists());
            (id, Value::Bool(ok))
        })
        .collect();
    Json(json!({"files": f, "outputs": o}))
}

async fn preview_of(
    app: &Arc<App>,
    key: &str,
    path: std::path::PathBuf,
    format: &'static str,
) -> Response {
    let cache = app.cfg.data_dir.join("previews").join(format!("{key}.png"));
    if let Ok(bytes) = tokio::fs::read(&cache).await {
        return png(bytes);
    }
    let app2 = app.clone();
    let out = cache.clone();
    let res = tokio::task::spawn_blocking(move || {
        let args = vec![
            "__worker".to_string(),
            "thumb".to_string(),
            path.display().to_string(),
            format.to_string(),
            out.display().to_string(),
        ];
        let mut iso = app2.isolation();
        iso.timeout = Duration::from_secs(60);
        run_worker(&iso, &args, &CancelToken::new())
    })
    .await;
    match res {
        Ok(Ok(_)) => match tokio::fs::read(&cache).await {
            Ok(b) => png(b),
            Err(_) => no_preview("internal_error"),
        },
        Ok(Err(e)) => no_preview(e.code.as_str()),
        Err(_) => no_preview("internal_error"),
    }
}

fn png(bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, max-age=3600"),
        ],
        bytes,
    )
        .into_response()
}

fn no_preview(code: &str) -> Response {
    let mut r = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(code) {
        r.headers_mut().insert("x-openconverter-error", v);
    }
    r
}

pub async fn file_preview(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let rec = if valid_id(&id) {
        lock(&app.files)
            .get(&id)
            .map(|f| (f.path.clone(), f.format, f.meta["category"] == "image"))
    } else {
        None
    };
    match rec {
        None => ApiErr::not_found().into_response(),
        Some((_, _, false)) => error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_format",
            "Only images have previews.",
        ),
        Some((path, fmt, true)) => preview_of(&app, &id, path, fmt).await,
    }
}

pub async fn output_preview(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let rec = if valid_id(&id) {
        lock(&app.outputs)
            .get(&id)
            .map(|o| (o.path.clone(), o.format))
    } else {
        None
    };
    match rec {
        None => ApiErr::not_found().into_response(),
        Some((path, fmt)) => {
            let is_image = app
                .engine
                .format_by_name(fmt)
                .and_then(|f| app.engine.registry().format(f))
                .is_some_and(|d| d.category == Category::Image);
            if !is_image {
                return error_response(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "unsupported_format",
                    "Only images have previews.",
                );
            }
            preview_of(&app, &format!("o{id}"), path, fmt).await
        }
    }
}

fn job_snapshot(
    id: &str,
    file: &Value,
    target: &str,
    state: &str,
    progress: f64,
    stage: &str,
) -> Value {
    json!({
        "job_id": id,
        "file_id": file["file_id"],
        "source_format": file["detected_format"],
        "target_format": target,
        "original_name": file["original_name"],
        "state": state,
        "progress": progress,
        "stage": stage,
        "message": "",
        "result": null,
        "error": null,
        "created": now(),
    })
}

fn set_state(app: &App, id: &str, f: impl FnOnce(&mut Map<String, Value>)) {
    if let Some(j) = lock(&app.jobs).get(id) {
        j.tx.send_modify(|v| {
            if let Some(m) = v.as_object_mut() {
                f(m);
            }
        });
    }
}

/// Options for a job: the request's, with the user's saved defaults for what it leaves out.
fn job_options(
    app: &App,
    to: FormatId,
    out_path: &std::path::Path,
    given: Option<&Value>,
) -> R<Vec<(String, oc_core::options::OptionValue)>> {
    let schema = app
        .engine
        .registry()
        .format(to)
        .map(|d| d.writer_options.clone())
        .unwrap_or_default();
    let mut raw: Vec<(String, String)> = Vec::new();
    if let Some(opts) = given {
        let Value::Object(opts) = opts else {
            return Err(ApiErr::bad("options must be an object."));
        };
        for (k, v) in opts {
            if !schema.specs.iter().any(|s| s.key == k) {
                return Err(ApiErr::bad(format!("Unknown option '{k}'.")));
            }
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return Err(ApiErr::bad(format!("Invalid value for option '{k}'."))),
            };
            raw.push((k.clone(), s));
        }
    }
    for (key, setting) in [
        ("quality", "default_jpeg_quality"),
        ("level", "default_png_level"),
        ("background", "alpha_background"),
    ] {
        if schema.specs.iter().any(|s| s.key == key) && !raw.iter().any(|(k, _)| k == key) {
            let v = lock(&app.settings)[setting].clone();
            let text = match v {
                Value::String(s) => s,
                other => other.to_string(),
            };
            raw.push((key.to_string(), text));
        }
    }
    let options = app
        .engine
        .parse_options(out_path, &raw)
        .map_err(|e| ApiErr::bad(e.message))?;
    // Re-check the values against ranges now, so a bad one fails the request, not the job.
    schema
        .validate(&options)
        .map_err(|e| ApiErr::bad(e.message))?;
    Ok(options)
}

pub async fn create_job(
    State(app): State<Arc<App>>,
    Json(body): Json<Value>,
) -> R<(StatusCode, Json<Value>)> {
    let Value::Object(body) = body else {
        return Err(ApiErr::bad("The request must be a JSON object."));
    };
    for k in body.keys() {
        if !matches!(k.as_str(), "file_id" | "target_format" | "options") {
            return Err(ApiErr::bad(format!("Unknown field '{k}'.")));
        }
    }
    let file_id = body.get("file_id").and_then(Value::as_str).unwrap_or("");
    let target = body
        .get("target_format")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !valid_id(file_id) {
        return Err(ApiErr::not_found());
    }
    let (in_path, from, meta) = {
        let files = lock(&app.files);
        let f = files.get(file_id).ok_or_else(ApiErr::not_found)?;
        (f.path.clone(), f.format, f.meta.clone())
    };
    let to = app
        .engine
        .format_by_name(target)
        .ok_or_else(|| ApiErr::bad("Unknown target format."))?;
    let from_id = app
        .engine
        .format_by_name(from)
        .ok_or_else(ApiErr::not_found)?;
    if !app.engine.targets(from_id).contains(&to) {
        return Err(ApiErr::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_format",
            "This file cannot be converted to that format.",
        ));
    }
    let ext = format_ext(&app, to);
    let out_id = new_id();
    let out_path = app
        .cfg
        .data_dir
        .join("outputs")
        .join(format!("{out_id}.{ext}"));
    let options = job_options(&app, to, &out_path, body.get("options"))?;

    let job_id = new_id();
    let cancel = CancelToken::new();
    let (tx, _rx) = tokio::sync::watch::channel(job_snapshot(
        &job_id, &meta, target, "queued", 0.0, "queued",
    ));
    lock(&app.jobs).insert(
        job_id.clone(),
        JobRec {
            cancel: cancel.clone(),
            tx,
        },
    );
    let app2 = app.clone();
    let jid = job_id.clone();
    let target_s = target.to_string();
    tokio::spawn(async move {
        run_job(
            app2, jid, meta, target_s, in_path, out_path, out_id, options, None, cancel,
        )
        .await;
    });
    Ok((StatusCode::CREATED, Json(json!({"job_id": job_id}))))
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    app: Arc<App>,
    job_id: String,
    file: Value,
    target: String,
    in_path: std::path::PathBuf,
    out_path: std::path::PathBuf,
    out_id: String,
    options: Vec<(String, oc_core::options::OptionValue)>,
    pack: Option<(std::path::PathBuf, Vec<std::path::PathBuf>)>,
    cancel: CancelToken,
) {
    let permit = app.sem.acquire().await;
    if cancel.is_cancelled() {
        finish_cancelled(&app, &job_id);
        return;
    }
    set_state(&app, &job_id, |m| {
        m.insert("state".into(), "running".into());
        m.insert("stage".into(), "converting".into());
        m.insert("progress".into(), 5.0.into());
    });
    let started = std::time::Instant::now();
    let app2 = app.clone();
    let c2 = cancel.clone();
    let (ip, op) = (in_path.clone(), out_path.clone());
    let display_name = file["original_name"].as_str().map(str::to_string);
    let pack_dir = pack.clone();
    let result = tokio::task::spawn_blocking(move || {
        if let Some((_, sources)) = &pack {
            return create_isolated(
                &app2.engine,
                &app2.isolation(),
                sources,
                &op,
                true,
                &options,
                &c2,
            );
        }
        let req = ConvertRequest {
            input: ip,
            output: op,
            overwrite: true,
            options,
            display_name,
        };
        convert_isolated(&app2.engine, &app2.isolation(), &req, &c2)
    })
    .await;
    if let Some((dir, _)) = &pack_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    drop(permit);
    let duration_ms = started.elapsed().as_millis() as u64;
    let input_size = file["size"].as_u64().unwrap_or(0);
    let name = file["original_name"].as_str().unwrap_or("file");
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let ext = app
        .engine
        .format_by_name(&target)
        .map_or("bin", |f| format_ext(&app, f));
    let out_name = safe_name(&format!("{stem}.{ext}"));
    let history_base = |status: &str| {
        json!({
            "id": job_id, "job_id": job_id, "original_name": name, "output_name": out_name,
            "source_format": file["detected_format"], "target_format": target,
            "input_size": input_size, "duration_ms": duration_ms, "status": status,
            "timestamp": now(),
        })
    };
    match result {
        Ok(Ok(report)) => {
            let size = report.bytes_out;
            let notes: Vec<String> = report.notes.iter().map(|n| n.message.clone()).collect();
            let details: Vec<Value> = report
                .notes
                .iter()
                .map(|n| json!({"severity": format!("{:?}", n.severity).to_lowercase(), "code": n.code, "message": n.message}))
                .collect();
            let fmt: &'static str = app.engine.format_by_name(&target).map_or("", |f| f.0);
            lock(&app.outputs).insert(
                out_id.clone(),
                OutRec {
                    path: out_path,
                    name: out_name.clone(),
                    format: fmt,
                    created: now(),
                },
            );
            let mut info = Map::new();
            for k in ["width", "height"] {
                if !file[k].is_null() {
                    info.insert(k.into(), file[k].clone());
                }
            }
            let mut h = history_base("done");
            if let Some(m) = h.as_object_mut() {
                m.insert("output_id".into(), out_id.clone().into());
                m.insert("output_size".into(), size.into());
                m.insert("notes".into(), json!(notes));
                m.insert("error_code".into(), Value::Null);
                if let (Some(w), Some(hh)) = (file["width"].as_f64(), file["height"].as_f64()) {
                    m.insert("megapixels".into(), (w * hh / 1e6).into());
                }
            }
            app.push_history(h);
            set_state(&app, &job_id, |m| {
                m.insert("state".into(), "done".into());
                m.insert("progress".into(), 100.0.into());
                m.insert("stage".into(), "done".into());
                m.insert(
                    "result".into(),
                    json!({"output_id": out_id, "output_name": out_name, "size": size,
                           "input_size": input_size, "duration_ms": duration_ms,
                           "notes": notes, "note_details": details, "route": report.route,
                           "info": info}),
                );
            });
        }
        Ok(Err(e)) if e.code == ErrorCode::Cancelled => finish_cancelled(&app, &job_id),
        Ok(Err(e)) => fail_job(
            &app,
            &job_id,
            history_base("failed"),
            e.code.as_str(),
            &e.message,
        ),
        Err(_) => fail_job(
            &app,
            &job_id,
            history_base("failed"),
            "internal_error",
            "The conversion stopped unexpectedly.",
        ),
    }
    let _ = std::fs::remove_file(format!(
        "{}.oc-partial",
        out_path_display(&app, &out_id, &target)
    ));
}

fn out_path_display(app: &App, out_id: &str, target: &str) -> String {
    let ext = app
        .engine
        .format_by_name(target)
        .map_or("bin", |f| format_ext(app, f));
    app.cfg
        .data_dir
        .join("outputs")
        .join(format!("{out_id}.{ext}"))
        .display()
        .to_string()
}

fn finish_cancelled(app: &App, id: &str) {
    set_state(app, id, |m| {
        m.insert("state".into(), "cancelled".into());
        m.insert("stage".into(), "cancelled".into());
        m.insert(
            "error".into(),
            json!({"code": "cancelled", "message": "Cancelled."}),
        );
    });
}

fn fail_job(app: &App, id: &str, mut history: Value, code: &str, message: &str) {
    if let Some(m) = history.as_object_mut() {
        m.insert("error_code".into(), code.into());
        m.insert("notes".into(), json!([message]));
        m.insert("output_id".into(), Value::Null);
        m.insert("output_size".into(), Value::Null);
    }
    app.push_history(history);
    let (code, message) = (code.to_string(), message.to_string());
    set_state(app, id, |m| {
        m.insert("state".into(), "failed".into());
        m.insert("stage".into(), "failed".into());
        m.insert("error".into(), json!({"code": code, "message": message}));
    });
}

pub async fn list_jobs(State(app): State<Arc<App>>) -> Json<Value> {
    let jobs: Vec<Value> = lock(&app.jobs)
        .values()
        .map(|j| j.tx.borrow().clone())
        .collect();
    Json(json!({"jobs": jobs}))
}

pub async fn job(State(app): State<Arc<App>>, Path(id): Path<String>) -> R<Json<Value>> {
    if !valid_id(&id) {
        return Err(ApiErr::not_found());
    }
    lock(&app.jobs)
        .get(&id)
        .map(|j| Json(j.tx.borrow().clone()))
        .ok_or_else(ApiErr::not_found)
}

pub async fn cancel_job(State(app): State<Arc<App>>, Path(id): Path<String>) -> R<Json<Value>> {
    if !valid_id(&id) {
        return Err(ApiErr::not_found());
    }
    let cancel = lock(&app.jobs)
        .get(&id)
        .map(|j| j.cancel.clone())
        .ok_or_else(ApiErr::not_found)?;
    cancel.cancel();
    // A queued job notices when it reaches the front; show the cancellation right away.
    let state = lock(&app.jobs)
        .get(&id)
        .map(|j| j.tx.borrow()["state"].clone());
    if state.as_ref().and_then(Value::as_str) == Some("queued") {
        finish_cancelled(&app, &id);
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    lock(&app.jobs)
        .get(&id)
        .map(|j| Json(j.tx.borrow().clone()))
        .ok_or_else(ApiErr::not_found)
}

struct StreamGuard(Arc<App>);
impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.streams.fetch_sub(1, Ordering::Relaxed);
    }
}

pub async fn job_events(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    if !valid_id(&id) {
        return ApiErr::not_found().into_response();
    }
    let Some(mut rx) = lock(&app.jobs).get(&id).map(|j| j.tx.subscribe()) else {
        return ApiErr::not_found().into_response();
    };
    if app.streams.fetch_add(1, Ordering::Relaxed) >= MAX_STREAMS {
        app.streams.fetch_sub(1, Ordering::Relaxed);
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "Too many open progress streams.",
        );
    }
    let guard = StreamGuard(app.clone());
    let (tx, out) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(8);
    tokio::spawn(async move {
        let _guard = guard;
        let mut n = 0u64;
        loop {
            let snap = rx.borrow_and_update().clone();
            n += 1;
            let state = snap["state"].as_str().unwrap_or("").to_string();
            let name = if matches!(state.as_str(), "running") {
                "progress"
            } else {
                "state"
            };
            let ev = Event::default()
                .id(n.to_string())
                .event(name)
                .data(snap.to_string());
            if tx.send(Ok(ev)).await.is_err() {
                return;
            }
            if matches!(state.as_str(), "done" | "failed" | "cancelled") {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
    Sse::new(tokio_stream::wrappers::ReceiverStream::new(out))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("heartbeat"),
        )
        .into_response()
}

fn disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' && c != '%' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut enc = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            enc.push(b as char);
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{enc}")
}

pub async fn download(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let rec = if valid_id(&id) {
        lock(&app.outputs)
            .get(&id)
            .map(|o| (o.path.clone(), o.name.clone(), o.format))
    } else {
        None
    };
    let Some((path, name, fmt)) = rec else {
        return ApiErr::not_found().into_response();
    };
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return ApiErr::not_found().into_response();
    };
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mime = app
        .engine
        .format_by_name(fmt)
        .and_then(|f| app.engine.registry().format(f))
        .and_then(|d| d.mime.first().copied())
        .unwrap_or("application/octet-stream");
    let mut resp = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(file)));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Ok(v) = HeaderValue::from_str(&disposition(&name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

pub async fn history(State(app): State<Arc<App>>) -> Json<Value> {
    let outs = lock(&app.outputs);
    let entries: Vec<Value> = lock(&app.history)
        .iter()
        .map(|e| {
            let mut e = e.clone();
            let expired = match e["output_id"].as_str() {
                Some(id) => !outs.get(id).is_some_and(|o| o.path.exists()),
                None => false,
            };
            if let Some(m) = e.as_object_mut() {
                m.insert("expired".into(), expired.into());
            }
            e
        })
        .collect();
    Json(json!({"entries": entries}))
}

pub async fn clear_history(State(app): State<Arc<App>>) -> StatusCode {
    lock(&app.history).clear();
    app.save_history();
    StatusCode::NO_CONTENT
}

pub async fn delete_history(State(app): State<Arc<App>>, Path(id): Path<String>) -> R<StatusCode> {
    if !valid_id(&id) {
        return Err(ApiErr::not_found());
    }
    let removed = {
        let mut h = lock(&app.history);
        let before = h.len();
        h.retain(|e| e["id"].as_str() != Some(id.as_str()));
        before != h.len()
    };
    if !removed {
        return Err(ApiErr::not_found());
    }
    app.save_history();
    Ok(StatusCode::NO_CONTENT)
}

fn engine_err(e: oc_core::Error) -> ApiErr {
    let status = match e.code {
        ErrorCode::EncryptedNeedPassword | ErrorCode::WrongPassword => StatusCode::UNAUTHORIZED,
        ErrorCode::UnsupportedFormat | ErrorCode::UnsupportedVariant => {
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        }
        ErrorCode::InvalidOptions => StatusCode::BAD_REQUEST,
        ErrorCode::Cancelled => StatusCode::CONFLICT,
        ErrorCode::LimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::CorruptFile | ErrorCode::TruncatedFile => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiErr::new(status, e.code.as_str(), e.message)
}

fn password_of(body: &Value) -> Option<String> {
    body.get("password")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

fn archive_file(app: &App, id: &str) -> R<(std::path::PathBuf, Value)> {
    if !valid_id(id) {
        return Err(ApiErr::not_found());
    }
    let files = lock(&app.files);
    let f = files.get(id).ok_or_else(ApiErr::not_found)?;
    let is_arc = app
        .engine
        .format_by_name(f.format)
        .is_some_and(|fid| oc_engine::archive::is_archive(fid) && fid != oc_engine::archive::FILE);
    if !is_arc {
        return Err(ApiErr::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_format",
            "This file is not an archive.",
        ));
    }
    Ok((f.path.clone(), f.meta.clone()))
}

/// `POST /api/files/{id}/entries`: what an archive contains, without unpacking it.
pub async fn archive_entries(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> R<Json<Value>> {
    let body = body.map_or(Value::Null, |Json(b)| b);
    let (path, meta) = archive_file(&app, &id)?;
    let password = password_of(&body);
    let app2 = app.clone();
    let permit = app.sem.acquire().await;
    let listed = tokio::task::spawn_blocking(move || {
        list_isolated(
            &app2.engine,
            &app2.isolation(),
            &path,
            password.as_deref(),
            &CancelToken::new(),
        )
    })
    .await;
    drop(permit);
    let (fmt, entries) = listed
        .map_err(|_| {
            ApiErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "The listing stopped unexpectedly.",
            )
        })?
        .map_err(engine_err)?;
    let kind = |c: char| match c {
        'd' => "dir",
        'l' => "link",
        _ => "file",
    };
    let total: u64 = entries
        .iter()
        .filter(|e| e.kind == 'f')
        .map(|e| e.size)
        .sum();
    let list: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({"path": e.path, "kind": kind(e.kind), "size": e.size, "mode": e.mode,
                   "mtime": e.mtime, "encrypted": e.encrypted, "link": e.link})
        })
        .collect();
    Ok(Json(json!({
        "file_id": id,
        "name": meta["original_name"],
        "format": fmt.0,
        "count": list.len(),
        "total_size": total,
        "encrypted": entries.iter().any(|e| e.encrypted),
        "entries": list,
    })))
}

/// `POST /api/files/{id}/extract`: unpack chosen entries (or everything) and offer the result
/// as a download: the file itself when it is a single one, a ZIP of the tree otherwise.
pub async fn archive_extract(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> R<Json<Value>> {
    let (path, meta) = archive_file(&app, &id)?;
    let password = password_of(&body);
    let mut only: Vec<String> = Vec::new();
    if let Some(v) = body.get("paths") {
        let arr = v
            .as_array()
            .ok_or_else(|| ApiErr::bad("paths must be a list."))?;
        if arr.len() > 100_000 {
            return Err(ApiErr::bad("Too many paths."));
        }
        for p in arr {
            only.push(
                p.as_str()
                    .ok_or_else(|| ApiErr::bad("paths must be strings."))?
                    .to_string(),
            );
        }
    }
    let out_id = new_id();
    let work = app
        .cfg
        .data_dir
        .join("previews")
        .join(format!("x-{out_id}"));
    let orig = meta["original_name"]
        .as_str()
        .unwrap_or("archive")
        .to_string();
    let stem = orig
        .split('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("archive")
        .to_string();
    let outputs = app.cfg.data_dir.join("outputs");
    let app2 = app.clone();
    let out_id2 = out_id.clone();
    let permit = app.sem.acquire().await;
    let done = tokio::task::spawn_blocking(
        move || -> Result<(std::path::PathBuf, String, u64, usize, Vec<Value>), oc_core::Error> {
            let cancel = CancelToken::new();
            let iso = app2.isolation();
            let dest = work.join("tree");
            std::fs::create_dir_all(&dest)?;
            let opts = oc_archive::safety::ExtractOptions {
                only,
                ..Default::default()
            };
            let report = extract_isolated(&iso, &path, &dest, &opts, password.as_deref(), &cancel);
            let result = report.and_then(|report| {
                let skipped: Vec<Value> = report
                    .skipped
                    .iter()
                    .map(|(p, why)| json!({"path": p, "reason": why}))
                    .collect();
                if report.files == 0 && report.dirs == 0 && report.symlinks == 0 {
                    return Err(oc_core::Error::new(
                        ErrorCode::InvalidOptions,
                        "Nothing matched the selection.",
                    ));
                }
                let tops: Vec<std::path::PathBuf> = std::fs::read_dir(&dest)?
                    .flatten()
                    .map(|e| e.path())
                    .collect();
                if tops.len() == 1 && report.files == 1 && tops[0].is_file() {
                    let name = tops[0]
                        .file_name()
                        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
                    let out = outputs.join(format!("{out_id2}.bin"));
                    std::fs::rename(&tops[0], &out)?;
                    let size = std::fs::metadata(&out)?.len();
                    return Ok((out, name, size, 1, skipped));
                }
                let out = outputs.join(format!("{out_id2}.zip"));
                let rep = create_isolated(&app2.engine, &iso, &tops, &out, true, &[], &cancel)?;
                Ok((
                    out,
                    format!("{stem}-files.zip"),
                    rep.bytes_out,
                    report.files as usize,
                    skipped,
                ))
            });
            let _ = std::fs::remove_dir_all(&work);
            result
        },
    )
    .await;
    drop(permit);
    let (out, name, size, files, skipped) = done
        .map_err(|_| {
            ApiErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "The extraction stopped unexpectedly.",
            )
        })?
        .map_err(engine_err)?;
    let name = safe_name(&name);
    let fmt: &'static str = if name.ends_with(".zip") && out.extension().is_some_and(|e| e == "zip")
    {
        app.engine.format_by_name("zip").map_or("", |f| f.0)
    } else {
        oc_engine::archive::FILE.0
    };
    lock(&app.outputs).insert(
        out_id.clone(),
        OutRec {
            path: out,
            name: name.clone(),
            format: fmt,
            created: now(),
        },
    );
    Ok(Json(json!({
        "output_id": out_id, "output_name": name, "size": size,
        "files": files, "skipped": skipped,
    })))
}

/// `POST /api/archives`: pack several uploads into one archive. A job like any other, so
/// progress, cancelling, history and download work the same way.
pub async fn create_archive(
    State(app): State<Arc<App>>,
    Json(body): Json<Value>,
) -> R<(StatusCode, Json<Value>)> {
    let Value::Object(body) = body else {
        return Err(ApiErr::bad("The request must be a JSON object."));
    };
    for k in body.keys() {
        if !matches!(
            k.as_str(),
            "file_ids" | "target_format" | "options" | "name"
        ) {
            return Err(ApiErr::bad(format!("Unknown field '{k}'.")));
        }
    }
    let target = body
        .get("target_format")
        .and_then(Value::as_str)
        .unwrap_or("");
    let to = app
        .engine
        .format_by_name(target)
        .filter(|f| oc_engine::archive::is_archive(*f) && *f != oc_engine::archive::FILE)
        .ok_or_else(|| ApiErr::bad("Choose an archive format."))?;
    let ids = body
        .get("file_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiErr::bad("file_ids must be a list."))?;
    if ids.is_empty() || ids.len() > 5000 {
        return Err(ApiErr::bad("Choose between 1 and 5000 files."));
    }
    let mut picked: Vec<(std::path::PathBuf, String, u64)> = Vec::new();
    {
        let files = lock(&app.files);
        for v in ids {
            let id = v
                .as_str()
                .filter(|i| valid_id(i))
                .ok_or_else(ApiErr::not_found)?;
            let f = files.get(id).ok_or_else(ApiErr::not_found)?;
            picked.push((
                f.path.clone(),
                safe_name(f.meta["original_name"].as_str().unwrap_or("file")),
                f.meta["size"].as_u64().unwrap_or(0),
            ));
        }
    }
    let ext = format_ext(&app, to);
    let out_id = new_id();
    let out_path = app
        .cfg
        .data_dir
        .join("outputs")
        .join(format!("{out_id}.{ext}"));
    let options = job_options(&app, to, &out_path, body.get("options"))?;

    // Stage copies under their original names; two uploads called a.txt become a.txt and a (2).txt.
    let stage = app
        .cfg
        .data_dir
        .join("previews")
        .join(format!("p-{out_id}"));
    let stage2 = stage.clone();
    let staged =
        tokio::task::spawn_blocking(move || -> std::io::Result<Vec<std::path::PathBuf>> {
            std::fs::create_dir_all(&stage2)?;
            let mut used = std::collections::HashSet::new();
            let mut out = Vec::new();
            for (src, name, _) in picked {
                let mut cand = name.clone();
                let mut n = 2;
                while !used.insert(cand.to_lowercase()) {
                    cand = match name.rsplit_once('.') {
                        Some((s, e)) if !s.is_empty() => format!("{s} ({n}).{e}"),
                        _ => format!("{name} ({n})"),
                    };
                    n += 1;
                }
                let dst = stage2.join(&cand);
                std::fs::copy(&src, &dst)?;
                out.push(dst);
            }
            Ok(out)
        })
        .await;
    let sources = match staged {
        Ok(Ok(s)) => s,
        _ => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(ApiErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "io_error",
                "The files could not be prepared.",
            ));
        }
    };
    let total: u64 = ids
        .iter()
        .filter_map(|v| v.as_str())
        .filter_map(|i| {
            lock(&app.files)
                .get(i)
                .map(|f| f.meta["size"].as_u64().unwrap_or(0))
        })
        .sum();
    let label = body
        .get("name")
        .and_then(Value::as_str)
        .map(safe_name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            if sources.len() == 1 {
                sources[0]
                    .file_name()
                    .map_or_else(|| "archive".into(), |n| n.to_string_lossy().into_owned())
            } else {
                "archive".into()
            }
        });
    let meta = json!({
        "file_id": null, "detected_format": "file", "original_name": label,
        "size": total,
    });
    let job_id = new_id();
    let cancel = CancelToken::new();
    let (tx, _rx) = tokio::sync::watch::channel(job_snapshot(
        &job_id, &meta, target, "queued", 0.0, "queued",
    ));
    lock(&app.jobs).insert(
        job_id.clone(),
        JobRec {
            cancel: cancel.clone(),
            tx,
        },
    );
    let app2 = app.clone();
    let jid = job_id.clone();
    let target_s = target.to_string();
    tokio::spawn(async move {
        let first = out_path.clone();
        run_job(
            app2,
            jid,
            meta,
            target_s,
            first,
            out_path,
            out_id,
            options,
            Some((stage, sources)),
            cancel,
        )
        .await;
    });
    Ok((StatusCode::CREATED, Json(json!({"job_id": job_id}))))
}

#[derive(serde::Deserialize)]
pub struct TableQuery {
    sheet: Option<String>,
    rows: Option<usize>,
}

/// `GET /api/files/{id}/table?sheet=&rows=`: the first rows of a table or workbook, as JSON.
pub async fn table_preview(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(q): Query<TableQuery>,
) -> R<Response> {
    if !valid_id(&id) {
        return Err(ApiErr::not_found());
    }
    let (path, format) = {
        let files = lock(&app.files);
        let f = files.get(&id).ok_or_else(ApiErr::not_found)?;
        (f.path.clone(), f.format)
    };
    let args = vec![
        "__worker".to_string(),
        "table".to_string(),
        path.display().to_string(),
        format.to_string(),
        q.sheet.unwrap_or_default(),
        q.rows.unwrap_or(100).clamp(1, 1000).to_string(),
    ];
    let app2 = app.clone();
    let permit = app.sem.acquire().await;
    let res = tokio::task::spawn_blocking(move || {
        let mut iso = app2.isolation();
        iso.timeout = Duration::from_secs(60);
        run_worker(&iso, &args, &CancelToken::new())
    })
    .await;
    drop(permit);
    let out = res
        .map_err(|_| {
            ApiErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "The preview stopped unexpectedly.",
            )
        })?
        .map_err(engine_err)?;
    Ok(([(header::CONTENT_TYPE, "application/json")], out).into_response())
}

#[derive(serde::Deserialize)]
pub struct AudioQuery {
    buckets: Option<usize>,
}

/// Which stored file or output an audio request names.
fn audio_target(app: &App, id: &str, output: bool) -> R<(std::path::PathBuf, &'static str)> {
    if !valid_id(id) {
        return Err(ApiErr::not_found());
    }
    let rec = if output {
        lock(&app.outputs)
            .get(id)
            .map(|o| (o.path.clone(), o.format))
    } else {
        lock(&app.files).get(id).map(|f| (f.path.clone(), f.format))
    };
    let (path, format) = rec.ok_or_else(ApiErr::not_found)?;
    let is_audio = app
        .engine
        .format_by_name(format)
        .and_then(|f| app.engine.registry().format(f))
        .is_some_and(|d| d.category == Category::Audio);
    if !is_audio {
        return Err(ApiErr::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_format",
            "Only audio files have a waveform.",
        ));
    }
    Ok((path, format))
}

async fn audio_worker(app: &Arc<App>, args: Vec<String>) -> R<String> {
    let app2 = app.clone();
    let permit = app.sem.acquire().await;
    let res = tokio::task::spawn_blocking(move || {
        let mut iso = app2.isolation();
        iso.timeout = Duration::from_secs(120);
        run_worker(&iso, &args, &CancelToken::new())
    })
    .await;
    drop(permit);
    res.map_err(|_| {
        ApiErr::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "The preview stopped unexpectedly.",
        )
    })?
    .map_err(engine_err)
}

async fn audio_json(app: &Arc<App>, id: &str, q: AudioQuery, output: bool) -> R<Response> {
    let (path, format) = audio_target(app, id, output)?;
    let out = audio_worker(
        app,
        vec![
            "__worker".to_string(),
            "audio".to_string(),
            path.display().to_string(),
            format.to_string(),
            q.buckets.unwrap_or(800).clamp(16, 4000).to_string(),
        ],
    )
    .await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], out).into_response())
}

async fn audio_wav(app: &Arc<App>, id: &str, output: bool) -> R<Response> {
    let (path, format) = audio_target(app, id, output)?;
    let key = format!("{}{id}", if output { "o" } else { "f" });
    let cache = app.cfg.data_dir.join("previews").join(format!("{key}.wav"));
    if tokio::fs::metadata(&cache).await.is_err() {
        if let Some(dir) = cache.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        audio_worker(
            app,
            vec![
                "__worker".to_string(),
                "wavprev".to_string(),
                path.display().to_string(),
                format.to_string(),
                cache.display().to_string(),
                "90".to_string(),
            ],
        )
        .await?;
    }
    let bytes = tokio::fs::read(&cache).await.map_err(|_| {
        ApiErr::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "The preview could not be stored.",
        )
    })?;
    Ok((
        [
            (header::CONTENT_TYPE, "audio/wav"),
            (header::CACHE_CONTROL, "private, max-age=3600"),
        ],
        bytes,
    )
        .into_response())
}

/// `GET /api/files/{id}/audio?buckets=`: waveform peaks, loudness and tags as JSON.
pub async fn file_audio(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(q): Query<AudioQuery>,
) -> R<Response> {
    audio_json(&app, &id, q, false).await
}

pub async fn output_audio(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(q): Query<AudioQuery>,
) -> R<Response> {
    audio_json(&app, &id, q, true).await
}

/// `GET /api/files/{id}/audio.wav`: the first 90 seconds as 16-bit WAV, playable anywhere.
pub async fn file_audio_wav(State(app): State<Arc<App>>, Path(id): Path<String>) -> R<Response> {
    audio_wav(&app, &id, false).await
}

pub async fn output_audio_wav(State(app): State<Arc<App>>, Path(id): Path<String>) -> R<Response> {
    audio_wav(&app, &id, true).await
}
