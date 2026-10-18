"""Command-line interface: serve (default) | convert | formats."""

import argparse
import json
import os
import sys
import threading
import time
import webbrowser
from pathlib import Path

from openconverter import __version__
from openconverter.errors import BadRequest, ConversionCancelled, OpenConverterError

EXIT_OK, EXIT_FAIL, EXIT_USAGE, EXIT_INTERRUPTED = 0, 1, 2, 130

# CLI flag -> option key. Flags only apply when the chosen formats define the key.
OPTION_FLAGS = [
    ("--quality", "quality", int, "JPEG quality 1-100"),
    ("--subsampling", "subsampling", str, "JPEG chroma subsampling: auto, 4:4:4, 4:2:0"),
    ("--optimize-huffman", "optimize_huffman", "flag", "JPEG: build optimal Huffman tables"),
    ("--level", "compression_level", int, "PNG compression level 0-9"),
    ("--palette", "palette", str, "PNG palette: auto, off, on"),
    ("--rle", "rle", "flag", "TGA: RLE compression"),
    ("--ascii", "ascii", "flag", "PBM/PGM/PPM: plain (ASCII) encoding"),
    ("--background", "background", str, "Colour for flattening transparency, e.g. #ffffff"),
    ("--indent", "indent", int, "JSON indent: 0, 2 or 4"),
    ("--ensure-ascii", "ensure_ascii", "flag", "JSON: escape non-ASCII characters"),
    ("--delimiter", "delimiter", str, "CSV output delimiter: comma, semicolon, tab, pipe"),
    ("--source-delimiter", "source_delimiter", str, "CSV input delimiter: auto, comma, semicolon, tab, pipe"),
    ("--no-header", "has_header", "noflag", "CSV/TSV input has no header row"),
    ("--bom", "bom", "flag", "CSV/TSV: write a UTF-8 BOM"),
    ("--newline", "newline", str, "Line endings: lf, crlf (TXT also: preserve)"),
    ("--to-encoding", "to_encoding", str, "TXT output encoding (utf-8, utf-8-bom, utf-16le, ...)"),
    ("--source-encoding", "source_encoding", str, "Fallback source encoding for text input"),
    ("--unencodable", "unencodable", str, "TXT: error or replace"),
]


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="openconverter",
                                     description="OpenConverter: convert anything, keep everything local.")
    parser.add_argument("--version", action="version", version=f"OpenConverter {__version__}")
    sub = parser.add_subparsers(dest="command")

    serve = sub.add_parser("serve", help="Run the local web interface (default).")
    _add_serve_args(serve)

    conv = sub.add_parser("convert", help="Convert one file.")
    conv.add_argument("input", help="Input file (format detected from its content).")
    conv.add_argument("output", help="Output file (format taken from the extension unless --to is given).")
    conv.add_argument("--to", help="Target format id (png, jpeg, csv, ...).")
    conv.add_argument("--force", action="store_true", help="Overwrite the output if it exists.")
    for flag, key, kind, help_text in OPTION_FLAGS:
        if kind in ("flag", "noflag"):
            conv.add_argument(flag, dest="opt_" + key, action="store_true", help=help_text)
        else:
            conv.add_argument(flag, dest="opt_" + key, type=kind, help=help_text)

    fmts = sub.add_parser("formats", help="List supported formats.")
    fmts.add_argument("--json", action="store_true", help="Machine-readable output.")
    return parser


def _add_serve_args(p):
    p.add_argument("--host", default="127.0.0.1", help="Interface to bind (default 127.0.0.1).")
    p.add_argument("--port", type=int, default=8080, help="Port (default 8080).")
    p.add_argument("--no-browser", action="store_true", help="Do not open a browser window.")
    p.add_argument("--data-dir", help="Where uploads, outputs, settings and history are stored.")


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else list(argv)
    for stream in (sys.stdout, sys.stderr):
        try:  # never crash on a file name the console encoding cannot show
            stream.reconfigure(errors="replace")
        except (AttributeError, ValueError):
            pass
    parser = build_parser()
    # `openconverter` alone (or with only serve flags) means `serve`.
    if not argv or argv[0].startswith("-") and argv[0] not in ("-h", "--help", "--version"):
        argv = ["serve"] + argv
    args = parser.parse_args(argv)
    if args.command == "serve":
        return cmd_serve(args)
    if args.command == "convert":
        return cmd_convert(args)
    if args.command == "formats":
        return cmd_formats(args)
    parser.print_help()
    return EXIT_USAGE


# --------------------------------------------------------------------------
# serve
# --------------------------------------------------------------------------

def cmd_serve(args) -> int:
    from openconverter.config import default_data_dir
    from openconverter.server.app import App

    if not 0 < args.port < 65536:
        print("error: --port must be between 1 and 65535", file=sys.stderr)
        return EXIT_USAGE
    data_dir = Path(args.data_dir) if args.data_dir else default_data_dir()
    host = args.host
    if host not in ("127.0.0.1", "localhost", "::1"):
        bar = "!" * 72
        print(f"\n{bar}\n  WARNING: binding to {host!r} exposes OpenConverter beyond this computer.\n"
              f"  Anyone who can reach this address can upload files and run conversions.\n"
              f"  Only do this on a network you trust.\n{bar}\n", file=sys.stderr)
    bind_host = "127.0.0.1" if host == "localhost" else host
    app = App(data_dir, bind_host, args.port)
    try:
        app.bind()
    except OSError as exc:
        app.jobs.shutdown(wait=False)
        print(f"error: cannot listen on {host}:{args.port} ({exc.strerror or exc}).\n"
              f"Another program may be using this port. Try another one, for example:\n"
              f"    python -m openconverter serve --port {args.port + 1}", file=sys.stderr)
        return EXIT_FAIL
    print(f"OpenConverter {__version__} running at {app.url}")
    print(f"Data folder: {data_dir}")
    print("Press Ctrl+C to stop.")
    sys.stdout.flush()
    if not args.no_browser:
        threading.Timer(0.5, lambda: webbrowser.open(app.url)).start()
    try:
        app.serve_forever()
    except KeyboardInterrupt:
        print("\nStopping...", file=sys.stderr)
        app.shutdown()
    return EXIT_OK


