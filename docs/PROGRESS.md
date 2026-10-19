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
- [~] M1.2 oc-io: checked ByteReader, BitReader/BitWriter (MSB/LSB, round-trip tested); oc-checksum: CRC-32, CRC-32C, CRC-64/XZ, Adler-32, xxHash64 against published check values (scalar only; SIMD paths and bounded stream sinks are not written yet).
- [~] M1.3 oc-compress: DEFLATE inflate (bomb limit, cancellation, never panics on garbage fuzz loop) and deflate (levels 0-9, stored/fixed/dynamic blocks), zlib, gzip (multi-member), PackBits, LZW (GIF and TIFF flavours). Checked against CPython: a level-9 dynamic-Huffman stream decodes correctly, and our zlib/gzip output at levels 1, 6, 9 is accepted by Python. Measured with Python as reference: on a 200 kB 6-bit-noise input our level 6 output is 154,622 bytes vs zlib 151,443 (2% larger); on repetitive text we are smaller. Not done: speed tuning (the inflate Huffman decoder is bit-by-bit), the Windows-only budget measurements, and the LZW GIF/TIFF vectors from real files (only round trips so far).
- [~] M1.4 oc-text: UTF-8/16/32, ASCII, ISO-8859-1, windows-1252, BOM and heuristic detection, strict/replace policies with loss notes, line-ending detect/convert (6 tests). Not done: other legacy code pages (Shift-JIS, GBK, ISO-8859-2.., windows-125x) and Unicode normalisation (NFC/NFD), which need mapping data. oc-xml: pull parser with namespaces, char references, CDATA, comments, PIs; entity declarations and DOCTYPE internal subsets rejected (no XXE or entity bombs); depth and attribute limits; writer with escaping (8 tests, including a garbage loop and an entity-bomb test). Not done: DTD attribute defaults, `encoding=` re-decoding, line-ending normalisation inside the parser.
- [~] M1.6 (done before M1.5) oc-engine + oc-cli: text conversion end to end (`openconverter convert in.txt out.txt --set to_encoding=utf-16le --set line_ending=lf --set bom=true`, `formats [--json]`, `--version`), typed exit codes, no-overwrite by default, atomic output write, notes printed to stderr (4 engine tests; CLI exercised by hand). Not done: `info`, `routes`, `doctor`, `--explain`, completions, JSON result output; progress is only reported at 3 coarse steps.
- [ ] M1.5 not started: oc-worker process isolation with memory and time limits. It needs OS-specific code (Windows job objects, Linux rlimits/landlock) that should be verified on the real targets.
- [ ] M1.7 not started: fuzz targets, corpora manifest, release workflow skeleton. Today only deterministic garbage-input loops exist in the oc-compress and oc-xml tests; there is no cargo-fuzz harness.

## Known gaps
- 1.x Python test cases are not ported yet; they are available at tag v1.0.0. Port them as each Rust codec lands (M1.x, M2.x).
- cargo-deny not run locally (not installed).

## Next step
M1.5: oc-worker process isolation (design the worker protocol, then Linux and Windows implementations), then M1.7 fuzz targets.
