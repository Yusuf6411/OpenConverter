//! Engine: assembles the format registry and runs conversions.
//!
//! Currently covers text files (encoding and line-ending conversion). Further crates
//! register their formats here as they are written. Process isolation of decoders
//! (`oc-worker`) is not wired in yet.
#![deny(unsafe_code)]

use oc_core::options::{OptionKind, OptionSchema, OptionSpec, OptionValue, Options};
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Limits, Note, Notes,
    Ownership, Progress, Result, Severity,
};
use oc_text::{Encoding, LineEnding, Policy};
use std::path::{Path, PathBuf};

pub const TXT: FormatId = FormatId("txt");

pub struct Engine {
    registry: Registry,
    pub limits: Limits,
}

#[derive(Debug, Clone)]
pub struct ConvertRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub overwrite: bool,
    pub options: Vec<(String, OptionValue)>,
}

#[derive(Debug)]
pub struct ConvertReport {
    pub input_format: FormatId,
    pub output_format: FormatId,
    pub route: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub notes: Notes,
}

fn text_options() -> OptionSchema {
    OptionSchema {
        specs: vec![
            OptionSpec {
                key: "from_encoding",
                label: "Input encoding",
                help: "auto, utf-8, utf-16le, utf-16be, utf-32le, utf-32be, ascii, iso-8859-1, windows-1252",
                i18n_key: "opt.text.from_encoding",
                kind: OptionKind::Text { default: "auto".into() },
            },
            OptionSpec {
                key: "to_encoding",
                label: "Output encoding",
                help: "same names as the input encoding, except auto",
                i18n_key: "opt.text.to_encoding",
                kind: OptionKind::Text { default: "utf-8".into() },
            },
            OptionSpec {
                key: "line_ending",
                label: "Line endings",
                help: "keep, lf, crlf or cr",
                i18n_key: "opt.text.line_ending",
                kind: OptionKind::Enum { choices: vec!["keep", "lf", "crlf", "cr"], default: "keep" },
            },
            OptionSpec {
                key: "bom",
                label: "Write byte-order mark",
                help: "Only meaningful for Unicode encodings",
                i18n_key: "opt.text.bom",
                kind: OptionKind::Bool { default: false },
            },
            OptionSpec {
                key: "lossy",
                label: "Replace unencodable characters",
                help: "Otherwise a character the output encoding cannot hold is an error",
                i18n_key: "opt.text.lossy",
                kind: OptionKind::Bool { default: false },
            },
        ],
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        let mut registry = Registry::new();
        registry.add_format(FormatDescriptor {
            id: TXT,
            name: "Plain text",
            category: Category::Text,
            extensions: &["txt", "text", "log", "md"],
            mime: &["text/plain"],
            sniffers: Vec::new(),
            capabilities: Capabilities {
                read: true,
                write: true,
                ..Capabilities::default()
            },
            writer_options: text_options(),
            ownership: Ownership::Own,
            spec: "Unicode 15 / WHATWG Encoding (subset)",
        });
        registry.add_edge(Edge {
            from: TXT,
            to: TXT,
            kind: EdgeKind::Transcode,
            loss: 0,
            ownership: Ownership::Own,
            time: 1,
            description: "Decode text -> convert line endings -> encode text",
        });
        Self {
            registry,
            limits: Limits::default(),
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Identify a format from a path's extension.
    pub fn format_for_path(&self, path: &Path) -> Option<FormatId> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        self.registry_formats()
            .find(|f| f.extensions.contains(&ext.as_str()))
            .map(|f| f.id)
    }

    fn registry_formats(&self) -> impl Iterator<Item = &FormatDescriptor> {
        [TXT].into_iter().filter_map(|id| self.registry.format(id))
    }

    pub fn formats(&self) -> Vec<&FormatDescriptor> {
        self.registry_formats().collect()
    }

    pub fn convert(&self, req: &ConvertRequest, progress: &dyn Progress) -> Result<ConvertReport> {
        let from = self.format_for_path(&req.input).ok_or_else(|| {
            Error::new(ErrorCode::UnsupportedFormat, "unknown input file type")
                .with_detail(req.input.display().to_string())
        })?;
        let to = self.format_for_path(&req.output).ok_or_else(|| {
            Error::new(ErrorCode::UnsupportedFormat, "unknown output file type")
                .with_detail(req.output.display().to_string())
        })?;
        let route = self.registry.plan(from, to, false)?;
        if req.output.exists() && !req.overwrite {
            return Err(
                Error::new(ErrorCode::OutputExists, "output file already exists")
                    .with_detail(req.output.display().to_string()),
            );
        }
        let schema = &self
            .registry
            .format(to)
            .ok_or_else(|| Error::new(ErrorCode::InternalError, "format vanished"))?
            .writer_options;
        let opts = schema.validate(&req.options)?;
        progress.check_cancelled()?;

        let meta = std::fs::metadata(&req.input)?;
        self.limits.check_alloc(meta.len())?;
        let data = std::fs::read(&req.input)?;
        progress.report(1, 3, "read");
        progress.check_cancelled()?;

        let (out, notes) = convert_text(&data, &opts)?;
        progress.report(2, 3, "convert");
        progress.check_cancelled()?;

        let tmp = req.output.with_extension("oc-partial");
        std::fs::write(&tmp, &out)?;
        if let Err(e) = std::fs::rename(&tmp, &req.output) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        progress.report(3, 3, "write");
        Ok(ConvertReport {
            input_format: from,
            output_format: to,
            // Same-format conversions plan to an empty route; describe the actual work.
            route: if route.edges.is_empty() {
                "Decode text -> convert line endings -> encode text".to_string()
            } else {
                route.explain()
            },
            bytes_in: data.len() as u64,
            bytes_out: out.len() as u64,
            notes,
        })
    }
}

fn encoding_option(name: &str, key: &str) -> Result<Encoding> {
    Encoding::from_name(name)
        .ok_or_else(|| Error::invalid_option(key, format!("unknown encoding '{name}'")))
}

/// Convert text bytes according to validated options.
pub fn convert_text(data: &[u8], opts: &Options) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let from_name = opts.text("from_encoding").unwrap_or("auto");
    let (enc_in, skip) = if from_name == "auto" {
        let d = oc_text::detect(data);
        if !d.certain && d.encoding == Encoding::Windows1252 {
            notes.push(Note::new(
                Severity::Warning,
                "encoding_guessed",
                "input is not valid UTF-8; assumed windows-1252",
            ));
        }
        (d.encoding, d.bom_len)
    } else {
        let e = encoding_option(from_name, "from_encoding")?;
        let bom = oc_text::bom(e);
        (
            e,
            if !bom.is_empty() && data.starts_with(bom) {
                bom.len()
            } else {
                0
            },
        )
    };
    let to = encoding_option(opts.text("to_encoding").unwrap_or("utf-8"), "to_encoding")?;
    let policy = if opts.bool("lossy") == Some(true) {
        Policy::Replace
    } else {
        Policy::Strict
    };

    let (mut text, n) = oc_text::decode(data.get(skip..).unwrap_or(&[]), enc_in, policy)?;
    notes.extend(n);
    match opts.text("line_ending").unwrap_or("keep") {
        "lf" => text = oc_text::convert_line_endings(&text, LineEnding::Lf),
        "crlf" => text = oc_text::convert_line_endings(&text, LineEnding::CrLf),
        "cr" => text = oc_text::convert_line_endings(&text, LineEnding::Cr),
        _ => {}
    }
    let (body, n) = oc_text::encode(&text, to, policy)?;
    notes.extend(n);
    let mut out = Vec::with_capacity(body.len() + 4);
    if opts.bool("bom") == Some(true) {
        out.extend_from_slice(oc_text::bom(to));
    }
    out.extend_from_slice(&body);
    Ok((out, notes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_core::NoProgress;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oc-engine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).ok();
        d
    }

    fn req(dir: &Path, input: &[u8], opts: &[(&str, OptionValue)]) -> (ConvertRequest, PathBuf) {
        let i = dir.join("in.txt");
        std::fs::write(&i, input).ok();
        let o = dir.join("out.txt");
        (
            ConvertRequest {
                input: i,
                output: o.clone(),
                overwrite: false,
                options: opts
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), v.clone()))
                    .collect(),
            },
            o,
        )
    }

    #[test]
    fn utf16_to_utf8_with_crlf_normalised() {
        let dir = tmpdir("a");
        let input = oc_text::encode("a\r\nb\rc", Encoding::Utf16Le, Policy::Strict)
            .map(|r| r.0)
            .unwrap_or_default();
        let mut bytes = oc_text::bom(Encoding::Utf16Le).to_vec();
        bytes.extend(input);
        let (r, out) = req(
            &dir,
            &bytes,
            &[("line_ending", OptionValue::Text("lf".into()))],
        );
        let rep = Engine::new().convert(&r, &NoProgress).ok();
        assert_eq!(std::fs::read(out).ok().as_deref(), Some(&b"a\nb\nc"[..]));
        assert_eq!(rep.map(|r| r.route.contains("line endings")), Some(true));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unencodable_is_error_unless_lossy() {
        let dir = tmpdir("b");
        let (r, _) = req(
            &dir,
            "日本".as_bytes(),
            &[("to_encoding", OptionValue::Text("iso-8859-1".into()))],
        );
        let e = Engine::new().convert(&r, &NoProgress).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::UnsupportedVariant));
        let (mut r2, out) = req(
            &dir,
            "日本".as_bytes(),
            &[
                ("to_encoding", OptionValue::Text("iso-8859-1".into())),
                ("lossy", OptionValue::Bool(true)),
            ],
        );
        r2.overwrite = true;
        let rep = Engine::new().convert(&r2, &NoProgress).ok();
        assert_eq!(std::fs::read(out).ok().as_deref(), Some(&b"??"[..]));
        assert_eq!(rep.map(|r| r.notes.has_loss()), Some(true));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn existing_output_is_protected_and_bad_options_rejected() {
        let dir = tmpdir("c");
        let (r, out) = req(&dir, b"x", &[]);
        std::fs::write(&out, b"keep").ok();
        let e = Engine::new().convert(&r, &NoProgress).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::OutputExists));
        assert_eq!(std::fs::read(&out).ok().as_deref(), Some(&b"keep"[..]));
        let (mut r2, _) = req(
            &dir,
            b"x",
            &[("to_encoding", OptionValue::Text("klingon".into()))],
        );
        r2.overwrite = true;
        assert_eq!(
            Engine::new()
                .convert(&r2, &NoProgress)
                .err()
                .map(|e| e.code),
            Some(ErrorCode::InvalidOptions)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_extension_is_unsupported() {
        let dir = tmpdir("d");
        let r = ConvertRequest {
            input: dir.join("a.xyz"),
            output: dir.join("b.txt"),
            overwrite: false,
            options: vec![],
        };
        assert_eq!(
            Engine::new().convert(&r, &NoProgress).err().map(|e| e.code),
            Some(ErrorCode::UnsupportedFormat)
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
