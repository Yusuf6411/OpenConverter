"""On-disk storage: uploads/, outputs/, temp/, logs/ under the data directory.

Every stored file is named by a random UUID (hex); the user's original file
name is metadata only and never becomes part of a path.
"""

import json
import logging
import os
import re
import threading
import time
import uuid
from pathlib import Path

from openconverter.config import atomic_write_bytes
from openconverter.errors import NotFound

ID_RE = re.compile(r"^[0-9a-f]{32}$")
log = logging.getLogger("openconverter.storage")


def new_id() -> str:
    return uuid.uuid4().hex


def check_id(value: str) -> str:
    if not isinstance(value, str) or not ID_RE.match(value):
        raise NotFound("Not found.")
    return value


class Storage:
    def __init__(self, data_dir: Path) -> None:
        self.root = Path(data_dir)
        self.uploads = self.root / "uploads"
        self.outputs = self.root / "outputs"
        self.temp = self.root / "temp"
        self.logs = self.root / "logs"
        for d in (self.uploads, self.outputs, self.temp, self.logs):
            d.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()

    # ---- paths (ids are validated before they touch the filesystem) ----
    def upload_path(self, file_id: str) -> Path:
        return self.uploads / check_id(file_id)

    def output_path(self, output_id: str) -> Path:
        return self.outputs / check_id(output_id)

    def temp_path(self, name_id: str, suffix: str = ".part") -> Path:
        return self.temp / (check_id(name_id) + suffix)

    @staticmethod
    def _meta_path(path: Path) -> Path:
        return path.with_name(path.name + ".json")

    # ---- metadata sidecars ----
    def write_meta(self, path: Path, meta: dict) -> None:
        atomic_write_bytes(self._meta_path(path), json.dumps(meta).encode("utf-8"))

    def read_meta(self, path: Path) -> dict:
        try:
            return json.loads(self._meta_path(path).read_text(encoding="utf-8"))
        except (OSError, ValueError):
            raise NotFound("Not found.") from None

    def upload_meta(self, file_id: str) -> dict:
        path = self.upload_path(file_id)
        if not path.is_file():
            raise NotFound("The uploaded file no longer exists.")
        return self.read_meta(path)

    def output_meta(self, output_id: str) -> dict:
        path = self.output_path(output_id)
        if not path.is_file():
            raise NotFound("The converted file no longer exists.")
        return self.read_meta(path)

    def output_exists(self, output_id: str) -> bool:
        try:
            return self.output_path(output_id).is_file()
        except NotFound:
            return False

    def read_upload(self, file_id: str) -> bytes:
        path = self.upload_path(file_id)
        try:
            return path.read_bytes()
        except OSError:
            raise NotFound("The uploaded file no longer exists.") from None

    def store_output(self, data: bytes, meta: dict) -> str:
        """Write to temp/ then atomically move into outputs/."""
        output_id = new_id()
        tmp = self.temp_path(output_id)
        try:
            with open(tmp, "wb") as fh:
                fh.write(data)
            meta = dict(meta, output_id=output_id, size=len(data), created=time.time())
            final = self.output_path(output_id)
            self.write_meta(final, meta)
            os.replace(tmp, final)
        finally:
            try:
                tmp.unlink()
            except OSError:
                pass
        return output_id

    def delete_output(self, output_id: str) -> None:
        for p in (self.output_path(output_id), self._meta_path(self.output_path(output_id))):
            try:
                p.unlink()
            except OSError:
                pass

    def cached_preview(self, kind: str, item_id: str) -> Path:
        return self.temp / f"preview-{kind}-{check_id(item_id)}.png"

    # ---- cleanup ----
    def cleanup(self, retention_hours: float, protected: set[str] = frozenset(), now: float | None = None) -> int:
        """Delete uploads/outputs older than ``retention_hours`` and stale temp
        files. Ids in ``protected`` (running jobs) are never touched. Files in
        use on Windows (PermissionError) are retried on the next cycle."""
        now = time.time() if now is None else now
        cutoff = now - retention_hours * 3600
        temp_cutoff = now - 3600
        removed = 0
        for folder, limit in ((self.uploads, cutoff), (self.outputs, cutoff), (self.temp, temp_cutoff)):
            try:
                entries = list(folder.iterdir())
            except OSError:
                continue
            for p in entries:
                base = p.name.split(".", 1)[0].replace("preview-upload-", "").replace("preview-output-", "")
                base = base.replace(".tmp-", "")
                if base in protected:
                    continue
                try:
                    if p.stat().st_mtime < limit:
                        p.unlink()
                        removed += 1
                except PermissionError:
                    log.info("cleanup: %s in use, will retry", p.name)
                except OSError:
                    pass
        return removed
