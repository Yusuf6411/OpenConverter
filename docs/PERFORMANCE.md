# Performance

Measured with the release build of `openconverter convert --no-isolation` (wall clock of the whole process, including reading and writing the files) on a 4-core Intel Xeon cloud container, one run each, no warm-up. CI runners and the owner's machine have not been measured; the criterion benchmarks that MASTER_PROMPT §17.3 asks for do not exist yet. Numbers below are a first pass, not a regression gate.

| Operation | Input | Before | Now | Budget (§17.2) | Verdict |
|---|---|---|---|---|---|
| PNG encode, default level | 1920×1080 RGB noisy gradient | 505 ms | 489 ms | ≤ 150 ms | miss (3.3×) |
| PNG decode | same | 127 ms | 64 ms | ≤ 20 ms | miss (3×) |
| JPEG encode | same | 72 ms | 80 ms | ≤ 25 ms | miss (3.2×) |
| JPEG decode | same | 72 ms | 52 ms | ≤ 15 ms | miss (3.5×) |
| WebP lossless encode | same | 815 ms | 780 ms | ≤ 1.5 s | met |
| FLAC encode | 60 s stereo 44.1 kHz | 1.3 s (46× realtime) | 0.41 s (146×) | ≥ 200× | miss |
| MP3 encode | same | 1.6 s (37×) | 1.19 s (51×) | ≥ 50× | met |
| Opus encode | same | 0.9 s (65×) | 0.92 s (65×) | ≥ 30× | met |
| Deflate (gzip) encode | 100 MB source text | 23 MB/s (base64 text) | 27 MB/s; gzip -6 took 52 MB/s | ≥ 40 MB/s | miss |
| Inflate (extract) | 100 MB source text | 51 MB/s (base64 text) | 96 MB/s; gzip -d took 250 MB/s | ≥ 300 MB/s | miss (3×) |
| Zstandard encode (level 3) | 100 MB source text | 4.5 MB/s (base64 text); 34 MB/s (source text) | 116 MB/s, 13% larger than before | ≥ 150 MB/s | miss, close |
| Zstandard decode | 100 MB source text | 75 MB/s (base64 text); 400 MB/s | 356 MB/s | ≥ 800 MB/s | miss |
| XZ encode | 20 MB base64 text | 2.7 MB/s | 3.2 MB/s (xz -6: 1.8 MB/s) | ≥ 5 MB/s | miss, faster than xz |
| XZ decode | 100 MB base64 text | 22 MB/s | not re-measured | ≥ 60 MB/s | miss |
| Markdown (23 pages) → PDF | docs/MASTER_PROMPT.md | 194 ms | 194 ms | DOCX 50 pages ≤ 2 s | met |

Notes
- Image numbers include process start, file reading and a PPM or PNG step on the other side; the codecs alone are faster. The test picture is noise, which is the hard case for PNG and JPEG.
- The compression inputs are base64 text, which has little redundancy; ratios are not a measure here.
- Changed in the speed pass: table-driven Huffman decoding and a 64-bit bit buffer in Inflate; a two-table greedy parser with a growing step for Zstandard levels 1 to 3 (levels 4 and up keep the chain matcher); PNG unfiltering straight into the pixel buffer; JPEG YCbCr conversion without per-sample lookups; FLAC and MP3 choose by estimated size and write only the winner. MP3 output is byte-identical to before; FLAC output is 0.03% larger.
- Everything is scalar code. `oc-simd` has no kernels yet, and the codecs are single-threaded.
- Found and fixed during this pass: table writing recomputed the width per row, so CSV to JSON of 400,000 rows took minutes and now takes 0.3 s.

Not measured: Video (VP8/VP9/AV1), PDF page rendering at 150 DPI, remux throughput, XZ multi-thread encode.

See ADR-0015 for what is done about the misses.
