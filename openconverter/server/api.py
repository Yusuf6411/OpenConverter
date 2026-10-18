"""JSON API endpoint handlers. Each handler receives (request, app, *groups)
and returns either a dict (JSON 200) or a Response."""

import json
import os
import re

from openconverter import __version__
from openconverter.converters.image_converter import decode_image, thumbnail_png
from openconverter.core import files
from openconverter.core.registry import REGISTRY
from openconverter.core.storage import check_id, new_id
from openconverter.errors import (BadRequest, LimitExceeded, NotFound, OpenConverterError, ServiceUnavailable,
                                  UnsupportedFormat)
from openconverter.server import multipart, security, sse


class Response:
    def __init__(self, status=200, body=b"", content_type="application/json; charset=utf-8", headers=None,
                 file_path=None, stream=None):
        self.status = status
        self.body = body
        self.content_type = content_type
        self.headers = headers or {}
        self.file_path = file_path   # stream a file from disk
        self.stream = stream         # callable(write) for SSE


def health(req, app):
    return {"status": "ok", "version": __version__}


def formats(req, app):
    return REGISTRY.to_json(app.settings.option_defaults())


def upload_files(req, app):
    ctype = req.headers.get("Content-Type", "")
    boundary = multipart.get_boundary(ctype)
    length = req.content_length()
    limit = app.settings.get()["max_upload_mb"] * 1024 * 1024
    # multipart framing overhead is small; refuse obviously oversized bodies up front
    if length > limit + 1024 * 1024:
        req.must_close = True
        raise LimitExceeded(f"The upload is larger than the {app.settings.get()['max_upload_mb']} MB limit.")
    reader = multipart.MultipartReader(req.rfile, length, boundary, max_parts=100)
    results = []
    created = []
    total = 0
    try:
        for part in reader.parts():
            if part.filename is None:
                for chunk in reader.read_part():  # ignore non-file fields (bounded)
                    total += len(chunk)
                    if total > 64 * 1024:
                        raise BadRequest("Unexpected form field data.")
                continue
            file_id = new_id()
            path = app.storage.upload_path(file_id)
            created.append(path)
            size = 0
            with open(path, "wb") as fh:
                for chunk in reader.read_part():
                    size += len(chunk)
                    total += len(chunk)
                    if size > limit or total > limit:
                        raise LimitExceeded(
                            f"The upload is larger than the {app.settings.get()['max_upload_mb']} MB limit.")
                    fh.write(chunk)
            if size == 0:
                os.unlink(path)
                created.remove(path)
                results.append({"original_name": files.clean_display_name(part.filename), "size": 0,
                                "error": {"code": "bad_request", "message": "The file is empty."}})
                continue
            entry = files.finalize_upload(app.storage, file_id, part.filename, size)
            if "error" in entry:
                created.remove(path)
            results.append(entry)
    except OpenConverterError:
        req.must_close = True
        for p in created:
            for q in (p, p.with_name(p.name + ".json")):
                try:
                    os.unlink(q)
                except OSError:
                    pass
        raise
    if not results:
        raise BadRequest("No files were uploaded.")
    return {"files": results}


def file_info(req, app, file_id):
    return app.storage.upload_meta(file_id)


def _preview(app, kind, item_id, data_loader, fmt):
    spec = REGISTRY.get(fmt)
    if spec.category != "image":
        raise UnsupportedFormat("Previews are only available for images.")
    cache = app.storage.cached_preview(kind, item_id)
    if not cache.is_file():
        img = decode_image(data_loader(), fmt, limits=app.limits)
        data = thumbnail_png(img, 512)
        tmp = cache.with_name(cache.name + f".{new_id()}.part")
        tmp.write_bytes(data)
        os.replace(tmp, cache)
    return Response(200, content_type="image/png", file_path=cache, headers={"Cache-Control": "no-store"})


