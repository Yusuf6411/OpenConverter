"""Our own RFC 4180 CSV/TSV parser and writer."""

import re

from openconverter.errors import CorruptFile, LimitExceeded
from openconverter.limits import DEFAULT_LIMITS
from openconverter.models.progress import NULL_PROGRESS
from openconverter.models.table import Table

DELIMITERS = {"comma": ",", "semicolon": ";", "tab": "\t", "pipe": "|"}
_DELIM_NAMES = {v: k for k, v in DELIMITERS.items()}
_SPECIALS = {d: re.compile("[" + re.escape(d) + "\r\n\"]") for d in DELIMITERS.values()}
_END_FIELD = {d: re.compile("[" + re.escape(d) + "\r\n]") for d in DELIMITERS.values()}


def delimiter_name(d: str) -> str:
    return _DELIM_NAMES.get(d, d)


def detect_delimiter(text: str, default: str = ",") -> str:
    """Pick the delimiter whose per-line count (outside quotes) is most consistent."""
    lines = []
    in_quotes = False
    current = []
    for ch in text[:65536]:
        if ch == '"':
            in_quotes = not in_quotes
        elif ch == "\n" and not in_quotes:
            lines.append("".join(current))
            current = []
            if len(lines) >= 50:
                break
            continue
        elif not in_quotes:
            current.append(ch)
    if current:
        lines.append("".join(current))
    lines = [ln for ln in lines if ln.strip()]
    if not lines:
        return default
    best = None
    for d in (",", ";", "\t", "|"):
        counts = [ln.count(d) for ln in lines]
        if not any(counts):
            continue
        consistent = sum(1 for c in counts if c == counts[0] and c > 0)
        score = (consistent / len(counts), sum(counts) / len(counts))
        if best is None or score > best[0]:
            best = (score, d)
    return best[1] if best else default


def parse(text: str, delimiter: str, limits=DEFAULT_LIMITS, progress=NULL_PROGRESS) -> list[list[str]]:
    """Parse delimited text. Blank lines (outside quotes) are skipped."""
    if delimiter not in _END_FIELD:
        raise ValueError(f"unsupported delimiter {delimiter!r}")
    if text.startswith("﻿"):
        text = text[1:]
    end_field = _END_FIELD[delimiter].search
    rows = []
    row = []
    cells = 0
    n = len(text)
    i = 0
    max_field = limits.max_csv_field
    max_cells = limits.max_table_cells
    next_check = 1 << 16
    while i < n:
        if text[i] == '"':
            # quoted field
            j = i + 1
            parts = []
            while True:
                k = text.find('"', j)
                if k < 0:
                    raise CorruptFile(f"CSV has an unterminated quoted field (row {len(rows) + 1}).")
                parts.append(text[j:k])
                if k + 1 < n and text[k + 1] == '"':
                    parts.append('"')
                    j = k + 2
                    continue
                i = k + 1
                break
            value = "".join(parts)
            # Characters after the closing quote up to the delimiter are kept
            # verbatim (lenient, like most spreadsheet programs).
            m = end_field(text, i)
            stop = m.start() if m else n
            if stop > i:
                value += text[i:stop]
            i = stop
        else:
            m = end_field(text, i)
            stop = m.start() if m else n
            value = text[i:stop]
            i = stop
        if len(value) > max_field:
            raise LimitExceeded("A CSV field is longer than the allowed maximum.")
        row.append(value)
        cells += 1
        if cells > max_cells:
            raise LimitExceeded("The table has more cells than the allowed maximum.")
        if i >= n:
            break
        ch = text[i]
        if ch == delimiter:
            i += 1
            if i >= n:
                row.append("")  # trailing delimiter at end of file
            continue
        # newline
        if ch == "\r" and i + 1 < n and text[i + 1] == "\n":
            i += 2
        else:
            i += 1
        rows.append(row)
        row = []
        # skip blank lines
        while i < n and text[i] in "\r\n":
            i += 1
        if i >= next_check:
            next_check = i + (1 << 16)
            progress.check_cancelled()
            progress.report(i, n, "parsing")
    if row:
        rows.append(row)
    return rows


def to_table(rows: list[list[str]], has_header: bool, notes=None) -> Table:
    if has_header and rows:
        table = Table(rows[0], rows[1:])
    else:
        table = Table(None, rows)
    if table.is_ragged and notes is not None:
        notes.append("Rows have different numbers of fields; short rows were padded with empty values.")
    return table


def _quote(value: str, special) -> str:
    if special(value):
        return '"' + value.replace('"', '""') + '"'
    return value


def write(table: Table, delimiter: str, newline: str = "crlf", progress=NULL_PROGRESS) -> str:
    special = _SPECIALS[delimiter].search
    nl = "\r\n" if newline == "crlf" else "\n"
    width = table.width
    out = []
    if table.header is not None:
        hdr = table.header + [""] * (width - len(table.header))
        out.append(delimiter.join(_quote(v, special) for v in hdr))
    for idx, r in enumerate(table.padded_rows()):
        if len(r) == 1 and r[0] == "":
            out.append('""')  # keep a single empty field from becoming a blank line
        else:
            out.append(delimiter.join(_quote(v, special) for v in r))
        if (idx & 4095) == 0:
            progress.check_cancelled()
            progress.report(idx, max(1, len(table.rows)), "writing")
    return nl.join(out) + (nl if out else "")
