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


def _formats():
    from openconverter.converters.base import PARTIAL, FormatSpec, choice_option
    source_encoding = choice_option(
        "Source encoding (fallback)", textcodec.SOURCE_ENCODINGS, "windows-1252",
        "Used when the text has no byte-order mark and is not valid UTF-8.")
    return [
        FormatSpec("txt", "Plain text", "text", (".txt", ".text", ".log"), "text/plain",
                   lambda data: False, True, True, PARTIAL,
                   "UTF-8 (with/without BOM), UTF-16 LE/BE (with/without BOM), Latin-1, Windows-1252; "
                   "LF/CRLF line endings.",
                   read_options={"source_encoding": source_encoding},
                   write_options={
                       "to_encoding": choice_option("Output encoding", textcodec.TARGET_ENCODINGS, "utf-8",
                                                    labels={k: textcodec.label(k) for k in textcodec.TARGET_ENCODINGS}),
                       "newline": choice_option("Line endings", textcodec.NEWLINES, "preserve",
                                                labels={"preserve": "Keep as is", "lf": "LF (Unix)",
                                                        "crlf": "CRLF (Windows)"}),
                       "unencodable": choice_option("Unencodable characters", ["error", "replace"], "error",
                                                    labels={"error": "Stop with an error",
                                                            "replace": "Replace with ?"}),
                   },
                   identity_meaningful=True,
                   ownership_note="Character mapping uses Python's built-in text codecs."),
    ]


TEXT_FORMATS = _formats()
