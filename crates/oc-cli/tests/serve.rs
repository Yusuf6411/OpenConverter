//! `openconverter serve` end to end: the real binary on an ephemeral port, driven over raw HTTP.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

struct Server {
    child: Child,
    port: u16,
    dir: std::path::PathBuf,
}

impl Server {
    fn start(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("oc-serve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut child = Command::new(env!("CARGO_BIN_EXE_openconverter"))
            .args(["serve", "--no-browser", "--port", "0", "--data-dir"])
            .arg(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line
            .trim()
            .rsplit(':')
            .next()
            .unwrap()
            .trim_end_matches('/')
            .parse()
            .unwrap_or_else(|_| panic!("no port in {line:?}"));
        Server { child, port, dir }
    }

    /// Sends one request and returns (status, headers, body).
    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, String, Vec<u8>) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req += &format!("Host: 127.0.0.1:{}\r\n", self.port);
        }
        for (k, v) in headers {
            req += &format!("{k}: {v}\r\n");
        }
        req += &format!("Content-Length: {}\r\n\r\n", body.len());
        s.write_all(req.as_bytes()).unwrap();
        s.write_all(body).unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut body = raw[split + 4..].to_vec();
        if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            body = dechunk(&body);
        }
        (status, head, body)
    }

    fn post_headers(&self) -> Vec<(String, String)> {
        vec![
            ("Origin".into(), format!("http://127.0.0.1:{}", self.port)),
            ("X-OpenConverter".into(), "1".into()),
        ]
    }

    fn json(&self, method: &str, path: &str, body: &str) -> (u16, serde_like::Json) {
        let h = self.post_headers();
        let mut hs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        hs.push(("Content-Type", "application/json"));
        let (st, _, b) = self.request(method, path, &hs, body.as_bytes());
        (
            st,
            serde_like::Json(String::from_utf8_lossy(&b).to_string()),
        )
    }

    fn upload(&self, name: &str, data: &[u8]) -> (u16, serde_like::Json) {
        let boundary = "ocboundary7MA4YWxk";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let h = self.post_headers();
        let ct = format!("multipart/form-data; boundary={boundary}");
        let mut hs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        hs.push(("Content-Type", &ct));
        let (st, _, b) = self.request("POST", "/api/files", &hs, &body);
        (
            st,
            serde_like::Json(String::from_utf8_lossy(&b).to_string()),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(nl) = b.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };
        let n =
            usize::from_str_radix(std::str::from_utf8(&b[..nl]).unwrap().trim(), 16).unwrap_or(0);
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&b[nl + 2..nl + 2 + n]);
        b = &b[nl + 2 + n + 2..];
    }
}

/// Just enough JSON lookup for these tests: `"key":"value"` and `"key":number` by text search.
mod serde_like {
    pub struct Json(pub String);
    impl Json {
        pub fn str(&self, key: &str) -> String {
            let pat = format!("\"{key}\":\"");
            let at = self
                .0
                .find(&pat)
                .unwrap_or_else(|| panic!("no {key} in {}", self.0))
                + pat.len();
            self.0[at..].split('"').next().unwrap().to_string()
        }
        pub fn has(&self, s: &str) -> bool {
            self.0.contains(s)
        }
    }
}

fn png_1x1() -> Vec<u8> {
    // Written by the engine rather than hard-coded: a BMP is trivial to build by hand.
    let mut b = b"BM".to_vec();
    b.extend_from_slice(&58u32.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&1i32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0; 24]);
    b.extend_from_slice(&[10, 200, 30, 0]);
    b
}

