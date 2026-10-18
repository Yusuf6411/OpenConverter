"""HTTP server: ThreadingHTTPServer, router, security layer, lifecycle."""

import json
import logging
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

from openconverter import __version__
from openconverter.config import (CLEANUP_INTERVAL_SECONDS, MAX_JSON_BODY, MAX_SSE_STREAMS, SOCKET_TIMEOUT, Settings,
                                  setup_logging)
from openconverter.core.history import History
from openconverter.core.jobs import JobManager
from openconverter.core.storage import Storage
from openconverter.errors import (BadRequest, LimitExceeded, MethodNotAllowed, NotFound, OpenConverterError,
                                  error_body)
from openconverter.limits import DEFAULT_LIMITS
from openconverter.server import api, security, static

log = logging.getLogger("openconverter.server")
QUIET_ERRORS = (BrokenPipeError, ConnectionResetError, ConnectionAbortedError, socket.timeout, TimeoutError)


class App:
    """Everything a running server needs; created once per process (or test)."""

    def __init__(self, data_dir: Path, host: str = "127.0.0.1", port: int = 8080, max_workers=None,
                 limits=DEFAULT_LIMITS, web_root: Path | None = None) -> None:
        self.data_dir = Path(data_dir)
        self.host = host
        self.port = port
        self.limits = limits
        self.web_root = web_root or static.WEB_ROOT
        self.storage = Storage(self.data_dir)
        setup_logging(self.storage.logs)
        self.settings = Settings(self.data_dir / "settings.json")
        self.history = History(self.data_dir / "history.jsonl")
        self.jobs = JobManager(self.storage, self.settings, self.history, max_workers, limits)
        self.sse_slots = threading.BoundedSemaphore(MAX_SSE_STREAMS)
        self.stopping = threading.Event()
        self.server = None
        self.allowed = security.allowed_hosts(host, port)
        self._cleanup_thread = None

    def bind(self) -> "OCServer":
        self.server = OCServer((self.host, self.port), Handler, self)
        # The real port (when 0 was requested, e.g. in tests).
        self.port = self.server.server_address[1]
        self.allowed = security.allowed_hosts(self.host, self.port)
        return self.server

    @property
    def url(self) -> str:
        host = self.host if self.host not in ("", "0.0.0.0", "::") else "127.0.0.1"
        if ":" in host:
            host = f"[{host}]"
        return f"http://{host}:{self.port}/"

    def start_background(self) -> None:
        self.run_cleanup()
        self._cleanup_thread = threading.Thread(target=self._cleanup_loop, name="oc-cleanup", daemon=True)
        self._cleanup_thread.start()

    def run_cleanup(self) -> None:
        try:
            removed = self.storage.cleanup(self.settings.get()["retention_hours"], self.jobs.active_ids())
            if removed:
                log.info("cleanup removed %d old files", removed)
        except Exception:  # noqa: BLE001
            log.exception("cleanup failed")

    def _cleanup_loop(self) -> None:
        while not self.stopping.wait(CLEANUP_INTERVAL_SECONDS):
            self.run_cleanup()

    def serve_forever(self) -> None:
        if self.server is None:
            self.bind()
        self.start_background()
        try:
            self.server.serve_forever(poll_interval=0.25)
        finally:
            self.shutdown()

    def serve_in_thread(self) -> threading.Thread:
        if self.server is None:
            self.bind()
        self.start_background()
        t = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05},
                             name="oc-http", daemon=True)
        t.start()
        return t

    def shutdown(self) -> None:
        if self.stopping.is_set():
            return
        self.stopping.set()
        if self.server is not None:
            try:
                self.server.shutdown()
            except Exception:  # noqa: BLE001
                pass
        self.jobs.shutdown(wait=True)
        if self.server is not None:
            self.server.server_close()
        for p in self.storage.temp.glob("*.part"):
            try:
                p.unlink()
            except OSError:
                pass
        logging.getLogger("openconverter").info("server stopped")


class OCServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = False  # a busy port must fail loudly (and never be shared on Windows)
    request_queue_size = 64

    def __init__(self, address, handler, app: App):
        self.app = app
        if ":" in address[0]:
            self.address_family = socket.AF_INET6
        super().__init__(address, handler)

    def server_bind(self):
        if sys.platform == "win32" and hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        super().server_bind()

    def handle_error(self, request, client_address):
        exc = sys.exc_info()[1]
        if isinstance(exc, QUIET_ERRORS):
            return
        log.exception("unhandled server error")