# --------------------------------------------------------------------------
# convert
# --------------------------------------------------------------------------

class _TtyProgress:
    def __init__(self, stream):
        self.stream = stream
        self.enabled = stream.isatty()
        self.last = 0.0
        self.pct = 0.0

    def report(self, done, total, stage):
        if not self.enabled:
            return
        pct = 0.0 if total <= 0 else min(100.0, 100.0 * done / total)
        self.pct = max(self.pct, pct)
        now = time.monotonic()
        if now - self.last >= 0.1:
            self.last = now
            self.stream.write(f"\r[{self.pct:5.1f}%] {stage:<12}")
            self.stream.flush()

    def check_cancelled(self):
        pass

    def finish(self):
        if self.enabled:
            self.stream.write("\r" + " " * 24 + "\r")
            self.stream.flush()


def cmd_convert(args) -> int:
    from openconverter.core import detect
    from openconverter.core.registry import REGISTRY

    src_path = Path(args.input)
    out_path = Path(args.output)
    if not src_path.is_file():
        print(f"error: input file not found: {src_path}", file=sys.stderr)
        return EXIT_USAGE
    if out_path.exists() and not args.force:
        print(f"error: {out_path} already exists (use --force to overwrite).", file=sys.stderr)
        return EXIT_USAGE
    try:
        if args.to:
            target = REGISTRY.get(args.to.lower()).id
        else:
            spec = REGISTRY.by_extension(out_path.suffix)
            if spec is None:
                print(f"error: cannot infer the target format from {out_path.name!r}; use --to.", file=sys.stderr)
                return EXIT_USAGE
            target = spec.id
    except OpenConverterError as exc:
        print(f"error: {exc.message}", file=sys.stderr)
        return EXIT_USAGE
    progress = _TtyProgress(sys.stderr)
    try:
        data = src_path.read_bytes()
        source = detect.detect(data, src_path.name)
        if target not in REGISTRY.targets(source):
            print(f"error: cannot convert {source} to {target}.", file=sys.stderr)
            return EXIT_FAIL
        schema = REGISTRY.option_schema(source, target)
        options = {}
        for _flag, key, kind, _ in OPTION_FLAGS:
            value = getattr(args, "opt_" + key)
            if kind == "flag":
                value = True if value else None
            elif kind == "noflag":
                value = False if value else None
            if value is None:
                continue
            if key not in schema:
                print(f"error: option {_flag} does not apply to {source} -> {target}.", file=sys.stderr)
                return EXIT_USAGE
            options[key] = value
        try:
            options = REGISTRY.validate_options(source, target, options)
        except BadRequest as exc:
            print(f"error: {exc.message}", file=sys.stderr)
            return EXIT_USAGE
        full = REGISTRY.defaults(source, target)
        full.update(options)
        t0 = time.perf_counter()
        result = REGISTRY.convert(data, source, target, full, progress)
        elapsed = time.perf_counter() - t0
        progress.finish()
        tmp = out_path.with_name(f".{out_path.name}.{os.getpid()}.part")
        tmp.write_bytes(result.data)
        os.replace(tmp, out_path)
    except KeyboardInterrupt:
        progress.finish()
        print("interrupted", file=sys.stderr)
        return EXIT_INTERRUPTED
    except ConversionCancelled:
        return EXIT_INTERRUPTED
    except OpenConverterError as exc:
        progress.finish()
        print(f"error: {exc.message}", file=sys.stderr)
        return EXIT_FAIL
    except OSError as exc:
        progress.finish()
        print(f"error: {exc.strerror or exc}", file=sys.stderr)
        return EXIT_FAIL
    print(f"{src_path.name} ({source}) -> {out_path.name} ({target}): {len(data):,} -> {len(result.data):,} bytes "
          f"in {elapsed:.2f} s")
    for note in result.notes:
        print(f"  note: {note}")
    return EXIT_OK


# --------------------------------------------------------------------------
# formats
# --------------------------------------------------------------------------

def cmd_formats(args) -> int:
    from openconverter.core.registry import REGISTRY
    doc = REGISTRY.to_json()
    if args.json:
        print(json.dumps(doc, indent=2))
        return EXIT_OK
    print(f"{'ID':<6} {'NAME':<12} {'CATEGORY':<8} {'READ':<5} {'WRITE':<6} {'OWNERSHIP':<9} EXTENSIONS")
    for f in doc["formats"]:
        print(f"{f['id']:<6} {f['name']:<12} {f['category']:<8} {'yes' if f['can_read'] else 'no':<5} "
              f"{'yes' if f['can_write'] else 'no':<6} {f['ownership']:<9} {' '.join(f['extensions'])}")
    return EXIT_OK
