# Formats

Ownership: **OWN** = implemented in this repository; **PARTIAL** = relies on a
Python standard-library component; **EXTERNAL** = another backend (none in 1.0).

| Format | Read | Write | Ownership |
|---|---|---|---|
| PBM / PGM / PPM (P1–P6) | yes | yes | OWN |
| BMP | yes | yes | OWN |
| PNG | yes | yes | OWN (own DEFLATE and CRC-32) |
| JPEG (baseline) | yes | yes | OWN |
| GIF | yes (first frame) | no | OWN (own LZW) |
| TGA | yes | yes | OWN |
| QOI | yes | yes | OWN |
| TXT | yes | yes | PARTIAL (Python text codecs) |
| CSV / TSV | yes | yes | OWN |
| JSON | yes | yes | PARTIAL (Python `json` for syntax) |

Formats are detected from file **content**; the extension only breaks ties
between text formats (TXT/CSV/TSV/JSON). A card shows a warning when the
extension disagrees with the content.

## Images

### Conversion rules (`ops/image_ops.py`)

Every rule that is applied is added to the result notes.

| Rule | Formula |
|---|---|
| Alpha → flatten (target without alpha) | straight alpha onto the background colour (default white, selectable): `out = (c·a + bg·(255−a) + 127) // 255`; a fully opaque alpha channel is simply dropped |
| 16 → 8 bit | `(v·255 + 32767) // 65535` |
| 8 → 16 bit | `v·257` |
| Grey → RGB | replication |
| RGB → grey | BT.601 integer luma `(299R + 587G + 114B + 500) // 1000` |
| Low-depth grey (1/2/4-bit) | scaled to 0–255 (×255, ×85, ×17) |
| EXIF orientation (JPEG) | applied while decoding (rotation/flip), noted |

Lossless pairs keep what both sides can store: 16-bit PNG → 16-bit PPM/PGM
stays 16-bit; alpha survives PNG ↔ TGA/QOI/BMP-32. Writers choose the best
mode they support and convert only when required.

### PBM / PGM / PPM
* Read: P1–P6, ASCII and binary, maxval 1–65535 (≤ 255 → 8-bit, otherwise
  16-bit; other maxvals are rescaled to the full range), `#` comments,
  tolerant whitespace. Samples above maxval → corrupt.
* Write: binary P4/P5/P6 by default, ASCII P1/P2/P3 with the `ascii` option.
  PBM thresholds at 50 % grey (noted). Alpha is flattened (noted).
* Not supported: PAM (P7), multi-image files (only the first image is read).

### BMP
* Read: 1/4/8-bit palettised, 16-bit (5-5-5 default, 5-6-5 via bit fields),
  24-bit, 32-bit; BI_RGB, BI_BITFIELDS, BI_ALPHABITFIELDS, BI_RLE4, BI_RLE8;
  bottom-up and top-down; OS/2 BITMAPCOREHEADER and 2.x, INFO, V2–V5 headers.
  32-bit BI_RGB ignores the unused fourth byte; alpha comes from bit-field
  masks. Pixels skipped by RLE deltas use palette entry 0.
* Write: 24-bit BGR, or 32-bit BGRA with a V4 header when the image has
  (non-opaque) alpha. DPI is kept.
* Unsupported variants (typed error): JPEG/PNG-in-BMP, OS/2 Huffman-1D and RLE24.

### PNG
* Read: colour types 0, 2, 3, 4, 6 with every valid bit depth (1/2/4/8/16),
  all five filters, Adam7, `tRNS` (grey, RGB and palette), `PLTE`, `gAMA` and
  `pHYs` → metadata. Unknown ancillary chunks are ignored; unknown critical
  chunks → unsupported variant. Every chunk CRC is verified (mismatch →
  corrupt). APNG files decode their default image.