class _BodyReader:
    """Wraps the socket file so we always know how much of the body is unread."""

    def __init__(self, raw, length: int) -> None:
        self.raw = raw
        self.remaining = length

    def read(self, n: int = -1) -> bytes:
        if self.remaining <= 0:
            return b""
        if n < 0 or n > self.remaining:
            n = self.remaining
        data = self.raw.read(n)
        self.remaining -= len(data)
        if not data:
            self.remaining = 0
        return data

    def drain(self, max_bytes: int, max_seconds: float) -> bool:
        """Discard the rest of the body (bounded). True if fully drained."""
        deadline = time.monotonic() + max_seconds
        budget = max_bytes
        while self.remaining > 0 and budget > 0 and time.monotonic() < deadline:
            chunk = self.read(min(256 * 1024, budget))
            if not chunk:
                break
            budget -= len(chunk)
        return self.remaining == 0


DRAIN_MAX_BYTES = 64 * 1024 * 1024
DRAIN_MAX_SECONDS = 15.0


class Request:
    def __init__(self, handler: "Handler"):
        self.h = handler
        self.headers = handler.headers
        self.rfile = handler.rfile
        self.body = None
        self.must_close = False
        self.body_read = False
        self.query = parse_qs(urlsplit(handler.path).query, keep_blank_values=True)

    def content_length(self) -> int:
        if "Transfer-Encoding" in self.headers:
            self.must_close = True
            raise BadRequest("Chunked request bodies are not supported.")
        raw = self.headers.get("Content-Length")
        if raw is None:
            return 0
        try:
            n = int(raw)
        except ValueError:
            self.must_close = True
            raise BadRequest("Invalid Content-Length.") from None
        if n < 0:
            self.must_close = True
            raise BadRequest("Invalid Content-Length.")
        self.body_read = True  # the caller takes responsibility for the body
        if self.body is None:
            self.body = _BodyReader(self.h.rfile, n)
            self.rfile = self.body
        return n

    def json_body(self):
        main = self.headers.get("Content-Type", "").split(";", 1)[0].strip().lower()
        if main != "application/json":
            self.must_close = True
            raise BadRequest("Expected Content-Type: application/json.")
        n = self.content_length()
        if n > MAX_JSON_BODY:
            self.must_close = True
            raise LimitExceeded("The request body is too large.")
        raw = self.rfile.read(n) if n else b""
        if len(raw) < n:
            self.must_close = True
            raise BadRequest("The request body ended unexpectedly.")
        try:
            return json.loads(raw.decode("utf-8")) if raw else {}
        except (UnicodeDecodeError, ValueError):
            raise BadRequest("The request body is not valid JSON.") from None

    def ensure_no_body(self):
        n = self.content_length()
        if n:
            if n > MAX_JSON_BODY:
                self.must_close = True
                raise LimitExceeded("The request body is too large.")
            self.rfile.read(n)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "OpenConverter/" + __version__
    sys_version = ""
    timeout = SOCKET_TIMEOUT

    # ---- logging: request lines only (ids, never user paths or contents) ----
    def log_message(self, fmt, *args):
        log.info("%s %s", self.address_string(), fmt % args)

    def log_error(self, fmt, *args):
        log.warning("%s %s", self.address_string(), fmt % args)

    def send_error(self, code, message=None, explain=None):
        """Low-level protocol errors (bad request line, oversized headers,
        unsupported method) also get JSON bodies and security headers."""
        codes = {400: "bad_request", 404: "not_found", 405: "method_not_allowed", 408: "timeout",
                 413: "limit_exceeded", 414: "limit_exceeded", 431: "limit_exceeded", 501: "not_implemented"}
        body = json.dumps({"error": {"code": codes.get(code, "bad_request"),
                                     "message": message or "The request could not be processed."}}).encode("utf-8")
        if getattr(self, "request_version", "HTTP/0.9") == "HTTP/0.9":
            self.request_version = "HTTP/1.0"  # HTTP/0.9 is not supported: always send a status line
        try:
            self.send_response(code, message)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            for k, v in security.BASE_HEADERS.items():
                self.send_header(k, v)
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "close")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if self.command != "HEAD" and code >= 200 and code not in (204, 304):
                self.wfile.write(body)
        except QUIET_ERRORS:
            pass
        self.close_connection = True

    def do_GET(self):
        self._dispatch("GET")

    def do_HEAD(self):
        self._dispatch("HEAD")

    def do_POST(self):
        self._dispatch("POST")

    def do_PUT(self):
        self._dispatch("PUT")

    def do_DELETE(self):
        self._dispatch("DELETE")

    def do_OPTIONS(self):
        self._dispatch("OPTIONS")

    def do_PATCH(self):
        self._dispatch("PATCH")

    # ---- core dispatch ----
    def _dispatch(self, method: str) -> None:
        app: App = self.server.app
        req = Request(self)
        head_only = method == "HEAD"
        try:
            security.check_host(self.headers.get("Host"), app.allowed)
            if method not in ("GET", "HEAD"):
                security.check_csrf(self.headers, app.allowed)
            path = urlsplit(self.path).path
            if path.startswith("/api/"):
                resp = self._route(req, app, "GET" if head_only else method, path)
            else:
                if method not in ("GET", "HEAD"):
                    raise MethodNotAllowed("Only GET and HEAD are allowed for static files.")
                file_path, mime = static.resolve(self.path, app.web_root)
                resp = api.Response(200, content_type=mime, file_path=file_path,
                                    headers={"Cache-Control": "no-cache"})
            if isinstance(resp, dict):
                resp = api.Response(200, json.dumps(resp).encode("utf-8"))
            self._send(req, resp, head_only)
        except QUIET_ERRORS:
            self.close_connection = True
        except OpenConverterError as exc:
            self._send_error(req, exc, head_only)
        except Exception:  # noqa: BLE001
            log.exception("internal error handling %s %s", method, urlsplit(self.path).path)
            self._send_error(req, None, head_only)

    def _route(self, req, app, method, path):
        allowed_methods = set()
        for m, pattern, handler in api.COMPILED:
            match = pattern.match(path)
            if match:
                if m == method:
                    return handler(req, app, *match.groups())
                allowed_methods.add(m)
        if allowed_methods:
            raise MethodNotAllowed(f"Method not allowed; use {', '.join(sorted(allowed_methods))}.")
        raise NotFound("Not found.")

    def _common_headers(self, req, content_type, api_response=True):
        self.send_header("Content-Type", content_type)
        for k, v in security.BASE_HEADERS.items():
            self.send_header(k, v)
        if api_response:
            self.send_header("Cache-Control", "no-store")
        if req.must_close or (not req.body_read and self.headers.get("Content-Length", "0") not in ("", "0")):
            self.close_connection = True
            self.send_header("Connection", "close")

    def _send(self, req, resp, head_only):
        self.send_response(resp.status)
        is_api = urlsplit(self.path).path.startswith("/api/")
        extra = dict(resp.headers)
        cache = extra.pop("Cache-Control", None)
        self._common_headers(req, resp.content_type, api_response=False)
        self.send_header("Cache-Control", cache or ("no-store" if is_api else "no-cache"))
        for k, v in extra.items():
            self.send_header(k, v)
        if resp.stream is not None:
            self.close_connection = True
            self.send_header("Connection", "close")
            self.end_headers()
            if head_only:
                return

            def write(b):
                self.wfile.write(b)
                self.wfile.flush()
            try:
                resp.stream(write)
            except QUIET_ERRORS:
                pass
            return
        if resp.file_path is not None:
            size = Path(resp.file_path).stat().st_size
            self.send_header("Content-Length", str(size))
            self.end_headers()
            if head_only:
                return
            with open(resp.file_path, "rb") as fh:
                while True:
                    chunk = fh.read(256 * 1024)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
            return
        body = resp.body if isinstance(resp.body, bytes) else resp.body.encode("utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if not head_only:
            self.wfile.write(body)

    def _send_error(self, req, exc, head_only):
        status = exc.http_status if exc is not None else 500
        # Read (and discard) an unread request body first, so the client is not
        # reset mid-upload and actually receives this error response.
        try:
            has_body = self.headers.get("Content-Length", "0") not in ("", "0")
            if req.body is None and has_body and "Transfer-Encoding" not in self.headers:
                req.content_length()
            if req.body is not None and req.body.remaining:
                if not req.body.drain(DRAIN_MAX_BYTES, DRAIN_MAX_SECONDS):
                    req.must_close = True
        except (OpenConverterError, *QUIET_ERRORS):
            req.must_close = True
        body = json.dumps(error_body(exc) if exc is not None else error_body(RuntimeError())).encode("utf-8")
        try:
            self.send_response(status)
            self._common_headers(req, "application/json; charset=utf-8")
            if status == 405:
                self.send_header("Allow", "GET, HEAD, POST, PUT, DELETE")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if not head_only:
                self.wfile.write(body)
        except QUIET_ERRORS:
            self.close_connection = True


def wait_for_port_free(host: str, port: int) -> bool:
    """True if ``port`` can be bound right now (used for a friendly error)."""
    fam = socket.AF_INET6 if ":" in host else socket.AF_INET
    with socket.socket(fam, socket.SOCK_STREAM) as s:
        try:
            s.bind((host, port))
            return True
        except OSError:
            return False

