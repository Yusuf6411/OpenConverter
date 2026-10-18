"""TXT -> TXT: re-encoding and line-ending conversion."""

from openconverter.codecs import text as textcodec
from openconverter.limits import DEFAULT_LIMITS
from openconverter.models.progress import NULL_PROGRESS


def convert_text(data: bytes, options: dict, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS):
    """Returns (output_bytes, notes, info)."""
    notes = []
    progress.report(0, 3, "decoding")
    text, detected = textcodec.decode_text(data, options.get("source_encoding", "windows-1252"), limits)
    notes.append(f"Detected source encoding: {textcodec.label(detected)}.")
    progress.check_cancelled()
    progress.report(1, 3, "converting")
    newline = options.get("newline", "preserve")
    kind = textcodec.line_ending_kind(text)
    converted = textcodec.normalise_newlines(text, newline)
    if newline != "preserve" and kind not in ("none", newline):
        notes.append(f"Line endings converted from {kind.upper()} to {newline.upper()}.")
    elif kind == "mixed":
        notes.append("The text has mixed line endings (preserved).")
    target = options.get("to_encoding", "utf-8")
    progress.report(2, 3, "encoding")
    out = textcodec.encode_text(converted, target, options.get("unencodable", "error"), notes)
    if out == data:
        notes.append("No change: the output is identical to the input.")
    progress.report(3, 3, "encoding")
    return out, notes, {"source_encoding": detected, "line_endings": kind}
