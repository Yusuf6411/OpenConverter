# Architecture

OpenConverter is a single Python package (standard library only) plus a static
web front end. Everything runs on the user's machine.

```
openconverter/
├── __init__.py, __main__.py   version, `python -m openconverter`
├── cli.py                     serve (default) | convert | formats
├── config.py                  paths, limits, settings (atomic writes), logging
├── errors.py                  typed exceptions + error-code table
├── limits.py                  decoder resource limits
├── server/   app.py api.py multipart.py security.py static.py sse.py
├── core/     registry.py detect.py jobs.py progress.py storage.py history.py files.py
├── converters/ base.py image_converter.py text_converter.py table_converter.py
├── models/   image.py table.py progress.py
├── ops/      image_ops.py
└── codecs/   checksums.py deflate.py png.py jpeg/ gif.py bmp.py tga.py qoi.py netpbm.py
              text.py delimited.py jsontable.py
web/          index.html, css/app.css, js/{app,api,theme}.js, js/ui/*.js, favicon.svg
```

## Layering

```
server  ──▶  core  ──▶  converters  ──▶  codecs / ops / models
```

Lower layers never import upward:

* `codecs/` know nothing about HTTP, jobs or the registry. A decoder takes
  bytes (plus a `Progress` and `Limits`) and returns an `Image`; an encoder
  takes an `Image` and returns bytes, appending human-readable notes for every
  rule it applied.
* `models/progress.py` holds the tiny progress/cancellation interface so codecs
  depend on nothing above them; `core/progress.py` provides the job-aware
  implementation.
* `errors.py` and `limits.py` are leaf modules shared by all layers.

## Intermediate representations

**Image** (`models/image.py`): `width`, `height`, `mode` (`L`, `LA`, `RGB`,
`RGBA`), `bit_depth` (8 or 16), `pixels` (row-major, interleaved, top-down,
no padding; 16-bit samples big-endian) and `metadata` (source format, dpi,
gamma, orientation_applied, original bit depth, notes). The constructor checks
the buffer length. Palette and 1/2/4-bit sources are expanded by the reader.

**Table** (`models/table.py`): `header` (list of strings or `None`) and `rows`
(lists of strings). All cells are text.

Conversion rules between image modes and depths live in `ops/image_ops.py`
(alpha flattening, 16→8, 8→16, grey↔RGB, EXIF orientation, resizing) and are
documented in [FORMATS.md](FORMATS.md).

## Registry

`core/registry.py` holds one `FormatSpec` per format: id, name, category,
extensions, MIME type, `sniff()`, read/write flags, ownership (OWN / PARTIAL /
EXTERNAL), a variants note, and separate `read_options` / `write_options`
schemas (`type`, `min`, `max`, `default`, `label`, `choices`). Routing:

* any readable image → any writable image (through the Image IR);
* CSV ↔ TSV ↔ JSON (through the Table IR);
* TXT → TXT.

`GET /api/formats` serialises the registry; the web UI builds every format
list and options panel from it, so a new format appears in the UI without
JavaScript changes. Option values are validated against the schemas
(unknown keys, wrong types and out-of-range values are rejected).

## Job lifecycle

1. `POST /api/files` streams each multipart part to `uploads/<uuid>`, then
   `core/files.py` detects the format by content and writes a metadata
   sidecar (`<uuid>.json`).
2. `POST /api/jobs` validates options, fills unspecified ones from the user's
   settings, and submits the job to a `ThreadPoolExecutor`
   (`min(4, cpu_count)` workers). The HTTP thread never converts.
3. A worker decodes, converts and encodes, reporting progress through a
   `JobProgress`. States: `queued → running → done | failed | cancelled`.
4. The output is written to `temp/` and moved into `outputs/<uuid>` with
   `os.replace`, so a failed or cancelled job never leaves a partial file.
5. Every finished job appends a metadata-only line to `history.jsonl`.

Clients follow progress with Server-Sent Events (`GET /api/jobs/{id}/events`)
or by polling `GET /api/jobs/{id}`.

## Progress and cancellation contract

```python
class Progress:                      # models/progress.py
    def report(self, done: int, total: int, stage: str) -> None: ...
    def check_cancelled(self) -> None: ...   # raises ConversionCancelled
```

* Codecs call `report()` from their row/MCU/block loops and
  `check_cancelled()` every few rows (not per pixel).
* Converters split the work into stages (decoding → converting → encoding →
  finalizing) with `SubProgress`, weighted by each codec's measured cost.
* `JobProgress` clamps values to 0–100, never lets them decrease, and
  throttles notifications to at most 10 per second (stage changes always go
  through). `NullProgress` is used by the CLI and tests.

## Security model

Bound to 127.0.0.1 by default. Every request must carry a Host header naming
this server (DNS-rebinding defence); state-changing requests also need a
same-origin `Origin` (or `Referer`) and `X-OpenConverter: 1`. All responses
carry a strict Content-Security-Policy and related headers; the web UI uses no
inline scripts or styles. Uploaded files are stored under random UUIDs; user
file names are metadata only and are sanitised before use in
`Content-Disposition`.

## How to add a new format

Example: a hypothetical read/write "FOO" image format.

1. **Codec** — `openconverter/codecs/foo.py`:

   ```python
   from openconverter.errors import CorruptFile
   from openconverter.limits import DEFAULT_LIMITS, check_dimensions
   from openconverter.models.image import Image
   from openconverter.models.progress import NULL_PROGRESS
   from openconverter.ops import image_ops

   MAGIC = b"FOO1"

   def sniff(data: bytes) -> bool:
       return data[:4] == MAGIC

   def decode(data, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> Image:
       if len(data) < 12 or not sniff(data):
           raise CorruptFile("Not a FOO file.")
       w = int.from_bytes(data[4:8], "big")
       h = int.from_bytes(data[8:12], "big")
       check_dimensions(w, h, limits)          # before allocating anything
       body = data[12:12 + w * h * 3]
       if len(body) < w * h * 3:
           raise CorruptFile("FOO pixel data is truncated.")
       progress.report(1, 1, "decoding")
       return Image(w, h, "RGB", 8, bytearray(body), {"source_format": "foo"})

   def encode(img, progress=NULL_PROGRESS, notes=None, background=image_ops.WHITE, **_):
       img = image_ops.convert(img, "RGB", 8, background, notes)   # notes every rule
       progress.report(1, 1, "encoding")
       return MAGIC + img.width.to_bytes(4, "big") + img.height.to_bytes(4, "big") + bytes(img.pixels)
   ```

2. **Register it** — in `converters/image_converter.py` add a `FormatSpec` to
   `IMAGE_FORMATS` and the functions to `DECODERS`, `ENCODERS`,
   `DECODE_COST` and `ENCODE_COST`:

   ```python
   FormatSpec("foo", "FOO", "image", (".foo",), "image/x-foo", foo.sniff, True, True, OWN,
              "8-bit RGB only.", write_options={"background": BACKGROUND}),
   ```

3. **Detection** — add `"foo"` to `BINARY_ORDER` in `core/detect.py`.
4. **Tests** — add a fixture in `scripts/make_fixtures.py`, list it in
   `IMAGE_SOURCES` in `tests/integration/test_matrix.py` (the acceptance matrix
   iterates the registry, so every new pair is covered automatically), add the
   decoder to `tests/integration/test_fuzz.py`, and write codec unit tests.
5. **Docs** — describe variants and limitations in `docs/FORMATS.md`.

A non-image format (e.g. a document type) follows the same pattern with its
own IR and converter module; `Registry.convert()` dispatches on the category.
