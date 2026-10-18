"""Static file serving from web/, refusing anything outside it."""

from pathlib import Path
from urllib.parse import unquote

from openconverter.errors import NotFound

WEB_ROOT = Path(__file__).resolve().parents[2] / "web"

MIME = {
    ".html": "text/html; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".ico": "image/x-icon",
    ".json": "application/json; charset=utf-8",
    ".txt": "text/plain; charset=utf-8",
    ".webmanifest": "application/manifest+json",
}


def resolve(url_path: str, root: Path = WEB_ROOT) -> tuple[Path, str]:
    """Map a URL path to a file under ``root``. Raises NotFound for anything
    outside it (``..``, encoded traversal, backslashes, absolute paths, NUL
    bytes, symlink escapes) or for unknown file types."""
    path = url_path.split("?", 1)[0].split("#", 1)[0]
    try:
        decoded = unquote(path, errors="strict") if "%" in path else path
    except UnicodeDecodeError:
        raise NotFound("Not found.") from None
    if "\x00" in decoded or "\\" in decoded or "%" in decoded:
        raise NotFound("Not found.")
    if decoded in ("", "/"):
        decoded = "/index.html"
    parts = decoded.split("/")
    if any(p in ("..", ".") for p in parts) or ":" in decoded:
        raise NotFound("Not found.")
    rel = decoded.lstrip("/")
    if not rel or rel.startswith("/"):
        raise NotFound("Not found.")
    root_real = root.resolve()
    candidate = (root_real / rel).resolve()
    try:
        candidate.relative_to(root_real)
    except ValueError:
        raise NotFound("Not found.") from None
    if not candidate.is_file():
        raise NotFound("Not found.")
    mime = MIME.get(candidate.suffix.lower())
    if mime is None:
        raise NotFound("Not found.")
    return candidate, mime
