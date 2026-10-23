//! Python bindings (`import openconverter`): `convert`, `info` and `formats` over the engine.
//!
//! The extension module is `openconverter._native`; `python/openconverter/__init__.py` re-exports
//! it. Conversions release the GIL. They run in the calling process, without the sandboxed worker
//! the command line uses (see docs/python.md).
//!
//! PyO3's macros expand to `unsafe` code, so this crate cannot use `forbid(unsafe_code)`; it
//! contains none of its own.
use oc_core::{NoProgress, Severity};
use oc_engine::{ConvertRequest, Engine};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyOSError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::path::{Path, PathBuf};

create_exception!(
    _native,
    ConversionError,
    PyException,
    "A conversion failed. `code` is the stable error code, `detail` extra information."
);

fn to_py(py: Python<'_>, e: &oc_core::Error) -> PyErr {
    let err = ConversionError::new_err(e.message.clone());
    let v = err.value(py);
    let _ = v.setattr("code", e.code.as_str());
    let _ = v.setattr("detail", e.detail.clone());
    err
}

fn note_dict<'py>(py: Python<'py>, n: &oc_core::Note) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item(
        "severity",
        match n.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Loss => "loss",
        },
    )?;
    d.set_item("code", &n.code)?;
    d.set_item("message", &n.message)?;
    d.set_item("detail", n.detail.as_deref())?;
    Ok(d)
}

/// What a conversion did.
#[pyclass(frozen, get_all, module = "openconverter")]
pub struct Result {
    input_format: String,
    output_format: String,
    route: String,
    bytes_in: u64,
    bytes_out: u64,
    notes: Py<PyList>,
}

#[pymethods]
impl Result {
    fn __repr__(&self) -> String {
        format!(
            "Result({} -> {}, {} -> {} bytes)",
            self.input_format, self.output_format, self.bytes_in, self.bytes_out
        )
    }
}

fn option_text(v: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(b) = v.extract::<bool>() {
        return Ok(b.to_string());
    }
    Ok(v.str()?.to_string())
}

/// Convert `src` to `dst`; the output format follows the extension of `dst`.
///
/// Extra keyword arguments are format options (`quality=80`, `lossless=True`); see `formats()`.
#[pyfunction]
#[pyo3(signature = (src, dst, *, overwrite = false, **options))]
fn convert(
    py: Python<'_>,
    src: PathBuf,
    dst: PathBuf,
    overwrite: bool,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<Result> {
    let mut raw: Vec<(String, String)> = Vec::new();
    if let Some(o) = options {
        for (k, v) in o.iter() {
            raw.push((k.extract::<String>()?, option_text(&v)?));
        }
    }
    let outcome = py.detach(move || {
        let engine = Engine::new();
        let options = engine.parse_options_for(&src, &dst, &raw)?;
        engine.convert(
            &ConvertRequest {
                input: src,
                output: dst,
                overwrite,
                display_name: None,
                options,
            },
            &NoProgress,
        )
    });
    let report = outcome.map_err(|e| to_py(py, &e))?;
    let notes = PyList::empty(py);
    for n in report.notes.iter() {
        notes.append(note_dict(py, n)?)?;
    }
    Ok(Result {
        input_format: report.input_format.0.to_string(),
        output_format: report.output_format.0.to_string(),
        route: report.route,
        bytes_in: report.bytes_in,
        bytes_out: report.bytes_out,
        notes: notes.unbind(),
    })
}

/// Identify a file: its format, category and, for pictures, its size.
#[pyfunction]
fn info<'py>(py: Python<'py>, path: PathBuf) -> PyResult<Bound<'py, PyDict>> {
    let work = |p: &Path| -> std::result::Result<_, PyErr> {
        let data =
            std::fs::read(p).map_err(|e| PyOSError::new_err(format!("{}: {e}", p.display())))?;
        let engine = Engine::new();
        let id = engine
            .detect(&data, p)
            .ok_or_else(|| ConversionError::new_err("unknown file type"))?;
        let probe = engine
            .probe(id, &data, &NoProgress)
            .map_err(|e| Python::attach(|py| to_py(py, &e)))?;
        let (name, category) = engine
            .registry()
            .format(id)
            .map_or((id.0, String::new()), |f| {
                (f.name, format!("{:?}", f.category).to_lowercase())
            });
        Ok((
            id.0.to_string(),
            name.to_string(),
            category,
            data.len(),
            probe,
        ))
    };
    let p = path.clone();
    let (id, name, category, size, probe) = py.detach(move || work(&p))?;
    let d = PyDict::new(py);
    d.set_item("path", path.display().to_string())?;
    d.set_item("format", id)?;
    d.set_item("name", name)?;
    d.set_item("category", category)?;
    d.set_item("size", size)?;
    d.set_item("width", probe.width)?;
    d.set_item("height", probe.height)?;
    d.set_item("alpha", probe.alpha)?;
    Ok(d)
}

/// Every format the engine knows, as a list of dicts.
#[pyfunction]
fn formats(py: Python<'_>) -> PyResult<Bound<'_, PyList>> {
    let engine = Engine::new();
    let list = PyList::empty(py);
    for f in engine.formats() {
        let d = PyDict::new(py);
        d.set_item("id", f.id.0)?;
        d.set_item("name", f.name)?;
        d.set_item("category", format!("{:?}", f.category).to_lowercase())?;
        d.set_item("extensions", f.extensions.to_vec())?;
        d.set_item("mime", f.mime.to_vec())?;
        d.set_item("read", f.capabilities.read)?;
        d.set_item("write", f.capabilities.write)?;
        list.append(d)?;
    }
    Ok(list)
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(convert, m)?)?;
    m.add_function(wrap_pyfunction!(info, m)?)?;
    m.add_function(wrap_pyfunction!(formats, m)?)?;
    m.add_class::<Result>()?;
    m.add("ConversionError", m.py().get_type::<ConversionError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
