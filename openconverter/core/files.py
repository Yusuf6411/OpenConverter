"""Uploaded-file bookkeeping: detection, probing and metadata sidecars."""

import time
import unicodedata

from openconverter.converters import image_converter
from openconverter.core import detect
from openconverter.core.registry import REGISTRY
from openconverter.core.storage import new_id
from openconverter.errors import OpenConverterError

HEAD_BYTES = 1 << 20


def clean_display_name(name: str) -> str:
    """Original file name for display/metadata only (never used as a path)."""
    name = unicodedata.normalize("NFC", str(name or ""))
    name = name.replace("\\", "/").rsplit("/", 1)[-1]
    name = "".join(ch for ch in name if unicodedata.category(ch)[0] != "C")
    return name.strip()[:255] or "file"


def finalize_upload(storage, file_id: str, original_name: str, size: int) -> dict:
    """Detect the stored upload's format and write its metadata sidecar.

    Returns the per-file API entry; a file we cannot handle produces an entry
    with an ``error`` (the upload is kept only if it is usable).
    """
    path = storage.upload_path(file_id)
    name = clean_display_name(original_name)
    with open(path, "rb") as fh:
        head = fh.read(HEAD_BYTES)
    try:
        # Detection only needs the first MiB (magic numbers, headers, text sniffing).
        fmt = detect.detect(head, name)
    except OpenConverterError as exc:
        try:
            path.unlink()
        except OSError:
            pass
        return {"original_name": name, "size": size, "error": {"code": exc.code, "message": exc.message}}
    spec = REGISTRY.get(fmt)
    entry = {
        "file_id": file_id,
        "original_name": name,
        "size": size,
        "detected_format": fmt,
        "category": spec.category,
        "extension_mismatch": detect.extension_mismatch(fmt, name),
        "available_targets": REGISTRY.targets(fmt),
        "uploaded": time.time(),
    }
    if spec.category == "image":
        entry.update(image_converter.probe(head, fmt))
    storage.write_meta(path, entry)
    return entry


def store_upload_bytes(storage, data: bytes, original_name: str) -> dict:
    """Store an in-memory upload (CLI and tests) and finalise it."""
    file_id = new_id()
    with open(storage.upload_path(file_id), "wb") as fh:
        fh.write(data)
    return finalize_upload(storage, file_id, original_name, len(data))