* Write: best-fit colour type: palette when ≤ 256 colours and the packed data
  is smaller (`palette` option: auto/off/on); greyscale whose values are all
  multiples of 255/85/17 is written at 1/2/4 bits (lossless); otherwise
  L/LA/RGB/RGBA at 8 or 16 bits. Per-row adaptive filtering by minimum sum of
  absolute differences, `compression_level` 0–9, `pHYs` when the DPI is known.
* Large images (> 4 MP) use a shorter hash-chain search and say so in the notes
  ("Fast mode: large image").

### JPEG (baseline)
* Decode: SOF0 and SOF1 (8-bit), 1 or 3 components (Adobe APP14 transform
  flag honoured for RGB-stored files), any integer sampling factors (4:4:4,
  4:2:2, 4:2:0, 4:4:0, 4:1:1, …), restart markers (sequence verified),
  multiple DQT/DHT segments, 16-bit DQT, byte stuffing, 0xFF fill bytes, EXIF
  orientation 1–8. Chroma upsampling is triangle ("fancy") filtering for 2:1
  ratios, replication otherwise. Edge MCUs are cropped exactly.
* Encode: baseline JFIF, quality 1–100 (IJG scaling of the Annex K tables),
  4:2:0 (default below quality 90) or 4:4:4, greyscale as one component,
  JFIF density from the DPI, optional optimal Huffman tables (Annex K.2,
  ≤ 16-bit codes, no all-ones code). Alpha is flattened first (noted).
* Unsupported variants (typed error with a specific message): progressive
  (SOF2), lossless (SOF3), hierarchical, arithmetic coding, 12-bit, CMYK/YCCK.
* Accuracy: IDCT within ±1 of a reference float IDCT; decoded output matches
  Chromium (libjpeg-turbo) within 4 levels on the fixtures in
  `tests/fixtures/jpeg/`.

### GIF (read only)
* First frame only (a note says how many frames there are), global and local
  palettes, transparency index → alpha, interlacing; disposal is ignored.
  Our LZW decoder handles clear/EOI codes, code-size growth to 12 bits,
  deferred clear and rejects malformed codes. The area of the logical screen
  outside the first frame is transparent.

### TGA
* Read: types 1/2/3 and RLE 9/10/11, 8/15/16/24/32-bit, both origins and
  right-to-left, colour-map offsets, TGA 2.0 footer (extension data ignored).
* Write: top-left origin, uncompressed or RLE (`rle` option), 8-bit grey,
  24-bit RGB or 32-bit RGBA, with a TGA 2.0 footer.

### QOI
* Full specification: RGB/RGBA, all six op types, end marker. Greyscale is
  expanded to RGB and 16-bit reduced to 8-bit (both noted).

## Text and tables

### TXT
* Decoding order: byte-order mark → BOM-less UTF-16 (NUL pattern) → strict
  UTF-8 → the `source_encoding` fallback (default Windows-1252, whose five
  undefined bytes fall back to Latin-1). UTF-32 → unsupported variant.
* Output encodings: UTF-8, UTF-8 with BOM, UTF-16 LE/BE with or without BOM,
  Latin-1, Windows-1252. Line endings: keep, LF or CRLF (mixed endings noted).
* Unencodable characters stop with an error (default) or are replaced with `?`
  (`unencodable: replace`, noted with a count).

### CSV / TSV
* Our own RFC 4180 parser: quoted fields with embedded delimiters, quotes
  (`""`) and newlines; CRLF or LF; optional trailing newline; ragged rows
  tolerated (padded on write, noted); BOM stripped. Blank lines outside quotes
  are skipped. Input delimiter: auto-detect among `,` `;` tab `|`, or explicit.
* Output: UTF-8 (optional BOM for Excel), CRLF (RFC default) or LF, delimiter
  comma/semicolon/tab/pipe for CSV.

### JSON
* Read: an array of flat objects (header = keys in first-seen order across all
  objects, missing keys → empty) or an array of arrays. Nested values →
  unsupported variant naming the row and key. Numbers, booleans and null become
  text (`null` → empty) — so a JSON → CSV → JSON round trip yields strings.
