# OpenConverter

Convert anything. Keep everything local.

OpenConverter 2.x is a rewrite in Rust of the 1.x Python converter: a native engine with its own codecs, a command line, a local web UI and a desktop app, all working offline. Nothing is uploaded and nothing phones home. The 1.0.0 Python version is at tag `v1.0.0`.

## Use it

```
openconverter convert photo.png photo.webp --set quality=80
openconverter convert photo.jpg turned.jpg --set rotate=90 --set lossless=true
openconverter formats
openconverter serve          # local web UI, opens in your browser
openconverter-desktop        # the same UI in its own window
```

What it can convert is listed in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md). Today that is images (PNG, JPEG, WebP, GIF, BMP, TGA, QOI, Netpbm, ICO, ICNS, DDS, PSD, HDR, EXR, TIFF, animations), text encodings and tables (CSV, TSV, JSON). Each conversion runs in a separate worker process with a time and memory limit.

## Build

```
cargo build --release -p oc-cli          # CLI and web UI
cargo build --release -p oc-desktop      # needs WebKitGTK on Linux
cargo test --workspace
cargo xtask check-deps                   # no third-party codecs in the tree
```

## More

[docs/MASTER_PROMPT.md](docs/MASTER_PROMPT.md) is the plan, [docs/PROGRESS.md](docs/PROGRESS.md) the status, [docs/API.md](docs/API.md) the local API, [docs/DECISIONS.md](docs/DECISIONS.md) the decisions.
