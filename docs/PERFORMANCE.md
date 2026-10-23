# Performance

Measured with the release build of `openconverter convert --no-isolation` (wall clock of the whole process, including reading and writing the files) on a 4-core Intel Xeon cloud container, one run each, no warm-up. CI runners and the owner's machine have not been measured; the criterion benchmarks that MASTER_PROMPT §17.3 asks for do not exist yet. Numbers below are a first pass, not a regression gate.

| Operation | Input | Measured | Budget (§17.2) | Verdict |
|---|---|---|---|---|
| PNG encode, default level | 1920×1080 RGB noisy gradient | 505 ms | ≤ 150 ms | miss (3.4×) |
| PNG decode | same | 127 ms | ≤ 20 ms | miss |
| JPEG encode | same | 72 ms | ≤ 25 ms | miss (2.9×) |
| JPEG decode | same | 72 ms | ≤ 15 ms | miss (4.8×) |
| WebP lossless encode | same | 815 ms | ≤ 1.5 s | met |
| FLAC encode | 60 s stereo 44.1 kHz | 1.3 s (46× realtime) | ≥ 200× | miss |
| MP3 encode | same | 1.6 s (37× realtime) | ≥ 50× | miss |
| Opus encode | same | 0.9 s (65× realtime) | ≥ 30× | met |
| Deflate (gzip) encode | 20 MB base64 text | 885 ms (23 MB/s); gzip -6 took 725 ms | ≥ 40 MB/s | miss, near gzip |
| Inflate (extract) | 100 MB | 1.96 s (51 MB/s) | ≥ 300 MB/s | miss |
| Zstandard encode | 100 MB base64 text | 22 s (4.5 MB/s) | ≥ 150 MB/s | miss (33×) |
| Zstandard decode | 100 MB | 1.3 s (75 MB/s) | ≥ 800 MB/s | miss |
| XZ encode | 20 MB base64 text | 7.5 s (2.7 MB/s); xz -6 took 11 s | ≥ 5 MB/s | miss, faster than xz |
| XZ decode | 100 MB | 4.6 s (22 MB/s) | ≥ 60 MB/s | miss |
| Markdown (23 pages) → PDF | docs/MASTER_PROMPT.md | 194 ms | DOCX 50 pages ≤ 2 s | met |

Notes
- Image numbers include process start, file reading and a PPM or PNG step on the other side; the codecs alone are faster. The test picture is noise, which is the hard case for PNG and JPEG.
- The compression inputs are base64 text, which has little redundancy; ratios are not a measure here.
- Everything is scalar code. `oc-simd` has no kernels yet, and the codecs are single-threaded.
- Found and fixed during this pass: table writing recomputed the width per row, so CSV to JSON of 400,000 rows took minutes and now takes 0.3 s.

Not measured: Video (VP8/VP9/AV1), PDF page rendering at 150 DPI, remux throughput, XZ multi-thread encode.

See ADR-0015 for what is done about the misses.
