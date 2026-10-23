# OpenConverter

Convert anything. Keep everything local.

OpenConverter 2.x is a rewrite in Rust of the 1.x Python converter: a native engine with its own codecs, a command line, a local web UI and a desktop app, all working offline. Nothing is uploaded and nothing phones home. The 1.0.0 Python version is at tag `v1.0.0`.

## Use it

```
openconverter convert photo.png photo.webp --set quality=80
openconverter convert photo.jpg turned.jpg --set rotate=90 --set lossless=true
openconverter convert interview.wav interview.opus --set bitrate=64 --set channels=1 --set normalize=-16
openconverter convert song.flac song.mka          # moves the FLAC stream without re-encoding
openconverter formats
openconverter serve          # local web UI, opens in your browser
openconverter-desktop        # the same UI in its own window
```

What it can convert is listed in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md). Today that is images (PNG, JPEG, WebP, GIF, BMP, TGA, QOI, Netpbm, ICO, ICNS, DDS, PSD, HDR, EXR, TIFF, animations), audio (WAV, AIFF, CAF, FLAC, MP3, Ogg Vorbis, Opus, Apple Lossless in M4A, Matroska and WebM audio), text encodings, tables (CSV, TSV, JSON, XLSX, ODS) and archives (ZIP, TAR, 7z, CPIO, AR, CAB and ISO to read, and the gz, bz2, xz, zst, lz4 compressors). Each conversion runs in a separate worker process with a time and memory limit.

## Build

```
cargo build --release -p oc-cli          # CLI and web UI
cargo build --release -p oc-desktop      # needs WebKitGTK on Linux
cargo test --workspace
cargo xtask check-deps                   # no third-party codecs in the tree
```

## More

[docs/MASTER_PROMPT.md](docs/MASTER_PROMPT.md) is the plan, [docs/PROGRESS.md](docs/PROGRESS.md) the status, [docs/API.md](docs/API.md) the local API, [docs/DECISIONS.md](docs/DECISIONS.md) the decisions, [docs/formats/](docs/formats/README.md) what each format covers.

Python: see [docs/python.md](docs/python.md).
