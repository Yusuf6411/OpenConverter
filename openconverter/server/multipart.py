"""Streaming multipart/form-data parser (RFC 7578), stdlib only.

The body is read in fixed-size chunks; part bodies are passed to a sink as
they arrive, so memory stays bounded whatever the upload size.
"""

import re
from urllib.parse import unquote

from openconverter.errors import BadRequest, LimitExceeded

CHUNK = 64 * 1024
MAX_PART_HEADER = 16 * 1024
MAX_HEADER_LINES = 32
_BOUNDARY_RE = re.compile(r"^[0-9A-Za-z'()+_,\-./:=? ]{0,69}[0-9A-Za-z'()+_,\-./:=?]$")


def parse_content_type(value: str):
    """'multipart/form-data; boundary=xyz' -> ('multipart/form-data', {'boundary': 'xyz'})."""
    if not value:
        return "", {}
    main, params = _split_params(value)
    return main.lower(), params


def _split_params(value: str):
    parts = []
    buf = []
    in_q = False
    esc = False
    for ch in value:
        if esc:
            buf.append(ch)
            esc = False
        elif ch == "\\" and in_q:
            esc = True
        elif ch == '"':
            in_q = not in_q
            buf.append(ch)
        elif ch == ";" and not in_q:
            parts.append("".join(buf))
            buf = []
        else:
            buf.append(ch)
    parts.append("".join(buf))
    main = parts[0].strip()
    params = {}
    for p in parts[1:]:
        if "=" not in p:
            continue
        k, v = p.split("=", 1)
        k = k.strip().lower()
        v = v.strip()
        if len(v) >= 2 and v[0] == '"' and v[-1] == '"':
            v = v[1:-1]
        params[k] = v
    return main, params


def get_boundary(content_type: str) -> bytes:
    main, params = parse_content_type(content_type)
    if main != "multipart/form-data":
        raise BadRequest("Expected a multipart/form-data upload.")
    boundary = params.get("boundary", "")
    if not _BOUNDARY_RE.match(boundary):
        raise BadRequest("The multipart boundary is missing or invalid.")
    return boundary.encode("ascii")


class Part:
    def __init__(self, headers: dict):
        self.headers = headers
        disp, params = _split_params(headers.get("content-disposition", ""))
        if disp.strip().lower() != "form-data":
            raise BadRequest("A multipart part is missing Content-Disposition: form-data.")
        self.name = params.get("name", "")
        filename = None
        if "filename*" in params:
            filename = _decode_rfc5987(params["filename*"])
        if filename is None and "filename" in params:
            filename = params["filename"]
        self.filename = filename
        self.content_type = headers.get("content-type", "application/octet-stream")


def _decode_rfc5987(value: str):
    try:
        charset, _lang, encoded = value.split("'", 2)
    except ValueError:
        return None
    charset = charset.lower() or "utf-8"
    if charset not in ("utf-8", "iso-8859-1"):
        return None
    try:
        return unquote(encoded, encoding=charset, errors="strict")
    except (UnicodeDecodeError, LookupError):
        return None


class MultipartReader:
    """Iterate parts of a multipart body read from ``rfile``.

    Usage:
        for part in reader.parts():
            for chunk in reader.read_part():   # must be consumed
                ...
    """

    def __init__(self, rfile, content_length: int, boundary: bytes, max_parts: int = 100,
                 max_total: int | None = None) -> None:
        self.rfile = rfile
        self.remaining = content_length
        self.dash_boundary = b"--" + boundary
        self.delim = b"\r\n--" + boundary
        self.buf = bytearray()
        self.max_parts = max_parts
        self.max_total = max_total
        self.done = False
        self._in_body = False

    def _fill(self) -> bool:
        if self.remaining <= 0:
            return False
        chunk = self.rfile.read(min(CHUNK, self.remaining))
        if not chunk:
            raise BadRequest("The upload ended unexpectedly.")
        self.remaining -= len(chunk)
        self.buf += chunk
        return True

    def _read_line_after_boundary(self):
        """After a boundary: '--' ends the body; otherwise skip padding and CRLF."""
        while len(self.buf) < 2 and self._fill():
            pass
        if self.buf[:2] == b"--":
            self.done = True
            return False
        # transport padding (linear whitespace) then CRLF
        while True:
            i = self.buf.find(b"\r\n")
            if i >= 0:
                if self.buf[:i].strip(b" \t"):
                    raise BadRequest("Malformed multipart boundary line.")
                del self.buf[:i + 2]
                return True
            if len(self.buf) > 1024:
                raise BadRequest("Malformed multipart boundary line.")
            if not self._fill():
                raise BadRequest("The upload ended unexpectedly.")

    def parts(self):
        # preamble: find the first dash-boundary at the start of a line
        while True:
            if self.buf.startswith(self.dash_boundary):
                del self.buf[:len(self.dash_boundary)]
                break
            i = self.buf.find(b"\r\n" + self.dash_boundary)
            if i >= 0:
                del self.buf[:i + 2 + len(self.dash_boundary)]
                break
            if len(self.buf) > 64 * 1024:
                raise BadRequest("Multipart body does not contain the declared boundary.")
            if not self._fill():
                raise BadRequest("Multipart body does not contain the declared boundary.")
        count = 0
        while self._read_line_after_boundary():
            count += 1
            if count > self.max_parts:
                raise LimitExceeded("Too many files in one upload.")
            headers = self._read_headers()
            self._in_body = True
            yield Part(headers)
            if self._in_body:
                for _ in self.read_part():  # drain an unread part
                    pass
        # epilogue is ignored; drain the socket so keep-alive stays in sync
        while self.remaining > 0:
            chunk = self.rfile.read(min(CHUNK, self.remaining))
            if not chunk:
                break
            self.remaining -= len(chunk)

    def _read_headers(self) -> dict:
        while True:
            i = self.buf.find(b"\r\n\r\n")
            if i >= 0:
                raw = bytes(self.buf[:i])
                del self.buf[:i + 4]
                break
            if self.buf.startswith(b"\r\n"):  # part with no headers
                raw = b""
                del self.buf[:2]
                break
            if len(self.buf) > MAX_PART_HEADER:
                raise LimitExceeded("A multipart part header is too large.")
            if not self._fill():
                raise BadRequest("The upload ended unexpectedly.")
        if len(raw) > MAX_PART_HEADER:
            raise LimitExceeded("A multipart part header is too large.")
        headers = {}
        last = None
        lines = raw.split(b"\r\n") if raw else []
        if len(lines) > MAX_HEADER_LINES:
            raise LimitExceeded("A multipart part has too many header lines.")
        for line in lines:
            text = line.decode("utf-8", errors="replace")
            if text[:1] in (" ", "\t"):
                if last is None:
                    raise BadRequest("Malformed multipart header folding.")
                headers[last] += " " + text.strip()  # obsolete line folding
                continue
            if ":" not in text:
                raise BadRequest("Malformed multipart header.")
            k, v = text.split(":", 1)
            last = k.strip().lower()
            headers[last] = v.strip()
        return headers

    def read_part(self):
        """Yield the current part's body in chunks, stopping at the next boundary."""
        dlen = len(self.delim)
        while True:
            i = self.buf.find(self.delim)
            if i >= 0:
                if i:
                    yield bytes(self.buf[:i])
                del self.buf[:i + dlen]
                self._in_body = False
                return
            keep = dlen - 1
            if len(self.buf) > keep:
                out = bytes(self.buf[:-keep])
                del self.buf[:-keep]
                yield out
            if not self._fill():
                raise BadRequest("The upload ended before the multipart part was complete.")
