"""Paths, limits and user settings (atomic, validated, corruption-tolerant)."""

import json
import logging
import logging.handlers
import os
import sys
import tempfile
import threading
from pathlib import Path

from openconverter.errors import BadRequest

DEFAULT_PORT = 8080
DEFAULT_HOST = "127.0.0.1"
MAX_JSON_BODY = 1024 * 1024
MAX_HEADER_BYTES = 64 * 1024
MAX_SSE_STREAMS = 32
SOCKET_TIMEOUT = 30
CLEANUP_INTERVAL_SECONDS = 3600

SETTINGS_SCHEMA = {
    "theme": {"type": "choice", "choices": ["system", "light", "dark"], "default": "system"},
    "default_jpeg_quality": {"type": "int", "min": 1, "max": 100, "default": 85},
    "default_png_level": {"type": "int", "min": 0, "max": 9, "default": 6},
    "alpha_background": {"type": "colour", "default": "#FFFFFF"},
    "retention_hours": {"type": "int", "min": 1, "max": 24 * 365, "default": 24},
    "max_upload_mb": {"type": "int", "min": 1, "max": 4096, "default": 200},
}
DEFAULT_SETTINGS = {k: v["default"] for k, v in SETTINGS_SCHEMA.items()}


def default_data_dir() -> Path:
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA")
        if base:
            return Path(base) / "OpenConverter"
        return Path.home() / "AppData" / "Local" / "OpenConverter"
    xdg = os.environ.get("XDG_DATA_HOME")
    if xdg:
        return Path(xdg) / "openconverter"
    return Path.home() / ".local" / "share" / "openconverter"


def atomic_write_bytes(path: Path, data: bytes) -> None:
    """Write via a temporary file in the same directory, then os.replace."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".tmp-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def validate_settings(values, partial: bool = True) -> dict:
    from openconverter.core.registry import validate_value
    if not isinstance(values, dict):
        raise BadRequest("Settings must be a JSON object.")
    clean = {}
    for key, value in values.items():
        if key not in SETTINGS_SCHEMA:
            raise BadRequest(f"Unknown setting {key!r}.")
        clean[key] = validate_value(key, SETTINGS_SCHEMA[key], value)
    if not partial:
        missing = set(SETTINGS_SCHEMA) - set(clean)
        if missing:
            raise BadRequest(f"Missing settings: {', '.join(sorted(missing))}.")
    return clean


class Settings:
    """Thread-safe settings stored in settings.json."""

    def __init__(self, path: Path) -> None:
        self.path = Path(path)
        self._lock = threading.Lock()
        self._values = self._load()

    def _load(self) -> dict:
        values = dict(DEFAULT_SETTINGS)
        try:
            raw = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return values
        if not isinstance(raw, dict):
            return values
        for key, value in raw.items():
            try:
                values.update(validate_settings({key: value}))
            except BadRequest:
                continue  # ignore bad/unknown keys from a damaged file
        return values

    def get(self) -> dict:
        with self._lock:
            return dict(self._values)

    def update(self, changes) -> dict:
        clean = validate_settings(changes)
        with self._lock:
            new = dict(self._values)
            new.update(clean)
            atomic_write_bytes(self.path, json.dumps(new, indent=2).encode("utf-8"))
            self._values = new
            return dict(new)

    def option_defaults(self) -> dict:
        v = self.get()
        return {"quality": v["default_jpeg_quality"], "compression_level": v["default_png_level"],
                "background": v["alpha_background"]}


def setup_logging(log_dir: Path) -> logging.Logger:
    log_dir.mkdir(parents=True, exist_ok=True)
    logger = logging.getLogger("openconverter")
    logger.setLevel(logging.INFO)
    for h in list(logger.handlers):
        if getattr(h, "_openconverter", False):
            logger.removeHandler(h)
            h.close()
    handler = logging.handlers.RotatingFileHandler(log_dir / "openconverter.log", maxBytes=1024 * 1024,
                                                   backupCount=3, encoding="utf-8")
    handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
    handler._openconverter = True
    logger.addHandler(handler)
    logger.propagate = False
    return logger
