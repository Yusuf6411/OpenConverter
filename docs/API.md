# HTTP API

Base URL: `http://127.0.0.1:8080` (or the `--host`/`--port` you chose).
All API responses are JSON (`application/json; charset=utf-8`) with
`Cache-Control: no-store`, except downloads, previews and the event stream.

## Request rules

* **Host check** — every request (including GET, SSE and downloads) must send
  `Host: 127.0.0.1:<port>`, `localhost:<port>` or `[::1]:<port>` (or the
  explicit `--host` value). Anything else gets `403 forbidden`.
* **CSRF** — every non-GET/HEAD request must send an `Origin` (or, as a
  fallback, `Referer`) matching the server origin, **and** `X-OpenConverter: 1`.
* JSON bodies need `Content-Type: application/json` and are limited to 1 MB.
  Chunked request bodies are rejected.
* IDs are 32 lowercase hex characters; anything else is `404`.

## Errors

```json
{"error": {"code": "unsupported_variant", "message": "This JPEG uses progressive encoding, which OpenConverter 1.0 doesn't support yet."}}
```

| Code | HTTP | Meaning |
|---|---|---|
| `bad_request` | 400 | invalid request, invalid/unknown option, bad JSON |
| `forbidden` | 403 | Host / Origin / X-OpenConverter check failed |
| `not_found` | 404 | unknown route or id, expired file |
| `method_not_allowed` | 405 | wrong HTTP method |
| `limit_exceeded` | 413 | upload size, pixel count, decompression bomb, table size |
| `unsupported_format` | 415 | file type not supported, or impossible conversion pair |
| `unsupported_variant` | 422 | supported format, unsupported feature (e.g. progressive JPEG) |
| `corrupt_file` | 422 | damaged or invalid file |
| `service_unavailable` | 503 | too many open progress streams |
| `internal_error` | 500 | unexpected (details only in the log file) |

A cancelled job reports `{"code": "cancelled"}` in its `error` field.
Responses never contain stack traces.

## Endpoints

### `GET /api/health`
```json
{"status": "ok", "version": "1.0.0"}
```

### `GET /api/formats`
The single source of truth for formats. Option defaults reflect the user's settings.
```json
{
  "formats": [
    {"id": "jpeg", "name": "JPEG", "category": "image", "extensions": [".jpg", ".jpeg", ".jpe", ".jfif"],
     "mime": "image/jpeg", "can_read": true, "can_write": true, "ownership": "OWN", "ownership_note": "",
     "variants_note": "Baseline (sequential Huffman) 8-bit ...",
     "read_options": {},
     "write_options": {
       "quality": {"type": "int", "label": "Quality", "min": 1, "max": 100, "default": 85, "help": "..."},
       "subsampling": {"type": "choice", "label": "Chroma subsampling", "choices": ["auto", "4:4:4", "4:2:0"], "default": "auto"},
       "optimize_huffman": {"type": "bool", "label": "Optimise Huffman tables", "default": false},
       "background": {"type": "colour", "label": "Background colour", "default": "#FFFFFF"}},
     "targets": ["png", "jpeg", "bmp", "tga", "qoi", "ppm", "pgm", "pbm"],
     "identity_meaningful": true}
  ],
  "categories": [{"id": "image", "name": "Images"}, {"id": "text", "name": "Text"}, {"id": "data", "name": "Tabular data"}]
}
```

### `POST /api/files`
`multipart/form-data` with one or more file parts. Bad files yield a per-file error entry.
```
POST /api/files
Content-Type: multipart/form-data; boundary=x
Origin: http://127.0.0.1:8080
X-OpenConverter: 1
```
```json
{"files": [
  {"file_id": "3f2a…", "original_name": "photo.png", "size": 52311, "detected_format": "png",
   "category": "image", "extension_mismatch": false, "available_targets": ["png", "jpeg", "bmp", "tga", "qoi", "ppm", "pgm", "pbm"],
   "uploaded": 1792354172.3, "width": 640, "height": 480, "may_have_alpha": true},
  {"original_name": "notes.pdf", "size": 1200, "error": {"code": "unsupported_format", "message": "PDF files are not supported in OpenConverter 1.0."}}
]}
```
Bodies over the `max_upload_mb` setting get `413 limit_exceeded` and leave no partial file.

