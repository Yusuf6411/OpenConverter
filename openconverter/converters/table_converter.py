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