#[test]
fn convert_through_the_api() {
    let s = Server::start("flow");
    let (st, head, body) = s.request("GET", "/api/health", &[], b"");
    assert_eq!(st, 200);
    assert!(head
        .to_ascii_lowercase()
        .contains("content-security-policy"));
    assert!(String::from_utf8_lossy(&body).contains("\"ok\""));

    let (st, up) = s.upload("pixel.bmp", &png_1x1());
    assert_eq!(st, 200, "{}", up.0);
    assert_eq!(up.str("detected_format"), "bmp");
    let file_id = up.str("file_id");

    let (st, job) = s.json(
        "POST",
        "/api/jobs",
        &format!("{{\"file_id\":\"{file_id}\",\"target_format\":\"png\"}}"),
    );
    assert_eq!(st, 201, "{}", job.0);
    let job_id = job.str("job_id");
    let mut done = None;
    for _ in 0..100 {
        let (_, _, b) = s.request("GET", &format!("/api/jobs/{job_id}"), &[], b"");
        let text = String::from_utf8_lossy(&b).to_string();
        if text.contains("\"state\":\"done\"") {
            done = Some(text);
            break;
        }
        assert!(!text.contains("\"state\":\"failed\""), "{text}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let done = serde_like::Json(done.unwrap());
    let out = done.str("output_id");
    let (st, head, png) = s.request("GET", &format!("/api/outputs/{out}/download"), &[], b"");
    assert_eq!(st, 200);
    assert!(head.contains("filename=\"pixel.png\""), "{head}");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

    let (st, _, hist) = s.request("GET", "/api/history", &[], b"");
    assert_eq!(st, 200);
    assert!(String::from_utf8_lossy(&hist).contains("pixel.bmp"));
}

#[test]
fn requests_are_checked() {
    let s = Server::start("guard");
    // Wrong Host (DNS rebinding).
    let (st, _, _) = s.request("GET", "/api/health", &[("Host", "evil.example")], b"");
    assert_eq!(st, 403);
    // A POST without the CSRF header or with a foreign Origin.
    let (st, _, _) = s.request(
        "POST",
        "/api/jobs",
        &[("Content-Type", "application/json")],
        b"{}",
    );
    assert_eq!(st, 403);
    let (st, _, _) = s.request(
        "POST",
        "/api/jobs",
        &[
            ("Origin", "http://evil.example"),
            ("X-OpenConverter", "1"),
            ("Content-Type", "application/json"),
        ],
        b"{}",
    );
    assert_eq!(st, 403);
    // Unknown ids, bad ids, unknown routes and paths outside the UI.
    assert_eq!(
        s.request(
            "GET",
            "/api/files/00000000000000000000000000000000",
            &[],
            b""
        )
        .0,
        404
    );
    assert_eq!(
        s.request("GET", "/api/files/..%2f..%2fetc", &[], b"").0,
        404
    );
    assert_eq!(s.request("GET", "/../Cargo.toml", &[], b"").0, 404);
    assert_eq!(s.request("GET", "/nope", &[], b"").0, 404);
    // The UI itself is served.
    let (st, head, body) = s.request("GET", "/", &[], b"");
    assert_eq!(st, 200);
    assert!(head.contains("text/html"));
    assert!(String::from_utf8_lossy(&body).contains("OpenConverter"));
    // Unsupported content is refused per file, with a typed error.
    let (st, up) = s.upload("broken.png", b"this is not a picture");
    assert_eq!(st, 200);
    assert!(up.has("unsupported_format"), "{}", up.0);
    // A file of an unknown type is accepted as a plain file that can go into an archive.
    let (st, up) = s.upload("notes.xyz", b"hello");
    assert_eq!(st, 200);
    assert!(!up.has("unsupported_format"), "{}", up.0);
    assert!(up.has("zip"), "{}", up.0);
    // Bad option values fail the request, not the job.
    let (_, ok) = s.upload("p.bmp", &png_1x1());
    let (st, _) = s.json(
        "POST",
        "/api/jobs",
        &format!(
            "{{\"file_id\":\"{}\",\"target_format\":\"jpeg\",\"options\":{{\"quality\":500}}}}",
            ok.str("file_id")
        ),
    );
    assert_eq!(st, 400);
}

#[test]
fn listening_beyond_this_computer_needs_consent_and_a_token() {
    let out = Command::new(env!("CARGO_BIN_EXE_openconverter"))
        .args(["serve", "--no-browser", "--host", "0.0.0.0", "--port", "0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--allow-remote"));
}

fn wait_job(s: &Server, job_id: &str) -> serde_like::Json {
    for _ in 0..200 {
        let (_, _, b) = s.request("GET", &format!("/api/jobs/{job_id}"), &[], b"");
        let text = String::from_utf8_lossy(&b).to_string();
        if text.contains("\"state\":\"done\"") || text.contains("\"state\":\"failed\"") {
            return serde_like::Json(text);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("job did not finish");
}

#[test]
fn archives_through_the_api() {
    let s = Server::start("archives");
    let (_, a) = s.upload("a.txt", b"alpha");
    let (_, b) = s.upload("a.txt", b"beta beta");
    let (_, c) = s.upload("notes.xyz", b"gamma");
    let ids = [a.str("file_id"), b.str("file_id"), c.str("file_id")];
    let body = format!(
        "{{\"file_ids\":[\"{}\",\"{}\",\"{}\"],\"target_format\":\"zip\"}}",
        ids[0], ids[1], ids[2]
    );
    let (st, job) = s.json("POST", "/api/archives", &body);
    assert_eq!(st, 201, "{}", job.0);
    let done = wait_job(&s, &job.str("job_id"));
    assert!(done.has("\"state\":\"done\""), "{}", done.0);
    let out = done.str("output_id");
    let (st, _, zip) = s.request("GET", &format!("/api/outputs/{out}/download"), &[], b"");
    assert_eq!(st, 200);
    assert_eq!(&zip[..2], b"PK");

    // Upload the result and look inside.
    let (st, up) = s.upload("bundle.zip", &zip);
    assert_eq!(st, 200, "{}", up.0);
    let zid = up.str("file_id");
    let (st, list) = s.json("POST", &format!("/api/files/{zid}/entries"), "{}");
    assert_eq!(st, 200, "{}", list.0);
    assert!(list.has("\"path\":\"a.txt\""), "{}", list.0);
    assert!(list.has("\"path\":\"a (2).txt\""), "{}", list.0);
    assert!(list.has("\"path\":\"notes.xyz\""), "{}", list.0);

    // One entry comes back as itself, several as a ZIP.
    let (st, one) = s.json("POST", &format!("/api/files/{zid}/extract"), "{\"paths\":[\"notes.xyz\"]}");
    assert_eq!(st, 200, "{}", one.0);
    assert_eq!(one.str("output_name"), "notes.xyz");
    let (_, _, data) = s.request("GET", &format!("/api/outputs/{}/download", one.str("output_id")), &[], b"");
    assert_eq!(data, b"gamma");
    let (st, all) = s.json("POST", &format!("/api/files/{zid}/extract"), "{}");
    assert_eq!(st, 200, "{}", all.0);
    assert_eq!(all.str("output_name"), "bundle-files.zip");
    let (st, none) = s.json("POST", &format!("/api/files/{zid}/extract"), "{\"paths\":[\"missing\"]}");
    assert_eq!(st, 400, "{}", none.0);

    // Not an archive; and a password-protected ZIP asks for its password.
    let (_, bmp) = s.upload("p.bmp", &png_1x1());
    let (st, _) = s.json("POST", &format!("/api/files/{}/entries", bmp.str("file_id")), "{}");
    assert_eq!(st, 415);
    let body = format!(
        "{{\"file_ids\":[\"{}\"],\"target_format\":\"zip\",\"options\":{{\"password\":\"hunter2\"}}}}",
        ids[0]
    );
    let (st, job) = s.json("POST", "/api/archives", &body);
    assert_eq!(st, 201, "{}", job.0);
    let done = wait_job(&s, &job.str("job_id"));
    let (_, _, enc) = s.request("GET", &format!("/api/outputs/{}/download", done.str("output_id")), &[], b"");
    let (_, up) = s.upload("secret.zip", &enc);
    let eid = up.str("file_id");
    let (st, r) = s.json("POST", &format!("/api/files/{eid}/extract"), "{}");
    assert_eq!(st, 401, "{}", r.0);
    assert!(r.has("encrypted_need_password"), "{}", r.0);
    let (st, _) = s.json("POST", &format!("/api/files/{eid}/extract"), "{\"password\":\"nope\"}");
    assert_eq!(st, 401);
    let (st, _) = s.json("POST", &format!("/api/files/{eid}/extract"), "{\"password\":\"hunter2\"}");
    assert_eq!(st, 200);
}
