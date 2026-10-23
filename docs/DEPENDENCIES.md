# Dependencies

xtask: `serde_json` (dev tool only, parses `cargo metadata`).
oc-io depends on oc-core (path dependency, no external crates).
oc-engine depends on oc-core and oc-text; oc-cli on oc-core and oc-engine (path dependencies only).

oc-python: `pyo3` (interpreter interface only, ADR-0014), path dependencies on oc-core and oc-engine.