* Write: array of objects when there is a header (empty names → `column_N`,
  duplicates → `name_2`, noted), otherwise an array of arrays. `indent`
  0 (compact) / 2 / 4, `ensure_ascii`.

## Limits

| Limit | Default |
|---|---|
| Upload size | 200 MB (setting `max_upload_mb`) |
| Pixels per image | 64 MP (hard maximum 100 MP) |
| Longest side | 30 000 px |
| Inflate output | declared image size + 4 KiB |
| PNG chunk length | 256 MiB |
| JPEG components | 4 (only 1 and 3 decode) |
| GIF LZW output | the frame's declared size |
| Table cells / CSV field | 20 M cells / 16 MiB |

Limits are checked before any large allocation; exceeding one gives `limit_exceeded`.

## Performance

All codecs are pure Python, so large images take time. Measured with
`python scripts/bench.py` (photo-like images: smooth gradients plus ±4 noise,
seeded) on the development machine:

Machine: Intel Core i5-1035G1 laptop (4 cores / 8 threads, 1.0 GHz base, up to 3.6 GHz), 20 GB RAM, Windows 11, Python 3.14.4 (CPython, single thread per conversion). Run on 2026-10-18.

| Operation | Image | Time | Output size |
|---|---|---|---|
| PNG write (level 6) | 1024x768 | 5.9 s | 1,237,493 B |
| PNG read | 1024x768 | 1.7 s | 1,237,493 B |
| JPEG encode (q85, 4:2:0) | 1024x768 | 1.4 s | 47,408 B |
| JPEG decode | 1024x768 | 2.3 s | 47,408 B |
| PNG -> JPG (full conversion) | 1024x768 | 4.2 s | 47,408 B |
| JPG -> PNG (full conversion) | 1024x768 | 4.9 s | 508,123 B |
| PNG write (level 6) | 1920x1080 | 16.6 s | 3,256,261 B |
| PNG read | 1920x1080 | 4.7 s | 3,256,261 B |
| JPEG encode (q85, 4:2:0) | 1920x1080 | 3.9 s | 114,583 B |
| JPEG decode | 1920x1080 | 2.9 s | 114,583 B |
| PNG -> JPG (full conversion) | 1920x1080 | 11.3 s | 114,583 B |
| JPG -> PNG (full conversion) | 1920x1080 | 14.8 s | 1,132,599 B |
| PPM write + read | 1920x1080 | 0.1 s | 6,220,817 B |
| BMP write + read | 1920x1080 | 0.1 s | 6,220,854 B |
| TGA write + read | 1920x1080 | 0.1 s | 6,220,844 B |
| QOI write + read | 1920x1080 | 4.8 s | 4,527,501 B |

Timing on this laptop varies a lot with its power/thermal state: earlier runs of the same code on battery/thermal-limited clocks were 3-4x slower (PNG write 1024x768: 20-27 s; 1920x1080: 68 s; JPEG decode 1920x1080: 15 s). All operations stayed below the hard ceilings in every run; the 15 s PNG-write target was met only at full clock speed.

Method: one run per operation, `time.perf_counter()`, nothing else heavy
running. PNG "write" includes adaptive filtering, DEFLATE and CRCs; the
conversions include decoding, conversion and encoding through the registry.

Optimisations used: local-variable binding in hot loops, precomputed lookup
tables (Huffman decode tables, colour conversion, clamping), whole-row
big-integer SWAR arithmetic for PNG filters (including a vectorised Paeth
predictor in the encoder), slice/`translate` tricks for channel shuffles and
palettes, sparse-coefficient IDCT, and batched progress/cancel checks.

DEFLATE levels keep zlib's (good, lazy, nice, chain) shape with shorter chains
(level 6 searches 8 candidates instead of zlib's 128) because each hash-chain
step costs far more in Python. On filtered photo data our level 6 output is
about 5 % larger than `zlib -6`.
