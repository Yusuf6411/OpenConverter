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
- [ ] 3. Own DEFLATE + perf measurements
- [ ] 4. PNG read/write
- [ ] 5. JPEG baseline decode/encode
- [ ] 6. GIF decode, resize/thumbnail
- [ ] 7. Text and table converters
- [ ] 8. Registry, detection, jobs, progress/cancel, storage, settings, history
- [ ] 9. HTTP server, API, security, multipart, SSE + integration tests
- [ ] 10. Web UI
- [ ] 11. CLI convert / formats
- [ ] 12. Robustness/fuzz suite, performance tuning
- [ ] 13. Browser verification (§17), screenshots
- [ ] 14. Docs, CHANGELOG, CI, verify_release.py; CI green
- [ ] 15. Tag and release

**Current milestone:** 3

## Decisions
- Version single source: `openconverter/__init__.py`; pyproject reads it via setuptools `dynamic` attr; a test asserts it.
- Local dev Python is 3.14; code avoids features newer than 3.12 because CI runs 3.12/3.13.

- Progress/cancel contract lives in `models/progress.py` (lowest layer) so codecs never import `core/`; `core/progress.py` holds the job-aware implementation.
- Decoder limits live in `openconverter/limits.py` (leaf module) and are passed into decoders.
- BMP: 32-bit BI_RGB ignores the 4th byte (spec says it is unused); alpha is read only from bit-field masks (V3+/BI_ALPHABITFIELDS). RLE pixels skipped by delta/EOL take palette index 0. Out-of-range palette indices raise CorruptFile.
- TGA: 32-bit uses alpha when descriptor alpha bits = 8, or when alpha bits = 0 but the 4th byte is not all zero (common writer bug).
- PBM writer thresholds at 50% grey (< 128 -> black) and notes it unless the image is already pure black/white.
- Writers drop an alpha channel that is fully opaque (lossless) rather than writing a 32-bit file.

## Known issues
- None yet.
