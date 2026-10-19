# OpenConverter

Convert anything. Keep everything local.

OpenConverter 2.x is a rewrite in Rust of the 1.x Python converter: a native engine with its own codecs, a CLI and a desktop app, working fully offline. It is at the very start (Phase 0: workspace skeleton). The 1.0.0 Python version is available at tag `v1.0.0`.

See [docs/MASTER_PROMPT.md](docs/MASTER_PROMPT.md) for the plan and [docs/PROGRESS.md](docs/PROGRESS.md) for current status.

## Try it (Phase 1)

```
cargo run -p oc-cli -- convert notes.txt notes-utf16.txt --set to_encoding=utf-16le --set bom=true
cargo run -p oc-cli -- formats
```

Only plain-text conversion exists so far.
