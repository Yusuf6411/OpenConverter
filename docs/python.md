# Python package

`crates/oc-python` builds the `openconverter` package (PyO3, abi3, CPython 3.9 and later) with maturin.

```python
import openconverter as oc
r = oc.convert("in.png", "out.webp", quality=80, overwrite=True)
r.route, r.bytes_in, r.bytes_out, r.notes   # notes: list of dicts (severity, code, message, detail)
oc.info("in.png")     # format, name, category, size, width, height, alpha
oc.formats()          # id, name, category, extensions, mime, read, write
```

- Keyword arguments are format options; booleans become `true`/`false`. A wrong option or value raises `ConversionError`.
- `ConversionError.code` is the stable code (`output_exists`, `unsupported_format`, ...), `.detail` extra text. A missing file raises `OSError` from `info`.
- Calls release the GIL, so conversions can run on threads (`tests/test_openconverter.py` checks that another thread keeps running).
- Type stubs and `py.typed` are included.

## Build and test
```
pip install maturin
cd crates/oc-python
maturin build --release --out wheels
pip install wheels/*.whl
python -m unittest discover -s tests
```

## Security
Conversions run inside the Python process. The command line and the server run decoders in a sandboxed worker process; the Python package does not. Convert files you do not trust with the command line, or from a subprocess of your own.

## Not done
- `open_image` returning a buffer-protocol object (pixels without a file): it needs a raw-pixel API in the engine and `unsafe` buffer code in the binding.
- Progress callbacks and cancellation from Python.
- Wheels for manylinux 2_28, musllinux, aarch64 and universal2 macOS: the CI jobs build wheels for the runner's platform only, and only a Linux x86_64 wheel was built and tested here.
- Publishing to PyPI: waits for the owner's confirmation and trusted-publisher setup. Nothing is published.
