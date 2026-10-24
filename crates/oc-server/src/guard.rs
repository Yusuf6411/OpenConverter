//! Request checks that apply to everything: Host allow-list (DNS rebinding), CSRF,
//! the access token for remote use, and security headers. Also the static UI files.
use crate::state::App;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

const CSP: &str = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self'; script-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";
const COOKIE: &str = "oc_token";

pub fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::json!({"error": {"code": code, "message": message}});
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn cookie_token(h: &HeaderMap) -> Option<&str> {
    header_str(h, "cookie")?
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v)
}

/// Compares without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

pub async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let host = header_str(req.headers(), "host")
        .unwrap_or("")
        .to_ascii_lowercase();
    if !app.host_allowed(&host) {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Unexpected Host header.",
        );
    }
    let method = req.method().clone();
    if !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
        let origin_ok = header_str(req.headers(), "origin")
            .or_else(|| header_str(req.headers(), "referer"))
            .is_some_and(|o| {
                let o = o.to_ascii_lowercase();
                let want = format!("http://{host}");
                o == want || o.starts_with(&format!("{want}/"))
            });
        if !origin_ok || header_str(req.headers(), "x-openconverter") != Some("1") {
            return error_response(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Cross-site request refused.",
            );
        }
    }
    let mut set_cookie = None;
    if let Some(token) = &app.token {
        let have = cookie_token(req.headers()).is_some_and(|c| same(c, token));
        if !have {
            // A one-time login: ?token=... on the front page sets the cookie.
            let query_ok = req.uri().path() == "/"
                && req
                    .uri()
                    .query()
                    .and_then(|q| q.split('&').find_map(|p| p.strip_prefix("token=")))
                    .is_some_and(|t| same(t, token));
            if query_ok {
                set_cookie = Some(format!(
                    "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/"
                ));
            } else {
                return error_response(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "An access token is required. Open the address printed when the server started.",
                );
            }
        }
    }
    let is_api = req.uri().path().starts_with("/api/");
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    if is_api && !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if let Some(c) = set_cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
        h.insert(header::SET_COOKIE, c);
    }
    resp
}

/// The embedded UI. Only files in the embedded table can be served, so no path can
/// escape it.
pub async fn static_file(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match crate::assets::ASSETS.iter().find(|(p, _, _)| *p == path) {
        Some((_, mime, bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, *mime)
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(*bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        None => error_response(StatusCode::NOT_FOUND, "not_found", "Not found."),
    }
}
