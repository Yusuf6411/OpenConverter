"""Conversion history (history.jsonl): metadata only, never file contents."""

import json
import threading
from pathlib import Path

from openconverter.config import atomic_write_bytes

FIELDS = ("id", "job_id", "original_name", "output_name", "source_format", "target_format", "input_size",
          "output_size", "duration_ms", "status", "timestamp", "notes", "output_id", "error_code")


class History:
    def __init__(self, path: Path) -> None:
        self.path = Path(path)
        self._lock = threading.Lock()

    def append(self, entry: dict) -> None:
        record = {k: entry.get(k) for k in FIELDS}
        line = json.dumps(record, ensure_ascii=False) + "\n"
        with self._lock:
            with open(self.path, "a", encoding="utf-8") as fh:
                fh.write(line)

    def _read_unlocked(self) -> list[dict]:
        try:
            text = self.path.read_text(encoding="utf-8")
        except OSError:
            return []
        out = []
        for line in text.splitlines():
            try:
                item = json.loads(line)
            except ValueError:
                continue  # skip a damaged line rather than losing everything
            if isinstance(item, dict) and item.get("id"):
                out.append(item)
        return out

    def entries(self, output_exists=None) -> list[dict]:
        with self._lock:
            items = self._read_unlocked()
        for item in items:
            if item.get("status") == "done":
                alive = bool(item.get("output_id")) and (output_exists is None or output_exists(item["output_id"]))
                item["expired"] = not alive
            else:
                item["expired"] = False
        items.reverse()  # newest first
        return items

    def delete(self, entry_id: str) -> bool:
        with self._lock:
            items = self._read_unlocked()
            kept = [i for i in items if i.get("id") != entry_id]
            if len(kept) == len(items):
                return False
            self._rewrite(kept)
            return True

    def clear(self) -> None:
        with self._lock:
            self._rewrite([])

    def get(self, entry_id: str) -> dict | None:
        with self._lock:
            for item in self._read_unlocked():
                if item.get("id") == entry_id:
                    return item
        return None

    def _rewrite(self, items) -> None:
        data = "".join(json.dumps(i, ensure_ascii=False) + "\n" for i in items).encode("utf-8")
        atomic_write_bytes(self.path, data)
