"""Request checks (Host, Origin, CSRF header), security headers and safe
download names."""

import re
import unicodedata
from urllib.parse import quote, urlsplit

from openconverter.errors import Forbidden

CSP = ("default-src 'self'; img-src 'self' data: blob:; style-src 'self'; script-src 'self'; "
       "connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'")

BASE_HEADERS = {
    "Content-Security-Policy": CSP,
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Cross-Origin-Resource-Policy": "same-origin",
    "X-Frame-Options": "DENY",
}

LOOPBACK_NAMES = ("127.0.0.1", "localhost", "[::1]")


def allowed_hosts(bind_host: str, port: int) -> set[str]:
    hosts = {f"{h}:{port}" for h in LOOPBACK_NAMES}
    if bind_host not in ("127.0.0.1", "localhost", "::1", ""):
        h = f"[{bind_host}]" if ":" in bind_host and not bind_host.startswith("[") else bind_host
        hosts.add(f"{h.lower()}:{port}")
    return hosts


def check_host(host_header, allowed: set[str]) -> None:
    """DNS-rebinding defence: the Host header must name this server exactly."""
    if not host_header or host_header.strip().lower() not in allowed:
        raise Forbidden("Request rejected: unexpected Host header.")


def check_csrf(headers, allowed: set[str]) -> None:
    """State-changing requests need a same-origin Origin (or Referer) and X-OpenConverter: 1."""
    origin = headers.get("Origin")
    if origin:
        parts = urlsplit(origin)
        if parts.scheme != "http" or parts.netloc.lower() not in allowed or parts.path not in ("", "/"):
            raise Forbidden("Request rejected: cross-origin request.")
    else:
        referer = headers.get("Referer")
        if not referer:
            raise Forbidden("Request rejected: missing Origin.")
        parts = urlsplit(referer)
        if parts.scheme != "http" or parts.netloc.lower() not in allowed:
            raise Forbidden("Request rejected: cross-origin request.")
    if headers.get("X-OpenConverter") != "1":
        raise Forbidden("Request rejected: missing X-OpenConverter header.")


_RESERVED = re.compile(r"^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(\..*)?$", re.IGNORECASE)
MAX_NAME = 150


def safe_filename(name: str, fallback: str = "download") -> str:
    """Sanitise a download name (never used as a server-side path)."""
    name = unicodedata.normalize("NFC", str(name or ""))
    name = name.replace("\\", "/").split("/")[-1]
    name = "".join(ch for ch in name if unicodedata.category(ch)[0] != "C")
    name = re.sub(r'[<>:"|?*]', "_", name)
    name = name.strip().rstrip(". ")
    if not name or name in (".", ".."):
        name = fallback
    if _RESERVED.match(name):
        name = "_" + name
    if len(name) > MAX_NAME:
        stem, dot, ext = name.rpartition(".")
        if dot and len(ext) <= 10:
            name = stem[:MAX_NAME - len(ext) - 1].rstrip(". ") + "." + ext
        else:
            name = name[:MAX_NAME].rstrip(". ")
    return name


def content_disposition(name: str) -> str:
    safe = safe_filename(name)
    ascii_name = unicodedata.normalize("NFKD", safe).encode("ascii", "ignore").decode("ascii")
    ascii_name = re.sub(r"[^A-Za-z0-9._ ()\-]", "_", ascii_name).strip() or "download"
    if _RESERVED.match(ascii_name):
        ascii_name = "_" + ascii_name
    return f'attachment; filename="{ascii_name}"; filename*=UTF-8\'\'{quote(safe, safe="")}'
