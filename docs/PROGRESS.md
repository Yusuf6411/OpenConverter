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
- [ ] 7. Text and table converters
- [ ] 8. Registry, detection, jobs, progress/cancel, storage, settings, history
- [ ] 9. HTTP server, API, security, multipart, SSE + integration tests
- [ ] 10. Web UI
- [ ] 11. CLI convert / formats
- [ ] 12. Robustness/fuzz suite, performance tuning
- [ ] 13. Browser verification (§17), screenshots
- [ ] 14. Docs, CHANGELOG, CI, verify_release.py; CI green
- [ ] 15. Tag and release

**Current milestone:** 7

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

## Performance log
| Date | Operation | Input | Result |
|---|---|---|---|
| 2026-10-18 | deflate level 1 / 6 / 9 | 1024x768 photo-like RGB, raw 2.36 MB | 9.4 s / 11-17 s / 12.4 s (machine timing is noisy) |
| 2026-10-18 | inflate (our stream) | same | 2.7-3.8 s |
| 2026-10-18 | JPEG encode q85 4:2:0 / decode | 1024x768 photo-like | 6.0 s / 4.1 s |

## Known issues
- None yet.
