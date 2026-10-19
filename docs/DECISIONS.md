# Decisions

## ADR-0001: Rust engine, own codecs, Tauri shell
Context: OpenConverter 1.x is pure Python. 2.x needs native speed, process isolation and a desktop app.
Decision: a Rust workspace with our own format code and only vetted infrastructure crates (MASTER_PROMPT §2, §5); Tauri v2 for the desktop shell.
Consequences: 1.x is retired from main and stays available at tag v1.0.0. Releases restart at 2.0.0.

## ADR-0002: Work lands by pull request, not direct push to main
Context: MASTER_PROMPT §28 says push to main after each milestone; this thread develops on a feature branch.
Decision: each milestone goes through a PR from the working branch.
Consequences: the push gate runs before each PR update; CI is checked on the PR.

## ADR-0003: Track latest stable Rust
Context: CI runs the newest stable toolchain, whose clippy added a lint the local 1.97 did not have (`chunks_exact_to_as_chunks`) and failed the first Phase 1 push.
Decision: keep `channel = "stable"` and run the push gate with `RUSTUP_TOOLCHAIN=stable` after `rustup update`; fix new lints when they appear. Revisit and pin if this becomes noisy.
Consequences: a toolchain update can turn CI red without a code change.

## ADR-0004: VP8 constant tables come from a libwebp binary, checked into source
Context: rfc-editor.org is unreachable from the build environment, and the VP8 specification (RFC 6386) is mostly tables: quantizer steps, token probabilities, key-frame mode probabilities.
Decision: `crates/oc-vp8/tools/gen_tables.py` reads those tables from a system libwebp shared object, asserts their known first and last values, and writes `src/tables.rs`. Nothing at runtime depends on libwebp. The decoder and its tests compare against libwebp output at development time only (`tools/make_fixtures.py`, `crates/oc-image/tools/make_lossy_webp_fixtures.py`).
Consequences: the tables are numeric constants of the format; the tool documents where they came from so they can be re-checked against the RFC text when network access allows.

## ADR-0005: Desktop shell embeds the local server; tauri's own dependencies are exempt
Context: MASTER_PROMPT §2 wants a Tauri v2 shell with an IPC adapter. Tauri's windowing stack (tauri-utils, tauri-codegen, plist, ico, wry) pulls `png`, `quick-xml`, `miniz_oxide` and `flate2` for icon handling and bundling, which the dependency ban forbids for conversion code.
Decision: `oc-desktop` starts the same loopback HTTP server used by `openconverter serve` (with a per-run token) and points a Tauri window at it, so the desktop app and the browser UI share one API and one security model. Conversions still run in sandboxed worker processes. `cargo xtask check-deps` does not descend into `tauri`, `tauri-*`, `wry` and `tao`; `deny.toml` allows the banned names only through those wrappers. None of those crates sees user files.
Consequences: the desktop crate is the only place with a GUI dependency tree; the CLI and server trees stay free of banned crates. The IPC adapter is dropped; native file dialogs can be added later through tauri plugins without touching the engine.
