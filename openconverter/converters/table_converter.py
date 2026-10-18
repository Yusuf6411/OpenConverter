"""CSV / TSV / JSON conversions through the Table IR."""

from openconverter.codecs import delimited, jsontable
from openconverter.codecs import text as textcodec
from openconverter.limits import DEFAULT_LIMITS
from openconverter.models.progress import NULL_PROGRESS, SubProgress
from openconverter.models.table import Table

DELIMITER_CHOICES = ["auto", "comma", "semicolon", "tab", "pipe"]


def read_table(data: bytes, fmt: str, options: dict, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS, notes=None):
    text, detected = textcodec.decode_text(data, options.get("source_encoding", "windows-1252"), limits)
    if detected not in ("utf-8", "utf-8-bom") and notes is not None:
        notes.append(f"Detected source encoding: {textcodec.label(detected)}.")
    if fmt == "json":
        return jsontable.read(text, limits)
    choice = options.get("source_delimiter", "auto")
    if choice == "auto":
        if fmt == "tsv":
            delim = "\t"
        else:
            delim = delimited.detect_delimiter(text, ",")
            if delim != "," and notes is not None:
                notes.append(f"Detected delimiter: {delimited.delimiter_name(delim)}.")
    else:
        delim = delimited.DELIMITERS[choice]
    rows = delimited.parse(text, delim, limits, progress)
    return delimited.to_table(rows, bool(options.get("has_header", True)), notes)


def write_table(table: Table, fmt: str, options: dict, progress=NULL_PROGRESS, notes=None) -> bytes:
    if fmt == "json":
        out = jsontable.write(table, int(options.get("indent", 2)), bool(options.get("ensure_ascii", False)), notes)
        return out.encode("utf-8")
    if fmt == "tsv":
        delim = "\t"
    else:
        delim = delimited.DELIMITERS[options.get("delimiter", "comma")]
    if table.is_ragged and notes is not None:
        note = "Rows have different numbers of fields; short rows were padded with empty values."
        if note not in notes:
            notes.append(note)
    out = delimited.write(table, delim, options.get("newline", "crlf"), progress)
    bom = b"\xef\xbb\xbf" if options.get("bom", False) else b""
    return bom + out.encode("utf-8")


def convert_table(data: bytes, source: str, target: str, options: dict, progress=NULL_PROGRESS,
                  limits=DEFAULT_LIMITS):
    """Returns (output_bytes, notes, info)."""
    notes = []
    table = read_table(data, source, options, SubProgress(progress, 0.0, 0.6), limits, notes)
    progress.check_cancelled()
    out = write_table(table, target, options, SubProgress(progress, 0.6, 1.0), notes)
    if table.metadata.get("coerced_scalars"):
        notes.append("JSON numbers, booleans and null were written as text (null becomes an empty value).")
    if out == data:
        notes.append("No change: the output is identical to the input.")
    progress.report(1, 1, "finalizing")
    notes = list(dict.fromkeys(notes))
    return out, notes, {"rows": len(table.rows), "columns": table.width}


def _formats():
    from openconverter.converters.base import (OWN, PARTIAL, FormatSpec, bool_option, choice_option,
                                               int_option)
    source_encoding = choice_option(
        "Source encoding (fallback)", textcodec.SOURCE_ENCODINGS, "windows-1252",
        "Used when the file has no byte-order mark and is not valid UTF-8.")
    read_delimited = {
        "source_delimiter": choice_option("Input delimiter", DELIMITER_CHOICES, "auto",
                                          labels={"auto": "Detect", "comma": "Comma", "semicolon": "Semicolon",
                                                  "tab": "Tab", "pipe": "Pipe"}),
        "has_header": bool_option("First row is a header", True),
        "source_encoding": source_encoding,
    }
    write_common = {
        "newline": choice_option("Line endings", ["crlf", "lf"], "crlf",
                                 labels={"crlf": "CRLF (RFC 4180)", "lf": "LF"}),
        "bom": bool_option("UTF-8 BOM (for Excel)", False),
    }
    csv_write = {"delimiter": choice_option("Delimiter", ["comma", "semicolon", "tab", "pipe"], "comma",
                                            labels={"comma": "Comma", "semicolon": "Semicolon", "tab": "Tab",
                                                    "pipe": "Pipe"}), **write_common}
    note = ("RFC 4180: quoted fields, embedded delimiters/newlines/quotes, CRLF or LF, ragged rows "
            "(padded on write), BOM stripped on read.")
    return [
        FormatSpec("csv", "CSV", "data", (".csv",), "text/csv", lambda data: False, True, True, OWN, note,
                   read_options=read_delimited, write_options=csv_write, identity_meaningful=True),
        FormatSpec("tsv", "TSV", "data", (".tsv", ".tab"), "text/tab-separated-values", lambda data: False,
                   True, True, OWN, note, read_options=read_delimited, write_options=dict(write_common),
                   identity_meaningful=True),
        FormatSpec("json", "JSON", "data", (".json",), "application/json", lambda data: False, True, True,
                   PARTIAL, "Arrays of flat objects (header = keys in first-seen order) or arrays of arrays. "
                   "Nested values are rejected; numbers/booleans/null become text.",
                   read_options={"source_encoding": source_encoding},
                   write_options={"indent": choice_option("Indent", [0, 2, 4], 2, labels={0: "Compact", 2: "2 spaces",
                                                                                         4: "4 spaces"}),
                                  "ensure_ascii": bool_option("Escape non-ASCII characters", False)},
                   identity_meaningful=True,
                   ownership_note="JSON syntax is parsed and written by Python's standard json module."),
    ]


TABLE_FORMATS = _formats()
