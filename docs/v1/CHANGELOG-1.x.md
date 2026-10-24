# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [1.0.0] - 2026-10-18

First release: a free, open-source, offline file converter that runs on your
own machine with nothing installed beyond Python 3.12+.

### Highlights
- **Our own codecs, zero dependencies**: PNG (with our own DEFLATE, CRC-32 and
  Adler-32), baseline JPEG (encoder and decoder), GIF (own LZW), BMP, TGA,
  QOI and PBM/PGM/PPM — all written from scratch on the Python standard library.
- **Images**: every pair among PPM, PGM, PBM, BMP, PNG, JPEG, TGA and QOI, plus
  GIF → any of them. 16-bit and alpha are preserved where the target allows;
  every lossy rule (alpha flattening, bit-depth reduction, greyscale) is
  reported in the result notes.
- **Text and tables**: TXT re-encoding (UTF-8/UTF-16 with or without BOM,
  Latin-1, Windows-1252; LF/CRLF) and CSV ↔ TSV ↔ JSON with our own RFC 4180
  parser.
- **Web interface**: drag-and-drop queue with real progress bars (Server-Sent
  Events with polling fallback), per-format options generated from the
  server's schemas, batch actions, history, settings, light/dark themes,
  keyboard and screen-reader support.
- **CLI**: `python -m openconverter convert in.png out.jpg --quality 85`,
  `formats`, and `serve`.
- **Local and private by design**: binds to 127.0.0.1, DNS-rebinding and CSRF
  protection, strict Content-Security-Policy, no network access, no telemetry.
- **Robustness**: decoder limits checked before allocation, decompression-bomb
  guards, typed errors, and a seeded fuzz suite over every decoder.

### Known limitations
- Pure-Python codecs: large images take seconds to minutes (see
  `docs/FORMATS.md` for measured timings).
- Not yet supported: progressive JPEG, GIF writing, WebP, animated output,
  ICC colour management, EXIF preservation, PDF, DOCX, audio, video, archives.