### `GET /api/files/{file_id}`
The stored metadata of an upload (same shape as an entry above).

### `GET /api/files?ids=…&outputs=…` *(added for page-reload reconciliation)*
Batch existence check used by the UI after a reload, so expired files do not cause one `404` each.
```
GET /api/files?ids=3f2a…,0000…&outputs=77ab…
```
```json
{"files": {"3f2a…": {"file_id": "3f2a…", "detected_format": "png", "...": "..."}, "0000…": null},
 "outputs": {"77ab…": true}}
```

### `GET /api/files/{file_id}/preview`
PNG thumbnail (≤ 512 px) made with our own resize and PNG encoder. Images only (`415` otherwise).
If the image cannot be decoded (damaged, unsupported variant, over the limits) the response is
`204 No Content` with an `X-OpenConverter-Error: <code>` header; the conversion itself reports the error.

### `POST /api/jobs`
```json
{"file_id": "3f2a…", "target_format": "jpeg", "options": {"quality": 80}}
```
→ `201 {"job_id": "9c1d…"}`. Unknown fields or option keys, wrong types and
out-of-range values → `400`. Unspecified options use the user's settings
(JPEG quality, PNG level, background) or the schema defaults.

### `GET /api/jobs` *(added for page-reload reconciliation)*
```json
{"jobs": [{"job_id": "9c1d…", "state": "running", "progress": 42.5, "...": "..."}]}
```

### `GET /api/jobs/{job_id}`
```json
{"job_id": "9c1d…", "file_id": "3f2a…", "source_format": "png", "target_format": "jpeg",
 "original_name": "photo.png", "state": "done", "progress": 100, "stage": "done", "message": "",
 "result": {"output_id": "77ab…", "output_name": "photo.jpg", "size": 18234, "input_size": 52311,
            "duration_ms": 1412, "notes": ["Alpha flattened onto #FFFFFF.", "Chroma subsampling 4:2:0."],
            "info": {"width": 640, "height": 480, "mode": "RGBA", "bit_depth": 8}},
 "error": null, "created": 1792354172.4}
```
`state` is one of `queued`, `running`, `done`, `failed`, `cancelled`.

### `GET /api/jobs/{job_id}/events`
Server-Sent Events. Each event carries the same JSON as `GET /api/jobs/{id}`:
```
retry: 2000

id: 1
event: state
data: {"job_id":"9c1d…","state":"queued","progress":0,...}

id: 2
event: progress
data: {"job_id":"9c1d…","state":"running","progress":37.5,"stage":"encoding",...}

: heartbeat
```
A comment heartbeat is sent every 15 s; the stream ends after the final state.
At most 32 concurrent streams (`503` beyond that; clients fall back to polling).

### `POST /api/jobs/{job_id}/cancel`
Cancels a queued or running job; returns the job snapshot. No output is kept.

### `GET /api/outputs/{output_id}/download`
The converted file as an attachment:
`Content-Disposition: attachment; filename="photo.jpg"; filename*=UTF-8''photo.jpg`.
The name is `<original stem>.<new extension>`, sanitised (path separators,
control characters and reserved Windows names removed).

### `GET /api/outputs/{output_id}/preview`
PNG thumbnail of an image output (≤ 512 px), same `204` rule as above.

### `GET /api/history`
```json
{"entries": [{"id": "9c1d…", "job_id": "9c1d…", "original_name": "photo.png", "output_name": "photo.jpg",
  "source_format": "png", "target_format": "jpeg", "input_size": 52311, "output_size": 18234,
  "duration_ms": 1412, "status": "done", "timestamp": 1792354173.8, "notes": ["..."],
  "output_id": "77ab…", "error_code": null, "megapixels": 0.307, "expired": false}]}
```
Newest first. `expired: true` means the output file has been cleaned up.

### `DELETE /api/history` / `DELETE /api/history/{id}`
Clear all entries / delete one (`404` if it does not exist). Output files are
not deleted; they expire after `retention_hours`.

### `GET /api/settings` / `PUT /api/settings`
```json
{"theme": "system", "default_jpeg_quality": 85, "default_png_level": 6,
 "alpha_background": "#FFFFFF", "retention_hours": 24, "max_upload_mb": 200}
```
`PUT` accepts any subset; unknown keys or invalid values → `400`. Returns the full settings.
