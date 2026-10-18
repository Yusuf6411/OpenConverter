# OpenConverter 1.0 — Progress

## Resume
```powershell
cd "D:\Personal Projects\OpenConverter"
python -m compileall -q openconverter; python -m unittest discover -s tests -v
```
Then re-read this file and continue with the current milestone (spec §18).

## Preflight (2026-10-18)
| Check | Result |
|---|---|
| git | 2.55.0.windows.3 |
| Python | 3.14.4 (`python` and `py -3`), >= 3.12 OK |
| git identity | Yusuf6411 / muhammedyusuf6411@outlook.com |
| GitHub repo | `git ls-remote` succeeds, repo empty |
| GitHub MCP | Not available in this session; `gh` CLI not installed. CI status is checked through the public GitHub REST API / web pages instead. |
| Playwright MCP | Not available; the built-in browser pane (same capabilities: navigate, console, network, screenshots) is used for §17. |
| Port 8080 | Free |
| Machine | Intel Core i5-1035G1 @ 1.00 GHz, 20 GB RAM, Windows 11 Pro |

## Milestones
- [x] 1. Repo bootstrap, package skeleton, `python -m openconverter --version`
- [x] 2. Image IR, image ops, checksums, Netpbm, BMP, TGA, QOI
- [x] 3. Own DEFLATE + perf measurements
- [x] 4. PNG read/write
- [x] 5. JPEG baseline decode/encode
- [x] 6. GIF decode, resize/thumbnail
- [x] 7. Text and table converters
- [x] 8. Registry, detection, jobs, progress/cancel, storage, settings, history
- [x] 9. HTTP server, API, security, multipart, SSE + integration tests
- [x] 10. Web UI
- [x] 11. CLI convert / formats
- [x] 12. Robustness/fuzz suite, performance tuning
- [x] 13. Browser verification (§17), screenshots
- [x] 14. Docs, CHANGELOG, CI, verify_release.py; CI green
- [x] 15. Tag and release (tag v1.0.0 pushed; GitHub release notes handed to the owner because no GitHub MCP/gh/token is available in this session)

**Current milestone:** done (1.0.0)

## Decisions
- Version single source: `openconverter/__init__.py`; pyproject reads it via setuptools `dynamic` attr; a test asserts it.
- Local dev Python is 3.14; code avoids features newer than 3.12 because CI runs 3.12/3.13.

- Progress/cancel contract lives in `models/progress.py` (lowest layer) so codecs never import `core/`; `core/progress.py` holds the job-aware implementation.
- Decoder limits live in `openconverter/limits.py` (leaf module) and are passed into decoders.
- BMP: 32-bit BI_RGB ignores the 4th byte (spec says it is unused); alpha is read only from bit-field masks (V3+/BI_ALPHABITFIELDS). RLE pixels skipped by delta/EOL take palette index 0. Out-of-range palette indices raise CorruptFile.
- TGA: 32-bit uses alpha when descriptor alpha bits = 8, or when alpha bits = 0 but the 4th byte is not all zero (common writer bug).
- PBM writer thresholds at 50% grey (< 128 -> black) and notes it unless the image is already pure black/white.
- Writers drop an alpha channel that is fully opaque (lossless) rather than writing a 32-bit file.

