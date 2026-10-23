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

## ADR-0006: Patent-encumbered audio codecs stay out of the default build
Context: MASTER_PROMPT §7.4 puts AAC and AC-3 behind an `encumbered` Cargo feature that is off by default, and asks for an explicit decision before a release includes it. No such decision has been made.
Decision: Phase 4 implements no AAC or AC-3 code at all. Files that hold them fail with `codec_not_in_build` and a message that suggests Apple Lossless, FLAC or Opus (`.m4a`, Matroska and WebM readers). Stream copy of such tracks arrives with the remux work in Phase 5a, which needs no codec.
Consequences: nothing in the 2.2.0 artifacts is subject to the §7.4 question. If the feature is added later, the `.rpm` stays without it.

## ADR-0007: MP3 is in the default build
Context: the MP3 patents are reported as expired (the last one in 2017); this is a record of the engineering decision, not a legal conclusion.
Decision: the layer III decoder and encoder are always built. The encoder writes a Xing/LAME-style header whose encoder string is `LAMEOC2.2`, not a LAME version number, so a player never assumes LAME's delay values apply to a stream they were not tuned for (the header stores an encoder delay of 528 samples, which with the usual 529-sample decoder delay gives gapless playback in ffmpeg and libmpg123).
Consequences: MP3 appears in every artifact. Layer I and II are not read yet.

## ADR-0008: Opus decoder covers CELT; SILK and hybrid are deferred, the encoder is CELT-only and simple
Context: Opus in the master prompt means SILK, CELT and hybrid. The CELT tables (band layout, allocation, cache, PVQ tables, window, comb filter taps) were read back from a libopus binary with a script, as ADR-0004 did for VP8, and the decoder was validated bit-exactly on the range-coder state against libopus. The SILK codebooks (about 40 KB of NLSF, gain, pitch and LTP tables) cannot be recovered the same way from the stripped `libopus.so.0` available here (no symbols to anchor on) and RFC 6716 was unreachable (rfc-editor.org is blocked by the egress proxy). Copying libopus source from the network was not done: it is the reference implementation, not a specification, and the project rule is to write from the specification.
Decision: CELT frames are decoded and encoded; a SILK or hybrid packet raises `unsupported_variant` for that packet and the rest of the file still decodes with a warning. The encoder writes CELT with long blocks, CBR/VBR/CVBR, dual stereo, no transient detection, no pitch pre-filter. Its output is valid for every decoder (checked with libopus) and its quality-for-bits is behind libopus's.
Consequences: Opus files from low-rate voice encoders do not decode here until SILK is added. Revisit when the RFC text or the tables can be obtained from a documented source.

## ADR-0009: Audio stream copy
Context: converting Opus from Ogg to WebM, or FLAC to Matroska, by decode and encode would lose quality for no reason.
Decision: `oc-audio::copy` moves packets between Ogg, Matroska/WebM and FLAC files when the codec fits the target and no option changes the sound (processing, bit depth, encoder settings). The route text says "no re-encoding". `codec=auto` is the default of the Matroska/WebM writers for that reason.
Consequences: the copy path must carry tags, pictures, pre-skip and end trimming exactly; round trips are tested packet for packet.
