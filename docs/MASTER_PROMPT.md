OpenConverter 2.0: Master Build Prompt for Claude Code

How to use this prompt

Open a terminal in D:\Personal Projects\OpenConverter and start Claude Code.
Paste everything below the line.
This project is far too large for one session. Claude Code keeps docs/PROGRESS.md up to date. When a session ends or runs out of context, start a new session and say: "Continue OpenConverter per docs/MASTER_PROMPT.md and docs/PROGRESS.md." (Milestone 0 copies this prompt into the repo as docs/MASTER_PROMPT.md.)
Each phase ends in a tagged, installable release, so you have something usable long before everything is finished.
PART I: MISSION, PRINCIPLES AND RULES
1. Mission

You are rebuilding OpenConverter as version 2.x: a free, open-source, fully local converter for images, audio, video, documents, PDFs, spreadsheets, subtitles, fonts and archives. It publishes to https://github.com/Yusuf6411/OpenConverter.

OpenConverter: Convert anything. Keep everything local.

The product has three faces, all built on one Rust engine:

OpenConverter Desktop: a double-click native app (Windows .exe installer, portable .exe, Linux .deb, .rpm, AppImage, Flatpak; macOS .dmg as a secondary target).
openconverter CLI: a scriptable command-line tool with the same capabilities, also shipped as a headless package for servers.
openconverter Python package (optional, later phase): native bindings so Python users can pip install openconverter.

The engine's format logic (every codec, container, compressor and document format) is written by us in Rust. General-purpose infrastructure (GUI shell, CLI parsing, serialization, threading, cryptography) may use vetted crates under the policy in §5.

Work autonomously, phase by phase, until the Definition of Done for each phase (Part VIII) is true. Never fake anything. Every conversion, progress value, preview, timing and result shown anywhere must come from real work.

2. Language decision: Rust, not C

Use Rust (stable) for the engine. Reasons, which you should also record in docs/ARCHITECTURE.md:

The core job is parsing untrusted files. Memory-safety bugs in C parsers are the main source of real-world exploits in media and document libraries. Safe Rust removes that whole class.
Rust is as fast as C when written well, supports SIMD (std::arch) with runtime CPU detection, and has data-race-free threading.
cargo gives reproducible builds, tests, fuzzing, benchmarking and cross-compilation in one toolchain.
Packaging a single Rust binary as .exe, .deb and .rpm is far simpler than packaging Python plus compiled extensions.

No C code is written. If a C dependency is pulled in indirectly by an approved infrastructure crate (for example the system WebView on Linux), that is acceptable and documented.

3. Relationship to OpenConverter 1.x (Python)

Inspect the repository first.

If it contains the Python 1.x implementation: ensure its final state is tagged (v1.0.0 if released, otherwise v1-python-final). Port its test fixtures and test cases into the Rust test suite (they are valuable oracles). Then remove the Python implementation from main in a single clearly-described commit. It remains available through the tag. Do not delete tags or rewrite history.
If the repository is empty or only has a stub: start fresh.
In both cases the new line of releases starts at 2.0.0. Record which case applied in docs/PROGRESS.md.
4. Non-negotiable principles
#	Principle	Meaning
P1	Local and offline at runtime	The shipped app and CLI make zero network connections: no telemetry, no analytics, no update checks, no crash reporting, no CDNs, no remote fonts, no remote images, no license servers. Network access is permitted only during development and in CI (fetching crates, specs, test corpora, build tools).
P2	Own format code	All codecs, containers, compressors, document parsers/writers, renderers and font engines are written by us. See the forbidden list in §5.
P3	Honesty	Every route through the engine declares ownership (§9.6). Every lossy step, dropped feature, approximation or fallback appears as a structured note in the result. Unsupported inputs fail with a precise typed error, never with a broken or silently degraded file.
P4	Safety on hostile input	Every decoder is fuzzed, bounded in memory and time, and runs in an isolated worker process (§16). A malicious file must never crash the app, hang it, exhaust memory, write outside the chosen output folder, or execute anything.
P5	Correctness before speed, speed before polish	Conformance with the published specification comes first, measured against reference tools in CI. Then performance budgets. Then UI polish.
P6	Privacy	Files never leave the machine. Logs never contain file contents and never contain full user paths unless the user enables debug logging, which is clearly labelled.
P7	Accessible and international	Keyboard operable, screen-reader friendly, WCAG 2.2 AA contrast, all UI strings externalised for translation, Unicode-correct throughout (paths, filenames, text, metadata).
P8	Legally clean	Implement only from public specifications and own reasoning. Never copy code from other implementations, regardless of their license, into format crates. Patent-encumbered codecs are behind an off-by-default build feature (§7.4).

Precedence when rules conflict: safety/security > correctness > honesty > legal cleanliness > scope > performance > polish.

5. Dependency policy
5.1 Forbidden in all shipped crates

Any crate or library that implements a file format, codec, compressor, or document model for us, including (non-exhaustive): image, png, jpeg-decoder, zune-*, gif, tiff, webp, ravif, rav1e, dav1d, libwebp*, flate2, miniz_oxide, zlib*, libz-*, zstd, ruzstd, lzma-rs, xz2, bzip2, brotli, zip, tar, sevenz-rust, unrar, symphonia, hound, claxon, lewton, minimp3, opus, ffmpeg*, gstreamer*, mp4, matroska, lopdf, pdf, pdfium*, printpdf, docx-rs, calamine, quick-xml, roxmltree, xml-rs, pulldown-cmark, comrak, ttf-parser, rustybuzz, fontdue, ab_glyph, tiny-skia, resvg, usvg, lyon, encoding_rs (we write our own legacy text encodings), chardetng. Also forbidden: invoking external programs (ffmpeg, ImageMagick, LibreOffice, Ghostscript, 7z) at runtime.

