# OpenConverter 2.x: Progress

Source of truth: [MASTER_PROMPT.md](MASTER_PROMPT.md). Decisions: [DECISIONS.md](DECISIONS.md).
The 1.x Python notes are kept in [v1/](v1/).

## Phase 0: Bootstrap and migration
- Case applied (§3): the repo held the Python 1.x implementation, tagged `v1.0.0`. It was removed from this branch; fixtures kept in `tests/fixtures`, and the 1.x frontend moved to `ui/` as the seed for the shared UI.
- [x] M0.1 Master prompt copied, PROGRESS and DECISIONS created. Preflight run in the cloud container (cargo 1.97.0, Linux); the Windows preflight (MSVC, WebView2, git identity) is still to be done on the user's machine.
- [x] M0.2 Python 1.x removed in one commit.
- [x] M0.3 Cargo workspace with 26 skeleton crates, workspace lints, rust-toolchain.toml, deny.toml, .gitattributes, xtask `check-deps`. `xtask fixtures/corpora/verify` are not written yet.
- [~] M0.4 `ci.yml` written (fmt, clippy, test, check-deps, cargo-deny on Windows/Ubuntu/macOS); not yet seen green.
- [ ] M0.5 ARCHITECTURE.md, SECURITY.md, CONTRIBUTING.md.

## Phase 1: Foundations
- [~] M1.1 oc-core: errors with stable codes and CLI exit codes, notes, limits, progress/cancel token, option schemas with validation, format descriptors with magic-byte sniffers, registry and Dijkstra route planner (17 unit tests). Not yet done: the IR types (Image, Audio, Video, Document, ...) are deferred to the milestone that first needs each one; `Registry::detect` has no test yet.
- [ ] M1.2 to M1.7 not started.

## Known gaps
- 1.x Python test cases are not ported yet; they are available at tag v1.0.0. Port them as each Rust codec lands (M1.x, M2.x).
- cargo-deny not run locally (not installed).

## Next step
Finish M1.1 (detect test), then M1.2 (oc-io readers, bit I/O, oc-checksum).
