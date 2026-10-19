//! A small streaming `multipart/form-data` reader (RFC 7578), so uploads go to disk
//! chunk by chunk and no charset-conversion crate is needed.
use axum::body::{Body, BodyDataStream};
use tokio_stream::StreamExt;

const MAX_HEADER: usize = 8192;

pub struct Part {
    pub name: String,
    pub filename: Option<String>,
}

#[derive(Debug)]
pub struct MpError(pub &'static str);

enum State {
    Start,
    AfterDelimiter,
    Body,
    Done,
}

pub struct Multipart {
    stream: BodyDataStream,
    buf: Vec<u8>,
    delim: Vec<u8>,
    state: State,
    ended: bool,
}

/// The boundary parameter of a `Content-Type` value.
pub fn boundary(content_type: &str) -> Option<String> {
    let (kind, params) = content_type.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    params
        .split(';')
        .find_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case("boundary")
                .then(|| v.trim().trim_matches('"').to_string())
        })
        .filter(|b| !b.is_empty() && b.len() <= 70)
}

impl Multipart {
    pub fn new(body: Body, boundary: &str) -> Self {
        let mut delim = b"\r\n--".to_vec();
        delim.extend_from_slice(boundary.as_bytes());
        Self {
            stream: body.into_data_stream(),
            // A virtual line break lets the first boundary match like every other.
            buf: b"\r\n".to_vec(),
            delim,
            state: State::Start,
            ended: false,
        }
    }

    async fn fill(&mut self) -> Result<(), MpError> {
        if self.ended {
            return Err(MpError("the upload ended early"));
        }
        match self.stream.next().await {
            Some(Ok(b)) => {
                self.buf.extend_from_slice(&b);
                Ok(())
            }
            Some(Err(_)) => Err(MpError("the upload was interrupted")),
            None => {
                self.ended = true;
                Err(MpError("the upload ended early"))
            }
        }
    }

    fn find_delim(&self) -> Option<usize> {
        self.buf
            .windows(self.delim.len())
            .position(|w| w == self.delim.as_slice())
    }

    /// Moves to the next part, skipping whatever is left of the current one.
    pub async fn next_part(&mut self) -> Result<Option<Part>, MpError> {
        loop {
            match self.state {
                State::Done => return Ok(None),
                State::Start => match self.find_delim() {
                    Some(i) => {
                        self.buf.drain(..i + self.delim.len());
                        self.state = State::AfterDelimiter;
                    }
                    None => {
                        // Keep a tail that could be the start of the delimiter.
                        let keep = self.delim.len() - 1;
                        if self.buf.len() > keep {
                            let cut = self.buf.len() - keep;
                            self.buf.drain(..cut);
                        }
                        self.fill().await?;
                    }
                },
                State::AfterDelimiter => {
                    if self.buf.len() < 2 {
                        self.fill().await?;
                        continue;
                    }
                    if self.buf.starts_with(b"--") {
                        self.state = State::Done;
                        return Ok(None);
                    }
                    if !self.buf.starts_with(b"\r\n") {
                        return Err(MpError("malformed multipart boundary line"));
                    }
                    self.buf.drain(..2);
                    // Headers.
                    loop {
                        if let Some(end) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&self.buf[..end]).to_string();
                            self.buf.drain(..end + 4);
                            self.state = State::Body;
                            return Ok(Some(parse_headers(&head)));
                        }
                        if self.buf.len() > MAX_HEADER {
                            return Err(MpError("multipart headers are too long"));
                        }
                        self.fill().await?;
                    }
                }
                State::Body => while self.chunk().await?.is_some() {},
            }
        }
    }

    /// The next piece of the current part's data, or `None` at its end.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, MpError> {
        if !matches!(self.state, State::Body) {
            return Ok(None);
        }
        loop {
            if let Some(i) = self.find_delim() {
                if i > 0 {
                    return Ok(Some(self.buf.drain(..i).collect()));
                }
                self.buf.drain(..self.delim.len());
                self.state = State::AfterDelimiter;
                return Ok(None);
            }
            let keep = self.delim.len() - 1;
            if self.buf.len() > keep {
                let cut = self.buf.len() - keep;
                return Ok(Some(self.buf.drain(..cut).collect()));
            }
            self.fill().await?;
        }
    }
}

fn parse_headers(head: &str) -> Part {
    let mut part = Part {
        name: String::new(),
        filename: None,
    };
    for line in head.split("\r\n") {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if !k.trim().eq_ignore_ascii_case("content-disposition") {
            continue;
        }
        for p in v.split(';') {
            let Some((pk, pv)) = p.trim().split_once('=') else {
                continue;
            };
            let value = pv.trim().trim_matches('"').replace("\\\"", "\"");
            match pk.trim().to_ascii_lowercase().as_str() {
                "name" => part.name = value,
                "filename" => part.filename = Some(value),
                _ => {}
            }
        }
    }
    part
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    async fn parts(body: &[u8], b: &str) -> Vec<(String, Option<String>, Vec<u8>)> {
        let mut mp = Multipart::new(Body::from(body.to_vec()), b);
        let mut out = Vec::new();
        while let Some(p) = mp.next_part().await.unwrap() {
            let mut data = Vec::new();
            while let Some(c) = mp.chunk().await.unwrap() {
                data.extend(c);
            }
            out.push((p.name, p.filename, data));
        }
        out
    }

    #[tokio::test]
    async fn reads_parts_with_binary_data() {
        let body = b"preamble\r\n--XX\r\nContent-Disposition: form-data; name=\"files\"; filename=\"a b.bin\"\r\n\r\n\x00\r\n--XY\r\n--\x01\r\n--XX\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\n\r\n--XX--\r\nepilogue";
        let got = parts(body, "XX").await;
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "files");
        assert_eq!(got[0].1.as_deref(), Some("a b.bin"));
        assert_eq!(got[0].2, b"\x00\r\n--XY\r\n--\x01");
        assert_eq!(got[1].2, b"");
    }

    #[tokio::test]
    async fn truncated_bodies_are_errors() {
        let mut mp = Multipart::new(
            Body::from(&b"--XX\r\nContent-Disposition: form-data; name=\"f\"\r\n\r\nabc"[..]),
            "XX",
        );
        assert!(mp.next_part().await.unwrap().is_some());
        let mut err = false;
        for _ in 0..4 {
            if mp.chunk().await.is_err() {
                err = true;
                break;
            }
        }
        assert!(err);
    }

    #[test]
    fn boundary_is_found() {
        assert_eq!(
            boundary("multipart/form-data; boundary=\"abc\"").as_deref(),
            Some("abc")
        );
        assert!(boundary("application/json").is_none());
    }
}