5.2 Allowed infrastructure (examples; prefer fewer, well-maintained crates)
GUI shell: tauri v2 and the minimal official plugins needed (dialog, opener for "show in folder"). No Tauri HTTP, updater, shell-execute or remote-URL plugins.
CLI: clap (derive).
Serialization: serde, serde_json (for our own config, IPC and the JSON table format's syntax layer; ownership of JSON conversion stays PARTIAL, see §9.6).
Errors/logging: thiserror, tracing, tracing-subscriber, tracing-appender.
Concurrency: rayon, crossbeam-channel, parking_lot.
Platform: directories, getrandom, uuid, memmap2, windows-sys / rustix / libc for process isolation and sandboxing, landlock (Linux sandbox).
Headless server mode: tokio and axum (or hyper) for openconverter serve.
Cryptography (decrypting/encrypting PDFs and archives): RustCrypto crates (aes, cbc, ctr, sha1, sha2, md-5, hmac, pbkdf2, rc4). Do not hand-roll cryptography. This is the one deliberate exception to "own code". Mark affected routes PARTIAL.
Python bindings: pyo3, built with maturin.
Dev/test only (never in shipped binaries): proptest, criterion, insta, tempfile, cargo-fuzz/libfuzzer-sys, arbitrary.
5.3 Enforcement
deny.toml for cargo-deny: license allow-list (MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode-3.0, MPL-2.0 for infra only, CC0-1.0); bans for every crate in §5.1 and every HTTP-client crate (reqwest, ureq, curl, isahc, attohttpc, hyper with client features) in shipped crates.
A test (xtask check-deps) parses cargo metadata and fails if a banned crate appears in the dependency tree of any shipped binary.
Every new dependency requires a one-line justification in docs/DEPENDENCIES.md.
Generate THIRD_PARTY_LICENSES.html with cargo-about for every release and ship it inside every package.
6. Operating protocol for you, Claude Code
Preflight (§8) before any code. Stop and report if a hard requirement is missing.
Copy this prompt to docs/MASTER_PROMPT.md in Milestone 0 and treat it as the source of truth. If you change a decision, record it in docs/DECISIONS.md (ADR style: context, decision, consequences), never by silently editing the prompt.
Living progress file docs/PROGRESS.md (committed after every milestone): current phase and milestone, checklist, measured numbers, open issues, deviations, and the exact next step. At the start of every session, read docs/MASTER_PROMPT.md, docs/PROGRESS.md and docs/DECISIONS.md before doing anything.
Milestone gate (Part IX push gate): a milestone is complete only when its tests exist and pass, the whole workspace passes, and it is committed and pushed. Never begin a milestone on a red build.
Spec first. Before implementing a format, write docs/formats/<format>.md: the spec document and version you are following, the subset supported, known variants rejected, limits, test plan. Fetch specifications from official sources during development (allowed: dev-time network) and cite section numbers in code comments where non-obvious.
Root causes only. Never skip, ignore, weaken or delete a test to get green. Never widen a tolerance without an ADR explaining why.
Decide, don't stall. Resolve ambiguity with the simplest choice consistent with this prompt, record it in docs/DECISIONS.md, and continue. Ask me only about: missing prerequisites, credentials/signing, legal choices about encumbered codecs, or an irreconcilable conflict between rules.
Subagents may be used for independent, bounded tasks (writing fuzz targets, test corpora scripts, docs, packaging scripts). Core codec logic is written and reviewed by you. Review every subagent result before committing.
Small commits, Conventional Commits (feat(png):, fix(zip):, test(jpeg):, perf(deflate):, docs:, ci:, build:, chore:). Never force-push. Never commit secrets, user files, build outputs, large binaries or corpora (corpora are fetched and cached in CI, §22.4).
Windows/PowerShell hygiene: use ; to chain commands (PowerShell 5.1 compatible), quote paths with spaces, stop background processes you start, never leave orphaned processes. Linux packages are built in CI (or WSL2 if available, §8). Use .gitattributes so binary fixtures are never line-ending converted.
PART II: ENVIRONMENT AND REPOSITORY
7. Platforms and support targets
7.1 Operating systems and architectures
Platform	Arch	Tier	Artifacts
Windows 10 (22H2) / 11	x86_64	Tier 1	NSIS setup .exe, MSI, portable .zip (contains OpenConverter.exe and openconverter.exe CLI)
Windows 11	ARM64	Tier 2	NSIS setup .exe, portable .zip
Debian 12+, Ubuntu 22.04 LTS+ and derivatives (Mint, Pop!_OS, elementary)	x86_64, aarch64	Tier 1	.deb (desktop), .deb (openconverter-cli, no GUI deps)
Fedora (all currently supported releases)	x86_64, aarch64	Tier 1	.rpm (desktop), .rpm (openconverter-cli)
RHEL / Alma / Rocky 9+	x86_64, aarch64	Tier 1 (CLI), Tier 2 (desktop, §23.1)	.rpm (openconverter-cli); desktop .rpm only if WebKitGTK 4.1 is available
Any glibc Linux	x86_64	Tier 1	AppImage
Flathub	x86_64, aarch64	Tier 2	Flatpak manifest (io.github.yusuf6411.OpenConverter)
Arch Linux	x86_64	Tier 3	PKGBUILD in packaging/aur/
Any Linux	x86_64, aarch64	Tier 1	static musl CLI tarball
macOS 12+	universal (x86_64 + arm64)	Tier 2	.dmg, CLI tarball (unsigned unless signing secrets exist, §23.5)

Tier 1 = built, tested and smoke-tested in CI for every release. Tier 2 = built and unit-tested in CI. Tier 3 = provided, best effort.

7.2 Minimum versions (verify at implementation time and record in docs/DECISIONS.md)
Rust: pin the current stable toolchain in rust-toolchain.toml (read rustc --version and pin that exact version). MSRV = that version.
Tauri: the current stable v2 release, pinned in Cargo.lock.
Linux desktop runtime: WebKitGTK 4.1 (libwebkit2gtk-4.1), GTK 3. Build Linux binaries on the oldest supported base (Ubuntu 22.04 / Debian 12 class glibc) so they run on newer systems.
Windows desktop runtime: Microsoft Edge WebView2 (preinstalled on Windows 11 and on updated Windows 10).
7.3 Offline installer note (WebView2)
The standard Windows installer uses Tauri's webviewInstallMode set to skip installation when WebView2 is present, and shows a clear local message with instructions if it is missing. It must not download anything silently.
Also produce an "offline" Windows installer variant that embeds the WebView2 offline runtime installer (offlineInstaller mode), so machines without internet and without WebView2 can install. Document the size difference.
The portable .zip requires WebView2 to be present; document this. The CLI never needs WebView2.
7.4 Patent-encumbered codecs

Some widely used codecs may still be covered by patents in some countries (for example H.264/AVC, H.265/HEVC, AAC). I am not asking you for legal advice and you must not give a legal conclusion.

Put all such codec implementations behind a Cargo feature encumbered, off by default.
Official release artifacts are built without encumbered unless docs/DECISIONS.md contains a decision I made explicitly. Ask me before the first release that would include it.
Fedora's official guidelines exclude such codecs, so the .rpm must always be built without it.
The UI and CLI explain clearly when a file needs a codec not included in this build (error code codec_not_in_build), and suggest remuxing or alternatives where possible.
Formats not legally implementable from a public spec are out of scope (for example creating RAR archives). RAR reading is out of scope too unless I decide otherwise in an ADR.
8. Preflight

Run and record results in docs/PROGRESS.md:

powershell
git --version
git config user.name; git config user.email   # if empty, ask me. Do not invent an identity.
rustup --version; rustc --version; cargo --version
rustup target list --installed
cargo install --list
wsl --status   # optional, for building Linux packages locally

Also check:

MSVC Build Tools with the "Desktop development with C++" workload and a Windows SDK (required by the Rust MSVC target and Tauri). If missing, stop and tell me exactly what to install.
WebView2 runtime present (needed to run the desktop app locally).
The GitHub MCP can see Yusuf6411/OpenConverter; the Playwright MCP is available.
Install (via cargo install --locked) only dev tools you need, and record their versions: tauri-cli (v2), cargo-deny, cargo-about, cargo-fuzz (fuzzing needs a nightly toolchain installed alongside stable: rustup toolchain install nightly), cargo-llvm-cov, cargo-nextest (optional), maturin (Phase 7).
No Node.js or npm is required. The frontend has no build step (§18). Do not introduce one.

If rustup is missing, stop and tell me to install it from the official site. Do not try to install toolchains through unofficial channels.

9. Repository layout and engine architecture
9.1 Workspace layout
OpenConverter/
├── Cargo.toml                    # [workspace], shared lints, release profile
├── rust-toolchain.toml
├── deny.toml  about.toml  clippy.toml  rustfmt.toml  .gitattributes  .gitignore
├── crates/
│   ├── oc-core/                  # IRs, registry, route planner, errors, notes, progress/cancel, limits
│   ├── oc-io/                    # bounded readers, bit readers/writers, streaming sinks, temp files
│   ├── oc-simd/                  # the ONLY crate allowed `unsafe` (SIMD kernels + scalar fallbacks)
│   ├── oc-checksum/              # CRC-32, CRC-32C, CRC-64/XZ, Adler-32, xxHash64, MD5-free* (see note)
│   ├── oc-compress/              # deflate, zlib, gzip, lzw, packbits, bzip2, lzma/lzma2/xz, zstd, ccitt, lz4
│   ├── oc-text/                  # encodings, BOM/charset detection, line endings, Unicode utilities
│   ├── oc-xml/                   # own XML 1.0 pull parser + writer (namespaces, entities, safe by default)
│   ├── oc-image/                 # image codecs, colour management, image ops
│   ├── oc-audio/                 # audio codecs, resampler, mixer, dither
│   ├── oc-vp8/                   # VP8 codec, shared by WebP lossy (oc-image) and video (oc-video)
│   ├── oc-video/                 # video codecs, scaler, pixel formats, frame-rate conversion
│   ├── oc-container/             # RIFF/AVI, ISO-BMFF (MP4/MOV), Matroska/WebM, Ogg, MPEG-TS, Y4M
│   ├── oc-archive/               # zip, tar, gzip/bz2/xz/zst wrappers, 7z, cpio, ar, iso9660 (read)
│   ├── oc-font/                  # TrueType/OpenType/CFF/Type1 parsing, rasterization, subsetting
│   ├── oc-raster/                # 2D vector rasterizer (paths, fills, strokes, clips, gradients, images)
│   ├── oc-document/              # Document IR + DOCX, ODT, RTF, HTML, Markdown, EPUB, TXT, layout engine
│   ├── oc-sheet/                 # Table IR + CSV, TSV, JSON, XLSX, ODS
│   ├── oc-pdf/                   # PDF parser, writer, text extraction, renderer (uses oc-raster, oc-font)
│   ├── oc-svg/                   # SVG parser + renderer (uses oc-xml, oc-raster, oc-font)
│   ├── oc-subtitle/              # SRT, WebVTT, ASS/SSA, TTML (read)
│   ├── oc-engine/                # registry assembly, planner, job runner, worker-process protocol
│   ├── oc-worker/                # sandboxed worker binary (one conversion per process)
│   ├── oc-cli/                   # `openconverter` binary
│   ├── oc-server/                # headless `openconverter serve` (localhost web UI)
│   ├── oc-desktop/               # Tauri app (`OpenConverter`)
│   └── oc-python/                # PyO3 bindings (Phase 7)
├── ui/                           # shared frontend (plain HTML/CSS/ES modules), used by desktop and serve
├── xtask/                        # cargo xtask: fixtures, corpora, check-deps, verify-release, bench, package
├── fuzz/                         # cargo-fuzz targets, one per decoder/parser
├── tests/                        # cross-crate integration and acceptance tests
├── corpora/                      # manifest.toml only (URLs + SHA-256); data is fetched, never committed
├── packaging/                    # windows/ linux/{deb,rpm,appimage,flatpak} aur/ macos/ desktop entries, icons
├── docs/                         # MASTER_PROMPT, PROGRESS, DECISIONS, ARCHITECTURE, FORMATS, CLI, API,
│                                 # SECURITY, DEPENDENCIES, PACKAGING, CONTRIBUTING, formats/<id>.md, screenshots/
├── .github/workflows/            # ci.yml, fuzz-nightly.yml, release.yml, corpora.yml
└── README.md  CHANGELOG.md  LICENSE  SECURITY.md  CODE_OF_CONDUCT.md

Note on MD5/SHA: checksums that are integrity checks within formats (CRC, Adler, xxHash) are ours. Cryptographic hashes used for decryption or verification (MD5 in PDF security handlers, SHA-256 in 7z AES) come from RustCrypto (§5.2).

9.2 Layering rules

oc-cli / oc-desktop / oc-server / oc-python → oc-engine → format crates → oc-core / oc-io / oc-simd / oc-checksum / oc-compress.

Format crates may depend on lower-level format crates (for example oc-image on oc-vp8 and oc-compress, oc-pdf on oc-image, oc-font and oc-raster), but the dependency graph must stay acyclic and is documented in docs/ARCHITECTURE.md.
Format crates never depend on oc-engine, UI crates, Tauri, tokio or filesystem paths. They read from oc-io traits and write to sinks.
#![forbid(unsafe_code)] in every crate except oc-simd (and the minimal OS-specific code in oc-worker/oc-engine for process isolation, which must use #![deny(unsafe_code)] with each unsafe block individually allowed, commented with a // SAFETY: justification and reviewed).
Workspace lints: clippy::pedantic warnings reviewed; -D warnings in CI; unwrap()/expect()/indexing that can panic on untrusted data are forbidden in decoders (use checked access; a lint or test enforces it, see §22.7).
9.3 Intermediate representations (in oc-core)

All conversions go through typed IRs so any reader can feed any compatible writer.

Image IR

rust
pub struct Image {
    pub width: u32, pub height: u32,
    pub layout: PixelLayout,          // Gray, GrayA, Rgb, Rgba, Cmyk, Cmyka, Indexed(Palette)
    pub sample: SampleType,           // U8, U16, F16, F32
    pub alpha: AlphaMode,             // None, Straight, Premultiplied
    pub color: ColorInfo,             // ICC profile bytes and/or CICP (primaries, transfer, matrix, range), gamma, chromaticities
    pub orientation: Orientation,     // EXIF 1-8; applied or preserved per writer capability
    pub metadata: ImageMetadata,      // EXIF, XMP, IPTC blobs, text chunks, DPI, creation time
    pub planes: PixelBuffer,          // interleaved by default; planar variant for YUV/CMYK pipelines; tiled for huge images
}
pub struct AnimatedImage { pub canvas: (u32,u32), pub frames: Vec<Frame>, pub loop_count: LoopCount, pub background: Option<Rgba> }
pub struct Frame { pub image: Image, pub offset: (u32,u32), pub duration: Rational, pub dispose: Dispose, pub blend: Blend }

Images above a configurable size (default 64 MP) are processed in tiles/strips through a streaming pipeline so memory stays bounded.

Audio IR: streaming: AudioStream yields AudioChunk { format: SampleFormat (I16/I24/I32/F32/F64), channels: ChannelLayout, sample_rate: u32, frames: usize, data } plus AudioMetadata (tags, cover art, ReplayGain, chapters).

Video IR: streaming: VideoStream yields VideoFrame { pts: Rational, duration: Rational, format: PixelFormat (yuv420p/422p/444p 8/10/12-bit, nv12, rgb24, rgba, gray...), color: ColorInfo, planes }. Packet-level IR for remuxing without decoding: Packet { stream_index, pts, dts, duration, keyframe, data } and StreamInfo { codec_id, codec_private, time_base, ... }.

Media IR: MediaFile { streams: Vec<StreamInfo>, chapters, attachments (fonts, cover art), metadata }.

Document IR: a structured tree:

Document { meta, styles, sections, notes (foot/endnotes), assets (images) }
Block = Paragraph (style, runs), Heading(level), List(ordered/bullet, nesting), Table (rows, cells, spans, borders), Image (asset ref, size), PageBreak, SectionBreak, CodeBlock, BlockQuote, HorizontalRule
Inline = Text (with character style: bold/italic/underline/strike/super/sub/font/size/color/highlight), Link, Image, LineBreak, Tab, FootnoteRef, Field (page number etc.)

Page IR (display list), shared by PDF, SVG and the document layout engine: Page { size, ops: Vec<DrawOp> } where DrawOp = Path (fill/stroke with paint, rule, transform), Text (positioned glyph runs with font ref), Image (transform, interpolation), Clip push/pop, Group (opacity, blend mode, soft mask). This is what oc-raster renders and what the PDF writer serializes.

Table IR: Sheet { name, header: Option<Vec<String>>, rows: Vec<Vec<Cell>> }, Cell = Empty | Text | Number(f64 with original text preserved) | Bool | Date(ISO 8601) | Formula(text, cached value) | Error. Workbooks hold multiple sheets.

Archive IR: streaming ArchiveReader yields Entry { path (normalised, validated), kind (file/dir/symlink/hardlink), size, mtime, mode, attributes, comment } with a lazily-read data stream. ArchiveWriter accepts the same.

Subtitle IR: Subtitles { cues: Vec<Cue { start, end, text (styled spans), position, style_ref }>, styles }.

Font IR (for font conversion and embedding): parsed table directory + glyph outlines + metrics + cmap.

9.4 Registry and route planner

Each format registers a FormatDescriptor: id, display name, category, extensions, MIME types, magic-byte sniffers with confidence scores, capabilities (read, write, animation, alpha, metadata, multi-page, streams), writer options schema (typed: int/float/bool/enum/color/string with min/max/default/label/help/i18n key), ownership, spec reference, and variant notes.

Each converter registers an edge in a graph: (from, to, cost, fidelity, ownership). Edges include decode, encode, IR-to-IR transforms (e.g. Document → Page via layout), remux edges (packet copy), and stream-copy shortcuts (e.g. MKV(VP8) → WebM without decoding).

The planner finds the best route with Dijkstra over a lexicographic cost: (fidelity loss, then ownership preference, then estimated time). Rules:

Prefer lossless and stream-copy paths. A route that re-encodes when a remux would work is a bug.
Every route returns a human-readable plan ("Demux MP4 → copy VP9 stream → mux WebM") shown in the UI and CLI (--explain).
Users can force re-encoding (--reencode) or force a specific intermediate.
GET formats / openconverter formats --json exports the entire graph; the UI is driven only by this data (no hard-coded format lists in the frontend).
9.5 Errors and notes

Typed errors in oc-core::Error with stable codes used by CLI exit codes, the UI and API:

unsupported_format, unsupported_variant (with variant detail), codec_not_in_build, corrupt_file (with byte offset when known), truncated_file, limit_exceeded (which limit), encrypted_need_password, wrong_password, drm_protected (we never circumvent DRM), cancelled, timeout, worker_crashed, io_error, output_exists, invalid_options (which key and why), internal_error.

Notes are structured, attached to every result: { severity: info|warning|loss, code, message, detail }, for example alpha_flattened, bit_depth_reduced, metadata_dropped (which fields), font_substituted, animation_first_frame_only, color_profile_converted, lossy_reencode, fast_mode_large_input, text_approximated, layout_approximated. Never drop data without a note.

9.6 Ownership levels
OWN: the whole route is our code.
PARTIAL: our code plus a vetted infrastructure component (RustCrypto for encrypted inputs/outputs; serde_json syntax for JSON; OS fonts used for layout).
EXTERNAL: not used in 2.x. Reserved for future optional plugins. The badge is shown on every route in the UI and in formats output.
9.7 Progress and cancellation contract

Every long loop in every codec receives &dyn Progress (report(done, total, stage), check_cancelled() -> Result<()>). Check at least every ~20 ms of work. Progress is real, monotonic, weighted per stage, throttled to ≤ 10 updates/s at the engine boundary. The engine supports pause/resume for queued jobs and cooperative cancellation for running ones; the worker process is killed only after a grace period (§16.1).

PART III: FORMAT CATALOGUE

Each format below lists the exact scope. "R" = read, "W" = write. Phase numbers refer to Part VIII. Anything not listed for a format is rejected with unsupported_variant and a precise message. Write docs/formats/<id>.md before implementing each one.

10. Foundations (Phase 1)
10.1 Checksums (oc-checksum)

CRC-32 (IEEE, slicing-by-8 plus SIMD/PCLMUL path in oc-simd with scalar fallback), CRC-32C, CRC-64/XZ, Adler-32 (deferred modulo, SIMD path), xxHash64 (for zstd). Incremental APIs. Tested against published check values and against reference tools in CI.

10.2 Compression (oc-compress), all streaming, all with output caps
Codec	R	W	Spec / notes
DEFLATE (raw)	✅	✅	RFC 1951. Inflate: table-driven fast decoder (multi-symbol lookup), all block types, strict validation (over-subscribed/incomplete codes, invalid distances, NLEN mismatch). Deflate: levels 0–9 + 10–12 "extra" (optimal parsing), hash chains + lazy matching, near-optimal block splitting, length-limited Huffman (package-merge), stored/fixed/dynamic selection.
zlib	✅	✅	RFC 1950, Adler-32 verified, FDICT rejected.
gzip	✅	✅	RFC 1952, multi-member, FNAME/FCOMMENT/FEXTRA, CRC + ISIZE verified.
LZW	✅	✅	GIF variant, TIFF variant (incl. old-style TIFF LZW bit order detection), PDF variant (EarlyChange).
PackBits	✅	✅	TIFF, PSD.
CCITT G3 (1D/2D) / G4	✅	✅	ITU-T T.4/T.6, for TIFF and PDF CCITTFaxDecode.
bzip2	✅	✅	BWT (SA-IS suffix array), MTF, RLE1/RLE2, multi-table Huffman, block CRC + stream CRC; parallel block decoding.
LZMA / LZMA2	✅	✅	Range coder, match finders (BT4, HC4), presets 0–9 + extreme.
XZ	✅	✅	XZ format spec 1.x: block headers, index, CRC32/CRC64/SHA-256 checks (SHA-256 via RustCrypto), BCJ filters (x86, ARM, ARM64, ARMThumb, PowerPC, IA64, SPARC, RISC-V), Delta filter; multi-block parallel compression and decompression.
Zstandard	✅	✅	RFC 8878: frames, skippable frames, FSE/Huffman literals, repeat offsets, dictionaries (read), checksums. Encoder levels 1–19 (fast, greedy, lazy, btopt strategies), long-distance mode optional; multithreaded compression by blocks.
LZ4	✅	✅	Frame format (for .lz4 files and some archives).
Brotli	✅	❌	RFC 7932 decoder (needed for WOFF2 fonts). Encoder in a later phase.
10.3 Text (oc-text)
Encodings, own tables generated at build time by an xtask from the WHATWG Encoding Standard index files (fetched at dev time, checksummed, the generated tables committed): UTF-8, UTF-16LE/BE, UTF-32LE/BE, ISO-8859-1…16, Windows-874 and 1250–1258, KOI8-R/U, IBM866, Mac Roman, Shift_JIS, EUC-JP, ISO-2022-JP, GBK/GB18030, Big5, EUC-KR.
BOM detection; heuristic charset detection (byte-pattern and frequency scoring) with a confidence value that the UI shows; user override always wins.
Line endings LF/CRLF/CR/preserve; Unicode normalisation NFC/NFD/NFKC/NFKD (own tables from the Unicode Character Database, generated by xtask); tab expansion; trailing whitespace trimming; unencodable characters → error / replace / numeric character reference / transliterate-basic (each noted with counts).
10.4 XML (oc-xml)

Own XML 1.0 (5th ed.) pull parser and writer with namespaces, CDATA, character references, predefined entities. Safe by default: no external entity resolution (XXE impossible), no DTD-defined entity expansion beyond strict limits (billion-laughs defence), depth and size limits. Used by DOCX/XLSX/ODF/EPUB/SVG/TTML/XMP.

11. Images (oc-image), Phase 2
11.1 Colour management (own CMM)
Parse ICC v2 and v4 profiles: matrix/TRC (RGB, gray), LUT-based (mft1, mft2, mAB, mBA) for CMYK, parametric curves.
Transforms: profile → profile via PCS (XYZ/Lab) with perceptual/relative-colorimetric intents and black-point compensation; built-in sRGB, Display P3, Adobe RGB-compatible, Rec. 709, Rec. 2020, linear sRGB, gray gamma 2.2, and a generic CMYK profile created by us (not a copied vendor profile).
CICP (ITU-T H.273) parsing for PNG cICP, AVIF/HEIF and video.
Default policy: preserve the embedded profile when the target can store it; convert to sRGB (noted) when it cannot. HDR (PQ/HLG) inputs to SDR targets are tone-mapped (BT.2390 EETF) with a note.
11.2 Image operations (oc-image::ops)

Resize (box, bilinear, bicubic Catmull-Rom/Mitchell, Lanczos3; linear-light option; correct alpha handling via premultiplication), crop, rotate 90/180/270, flip, EXIF orientation apply, arbitrary-angle rotate (bilinear), pad/fit/fill modes, alpha flatten (straight-alpha formula, user colour), sample-depth conversion with ordered or error-diffusion dithering (Floyd–Steinberg, Atkinson) when reducing, colour quantisation for palette targets (median cut and k-means refinement, optional dithering), grayscale conversion (BT.601/BT.709 luma, configurable), metadata strip/keep policies (keep all / strip GPS only / strip all). All ops use rayon over rows or tiles and SIMD kernels where profitable.

11.3 Image formats
Format	R	W	Scope
PNG (W3C PNG 3rd edition)	✅	✅	All colour types and bit depths, Adam7, all filters, PLTE, tRNS, gAMA, cHRM, sRGB, iCCP, cICP, pHYs, tEXt/zTXt/iTXt, eXIf, tIME; unknown ancillary chunks preserved when copying PNG→PNG (option). CRC verified. Write: best colour type/bit depth search, palette when lossless and smaller, per-row filter heuristics (min-sum-abs and entropy, "try all" at high levels), our deflate at levels 0–12, metadata preservation.
APNG	✅	✅	acTL/fcTL/fdAT, dispose/blend ops, sequence numbers validated, frame differencing optimisation on write.
JPEG	✅	✅	Read: baseline, extended 12-bit, progressive (spectral selection + successive approximation), arithmetic coding (both sequential and progressive), lossless (SOF3), hierarchical rejected; 1/3/4 components incl. CMYK and YCCK (Adobe APP14), any sampling factors, restart markers with error recovery, JFIF/Adobe/EXIF/ICC (multi-segment APP2)/XMP. DCT scaling (1/2, 1/4, 1/8) for fast thumbnails. Fancy upsampling. Write: baseline and progressive, quality 1–100 (IJG scaling) plus custom tables, subsampling 4:4:4/4:2:2/4:2:0, optimised Huffman, trellis quantisation (optional, slow mode), arithmetic coding option (off by default for compatibility), ICC/EXIF/XMP preservation, EXIF orientation preserved or applied. Lossless JPEG transforms (rotate/flip/crop on MCU boundaries without re-encoding) as a dedicated route.
GIF (89a)	✅	✅	All frames, local/global palettes, transparency, interlace, disposal, NETSCAPE loop extension, comments. Write: animated, per-frame palettes or global, quantisation + dithering, frame differencing with transparency, LZW optimal-ish code resets.
WebP (RFC 9649)	✅	✅	Lossy (VP8 key frames, shared with the VP8 video codec), lossless (VP8L: predictor, colour, subtract-green and colour-indexing transforms, colour cache, LZ77 backward refs), alpha (ALPH, filtered + compressed), animation (ANIM/ANMF), ICC/EXIF/XMP.
TIFF 6.0 + common extensions	✅	✅	Strips and tiles, chunky and planar, 1/2/4/8/16/32-bit int and 16/32-bit float, Gray/RGB/Palette/CMYK/YCbCr/CIELab (read), compression none/PackBits/LZW/Deflate/CCITT G3/G4/JPEG ("new-style" TIFF-JPEG; old-style rejected), predictor 2 and 3, multi-page, BigTIFF, EXIF/ICC/XMP, both byte orders. Write: LZW/Deflate/none, multi-page, BigTIFF when > 4 GB.
BMP / DIB	✅	✅	All header versions (CORE, INFO, V2–V5, OS/2 2.x), 1/2/4/8/16/24/32-bit, BITFIELDS/ALPHABITFIELDS, RLE4/RLE8/RLE24 (OS/2), Huffman 1D (OS/2), embedded PNG/JPEG (decoded via our codecs), ICC in V5. Write: 1/4/8 palettised, 24, 32 with V5 alpha.
ICO / CUR	✅	✅	BMP and PNG entries, AND masks, multi-size. Write multi-size icons (16–256) from one source.
ICNS	✅	✅	PNG/JPEG 2000-free subset (PNG and RLE24 + masks entries); others rejected.
QOI	✅	✅	Full spec.
TGA	✅	✅	All types 1/2/3/9/10/11, 8/15/16/24/32-bit, both origins, TGA 2.0 extension (alpha type, thumbnails).
Netpbm	✅	✅	P1–P7 (PAM with TUPLTYPE), PFM (float).
PSD / PSB	✅	❌	Read the merged composite image (all colour modes and depths, RLE/raw); layers listed in metadata but not rendered (note).
DDS	✅	✅	Uncompressed, BC1–BC5 (read + write), BC6H/BC7 (read), DX10 header, mipmaps (first level by default, option to export all).
Radiance HDR (.hdr)	✅	✅	RGBE, new-style RLE.
OpenEXR	✅	✅	Scanline and tiled, half/float/uint, NONE/RLE/ZIPS/ZIP/PIZ/PXR24/B44/B44A (read), NONE/ZIP/PIZ (write), single-part first, multi-part read.
JPEG 2000 (JP2/J2K)	✅	❌	Phase 6: needed for PDF JPXDecode. Full Part 1 decoding (EBCOT tier-1/2, 5/3 and 9/7 wavelets, all progression orders, ROI shift).
JPEG XL	✅	✅	Phase 6 (stretch): decoder for the full codestream (VarDCT + Modular) and lossless JPEG recompression reconstruction; encoder: lossless Modular and VarDCT basic.
AVIF	✅	✅	Phase 6 (stretch): HEIF container (own ISO-BMFF parser) + AV1 intra decoding (shared with AV1 video decoder) and a basic AV1 intra encoder.
HEIC / HEIF (HEVC)	✅	❌	Behind encumbered feature only (§7.4). Without it: codec_not_in_build, but HEIF container metadata and embedded JPEG thumbnails are still readable.
SVG (rasterize)	✅	✅*	Read via oc-svg (§14.4) → render at chosen size/DPI. *Write = raster-to-SVG embedding (<image> with data URI) and, for documents/PDF pages, true vector SVG export from the Page IR.
RAW camera (DNG)	✅	❌	Phase 6: DNG via TIFF reader + demosaic (bilinear and AHD), white balance, colour matrix, basic tone curve. Proprietary RAW (CR2/NEF/ARW) read only if implementable from public docs; otherwise roadmap.

Image acceptance: every readable format → every writable format through the IR, with mode/depth/alpha/metadata policies applied and noted.

12. Audio (oc-audio), Phase 4
12.1 DSP

Sample-format conversion with TPDF dither (and optional noise shaping) when reducing bit depth; channel layouts (mono … 7.1.4) with ITU-R BS.775 downmix and configurable upmix; high-quality resampler (polyphase windowed-sinc, Kaiser window, configurable quality presets; passband ripple and stopband attenuation documented and tested); EBU R128 / ITU-R BS.1770 loudness measurement and optional normalisation; ReplayGain read/write; trimming, fade, silence detection (optional "trim silence").

12.2 Audio formats
Format	R	W	Scope
WAV	✅	✅	PCM 8–32-bit, IEEE float 32/64, A-law/µ-law, IMA/MS ADPCM (read), WAVE_FORMAT_EXTENSIBLE with channel masks, RF64/BW64 for > 4 GB, BWF bext, LIST/INFO tags, cue .
AIFF / AIFF-C	✅	✅	PCM big/little endian (sowt), float, 80-bit extended sample rate, markers, ID3 chunk.
FLAC (RFC 9639)	✅	✅	All block sizes, fixed/LPC/verbatim/constant subframes, stereo decorrelation modes, 4–32-bit, Rice partitions, seek tables, Vorbis comments, pictures, cuesheet; MD5 of audio verified (via RustCrypto md-5); encoder levels 0–8 with LPC order search, exhaustive model search at 8; Ogg FLAC read.
ALAC	✅	✅	In MP4/M4A (own ISO-BMFF) and CAF.
MP3 (MPEG-1/2/2.5 Layer III; Layer I/II read)	✅	✅	Decoder bit-exact within the ISO 11172-4 compliance tolerance; ID3v1/v2.2/2.3/2.4, Xing/Info/VBRI headers, LAME gapless info, free-format. Encoder: CBR/ABR/VBR, psychoacoustic model, joint stereo, bit reservoir, LAME-compatible gapless header. (MP3 patents are generally reported as expired; still record this in docs/DECISIONS.md.)
Ogg Vorbis	✅	✅	Vorbis I spec full decoder; encoder with quality −1…10 (psychoacoustic floor/residue selection).
Opus (RFC 6716, Ogg Opus RFC 7845)	✅	✅	Decoder: SILK, CELT, hybrid, all frame sizes, packet loss concealment not needed for files. Encoder: CELT-only and hybrid modes, 6–510 kbps, VBR/CVBR. Output resampled to 48 kHz as required.
WavPack	✅	❌	Lossless and hybrid (read), Phase 6 stretch.
Monkey's Audio (APE)	✅	❌	Phase 6 stretch, only if implementable from public documentation.
AAC (LC, HE-AAC v1/v2)	✅	✅	encumbered feature only.
AC-3 / E-AC-3	✅	❌	encumbered feature only.
Containers for audio	✅	✅	M4A (ISO-BMFF), MKA (Matroska), Ogg, CAF, WebM audio; stream copy when the codec is compatible.

Tags and cover art are mapped between ID3, Vorbis comments, MP4 atoms, APE tags and Matroska tags through a common tag model, with a note for any tag that has no equivalent.

13. Video and containers (oc-video, oc-container), Phase 5
13.1 Containers (demux + mux, all streaming, all seekable where the format allows)
Container	Demux	Mux	Scope
Matroska / WebM (RFC 8794 EBML, RFC 9559 Matroska)	✅	✅	Clusters, SimpleBlock/BlockGroup, lacing (Xiph/EBML/fixed), cues (write cues for seeking), chapters, attachments (fonts), tags, multiple tracks, subtitles (S_TEXT/UTF8, S_TEXT/ASS, S_TEXT/WEBVTT). WebM writer restricts to WebM-allowed codecs.
MP4 / MOV / M4A / M4V / 3GP (ISO/IEC 14496-12, QuickTime)	✅	✅	moov/mdat in any order, 64-bit sizes, fragmented MP4 (moof), edit lists (honoured), ctts B-frame offsets, chapters, metadata (udta/meta/ilst), cover art; write faststart (moov before mdat) by default.
AVI (RIFF + OpenDML)	✅	✅	idx1 and OpenDML indexes, > 1 GB/2 GB files, MJPEG, uncompressed, PCM.
Ogg (RFC 3533)	✅	✅	Vorbis, Opus, FLAC, Theora (demux only).
MPEG-TS / M2TS	✅	✅	PAT/PMT, PES, PCR, continuity checks, multiple programs (select).
MPEG-PS	✅	❌	Read only.
FLV	✅	❌	Read only.
Y4M	✅	✅	All colour spaces/chroma tags.
Animated image as video	✅	✅	GIF, APNG, animated WebP: read as video streams; write from video with frame-rate/size/palette options.
Image sequences	✅	✅	Video → numbered PNG/JPEG/WebP frames; frames → video.

Remux is first-class: MP4 ↔ MKV ↔ MOV ↔ WebM ↔ TS conversions copy compatible streams without decoding (the planner chooses this automatically), handling timestamp rebasing, ctts/B-frame ordering, codec-private data conversion (e.g. avcC ↔ Annex B start codes for H.264 even when the encumbered decoder is not built, since this is bitstream reformatting, not decoding).

13.2 Video codecs
Codec	Dec	Enc	Scope
MJPEG	✅	✅	Reuses our JPEG codec (AVI/MOV variants, including missing-DHT MJPEG).
Uncompressed / raw	✅	✅	v210, YUY2, UYVY, rgb24, etc. in AVI/MOV/Y4M.
VP8 (RFC 6386)	✅	✅	Full decoder (all profiles, loop filter, segmentation, golden/altref). Encoder: rate control (CBR/VBR/CQ), motion search (diamond + subpel), mode decision, loop filter level search, two-pass optional, multi-threaded token partitions.
VP9	✅	✅	Decoder profiles 0–2 (8/10/12-bit, 4:2:0/4:2:2/4:4:4), tiles, superframes, loop filter, scaling references. Encoder: profile 0 with tiles for parallelism, speed presets, CQ/VBR.
AV1	✅	✅	Decoder: Main profile (8/10-bit 4:2:0), all tools required for conformance; Phase 6 adds High/Professional if feasible; multithreaded by tiles. Encoder: Main profile, speed presets, CQ/VBR, tiles. AV1 is the flagship royalty-free target; treat encoder quality as an ongoing project with measured BD-rate vs. the reference encoder documented.
Theora	✅	❌	Decoder (for old Ogg files).
H.264/AVC	✅	✅	encumbered only. Decoder: Constrained Baseline, Main, High (8-bit 4:2:0), CABAC/CAVLC, B-frames, interlaced (PAFF/MBAFF) read. Encoder: High profile, CABAC, B-frames, rate control.
H.265/HEVC	✅	❌	encumbered only. Main and Main10 decoder.
ProRes	✅	❌	Phase 6 stretch, only if implementable from public specification (SMPTE RDD 36).
13.3 Video processing

Scaling (bilinear/bicubic/Lanczos, per-plane, SIMD), pixel format and colour conversions (BT.601/709/2020, limited/full range, chroma siting), deinterlacing (yadif-style edge-directed or simple bob; documented), frame-rate conversion (drop/duplicate; blend optional), crop/pad/rotate (honour rotation metadata), trim by time, A/V sync with timestamp correction, audio track selection and re-encoding, subtitle burn-in (render via oc-font + oc-raster with ASS styling subset), thumbnail strip extraction for previews, "video → GIF/WebP" presets with palette optimisation.

13.4 Subtitles (oc-subtitle)

SRT ↔ WebVTT ↔ ASS/SSA ↔ TTML (read) ↔ Matroska/MP4 text tracks; extract and embed; encoding detection via oc-text; timing shift and frame-rate retiming; styling mapped where possible (noted where not).

14. Documents, spreadsheets, PDF, SVG, fonts
14.1 Document formats (oc-document), Phase 6
Format	R	W	Scope
TXT	✅	✅	All encodings (§10.3).
Markdown	✅	✅	CommonMark (current spec version) + GFM extensions (tables, task lists, strikethrough, autolinks, footnotes). Must pass the official CommonMark spec test suite 100% and the GFM extension tests.
HTML	✅	✅	Own HTML parser implementing the WHATWG tokenizer and tree-construction algorithm (pass the html5lib tokenizer and tree-construction tests, ≥ 99%, documenting failures); scripts never executed; remote resources never fetched (noted). Writer emits clean semantic HTML5 with embedded CSS and data-URI images (option: separate asset folder).
DOCX (ECMA-376 / ISO 29500, transitional + strict)	✅	✅	Paragraph/character styles with inheritance, numbering (lists), tables (merged cells, borders, widths), images (inline and anchored → nearest flow position, noted), headers/footers, footnotes/endnotes, hyperlinks, bookmarks, fields (cached results), sections (page size/margins/orientation/columns), tracked changes (accept-all or reject-all option, noted), comments (drop or append, option), embedded fonts (obfuscated, read). Writer produces files that open cleanly in Microsoft Word and LibreOffice (validated in CI against the OOXML schemas and by LibreOffice round-trip as an oracle).
ODT (ODF 1.3)	✅	✅	Same feature coverage as DOCX via the Document IR.
RTF (1.9.1)	✅	✅	Character/paragraph formatting, tables, pictures (PNG/JPEG/WMF-placeholder), Unicode (\uN), code pages via oc-text.
EPUB (2 and 3.3)	✅	✅	OPF/NCX/nav, spine order, XHTML content via our HTML parser, CSS subset, images, fonts, cover; writer validated with EPUBCheck in CI (oracle). DRM-protected EPUBs → drm_protected.
FB2	✅	❌	Read.
PDF	see §14.3		
DOC / XLS / PPT (legacy binary, MS-DOC/MS-XLS/MS-PPT open specs)	✅	❌	Phase 6 stretch: text, basic formatting, tables (DOC); cell values (XLS); slide text (PPT). Uses own Compound File Binary (MS-CFB) reader.
PPTX / ODP	✅	❌	Slides → text/Markdown outline and slides → images/PDF via the Page IR (shapes, text boxes, pictures, basic tables; SmartArt/charts as cached images when present, else noted).

Layout engine (Document IR → Page IR), used for DOCX/ODT/RTF/HTML/Markdown/EPUB → PDF/images:

Box model for blocks; paragraph line breaking with the Knuth–Plass algorithm (greedy fallback for very long paragraphs), hyphenation (Liang patterns for at least English, generated from published TeX pattern files whose license permits redistribution, verified per pattern file), justification, widows/orphans control, keep-with-next.
Tables with column width resolution (auto/fixed), row splitting across pages, repeated header rows.
Lists, footnotes placed on the referencing page, headers/footers with page numbers, multi-column sections.
Text shaping: own OpenType shaping for Latin, Greek, Cyrillic (kerning kern/GPOS, ligatures GSUB liga/clig, mark positioning). Complex scripts (Arabic joining + init/medi/fina/isol, Hebrew, Devanagari and other Indic, Thai) are a Phase 6b milestone with the Unicode Bidirectional Algorithm (UAX #9) and line breaking (UAX #14). Until done, those scripts render with a text_approximated note, never silently wrong.
Fonts: use fonts embedded in the document when present; otherwise system fonts (Windows %WINDIR%\Fonts, Linux fontconfig directories parsed by our own code, no libfontconfig), with metric-compatible substitution tables (e.g. Calibri → Carlito, Cambria → Caladea, Arial → Liberation Sans) when those fonts are installed; finally a bundled fallback font family under the SIL Open Font License (for example a Noto Sans subset covering Latin/Greek/Cyrillic plus Noto Sans Symbols). Ship the font license file. Note every substitution (font_substituted).
14.2 Spreadsheets (oc-sheet), Phase 3
Format	R	W	Scope
CSV / TSV / PSV	✅	✅	RFC 4180 + dialect detection (delimiter, quote char, escape style, header presence), encodings via oc-text, BOM option for Excel, ragged rows, huge files streamed (no full load).
JSON	✅	✅	Array of objects / array of arrays / NDJSON (JSON Lines); nested values flattened with dotted keys (option) or rejected. PARTIAL (serde_json syntax).
XLSX	✅	✅	Shared strings, inline strings, number formats (dates via 1900/1904 systems), multiple sheets, merged cells (read), formulas (keep formula text + cached value; no recalculation engine in 2.x, noted), styles (basic: bold, fills, number formats, column widths) on write. Validate output against OOXML schemas and LibreOffice round-trip in CI.
ODS	✅	✅	Same scope via ODF.
XLS (BIFF8)	✅	❌	Phase 6 stretch via MS-CFB.
Table → Document/PDF	—	✅	Render a sheet as a paginated table via the layout engine.
SQLite	✅	✅	Phase 6 stretch: own read-only B-tree reader for table export to CSV/XLSX; writer creates a simple table. (Own implementation of the documented file format.)
14.3 PDF (oc-pdf), Phase 6

Parsing: ISO 32000-1 and 32000-2: incremental updates, cross-reference tables and cross-reference streams, object streams, hybrid files, linearized files, broken-file recovery (rebuild xref by scanning for obj markers when the xref is damaged). Filters: Flate (with predictors), LZW (EarlyChange), ASCII85, ASCIIHex, RunLength, CCITTFax, DCT (our JPEG), JPX (our JPEG 2000, Phase 6), JBIG2 (own decoder: generic region, refinement, text region, symbol dictionary, halftone; Phase 6), Crypt. Security handlers: Standard R2–R6 (RC4 40/128, AES-128, AES-256) decryption with empty user password automatically, or a password supplied by the user (RustCrypto, PARTIAL); certificate (public-key) security → unsupported_variant. Never circumvent restrictions beyond what the user's password authorises; owner-password-only permission flags are reported, and conversions that the permissions forbid are refused with a clear message unless I record a different decision in docs/DECISIONS.md.

Text extraction: content stream interpreter (text state, TJ/Tj spacing), font encodings (Standard, WinAnsi, MacRoman, PDFDoc, Differences, glyph-name → Unicode via the Adobe Glyph List, ToUnicode CMaps, CID fonts with Identity-H/V and predefined CMaps), reading-order reconstruction (column detection, line grouping, de-hyphenation option), output to TXT, Markdown (headings by font size heuristic, lists, tables by alignment heuristic; noted as heuristic) and HTML. Also: metadata (Info dict + XMP), outlines/bookmarks, attachments listing/extraction, form-field values.

Rendering (PDF → PNG/JPEG/WebP/TIFF/SVG): full graphics state, paths, clipping, all colour spaces (DeviceGray/RGB/CMYK, CalGray/CalRGB, Lab, ICCBased via our CMM, Indexed, Separation/DeviceN with tint transforms, Pattern), functions (types 0, 2, 3, 4 with own PostScript calculator), shadings (types 1–7 incl. Coons/tensor patches), images incl. masks/soft masks/decode arrays/interpolation, transparency groups, blend modes, soft masks, Type 3 fonts, TrueType/Type1/CFF/OpenType fonts via oc-font, annotations appearance streams (option), optional content groups (default visibility). Anti-aliased rendering at any DPI with tiled rendering for huge pages. Visual tests against pdftoppm (Poppler) and MuPDF output as oracles in CI with SSIM thresholds (§22.5).

Writing: images → PDF (lossless: JPEG passthrough without re-encoding, PNG → Flate with predictors; page size options: fit image, A4, Letter, margins), Document/Page IR → PDF (vector text with font subsetting and embedding, ToUnicode CMaps so text stays searchable and copyable, hyperlinks, outlines from headings, metadata, PDF/A-2b compliance mode with embedded sRGB output intent and XMP, validated in CI with veraPDF as an oracle), PDF → PDF operations: merge, split, extract pages, rotate, reorder, compress (recompress streams, downsample images with a note), linearize, remove metadata, decrypt with password (when the user owns it), encrypt (AES-256, RustCrypto).

14.4 SVG (oc-svg), Phase 6

SVG 1.1 + common SVG 2 features: shapes, paths (all commands, arcs), transforms, use/symbol/defs, gradients (linear/radial with spread methods), patterns, clipping and masking, opacity, stroke properties (dashes, joins, caps, miter limits), text (via oc-font, basic layout + textPath stretch), CSS styling (presentation attributes, style, <style> with a selector subset: type/class/id/descendant/child/attribute), embedded raster images (data URIs only; external references are never fetched, noted), filters subset (feGaussianBlur, feOffset, feColorMatrix, feBlend, feComposite, feFlood, feMerge, feDropShadow). Scripts and animations are ignored (noted). Validate against the resvg test suite images (fetched in CI as an oracle; its license permits use as test data, verify and record) with SSIM thresholds.

14.5 Raster engine (oc-raster)

Scanline polygon rasterizer with exact-area anti-aliasing (sparse coverage accumulation), non-zero and even-odd fill rules, Bézier flattening with error tolerance, strokes (joins: miter/round/bevel; caps: butt/round/square; dashes), clipping by path (mask stack), paints (solid, linear/radial/sweep gradients, image patterns with transforms and filtering), compositing in premultiplied linear or gamma space (configurable for PDF/SVG compatibility), Porter-Duff operators and separable/non-separable blend modes, 8-bit and float pipelines. SIMD-accelerated spans with scalar fallback.

14.6 Fonts (oc-font), Phase 6

Parse TrueType (glyf, composite glyphs), CFF/CFF2, Type 1 (PFA/PFB, eexec decryption), OpenType tables (cmap all common formats, hmtx/vmtx, kern, GSUB, GPOS, GDEF, OS/2, name, post, fvar/gvar variable fonts for default instance + named instances), WOFF and WOFF2 (Brotli + transforms). Glyph outline extraction → oc-raster paths (no hinting; grayscale anti-aliasing). Subsetting for PDF embedding (TrueType and CFF, with GID remapping). Font conversion routes: TTF ↔ OTF(CFF→ TrueType quadratic conversion with tolerance) ↔ WOFF ↔ WOFF2(read; write after Brotli encoder).

15. Archives (oc-archive), Phase 3
Format	R	W	Scope
ZIP (PKWARE APPNOTE 6.3.x)	✅	✅	Stored, Deflate, Deflate64 (read), bzip2, LZMA, Zstandard (method 93), XZ (method 95); ZIP64; data descriptors; Unicode paths (UTF-8 flag + Info-ZIP Unicode Path extra field + CP437 fallback via oc-text); extended timestamps; Unix permissions; comments; WinZip AES (AE-1/AE-2) decrypt and encrypt (RustCrypto, PARTIAL); traditional ZipCrypto read only (insecure; writing refused with an explanation); split/spanned archives (read). Central-directory-first reading with local-header cross-validation.
TAR	✅	✅	ustar, POSIX pax (long names, UTF-8, large sizes, high-precision mtime, xattrs), GNU (long names/links, sparse files read), V7 read.
Compressed tar	✅	✅	.tar.gz/.tgz, .tar.bz2, .tar.xz, .tar.zst, .tar.lz4, .tar.lzma.
Single-file compressors	✅	✅	.gz, .bz2, .xz, .lzma, .zst, .lz4, .Z (Unix compress, read).
7z	✅	✅	Headers (incl. encoded/compressed headers), LZMA, LZMA2, PPMd (H variant, read), BZip2, Deflate, Copy, BCJ/BCJ2/ARM/ARM64 filters, Delta, solid blocks, empty files/dirs, attributes, timestamps; AES-256 + SHA-256 key derivation decrypt/encrypt (RustCrypto, PARTIAL), header encryption.
CPIO	✅	✅	newc, odc, bin (read).
AR	✅	✅	BSD and GNU variants (also lets users inspect .deb packages).
ISO 9660	✅	❌	Plus Joliet and Rock Ridge (read/extract).
CAB	✅	❌	MSZIP, LZX (read), Quantum rejected.
RAR	❌	❌	Out of scope (§7.4) unless I decide otherwise.

Archive conversions: any readable archive → any writable archive, streaming entries without extracting to disk, preserving paths, timestamps, permissions and symlinks where the target supports them (notes otherwise). Also: extract to folder, create from folder (with include/exclude globs), list, test integrity, recompress only (keep layout).

Archive safety (mandatory): reject or sanitise absolute paths, drive letters, .. components, UNC paths, NUL bytes, Windows reserved names and trailing dots/spaces (zip-slip impossible); symlinks and hardlinks are never followed outside the extraction root and are extracted as links only if the user opts in; duplicate names handled deterministically; bomb defence with total-size, per-entry, entry-count and compression-ratio limits (configurable; defaults documented); nested archives are not auto-expanded.

PART IV: ENGINE RUNTIME, SECURITY AND PERFORMANCE
16. Engine runtime and security
16.1 Process isolation (mandatory)
Every conversion runs in a separate oc-worker process launched by oc-engine. The worker receives a job description over a pipe (length-prefixed, versioned binary or JSON protocol), reads the input through an inherited read-only handle, writes the output to a temp file the engine created, and streams progress/notes/errors back.
A worker crash, panic, hang or memory blow-up therefore never affects the app. The engine reports worker_crashed or timeout with the stage it was in and keeps going.
Resource limits per worker:
Windows: a Job Object with a memory cap, CPU-time cap, kill-on-close, and UI-restriction flags; the worker starts with a restricted token where available (document what was achieved).
Linux: setrlimit (address space, CPU time, file size, open files), prctl(PR_SET_NO_NEW_PRIVS), Landlock (read only the input, write only the temp output directory) when the kernel supports it, seccomp filter denying network sockets (stretch). Fall back gracefully on older kernels and note it in diagnostics.
macOS: rlimits; sandbox profile stretch.
Network is unavailable to workers by design. No worker code path opens sockets; a test greps the worker binary's dependency tree and a runtime test attempts no sockets.
Default limits (configurable in Settings, documented): memory 2 GB per worker (or 50% of RAM, whichever is lower), wall-clock timeout scaled by input size (e.g. 60 s + 2 s per MB, max 2 h for video), max 64 MP per image (hard max 1 GP via tiled path), max side 65 535 px, max pages 10 000, max archive expansion 20× input and 50 GB total, max entries 1 M, max XML depth 256, max text line 64 MB.
Concurrency: number of parallel workers = physical cores (min 1, max 8 by default; configurable). Workers themselves use rayon with a thread count passed by the engine so the machine is never oversubscribed. Video jobs default to one at a time with internal parallelism.
16.2 File handling
Inputs are never modified. Outputs are written to <output>.partial in the destination folder, fsynced, then atomically renamed; a cancelled/failed job leaves nothing behind.
Output naming template (default {stem}.{ext}; tokens {stem} {ext} {date} {time} {n} {width} {height} {format} {preset}), conflict policy: ask (GUI) / --on-conflict skip|overwrite|rename (CLI; default rename to name (1).ext).
Paths are handled as OS-native (OsString/PathBuf), never lossy-converted. Long paths on Windows (\\?\ prefix when needed). Filenames written by us are sanitised for the target OS (reserved names, illegal characters, trailing dots/spaces, length limits).
Optional: preserve source modification time and permissions on outputs.
16.3 Logging and diagnostics
Rotating log files in the data directory (tracing-appender, 5 × 2 MB), no file contents, paths reduced to file names unless debug logging is enabled.
"Copy diagnostics" button and openconverter doctor command: version, build features, OS, CPU features detected, sandbox capabilities achieved, WebView version, limits. Never includes user files.
Crash handling: panics in the main process are caught at the top level, written to a local crash log, and the user is told where it is. Nothing is ever uploaded.
16.4 Data locations
Windows: %LOCALAPPDATA%\OpenConverter\ (settings, logs, cache, presets, history). Linux: XDG ($XDG_CONFIG_HOME, $XDG_DATA_HOME, $XDG_CACHE_HOME, $XDG_STATE_HOME for logs). macOS: ~/Library/Application Support/OpenConverter.
Portable mode: if a file portable.flag sits next to the executable (the portable .zip ships it), all data lives in .\data\ beside the executable.
Settings and history writes are atomic; a corrupt file falls back to defaults and is backed up, never crashes.
16.5 Threat model

Write docs/SECURITY.md with: assets (user files, privacy), attacker capabilities (crafted input files; malicious web pages trying to reach serve mode; malicious archives), mitigations mapped to sections of this prompt, and the vulnerability reporting process (GitHub private security advisories). Also SECURITY.md at the repo root.

17. Performance
17.1 Techniques (apply in this order)
Correct algorithms and data structures (table-driven Huffman/ANS decoding, hash chains, suffix arrays, fast DCT/IDCT, separable filters).
Avoid allocations in hot loops; reuse buffers; process in cache-friendly tiles/rows.
Branch-light bit readers (refill 64 bits at a time), lookup tables.
Parallelism: rayon over rows, tiles, MCU rows (restart intervals), bzip2/xz/zstd blocks, video tiles/slices/frames where the format allows, archive entries, PDF pages.
SIMD in oc-simd: SSE2/SSSE3/SSE4.1/AVX2 (and AVX-512 where clearly beneficial) on x86_64, NEON on aarch64, selected at runtime with is_x86_feature_detected! / std::arch::is_aarch64_feature_detected!. Every SIMD kernel has a scalar reference and a property test proving identical output (or documented tolerance for float kernels). Miri runs on the scalar paths; the SIMD paths are tested on CI runners that support them.
Release profile: opt-level = 3, lto = "fat", codegen-units = 1, panic = "unwind" (Cargo cannot set panic per binary, and the worker catches panics at its top level with catch_unwind and reports worker_crashed with the message), debug symbols split into separate files kept as release artifacts for local symbolication.
17.2 Budgets (measure with criterion on CI runners and my machine; record both in docs/PERFORMANCE.md; within 2× of the reference implementation is the goal, never slower than 4× without an ADR)
Operation (single thread unless noted)	Target
Inflate	≥ 300 MB/s output
Deflate level 6	≥ 40 MB/s input; ratio within 2% of zlib level 6
Zstd decode / encode level 3	≥ 800 MB/s / ≥ 150 MB/s
XZ decode / encode preset 6 (multi-thread encode)	≥ 60 MB/s / ≥ 5 MB/s per thread
JPEG decode 1920×1080 baseline 4:2:0	≤ 15 ms
JPEG encode 1920×1080 q85	≤ 25 ms
PNG decode / encode (default level) 1920×1080 RGB	≤ 20 ms / ≤ 150 ms
WebP lossless encode 1920×1080 default	≤ 1.5 s
FLAC encode level 5	≥ 200× realtime
MP3 encode 128 kbps / Opus encode 128 kbps	≥ 50× / ≥ 30× realtime
VP8 encode 1080p "good" speed (8 threads)	≥ 20 fps
AV1 decode 1080p (8 threads)	≥ 30 fps
AV1 encode 1080p fastest preset (8 threads)	≥ 5 fps (quality vs. speed documented)
PDF render A4 page at 150 DPI, typical text page	≤ 80 ms
DOCX (50 pages, text + images) → PDF	≤ 2 s
Remux 1 GB MP4 → MKV	I/O bound: ≥ 300 MB/s on SSD
17.3 Benchmarks

cargo xtask bench runs the criterion suite and writes a markdown table. CI runs a reduced benchmark set on every PR and fails on a regression > 15% against the stored baseline on the same runner type (store baselines as CI artifacts; tolerate runner noise with repeated runs).

PART V: USER INTERFACE
18. Desktop app and shared UI
18.1 Shell
OpenConverter Desktop is a Tauri v2 app (crates/oc-desktop). The frontend is the shared ui/ folder: plain HTML, CSS and ES modules, no build step, no npm, no frameworks, no external assets. Configure frontendDist to ../../ui and app.withGlobalTauri = true so the UI uses window.__TAURI__ without bundling.
The UI talks to the engine through a single adapter module ui/js/backend.js with two implementations: Tauri IPC (commands + events) in the desktop app, and HTTP + SSE in openconverter serve (§20). Everything above the adapter is identical.
Tauri security configuration: strict CSP in tauri.conf.json (default-src 'self'; img-src 'self' asset: data: blob:; media-src 'self' asset: blob:; style-src 'self'; script-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; frame-ancestors 'none'); verify the exact CSP string against the current Tauri v2 docs and test it. Minimal capabilities (only the commands we define plus dialog open/save and "reveal in folder"); no filesystem plugin scope beyond what the user picked; no remote domains in capabilities; navigation to any external URL blocked (external links open in the system browser only after a confirm dialog, via the opener plugin).
Single-instance: launching OpenConverter with files while it is already running adds those files to the queue of the running window (official single-instance plugin).
No inline scripts or styles anywhere (the CSP forbids them). Dynamic styling only through the CSSOM or classes/custom properties. Never use innerHTML with any data derived from files or users (filenames, metadata, document text); use textContent and DOM APIs. A test scans ui/ for violations.
18.2 Layout
Header: logo (inline SVG), app name, version, build badge (e.g. "Standard build" / "Includes encumbered codecs"), "Offline" badge with a tooltip explaining the privacy promise, theme toggle, settings, help.
Left sidebar: categories (All, Images, Audio, Video, Documents, PDF, Spreadsheets, Archives, Subtitles, Fonts), Presets, History, Settings.
Main area:
A large drop zone (whole window highlights while dragging) with "Add files", "Add folder" (recursive, with include/exclude filter) and keyboard activation.
Queue as a virtualised list (handles 10 000+ items smoothly). Each item: thumbnail/icon, name, detected format (by content; warning when the extension disagrees), size, key properties (dimensions, duration, pages, entries, codec list), target format picker (only valid targets from the route graph), preset picker, collapsible options panel generated from the options schema, ownership badge, route plan ("Remux, no re-encoding"), estimated time (from measured throughput history on this machine only, never invented), state, real progress bar with stage text and ETA, Cancel/Retry/Remove/Reveal output.
Inspector panel for the selected item: full metadata (EXIF/ID3/MP4/PDF info), streams list for media (with per-stream keep/drop/re-encode controls), pages for PDFs (thumbnails, page range selection), archive tree (select entries), document outline.
Compare view for images and PDF pages: side-by-side and swipe slider, zoom/pan, pixel peeper at 100–800%, difference heatmap, PSNR/SSIM shown.
Preview players: audio (waveform rendered from our decoder, play via the WebView's own <audio> element only for formats the WebView supports; otherwise play a short preview we transcode to WAV in a temp file), video (thumbnail strip, preview via WebView <video> only when natively supported, otherwise frame scrubbing from our decoder).
Batch bar: Set all targets, Apply preset to all, Output folder (same as source / chosen folder / subfolder template), Naming template, Conflict policy, Convert all, Pause queue, Cancel all, Clear finished, Reveal outputs.
Bottom status bar: active workers, CPU threads in use, queue totals, combined progress.
History: searchable, filterable table (date, source → target, sizes, size change %, duration, status, notes); reveal output if it still exists; re-run with same settings; delete entries; clear.
Settings dialog: theme; language; default output location and naming; conflict policy; concurrency (workers, threads); per-format defaults (JPEG quality, PNG level, WebP quality/lossless, AVIF/AV1 speed, MP3/Opus/Vorbis bitrates, FLAC level, PDF DPI, archive method/level); metadata policy (keep/strip GPS/strip all); colour policy (preserve profile/convert to sRGB); alpha background colour; resource limits; notifications on completion; preserve timestamps; history retention; debug logging toggle; "Open data folder"; "Copy diagnostics".
Presets: built-in (e.g. "Web image (WebP q80, max 2048 px, strip GPS)", "Email-friendly JPEG", "Lossless archive (PNG level 9 / FLAC 8)", "Podcast (Opus 64 kbps mono, loudness −16 LUFS)", "Share video (WebM VP9 1080p)", "Print PDF (PDF/A-2b)"), plus user presets (create, edit, duplicate, import/export as JSON).
18.3 Behaviour
Everything is driven by the engine's format/route graph and option schemas.
Errors appear inline per item with a friendly message keyed on the error code plus an expandable technical detail; one failing item never affects others.
Jobs survive UI reloads (desktop: the engine lives in the Rust side; serve: server-side job state).
Long operations show real ETA; huge inputs show a warning before starting when the estimate exceeds 10 minutes.
Local OS notification on queue completion when the window is unfocused (optional, setting).
Keyboard shortcuts: Ctrl+O add files, Ctrl+Shift+O add folder, Ctrl+Enter convert all, Delete remove selected, Ctrl+, settings, F1 help, Esc cancel dialogs; a shortcuts help sheet.
18.4 Design system
CSS custom properties for colour, spacing, radius, typography; light, dark and high-contrast themes; follows prefers-color-scheme, prefers-reduced-motion, prefers-contrast and Windows forced-colors mode.
System font stack. Icon set: our own inline SVGs in ui/icons/ (consistent 24 px grid, 1.5 px strokes).
Feels like a native desktop utility: dense but calm layout, no marketing-site styling, smooth 60 fps interactions, no layout shift when progress updates.
Responsive from 800×600 up to 4K; usable on narrower widths; DPI-aware.
18.5 Accessibility and internationalisation
WCAG 2.2 AA: labels on all controls, focus visible, logical tab order, roving tabindex in lists, ARIA live region for job completion/failure, role="progressbar" with values, dialogs trap and return focus, contrast checked in all themes. Test with axe-core run in CI against the serve UI (axe used as a dev-time oracle only; never shipped).
i18n: all strings in ui/i18n/<lang>.json (English complete; framework ready for others), ICU-style plurals, locale-aware number/date/size formatting via Intl, right-to-left layout support (dir="rtl") tested with a pseudo-locale.
18.6 OS integration
Windows installer (optional checkboxes): Start menu and desktop shortcuts; "Convert with OpenConverter" in the Explorer context menu for supported file types (classic shell verb under HKCU\Software\Classes, appears under "Show more options" on Windows 11; a Windows 11 modern context-menu entry is a Phase 8 stretch requiring a sparse package and is documented separately); registered as an "Open with" handler (never steals default associations); CLI added to the user PATH (optional).
Linux: .desktop file with MimeType= list generated from the format registry, AppStream metainfo.xml, hicolor icons (16–512 + scalable SVG), "Open with" integration in GNOME/KDE file managers, man pages for the CLI (generated from clap), shell completions (bash, zsh, fish) installed to standard locations.
macOS: .app bundle with document types from the registry, CLI symlink instructions.
PART VI: CLI, HEADLESS SERVER AND PYTHON BINDINGS
19. CLI (openconverter)
openconverter convert <INPUT>... [-o <OUTPUT|DIR>] [--to <FORMAT>] [--preset <NAME>]
                      [--opt key=value]... [--quality N] [--level N] [--resize WxH|N%|fit:WxH]
                      [--pages 1-3,7] [--streams v:0,a:1] [--copy|--reencode] [--password-file <FILE>]
                      [--metadata keep|strip-gps|strip] [--on-conflict skip|overwrite|rename]
                      [--name "{stem}-web.{ext}"] [--recursive] [--include GLOB] [--exclude GLOB]
                      [--jobs N] [--threads N] [--explain] [--dry-run] [--json] [--quiet|-v|-vv]
openconverter info <FILE> [--json]            # detected format, properties, streams, metadata
openconverter formats [--category image] [--json] [--graph dot|json]
openconverter routes <FROM> <TO> [--json]     # explain the planned route(s)
openconverter extract <ARCHIVE> -o <DIR> [--include GLOB] [--allow-symlinks]
openconverter create <ARCHIVE> <PATH>... [--method zstd --level 19]
openconverter pdf merge|split|rotate|extract|compress|decrypt|encrypt ...
openconverter test <FILE>...                  # integrity check (archives, PNG CRCs, FLAC MD5, ...)
openconverter presets list|show|import|export
openconverter serve [--host 127.0.0.1] [--port 8080] [--no-browser]
openconverter doctor [--json]
openconverter completions bash|zsh|fish|powershell
openconverter --version
Output format inferred from the output extension, --to, or a preset; input format always detected by content.
--json emits machine-readable results (one JSON object per input, NDJSON for batches) including notes and timings; stable schema documented in docs/CLI.md and versioned.
Progress: a single updating line per job on a TTY (stderr), nothing when piped unless -v.
stdin/stdout support (- as input/output) for streaming formats.
Passwords are never accepted as plain command-line arguments (visible in process lists): use --password-file, the OPENCONVERTER_PASSWORD environment variable, or an interactive prompt.
Exit codes: 0 ok, 1 one or more conversions failed, 2 usage error, 3 unsupported input, 4 limits exceeded, 5 codec not in build, 130 interrupted. Documented.
The CLI uses the same engine, sandboxed workers, limits and notes as the desktop app.
20. Headless server (openconverter serve)

For servers, NAS boxes and people who prefer a browser. It serves the same ui/ through HTTP and keeps every protection from 1.x:

Bind 127.0.0.1 by default; any other --host prints a prominent warning and requires --allow-remote as well; remote mode requires a randomly generated access token printed at startup (sent as a cookie after a one-time ?token= login, HttpOnly, SameSite=Strict).
DNS-rebinding defence (Host allow-list), CSRF defence (Origin/Referer match + X-OpenConverter: 1 on non-GET), security headers (CSP identical in spirit to §18.1 with connect-src 'self', X-Content-Type-Options, Referrer-Policy: no-referrer, Cross-Origin-Resource-Policy: same-origin, Cross-Origin-Opener-Policy: same-origin, Cache-Control: no-store for API).
Uploads streamed to disk with size limits enforced while streaming; downloads with sanitised RFC 6266/5987 filenames; static serving cannot escape ui/; UUID-validated IDs; request size and time limits; SSE for progress with polling fallback; cleanup of uploads/outputs after a retention period.
API documented in docs/API.md (OpenAPI 3.1 file docs/openapi.json generated from the route definitions and checked in CI to match).
21. Python bindings (openconverter on PyPI), Phase 7
crates/oc-python with PyO3, built by maturin as abi3 wheels (CPython 3.9+) for Windows x64/ARM64, manylinux 2_28 x86_64/aarch64, musllinux, macOS universal2.
API: openconverter.convert(src, dst, **options) -> Result, openconverter.info(path), openconverter.formats(), streaming openconverter.open_image(...) returning a buffer-protocol object (no numpy dependency; works with numpy if the user has it). Releases the GIL during conversions.
Type stubs (.pyi), docs, tests with unittest or pytest (dev only).
Published to PyPI only after I confirm (trusted publishing via GitHub Actions OIDC; ask me to set it up on PyPI).
PART VII: QUALITY, PACKAGING, CI/CD AND DOCUMENTATION
22. Testing and quality assurance
22.1 Test layers (all run by cargo test --workspace unless marked CI-only)
Unit tests inside each crate for every non-trivial function (bit readers, Huffman builders, DCTs, filters, parsers).
Property tests (proptest): round-trip identities for every lossless codec and container (decode(encode(x)) == x) over generated inputs including edge sizes (0, 1, odd, prime, max-limit), all modes and depths; idempotence of normalisation; SIMD vs scalar equivalence.
Hand-crafted fixtures built byte-by-byte in tests for edge cases (minimal valid files, every header variant, every filter/predictor, restart markers, ZIP64, pax headers, CRLF/LF/BOM text, RFC 4180 corner cases).
Generated fixtures from cargo xtask fixtures (deterministic seeded generators: gradients, checkerboards, noise "photos", alpha ramps, tones/chirps/noise for audio, moving test patterns for video, synthetic documents). Committed fixtures stay < 10 MB total; larger ones are generated at test time into a temp dir.
Conformance corpora (§22.4): official and widely used test suites.
Differential tests vs reference tools (CI-only oracles, §22.5).
Acceptance matrix (§22.3).
Fuzzing (§22.6).
UI tests (§22.8) and install smoke tests (§23.7).
Benchmarks with regression gates (§17.3).
22.2 Correctness bars per codec type
Type	Bar
Lossless image/audio/archive/compression codecs	Bit-exact round-trip; our decoder bit-exact with reference decoders on the corpus; our encoder's output accepted bit-exact by reference decoders.
Video decoders with normative output (VP8, VP9, AV1, H.264, HEVC, Theora)	Bit-exact frame hashes against the official conformance vectors' expected MD5s.
JPEG decoder	Lossless/baseline-equivalent paths: max absolute error ≤ 1 per sample vs libjpeg-turbo with the same upsampling and an accurate IDCT; progressive/arithmetic identical criteria.
Audio decoders with tolerance-defined conformance (MP3, AAC)	Pass the ISO compliance criteria (RMS and max-error limits) on the compliance streams where available; otherwise ≥ 90 dB SNR vs the reference decoder output.
Opus / Vorbis decoders	Opus: pass opus_compare on the RFC 8251 test vectors. Vorbis: ≥ 90 dB SNR vs reference on the corpus.
Lossy encoders (JPEG, WebP, MP3, Vorbis, Opus, VP8, VP9, AV1)	Output decodes with the reference decoder; quality thresholds at fixed settings (e.g. JPEG q90 PSNR ≥ 38 dB on the photo set, q75 ≥ 33 dB; WebP q80 SSIM ≥ 0.95; Opus 64 kbps and MP3 128 kbps pass a minimum ViSQOL-style or PEAQ-style objective score when such a tool is available in CI, otherwise SNR/spectral distance thresholds documented); rate-distortion curves recorded in docs/PERFORMANCE.md against reference encoders.
Renderers (PDF, SVG, document layout)	SSIM ≥ 0.98 at 150 DPI vs Poppler/MuPDF (PDF) and resvg reference images (SVG) on the corpus, with per-file documented exceptions; text extraction compared to pdftotext by normalised word-level F1 ≥ 0.97.
Document writers (DOCX, ODT, XLSX, ODS, EPUB, PDF)	Validate against official schemas (OOXML XSD, ODF RelaxNG), EPUBCheck, veraPDF (PDF/A), qpdf --check; open and re-save via LibreOffice headless in CI as an oracle with content comparison.
Markdown / HTML parsers	CommonMark spec tests 100%; GFM extension tests 100%; html5lib tokenizer/tree tests ≥ 99% with the remainder listed.
Text encodings / Unicode	WHATWG encoding test data; Unicode NormalizationTest.txt, LineBreakTest.txt, BidiTest.txt/BidiCharacterTest.txt 100% for implemented algorithms.
22.3 Acceptance matrix

An auto-generated test enumerates the route graph: for every readable format with a fixture and every writable target reachable from it, run the conversion through the real engine (including the worker process), then validate the output by (a) our decoder and (b) in CI, the reference tool. Assert dimensions, durations, stream counts, page counts, entry lists, and the correctness bar for that type. Adding a format extends coverage automatically. Results are written to docs/COMPATIBILITY.md (generated, committed at each release).

22.4 Corpora
corpora/manifest.toml lists each corpus: name, URL, version or commit, SHA-256, license, and which tests use it. Verify each license allows use as test data before adding it, and record the result.
Candidates to evaluate (verify availability and license yourself): PngSuite; libjpeg-turbo and imagetestsuite JPEGs; libwebp test data; TIFF and BMP suites (e.g. bmpsuite); WebM/VP8/VP9 test vectors; AOM AV1 conformance streams (Argon where license allows); Xiph test media; IETF CELLAR FLAC test files; Opus test vectors (RFC 8251); MP3 compliance streams; Matroska test files; ISO-BMFF conformance files; CommonMark spec; html5lib-tests; Unicode UCD test files; WHATWG encoding tests; pdf.js / qpdf / Poppler test PDFs; resvg test suite; OOXML/ODF samples; zstd, xz, bzip2 test files; 7-Zip and ZIP edge-case archives; EPUB samples.
cargo xtask corpora fetch downloads into a local cache (outside the repo), verifies hashes, and is used by CI with caching. Corpora are never committed and never shipped. Tests that need a corpus fail with a clear "run cargo xtask corpora fetch" message when it is absent locally, and are required in CI (no silent skipping).
22.5 Reference tools (CI-only oracles, never shipped, never called at runtime)

Installed in CI from distro repositories or official releases: libjpeg-turbo tools, pngcheck, optipng/oxipng (for comparison), libwebp tools, ffmpeg/ffprobe, flac, opus-tools, vorbis-tools, lame, mkvtoolnix, MP4Box, dav1d, aomdec, vpxdec, zstd, xz, bzip2, 7z, unzip/zipinfo, bsdtar, poppler-utils, qpdf, mutool, veraPDF, epubcheck, libreoffice --headless, resvg. A small harness in xtask wraps each tool and normalises its output for comparison. Record tool versions in CI logs.

22.6 Fuzzing
One cargo-fuzz target per decoder, demuxer, parser and archive reader (and structure-aware targets with arbitrary for encoders: random valid IR → encode → decode → compare).
PR CI: 60 s per target for crates changed in the PR. Nightly workflow: 30–60 min per target in rotation, corpus persisted as a cache/artifact.
Every crash/hang/OOM found is minimised, fixed at the root cause, and added to tests/regressions/<crate>/ as a permanent test.
Phase 8: apply to OSS-Fuzz (ask me first).
22.7 Panic-freedom and arithmetic safety in decoders
Decoder and parser modules enable #![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing, clippy::unreachable)]. Use get(), checked arithmetic (checked_mul, checked_add, try_from) for every size, offset and count derived from file data, and return corrupt_file/limit_exceeded.
All allocations sized from file headers go through oc-core::limits::alloc_checked which enforces the active limits before allocating.
Fuzz harnesses treat any panic as a failure even though production workers would contain it.
22.8 UI tests
Unit tests for UI modules run in CI with a small dev-only harness (Node + a DOM test runner is acceptable in CI only, under tests/e2e/ with a lockfile; never required to build or run the product).
End-to-end: Playwright tests (CI-only dev tooling) against openconverter serve, covering every flow in §26 Phase 2 DoD plus category-specific flows as phases add them; axe-core accessibility scan with zero serious/critical violations; network log asserts only 127.0.0.1 requests.
Desktop app: CI smoke test with tauri-driver (WebDriver) on Windows and Linux (under xvfb-run): launch, add a file via the IPC test hook available only in debug/test builds, convert, assert output exists, quit. The test hook is compiled out of release builds (cfg(feature = "test-hooks")).
Locally, use the Playwright MCP for exploratory verification of the serve UI before each release and save screenshots (§25).
22.9 Coverage

cargo llvm-cov in CI; format crates ≥ 85% line coverage, engine ≥ 80%; report uploaded as an artifact (no third-party coverage service unless I add one).

23. Packaging and distribution
23.1 Build matrix and glibc baseline
Linux GUI binaries link against the system WebKitGTK, so build them on the oldest distro of each family you support: .deb and AppImage in an Ubuntu 22.04 container, .rpm in the oldest currently-supported Fedora container. RHEL/Alma/Rocky desktop support is Tier 2 and only if WebKitGTK 4.1 is available there; otherwise those systems use the CLI package or Flatpak (document the outcome).
The CLI is also built as a static musl binary (x86_64, aarch64) that runs on any Linux.
Windows builds use the MSVC toolchain. ARM64 Windows via the aarch64-pc-windows-msvc target.
23.2 Windows artifacts
NSIS setup .exe (default download): per-user install without admin rights, Start menu shortcut, optional desktop shortcut, optional Explorer context-menu verb, optional "add CLI to PATH", "Open with" registration, clean uninstaller (removes registry entries and shortcuts; user data kept unless the user ticks "remove my settings").
Offline setup .exe variant with embedded WebView2 offline installer (§7.3).
MSI (WiX via the Tauri bundler) for per-machine/enterprise installs.
Portable .zip: OpenConverter.exe (GUI), openconverter.exe (CLI), oc-worker.exe, portable.flag, licenses. Double-click OpenConverter.exe runs with no installation.
Version info resources, app manifest (per-monitor DPI v2, long-path aware, UTF-8 active code page), high-quality .ico (16–256) generated by our own ICO encoder in xtask.
Optional winget manifest in packaging/winget/ (submission only after I confirm).
23.3 Debian/Ubuntu .deb
Package openconverter (desktop, depends on the WebKitGTK 4.1 and GTK 3 runtime packages computed by the bundler, reviewed by you) and openconverter-cli (no GUI dependencies; Conflicts/Replaces handled so both can coexist, desktop depends on cli or contains it; choose and document).
Contents: binaries in /usr/bin, worker in /usr/lib/openconverter/, .desktop, AppStream metainfo, icons, MIME list, man pages, shell completions, /usr/share/doc/openconverter/copyright in DEP-5 format including third-party licenses, Debian-format changelog.
lintian clean (no errors; warnings justified in packaging/linux/deb/lintian-overrides with reasons).
Install-tested in containers: Debian 12, Debian 13 (if released), Ubuntu 22.04, Ubuntu 24.04 and the newest Ubuntu release, for both packages.
23.4 Fedora/RHEL .rpm
Same split (openconverter, openconverter-cli), proper License: tag (SPDX expression including bundled crates' licenses as generated), %files with doc/license macros, desktop-file validation, AppStream validation.
Built without the encumbered feature, always.
rpmlint clean (justified exceptions only); install-tested with dnf install ./*.rpm in containers for every currently supported Fedora release, and Alma/Rocky 9 for the CLI package.
Provide a spec file in packaging/linux/rpm/openconverter.spec that builds from the vendored source tarball offline (for COPR or eventual Fedora packaging), plus instructions for a COPR repository (set up only after I confirm).
23.5 Signing and integrity
Windows Authenticode signing when signing secrets are configured (WINDOWS_CERTIFICATE, WINDOWS_CERTIFICATE_PASSWORD, or Azure Trusted Signing variables). Without them, ship unsigned and explain SmartScreen's "More info → Run anyway" in the README. Never commit certificates.
SHA256SUMS for every release, GPG-signed when GPG_PRIVATE_KEY is configured.
GitHub artifact attestations (build provenance) for all release artifacts.
CycloneDX SBOM (cargo-cyclonedx) attached to every release.
macOS signing/notarization only when Apple secrets exist; otherwise unsigned with Gatekeeper instructions.
23.6 Other Linux channels
AppImage (x86_64; aarch64 if the toolchain allows) with desktop integration metadata.
Flatpak: manifest packaging/linux/flatpak/io.github.yusuf6411.OpenConverter.yml on the current GNOME runtime, offline cargo sources generated by flatpak-cargo-generator, finish-args with no network permission and file access via portals (plus optional --filesystem=home documented). Build and test it in CI. Flathub submission only after I confirm.
AUR: PKGBUILD for source builds and -bin variant.
Optional (Phase 8): signed APT and DNF repositories hosted on GitHub Pages.
23.7 Install smoke tests (CI, every release candidate)
Windows: silent NSIS install (/S) on a clean runner, run openconverter --version, convert a fixture via CLI, launch the GUI for 10 s and confirm it stays alive with no errors in its log, uninstall silently, confirm removal. Repeat for MSI and portable zip.
Linux: for each distro container in §23.3/23.4: install, openconverter --version, run conversions across categories, launch GUI under xvfb-run for 10 s (desktop package), confirm the .desktop file validates, uninstall.
AppImage and Flatpak: launch and convert.
23.8 Reproducibility and source releases

SOURCE_DATE_EPOCH, --remap-path-prefix, locked dependencies, deterministic archive ordering in our packaging steps. Each release also publishes openconverter-<version>-vendored.tar.xz (source + cargo vendor output) created with our own tar/xz implementation once Phase 3 is done (before that, with standard tools in CI).

24. CI/CD
ci.yml (push/PR): cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace on windows-latest, ubuntu-22.04 and macos-latest with the pinned stable toolchain plus a MSRV job; cargo deny check; cargo xtask check-deps; cargo xtask verify (§25.3); docs build; coverage; fuzz smoke; reduced benchmarks; e2e UI tests; corpus-based conformance and differential tests on Linux (with cached corpora and installed reference tools).
fuzz-nightly.yml, corpora.yml (weekly re-verification of corpus hashes and licenses), bench.yml (weekly full benchmarks).
release.yml on tags v*: build every artifact in §23 across the matrix, sign when secrets exist, run all install smoke tests, generate SBOM/checksums/attestations, create a draft GitHub release with notes from CHANGELOG.md, upload artifacts, and publish the release only when every job passed. Pre-release tags (-alpha, -beta, -rc) are marked as pre-releases.
Security of workflows: pin third-party actions to full commit SHAs with version comments; least-privilege permissions: per job; no pull_request_target with checkout of untrusted code; secrets only in the release job; Dependabot for GitHub Actions and Cargo.
Caching with a well-known Rust cache action; corpora and fuzz corpora cached separately.
25. Documentation
25.1 Files
README.md: tagline; what it is; privacy promise; screenshots (light/dark, queue, inspector, compare view); download table per OS with which file to pick; supported-formats summary with ownership badges and a link to docs/COMPATIBILITY.md; quick start for desktop and CLI; CLI examples; data locations and portable mode; security model; build-from-source instructions (Windows and Linux); encumbered-codec policy explained neutrally; roadmap; contributing; license.
docs/: ARCHITECTURE.md (crates, layering, IRs, planner, worker isolation, with diagrams in Mermaid), FORMATS.md (index) + formats/<id>.md (one per format: spec, supported variants, rejected variants, limits, notes produced, test corpus, conformance status), COMPATIBILITY.md (generated matrix), PERFORMANCE.md (method, machines, tables, RD curves), CLI.md, API.md + openapi.json, SECURITY.md (threat model), PACKAGING.md (how each artifact is built and tested, how to package for a distro), DEPENDENCIES.md, DECISIONS.md, PROGRESS.md, CONTRIBUTING.md (including "How to add a new format" step-by-step with a skeleton crate module, fixture, fuzz target and acceptance test), TRANSLATING.md.
User guide: an mdBook in docs/book/ (mdBook is a dev-time tool only), built in CI and published to GitHub Pages after I confirm Pages is enabled. The same content ships offline inside the app's Help screen (static HTML, no network).
CHANGELOG.md in Keep a Changelog format, one section per release with real dates.
LICENSE: MIT, copyright "Yusuf" (keep the existing license; if I later want MIT OR Apache-2.0 dual licensing, that is my decision).
25.2 Screenshots

Captured with the Playwright MCP from serve mode (and of the desktop app on Windows if you can capture it), stored in docs/screenshots/ as PNGs written or optimised by our own PNG encoder, each < 500 KB.

25.3 Release verifier (cargo xtask verify)

Fails on any of: banned dependency in a shipped binary; network-capable crate in a shipped binary; http:// or https:// URLs in ui/ or shipped crate sources outside an allow-list of documentation comments; inline scripts/styles/innerHTML misuse in ui/; version mismatch between Cargo.toml workspace version, Tauri config, packaging metadata and CHANGELOG; missing docs/formats/<id>.md for any registered format; registered format without a fuzz target; registered route without acceptance coverage; screenshots over 500 KB; missing license texts in packages; git status not clean (skipped in CI).

PART VIII: PHASES, MILESTONES AND DEFINITIONS OF DONE
26. Phases

Each phase ends with a tagged release that installs and works on its own. Do the phases in order. Within a phase, the milestones are in order unless marked parallelisable. Every milestone passes the push gate (Part IX).

Phase 0: Bootstrap and migration (no release)
M0.1 Preflight (§8); copy this prompt to docs/MASTER_PROMPT.md; create PROGRESS.md, DECISIONS.md with ADR-0001 "Rust engine, own codecs, Tauri shell".
M0.2 Handle the 1.x code per §3.
M0.3 Cargo workspace with all crates as empty skeletons, workspace lints, rust-toolchain.toml, deny.toml, .gitattributes, .gitignore, xtask skeleton (check-deps, verify, fixtures, corpora).
M0.4 CI ci.yml green on Windows/Ubuntu/macOS with fmt, clippy, tests, deny, check-deps.
M0.5 docs/ARCHITECTURE.md first version; SECURITY.md; CONTRIBUTING.md skeleton.
Phase 1: Foundations → v2.0.0-alpha.1
M1.1 oc-core: IR types, errors, notes, limits, progress/cancel, options schema types, registry and planner (with unit tests on synthetic graphs).
M1.2 oc-io bounded readers, bit readers/writers (MSB/LSB first), sinks; oc-checksum (all, with SIMD paths in oc-simd).
M1.3 oc-compress: DEFLATE/zlib/gzip (full, tuned to budget), LZW, PackBits.
M1.4 oc-text (encodings + detection + line endings + normalisation) and oc-xml.
M1.5 oc-engine job runner + oc-worker process isolation with limits on Windows and Linux; protocol versioning; crash/timeout tests (a test codec that panics, loops and over-allocates must be contained).
M1.6 oc-cli: convert, info, formats, routes, doctor, --json, exit codes, completions; text conversion end to end.
M1.7 Fuzz targets for everything so far; corpora manifest started; release workflow skeleton producing CLI archives only.
Phase 2: Images, desktop app and packages → v2.0.0 (first full release)
M2.1 Image IR, ops (resize, rotate, orientation, flatten, depth conversion with dithering, quantisation), CMM (ICC v2/v4 + CICP).
M2.2 PNG + APNG; Netpbm; QOI; TGA; BMP/DIB; ICO/CUR; ICNS.
M2.3 JPEG decoder (baseline, progressive, arithmetic, lossless, 12-bit, CMYK/YCCK) and encoder (baseline, progressive, optimised Huffman, trellis option) + lossless transforms.
M2.4 GIF (animated read/write); WebP (lossy via a VP8 intra-frame decoder/encoder in the dedicated oc-vp8 crate, which gets inter-frame support in Phase 5a; plus VP8L lossless, alpha and animation); TIFF (incl. BigTIFF, CCITT, JPEG-in-TIFF); PSD composite; DDS; HDR; OpenEXR.
M2.5 CSV/TSV/JSON tables (from oc-sheet basics) so 1.x parity is complete.
M2.6 Tauri desktop app with the full §18 UI for images/text/tables; serve mode with §20 protections.
M2.7 Windows NSIS/offline/MSI/portable, .deb (both packages), .rpm (both packages), AppImage, static musl CLI; install smoke tests green; SBOM, checksums, attestations.
M2.8 Playwright MCP verification of the serve UI and desktop smoke tests; screenshots; docs for everything so far; docs/COMPATIBILITY.md generated.
M2.9 Tag v2.0.0, GitHub release with all artifacts.
Phase 3: Archives, compression and spreadsheets → v2.1.0
M3.1 bzip2, LZMA/LZMA2/XZ (with BCJ and Delta filters, multithreading), Zstandard, LZ4, Brotli decoder, Unix compress (read), CCITT already done.
M3.2 ZIP (all methods, ZIP64, Unicode, AES via RustCrypto), TAR (ustar/pax/GNU), compressed tars, single-file compressors.
M3.3 7z (read + write, AES), CPIO, AR, ISO 9660 (read), CAB (read).
M3.4 Archive UI: tree inspector, select entries, extract, create from folder, recompress; archive safety tests (zip-slip, symlink, bomb) all passing.
M3.5 XLSX and ODS read/write; table preview in the inspector.
M3.6 Release v2.1.0.
Phase 4: Audio → v2.2.0
M4.1 Audio IR, DSP (format conversion, dither, channel mixing, resampler with measured quality, loudness).
M4.2 WAV/RF64/BWF, AIFF/AIFC, FLAC (enc/dec), ALAC (enc/dec), CAF.
M4.3 MP3 decoder and encoder.
M4.4 Vorbis decoder and encoder; Opus decoder and encoder; Ogg container.
M4.5 Common tag model and cover art; audio in M4A/MKA via the container crate's minimal ISO-BMFF/Matroska audio paths (extended in Phase 5).
M4.6 Audio UI: waveform preview, playback preview, loudness display, audio presets.
M4.7 Release v2.2.0.
Phase 5a: Containers, remux, subtitles, VP8 → v2.3.0
M5.1 Media/Packet IR; Matroska/WebM, MP4/MOV/fMP4, AVI/OpenDML, Ogg (video), MPEG-TS, MPEG-PS (read), FLV (read), Y4M.
M5.2 Remux planner with stream copy for all containers (including avcC↔Annex B and hvcC reformatting without decoding).
M5.3 Subtitles (SRT, WebVTT, ASS/SSA, TTML read) with extract/embed/retime.
M5.4 MJPEG and raw codecs; animated GIF/APNG/WebP as video in and out; image sequences.
M5.5 VP8 full decoder (inter frames) and encoder with rate control and threading.
M5.6 Video processing: scaler, pixel formats, colour conversion, deinterlace, frame-rate conversion, crop/pad/rotate, trim, A/V sync.
M5.7 Video UI: streams inspector with keep/drop/copy/re-encode, thumbnail strip, trimming controls, presets.
M5.8 Release v2.3.0.
Phase 5b: VP9 and AV1 → v2.4.0
M5.9 VP9 decoder (profiles 0–2), bit-exact on conformance vectors; VP9 encoder (profile 0, tiles).
M5.10 AV1 decoder (Main profile), bit-exact on conformance streams, multithreaded; AV1 encoder (Main, speed presets, tiles); AVIF intra still images now available (moves AVIF from Phase 6c into this release).
M5.11 Subtitle burn-in (needs the Phase 6a font/raster work: if not yet available, defer burn-in to Phase 6a and record it).
M5.12 Release v2.4.0.
Phase 6a: Fonts, raster, SVG, PDF → v2.5.0
M6.1 oc-font (TrueType, CFF/CFF2, Type 1, OpenType tables, WOFF/WOFF2, subsetting) and font conversion routes.
M6.2 oc-raster (anti-aliased paths, strokes, clips, gradients, blending) with visual tests.
M6.3 oc-svg render + export.
M6.4 PDF parser (incl. recovery, all filters except JPX/JBIG2), security handlers (decrypt), text extraction to TXT/Markdown/HTML, metadata, outlines, attachments.
M6.5 JPEG 2000 decoder and JBIG2 decoder (for PDF and standalone JP2/J2K images).
M6.6 PDF renderer (full §14.3 rendering scope) with visual tests vs Poppler/MuPDF.
M6.7 PDF writer: images → PDF, page operations (merge/split/rotate/extract/reorder/compress/linearize/encrypt/decrypt), PDF/A-2b mode.
M6.8 PDF UI: page thumbnails, range selection, page operations, password prompt.
M6.9 Release v2.5.0.
Phase 6b: Documents and layout → v2.6.0
M6.10 Document IR; Markdown (CommonMark + GFM, 100% spec tests); HTML parser (WHATWG) and writer; TXT ↔ Document.
M6.11 DOCX, ODT, RTF read/write; EPUB read/write; FB2 read.
M6.12 Layout engine (Knuth–Plass, hyphenation, tables, lists, footnotes, headers/footers, columns) → Page IR → PDF/images/SVG; system font discovery and substitution; bundled OFL fallback fonts.
M6.13 Complex-script shaping, UAX #9 bidi, UAX #14 line breaking (tests 100%).
M6.14 PPTX/ODP → text/outline and → PDF/images.
M6.15 Document UI: outline inspector, page preview, conversion options (page size, margins, fonts, PDF/A).
M6.16 Release v2.6.0.
Phase 6c: Stretch formats → v2.7.0

JPEG XL (decode, then encode), DNG/RAW, legacy DOC/XLS/PPT (via MS-CFB), WavPack, APE (if documentable), ProRes decode (if documentable), SQLite tables, Theora decode, Brotli encoder (WOFF2 writing), anything deferred from earlier phases. Each item is independent; release when the batch is complete.

Phase 7: Python bindings → v2.8.0

§21 in full; wheels built in CI; PyPI publishing after my confirmation.

Phase 8: Distribution polish → v3.0.0

Flathub, winget, COPR, signed APT/DNF repositories, Windows 11 modern context menu, macOS signing/notarization (if I provide credentials), translations infrastructure with at least a pseudo-locale and one real community language if contributed, OSS-Fuzz application, performance pass against every budget in §17.2, full documentation review, and the encumbered build only if I have recorded a decision for it.

27. Definition of Done
27.1 Every phase
 cargo fmt --check, cargo clippy --workspace --all-targets -- -D warnings, and cargo test --workspace pass locally on Windows with zero failures and zero ignored tests (any #[ignore] requires an ADR and is run in CI).
 CI green on the release commit for every job in §24 (give the run URL).
 Every new format has docs/formats/<id>.md, a fuzz target, conformance/differential coverage meeting §22.2, and appears in docs/COMPATIBILITY.md.
 Performance measured against §17.2 for the new codecs and recorded in docs/PERFORMANCE.md, with any miss explained in an ADR.
 cargo xtask verify passes.
 All release artifacts for that phase build, install and pass smoke tests on every Tier 1 platform.
 The UI exposes the new formats with options, previews and notes; Playwright MCP exploratory pass done with no console errors and no non-local requests; screenshots updated where the UI changed.
 README, CHANGELOG, docs updated; tag pushed; GitHub release published with notes, checksums, SBOM and attestations.
 git status clean; main pushed; docs/PROGRESS.md updated with the next phase.
27.2 Additional for v2.0.0
 Double-clicking OpenConverter.exe from the portable zip and from the installed Start-menu shortcut both open the app on a clean Windows 11 machine with WebView2.
 sudo apt install ./openconverter_*.deb on Ubuntu 22.04/24.04 and Debian 12, and sudo dnf install ./openconverter-*.rpm on supported Fedora releases, install cleanly and the app launches from the desktop menu.
 Every image route in the acceptance matrix passes; JPEG progressive/arithmetic/CMYK inputs that failed in 1.x now convert correctly.
 Hostile-input tests: zip-bomb-like PNG (huge IDAT), decompression bombs, malformed JPEGs, 1 GP declared images, and a worker deliberately crashed and hung, all handled without affecting the app.
PART IX: PUSH GATE AND REPORTING
28. Push gate (after every milestone and before every tag)

Push to origin main (https://github.com/Yusuf6411/OpenConverter) only when all of these pass, in this order:

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace with 0 failures (and 0 ignored without an ADR)
cargo deny check and cargo xtask check-deps
cargo xtask verify (once it exists; git status check skipped until after the commit)
git status shows only intended files (no build outputs, corpora, user files, logs, secrets)

If every check passes: commit with a Conventional Commit message, then git push origin main. If any check fails: do not push. Fix the root cause, re-run all checks from step 1, then push. Never skip or weaken a test. Never force-push.

After each push, use the GitHub MCP to check the CI run for that commit. If CI fails, fix it before starting the next milestone. If a push fails on authentication, stop and tell me the exact credential step needed (for example gh auth login or signing in through Git Credential Manager). Do not try to work around authentication.

Releases: create an annotated tag (git tag -a vX.Y.Z -m "OpenConverter X.Y.Z"), push it (git push origin vX.Y.Z), let release.yml build and publish, then verify the release page with the GitHub MCP. If the release workflow cannot publish, give me the exact failure and the notes text.

29. Reporting

End of every session: update docs/PROGRESS.md and tell me in a few lines: milestones completed, test totals, CI status link, anything blocked and why, and the exact next step.

End of every phase: a short release report: what shipped (formats and features), test count and result, conformance results summary, CI run link, release link, measured performance vs §17.2 (with machine details), known limitations, decisions made (ADR list), anything not completed with the exact reason.

Never report something as done, passing or measured unless you ran it and saw the result in this session or in CI.