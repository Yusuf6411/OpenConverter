"""Content-based format detection (the extension is only a tie-breaker for text)."""

from pathlib import PurePath

from openconverter.codecs import delimited
from openconverter.codecs import text as textcodec
from openconverter.core.registry import REGISTRY
from openconverter.errors import CorruptFile, UnsupportedFormat, UnsupportedVariant

# Strong magic numbers first; TGA (no magic) is tried after them.
BINARY_ORDER = ["png", "jpeg", "gif", "bmp", "qoi", "ppm", "pgm", "pbm", "tga"]
_KNOWN_UNSUPPORTED = [
    (b"%PDF", "PDF"), (b"PK\x03\x04", "ZIP/Office"), (b"RIFF", "RIFF (WAV/WebP/AVI)"),
    (b"\x1f\x8b", "gzip"), (b"BZh", "bzip2"), (b"7z\xbc\xaf", "7-Zip"), (b"fLaC", "FLAC"),
    (b"ID3", "MP3"), (b"OggS", "Ogg"), (b"II*\x00", "TIFF"), (b"MM\x00*", "TIFF"),
    (b"\x00\x00\x01\x00", "ICO"), (b"8BPS", "Photoshop"), (b"P7", "PAM"),
]


def _is_probably_text(data: bytes) -> bool:
    if not data:
        return True
    if data[:3] == b"\xef\xbb\xbf" or data[:2] in (b"\xff\xfe", b"\xfe\xff"):
        return True
    if textcodec._utf16_guess(data):
        return True
    sample = data[:65536]
    if b"\x00" in sample:
        return False
    control = sum(1 for b in sample if b < 32 and b not in (9, 10, 12, 13, 27))
    return control <= len(sample) // 100


def detect(data: bytes, filename: str = "") -> str:
    """Return the format id for ``data``. Raises UnsupportedFormat if unknown."""
    head = data[:65536]
    for fmt in BINARY_ORDER:
        if REGISTRY.get(fmt).sniff(head if fmt != "tga" else data):
            return fmt
    for magic, name in _KNOWN_UNSUPPORTED:
        if data.startswith(magic):
            raise UnsupportedFormat(f"{name} files are not supported in OpenConverter 1.0.")
    if not _is_probably_text(data):
        raise UnsupportedFormat("This file type is not recognised.")
    ext = PurePath(filename).suffix.lower()
    by_ext = REGISTRY.by_extension(ext) if ext else None
    if by_ext is not None and by_ext.category in ("text", "data"):
        return by_ext.id
    # No helpful extension: look at the text itself.
    try:
        text, _ = textcodec.decode_text(data[:1 << 20])
    except (CorruptFile, UnsupportedVariant):
        return "txt"
    stripped = text.lstrip("\ufeff \t\r\n")
    if stripped[:1] == "[":
        return "json"
    lines = [ln for ln in text[:65536].splitlines() if ln.strip()][:20]
    if len(lines) >= 2:
        d = delimited.detect_delimiter(text[:65536], "")
        if d:
            counts = [ln.count(d) for ln in lines]
            if all(c == counts[0] for c in counts) and counts[0] > 0:
                return "tsv" if d == "\t" else "csv"
    return "txt"


def extension_mismatch(fmt: str, filename: str) -> bool:
    ext = PurePath(filename).suffix.lower()
    if not ext:
        return False
    spec = REGISTRY.get(fmt)
    if ext in spec.extensions:
        return False
    # Netpbm variants share the generic .pnm extension.
    if ext == ".pnm" and fmt in ("ppm", "pgm", "pbm"):
        return False
    return True
