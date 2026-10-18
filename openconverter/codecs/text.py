"""Plain-text decoding/encoding: BOM detection, UTF-8/UTF-16/Latin-1/Windows-1252,
line-ending handling. Character mapping uses Python's built-in text codecs
(hence TXT is marked PARTIAL ownership)."""

from openconverter.errors import CorruptFile, LimitExceeded, UnsupportedVariant
from openconverter.limits import DEFAULT_LIMITS

SOURCE_ENCODINGS = ["windows-1252", "latin-1", "utf-8", "utf-16le", "utf-16be"]
TARGET_ENCODINGS = ["utf-8", "utf-8-bom", "utf-16le", "utf-16le-bom", "utf-16be", "utf-16be-bom",
                    "latin-1", "windows-1252"]
NEWLINES = ["preserve", "lf", "crlf"]

_PY = {"utf-8": "utf-8", "utf-8-bom": "utf-8", "utf-16le": "utf-16-le", "utf-16le-bom": "utf-16-le",
       "utf-16be": "utf-16-be", "utf-16be-bom": "utf-16-be", "latin-1": "latin-1",
       "windows-1252": "cp1252"}
_BOM = {"utf-8-bom": b"\xef\xbb\xbf", "utf-16le-bom": b"\xff\xfe", "utf-16be-bom": b"\xfe\xff"}
_LABEL = {"utf-8": "UTF-8", "utf-8-bom": "UTF-8 with BOM", "utf-16le": "UTF-16 LE",
          "utf-16le-bom": "UTF-16 LE with BOM", "utf-16be": "UTF-16 BE", "utf-16be-bom": "UTF-16 BE with BOM",
          "latin-1": "Latin-1", "windows-1252": "Windows-1252"}


def _utf16_guess(data: bytes):
    """Detect BOM-less UTF-16 from the NUL-byte pattern of mostly-ASCII text."""
    sample = data[:4096]
    if len(sample) < 2 or len(sample) % 2 and len(data) == len(sample):
        return None
    even = sample[0::2]
    odd = sample[1::2]
    n = len(odd)
    zero_even = even.count(0)
    zero_odd = odd.count(0)
    if zero_odd > 0.3 * n and zero_even < 0.05 * n:
        return "utf-16le"
    if zero_even > 0.3 * n and zero_odd < 0.05 * n:
        return "utf-16be"
    return None


def decode_text(data: bytes, source_encoding: str = "windows-1252", limits=DEFAULT_LIMITS):
    """Return (text, encoding_label). Order: BOM, BOM-less UTF-16 pattern,
    strict UTF-8, then ``source_encoding`` as the fallback."""
    if len(data) > limits.max_text_bytes:
        raise LimitExceeded("The text file is larger than the allowed maximum.")
    if source_encoding not in SOURCE_ENCODINGS:
        raise ValueError(f"unknown source encoding {source_encoding!r}")
    if data[:4] in (b"\xff\xfe\x00\x00", b"\x00\x00\xfe\xff"):
        raise UnsupportedVariant("UTF-32 text is not supported.")
    try:
        if data.startswith(b"\xef\xbb\xbf"):
            return data[3:].decode("utf-8"), "utf-8-bom"
        if data.startswith(b"\xff\xfe"):
            return data[2:].decode("utf-16-le"), "utf-16le-bom"
        if data.startswith(b"\xfe\xff"):
            return data[2:].decode("utf-16-be"), "utf-16be-bom"
    except UnicodeDecodeError as exc:
        raise CorruptFile(f"The text has a byte-order mark but is not valid {exc.encoding}.") from None
    guess = _utf16_guess(data)
    if guess:
        try:
            return data.decode(_PY[guess]), guess
        except UnicodeDecodeError:
            pass
    try:
        return data.decode("utf-8"), "utf-8"
    except UnicodeDecodeError:
        pass
    if source_encoding in ("utf-16le", "utf-16be", "utf-8"):
        try:
            return data.decode(_PY[source_encoding]), source_encoding
        except UnicodeDecodeError:
            raise CorruptFile(f"The text is not valid {_LABEL[source_encoding]}.") from None
    if source_encoding == "windows-1252":
        try:
            return data.decode("cp1252"), "windows-1252"
        except UnicodeDecodeError:
            # 0x81, 0x8D, 0x8F, 0x90, 0x9D are undefined in Windows-1252.
            return data.decode("latin-1"), "latin-1"
    return data.decode("latin-1"), "latin-1"


def line_ending_kind(text: str) -> str:
    crlf = text.count("\r\n")
    cr = text.count("\r") - crlf
    lf = text.count("\n") - crlf
    kinds = [k for k, v in (("crlf", crlf), ("lf", lf), ("cr", cr)) if v]
    if not kinds:
        return "none"
    return kinds[0] if len(kinds) == 1 else "mixed"


def normalise_newlines(text: str, newline: str) -> str:
    if newline == "preserve":
        return text
    unified = text.replace("\r\n", "\n").replace("\r", "\n")
    return unified.replace("\n", "\r\n") if newline == "crlf" else unified


def encode_text(text: str, to_encoding: str = "utf-8", unencodable: str = "error", notes=None) -> bytes:
    if to_encoding not in TARGET_ENCODINGS:
        raise ValueError(f"unknown target encoding {to_encoding!r}")
    codec = _PY[to_encoding]
    try:
        body = text.encode(codec)
    except UnicodeEncodeError:
        bad = sorted({ch for ch in set(text) if not _encodable(ch, codec)})
        count = sum(text.count(ch) for ch in bad)
        sample = ", ".join(repr(ch) for ch in bad[:5])
        if unencodable != "replace":
            raise UnsupportedVariant(
                f"{count} character(s) cannot be represented in {_LABEL[to_encoding]} "
                f"(for example {sample}). Choose 'replace' to substitute them with '?'.") from None
        body = text.encode(codec, errors="replace")
        if notes is not None:
            notes.append(f"Replaced {count} character(s) not representable in {_LABEL[to_encoding]} with '?'.")
    return _BOM.get(to_encoding, b"") + body


def _encodable(ch: str, codec: str) -> bool:
    try:
        ch.encode(codec)
        return True
    except UnicodeEncodeError:
        return False


def label(encoding: str) -> str:
    return _LABEL.get(encoding, encoding)
