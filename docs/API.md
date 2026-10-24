# OpenConverter 2 local API

`openconverter serve` (and the desktop app) expose a small JSON API on the loopback address. The web UI is a client of this API and nothing else. Everything runs on your computer; nothing is sent anywhere.

## Access rules

- Default bind: `127.0.0.1` on a free port. Binding to another address needs `--allow-remote` and prints an access token; the token is accepted once as `/?token=...` and then kept in an `HttpOnly; SameSite=Strict` cookie.
- The `Host` header must be an allowed loopback name (blocks DNS rebinding).
- Any request that is not GET/HEAD/OPTIONS must carry `X-OpenConverter: 1` and an `Origin` (or `Referer`) equal to the server's own address (CSRF).
- Every response carries a strict Content-Security-Policy, `nosniff`, `no-referrer` and `same-origin` resource policy.
- Errors are `{"error": {"code": "...", "message": "..."}}` with a matching HTTP status. Messages are written for people, codes are stable.
- Ids (files, jobs, outputs) are random hex strings; client file names are never used as paths.

## Endpoints

| Method and path | Purpose |
| --- | --- |
| `GET /api/health` | `{"status":"ok","version":"..."}` |
| `GET /api/formats` | Every format with `id`, `name`, `category`, `extensions`, `mime`, `can_read`, `can_write`, `write_options` (the option schema), `targets` (what it can be converted to). Also `categories`. |
| `GET /api/settings`, `PUT /api/settings` | Read and change `theme`, `default_jpeg_quality`, `default_png_level`, `alpha_background`, `retention_hours`, `max_upload_mb`. Unknown keys and out-of-range values are 400. |
| `POST /api/files` | Multipart upload (field name is free; one or more files). Streamed to disk, size checked against `max_upload_mb`. Returns `{"files":[{id, original_name, size, format, targets, ...}]}`; a file that cannot be recognised comes back with an `error` entry instead. |
| `GET /api/files?name=...` | Look up an uploaded file. |
| `GET /api/files/{id}` | Metadata and probe results. |
| `GET /api/files/{id}/preview` | Thumbnail (PNG) produced in a sandboxed worker. |
| `POST /api/jobs` | Body `{"file_id","target_format","options":{...}}`, unknown fields are rejected. Options are validated against the target format's schema; the user's saved defaults fill in what is left out. Returns 201 `{"job_id"}`. |
| `POST /api/files/{id}/entries` | Body `{"password"?}`. Lists an uploaded archive without unpacking it: `{format, count, total_size, encrypted, entries:[{path, kind: file\|dir\|link, size, mode, mtime, encrypted, link}]}`. 401 `encrypted_need_password` / `wrong_password`, 415 for a file that is not an archive. |
| `POST /api/files/{id}/extract` | Body `{"paths"?: [...], "password"?}`. Unpacks the chosen entries (everything when `paths` is empty) in a sandboxed worker that refuses unsafe paths and links. Returns `{output_id, output_name, size, files, skipped}`; a single file comes back as itself, anything else as a ZIP, downloadable through `/api/outputs/{id}/download`. |
| `POST /api/archives` | Body `{"file_ids":[...],"target_format":"zip","options":{...},"name"?}`. Packs uploads under their original names (clashes become `name (2).ext`) into one archive. A job like `POST /api/jobs`; returns 201 `{"job_id"}`. |
| `GET /api/jobs`, `GET /api/jobs/{id}` | Job state: `queued`, `running`, `done`, `error`, `cancelled`, with progress, notes (warnings and substitutions) and output info. |
| `GET /api/jobs/{id}/events` | Server-sent events with the same job object on every change. |
| `POST /api/jobs/{id}/cancel` | Stops the worker process. |
| `GET /api/outputs/{id}/download` | The converted file with a safe `Content-Disposition`. |
| `GET /api/outputs/{id}/preview` | Output thumbnail. |
| `GET /api/history`, `DELETE /api/history`, `DELETE /api/history/{id}` | Past conversions, kept for `retention_hours`. |

## Isolation

Each conversion, probe and thumbnail runs in a separate worker process with a memory ceiling, a time limit and (where the OS supports it) a syscall and filesystem sandbox. A crash in a decoder cannot reach the server.
