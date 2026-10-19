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