def file_preview(req, app, file_id):
    meta = app.storage.upload_meta(file_id)
    return _preview(app, "upload", file_id, lambda: app.storage.read_upload(file_id), meta["detected_format"])


def output_preview(req, app, output_id):
    meta = app.storage.output_meta(output_id)
    path = app.storage.output_path(output_id)
    return _preview(app, "output", output_id, path.read_bytes, meta["format"])


def output_download(req, app, output_id):
    meta = app.storage.output_meta(output_id)
    path = app.storage.output_path(output_id)
    return Response(200, content_type=meta.get("mime", "application/octet-stream"), file_path=path,
                    headers={"Content-Disposition": security.content_disposition(meta.get("output_name", "download")),
                             "Cache-Control": "no-store"})


def create_job(req, app):
    body = req.json_body()
    if not isinstance(body, dict):
        raise BadRequest("Expected a JSON object.")
    unknown = set(body) - {"file_id", "target_format", "options"}
    if unknown:
        raise BadRequest(f"Unknown field(s): {', '.join(sorted(unknown))}.")
    file_id = body.get("file_id")
    target = body.get("target_format")
    if not isinstance(file_id, str) or not isinstance(target, str):
        raise BadRequest("file_id and target_format are required strings.")
    job = app.jobs.submit(file_id, target, body.get("options") or {})
    return Response(201, json.dumps({"job_id": job.id}).encode("utf-8"))


def list_jobs(req, app):
    return {"jobs": [j.snapshot() for j in app.jobs.list()]}


def job_status(req, app, job_id):
    return app.jobs.get(job_id).snapshot()


def job_events(req, app, job_id):
    app.jobs.get(job_id)  # 404 before switching to a stream
    if not app.sse_slots.acquire(blocking=False):
        raise ServiceUnavailable("Too many open progress streams.")

    def stream(write):
        try:
            sse.stream_job(write, app.jobs, job_id, app.stopping)
        finally:
            app.sse_slots.release()

    return Response(200, content_type="text/event-stream; charset=utf-8", stream=stream,
                    headers={"Cache-Control": "no-store", "X-Accel-Buffering": "no"})


def cancel_job(req, app, job_id):
    req.ensure_no_body()
    return app.jobs.cancel(job_id).snapshot()


def history(req, app):
    return {"entries": app.history.entries(app.storage.output_exists)}


def clear_history(req, app):
    req.ensure_no_body()
    app.history.clear()
    return {"ok": True}


def delete_history(req, app, entry_id):
    req.ensure_no_body()
    check_id(entry_id)
    if not app.history.delete(entry_id):
        raise NotFound("History entry not found.")
    return {"ok": True}


def get_settings(req, app):
    return app.settings.get()


def put_settings(req, app):
    body = req.json_body()
    return app.settings.update(body)


ID = r"([0-9a-f]{32})"
ROUTES = [
    ("GET", r"/api/health", health),
    ("GET", r"/api/formats", formats),
    ("POST", r"/api/files", upload_files),
    ("GET", rf"/api/files/{ID}", file_info),
    ("GET", rf"/api/files/{ID}/preview", file_preview),
    ("POST", r"/api/jobs", create_job),
    ("GET", r"/api/jobs", list_jobs),
    ("GET", rf"/api/jobs/{ID}", job_status),
    ("GET", rf"/api/jobs/{ID}/events", job_events),
    ("POST", rf"/api/jobs/{ID}/cancel", cancel_job),
    ("GET", rf"/api/outputs/{ID}/download", output_download),
    ("GET", rf"/api/outputs/{ID}/preview", output_preview),
    ("GET", r"/api/history", history),
    ("DELETE", r"/api/history", clear_history),
    ("DELETE", rf"/api/history/{ID}", delete_history),
    ("GET", r"/api/settings", get_settings),
    ("PUT", r"/api/settings", put_settings),
]
COMPILED = [(m, re.compile("^" + p + "$"), h) for m, p, h in ROUTES]