- DEFLATE: hash chains use zlib's rolling 15-bit hash (head/prev lists, candidates verified byte-wise); the hot loop is a closure-free generator because cell-variable access was ~40% slower. Levels use zlib's (good, lazy, nice, chain) table. Blocks are cut every ~32k tokens (checked every 16 KiB of input).
- DEFLATE trees are always complete (a lone symbol is paired with a dummy); length limiting clamps then repairs the Kraft sum (zlib-style heuristic).
- DEFLATE level table keeps zlib's shape but scales chain/nice limits down (level 6 = chain 16, nice 64): each chain step costs ~0.1 us in C but ~1 us in Python. Measured on filtered photo data the size cost vs zlib -6 is ~3%.
- PNG writer: palette chosen in `palette=auto` when <= 256 colours and the packed raw size is smaller; L images whose values are all multiples of 255/85/17 are written at 1/2/4 bits (lossless). Adaptive MSAD filtering for >= 8-bit non-palette images, filter None otherwise. Filters Sub/Up/Average use whole-row SWAR big-int arithmetic; Paeth uses a per-byte loop.
- PNG reader: an unknown critical chunk raises UnsupportedVariant (it is a valid file we cannot interpret); a chunk longer than the configured maximum raises LimitExceeded. DPI is rounded to 0.1.
- JPEG IDCT: precomputed cosine basis summed over non-zero coefficients (sparse blocks are typical), DC-only fast path; FDCT: float AAN with scaling folded into the quantiser. Chroma upsampling is triangle ("fancy") for h2v1, h1v2 and h2v2, matching libjpeg-turbo; other ratios replicate.
- JPEG interop fixtures in tests/fixtures/jpeg were produced with the browser pane (Chromium/libjpeg-turbo): Chromium-encoded JPEGs with Chromium's decoded pixels, and our encoder's output with Chromium's decode. Our decoder matches within 4 levels.
- JPEG encoder default subsampling: 4:2:0 for q < 90, 4:4:4 otherwise (noted in the result). Internal `colour_transform=False` writes Adobe RGB JPEGs for decoder tests only.
- `.claude/` (local preview launch config) is git-ignored.
- GIF: area outside the first frame is transparent (as browsers render it); out-of-palette indices (other than the transparent index) raise CorruptFile; LZW data that ends before the frame is full raises CorruptFile. A GIF fixture built by the test GIF writer was decoded by Chromium identically (stored with Chromium's pixels).
- Thumbnails: integer-factor box reduction (ceil(longest/512)), so previews are <= 512 px on the long side, sometimes smaller; area-average resize only for extremely thin images.
- Text decoding order: BOM -> BOM-less UTF-16 (NUL-byte pattern in the first 4 KiB) -> strict UTF-8 -> the `source_encoding` fallback (default Windows-1252; its 5 undefined bytes fall back to Latin-1). UTF-32 -> UnsupportedVariant. Unencodable characters raise UnsupportedVariant (default) or are replaced with '?' and counted.
- Target encodings: utf-8, utf-8-bom, utf-16le(-bom), utf-16be(-bom), latin-1, windows-1252. Newline: preserve (default for TXT), lf, crlf (default for CSV output, per RFC 4180).
- CSV: blank lines outside quotes are skipped; text after a closing quote is kept (lenient); single-empty-field rows are written as `""`. Header de-dup: empty -> column_N, duplicates -> name_2 (noted). Cells stay strings (no type inference); JSON scalars become text (null -> empty) and this is noted only when it happened.
- Option keys are distinct for reading vs writing (source_delimiter/has_header/source_encoding vs delimiter/bom/newline/indent/ensure_ascii/to_encoding/unencodable) so CSV->CSV is unambiguous.
- FormatSpec lives in converters/base.py (core imports converters, never the reverse); each format has separate read_options/write_options schemas; /api/formats defaults reflect user settings (quality, compression_level, background).
- Detection: strong magic numbers, then TGA header validation, then known-unsupported signatures (PDF, ZIP, RIFF...), then text. For text the extension picks txt/csv/tsv/json; without one, '[' -> JSON, consistent delimiter counts -> CSV/TSV, else TXT. Only the first MiB is examined.
- Upload metadata and output metadata live in `<id>.json` sidecars next to the UUID-named files, so history and downloads survive restarts. IDs are uuid4 hex (32 chars) validated by regex before any path is built.
- A cancelled job never leaves output: outputs are written to temp/ and os.replace'd; a cancel that races the final write deletes the output.
- Server: an error raised before/while reading a request body first drains up to 64 MB / 15 s of the unread body, so clients receive the JSON error instead of a TCP reset (seen on Windows for early 413s); the UI also checks max_upload_mb before uploading. Chunked request bodies are rejected (400). HTTP/0.9 requests get an HTTP/1.0 status line. Low-level errors (bad request line, 431) also return JSON with security headers.
- Added GET /api/jobs (list of jobs known to this server process) so the UI can reconcile in-flight jobs after a reload; documented in docs/API.md.
- SO_EXCLUSIVEADDRUSE on Windows and allow_reuse_address=False so a busy port fails with a clear message instead of silently sharing.
- Web UI: uploads use XMLHttpRequest (one file at a time) for real upload progress; conversions use SSE with polling fallback and a 10 s SSE retry. Queue cards persist in localStorage and are reconciled on load with GET /api/jobs and /api/history. The UI sends only user-changed options; the server fills the rest from settings, and /api/formats shows those same defaults.
- The background-colour option is shown only when the target flattens alpha and the upload's header suggests transparency (`may_have_alpha`, best effort); the conversion itself always uses the real pixels. Input (read) options sit in a collapsed "Input options" section.
- Large-image (> 4 MP) time estimate = median(duration/megapixel) of this machine's finished conversions (same target preferred), from history.jsonl `megapixels`; with no history it says so instead of guessing.
- CLI: option flags map 1:1 to schema keys (--quality, --level, --background, --indent, --to-encoding, ...); a flag that does not apply to the detected source/target pair is a usage error (exit 2). CLI uses schema defaults (not the web settings file). Output is written to a temp name then os.replace'd. Console streams use errors='replace'.
- Fuzz suite (tests/integration/test_fuzz.py): seeded truncations, byte flips, garbage-after-magic, absurd dimensions and raw inflate garbage for every decoder (PNG, JPEG, GIF, BMP, TGA, QOI, Netpbm, zlib, text, CSV, JSON), each in a watchdog thread; only OpenConverterError subclasses are accepted. An extra offline campaign (1,500 mutations per fixture) found no untyped exceptions or slow cases.
- Performance tuning: DEFLATE rolling hash widened to 16 bits, globals bound to locals, `for range(chain)` candidate loop with cheap byte rejects; level table shortened (level 6 chain 8; > 4 MP images use chain 4 with a note). PNG encoder Paeth predictor vectorised with 16-bit SWAR lanes (verified against the scalar predictor). PNG write 1024x768 went from 34 s to 20-27 s on this machine; still above the 15 s target (below the 40 s ceiling) - see Known issues.
- QOI: hashes and channel differences are precomputed for the whole image with byte-lane big-int arithmetic, and the decoder builds packed 32-bit pixels in a list (array.tobytes at the end): 1920x1080 write+read went from 27.5 s to 10.9 s.
- scripts/verify_release.py and its meta tests are kept out of commits until the docs/screenshots it checks exist (M14), so the push gate stays meaningful.
- §17 browser pass (built-in browser pane, Chromium, 2026-10-18), server started with `python -m openconverter serve --no-browser --data-dir <scratch>` at http://127.0.0.1:8080. Test files were generated in the page (canvas PNG/JPEG, hand-built BMP/PPM, the GIF fixture) and added through the real file input; clicks used the real buttons. Results: PNG with alpha -> JPG q85 (note "Alpha flattened onto #FFFFFF."); JPG -> PNG 200x150; BMP+PPM+GIF "Set all to PNG" + "Convert all" all done (BMP->PNG pixel-identical per Chromium); CSV with quoted comma+newline -> JSON exactly correct; truncated PNG -> friendly error, other cards fine; 4000x3000 noisy PNG -> Cancel -> cancelled, no output; theme toggle and settings (quality 70) persisted across reload and applied to a new card; history re-download, delete, clear all; keyboard-only operation; 900/700 px with no horizontal scroll; all 56 required conversions driven through the UI, PNG/JPEG/BMP outputs decoded by Chromium and all 75 outputs decoded by our codecs; console clean; only 127.0.0.1/localhost requests.
- Fixes from the pass: focus is kept on card controls across re-renders (it was lost after changing the target); cancellation is checked every ~16 KiB of PNG rows (was every 32 rows: ~2 s latency on wide images, now ~150 ms) and the card shows "Cancelling..."; previews of undecodable images return 204 (no console error) and saved cards are reconciled with one `GET /api/files?ids=&outputs=` lookup instead of per-file 404s; settings form error styled; small thumbnails scale up.
- Screenshots: browser-pane captures converted to PNG with our own JPEG decoder + PNG encoder (docs/screenshots, 180-220 KB each).

## Performance log
| Date | Operation | Input | Result |
|---|---|---|---|
| 2026-10-18 | deflate level 1 / 6 / 9 | 1024x768 photo-like RGB, raw 2.36 MB | 9.4 s / 11-17 s / 12.4 s (machine timing is noisy) |
| 2026-10-18 | inflate (our stream) | same | 2.7-3.8 s |
| 2026-10-18 | JPEG encode q85 4:2:0 / decode | 1024x768 photo-like | 6.0 s / 4.1 s |
| 2026-10-18 | PNG write level 6 (before tuning) | 1024x768 | 33.9 s |
| 2026-10-18 | PNG write level 6 (after tuning) | 1024x768 / 1920x1080 | 20-27 s / 68.5 s |
| 2026-10-18 | PNG read | 1024x768 / 1920x1080 | 4.8-7.5 s / 15.1 s |
| 2026-10-18 | JPEG encode / decode | 1920x1080 | 17.1 s / 14.6 s |
| 2026-10-18 | QOI write + read (after tuning) | 1920x1080 | 5.6 s + 5.3 s |
| 2026-10-18 | Final bench (full clocks): PNG write / read, JPEG enc / dec, PNG->JPG, JPG->PNG | 1024x768 | 5.9 / 1.7 / 1.4 / 2.3 / 4.2 / 4.9 s |
| 2026-10-18 | same | 1920x1080 | 16.6 / 4.7 / 3.9 / 2.9 / 11.3 / 14.8 s |

- CI: green on Windows + Ubuntu x Python 3.12 + 3.13 (first green run: https://github.com/Yusuf6411/OpenConverter/actions/runs/39175628331). The first CI run failed because the unanchored `data/` ignore pattern had kept tests/fixtures/data out of git; fixed and guarded by verify_release.py.

## Known issues
- Timings on the dev laptop vary 3-4x with its power/thermal state: final run (full clocks) PNG write 1024x768 5.9 s, 1920x1080 16.6 s; earlier throttled runs 20-27 s / 68 s. Both documented in FORMATS.md.
- A thumbnail request for a very large image decodes the whole image in an HTTP thread (not cancellable); it only costs time.
