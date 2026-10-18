"""JSON <-> Table. Syntax handling uses the standard library ``json`` module
(hence JSON is marked PARTIAL ownership); the table mapping is ours."""

import json

from openconverter.errors import CorruptFile, LimitExceeded, UnsupportedVariant
from openconverter.limits import DEFAULT_LIMITS
from openconverter.models.table import Table


def _scalar_text(value, where):
    if isinstance(value, str):
        return value
    if value is None:
        return ""
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return json.dumps(value)
    raise UnsupportedVariant(
        f"JSON value at {where} is a nested {'object' if isinstance(value, dict) else 'array'}; "
        "only flat tables (arrays of flat objects or arrays) can be converted.")


def read(text: str, limits=DEFAULT_LIMITS) -> Table:
    try:
        doc = json.loads(text)
    except json.JSONDecodeError as exc:
        raise CorruptFile(f"Invalid JSON: {exc.msg} (line {exc.lineno}, column {exc.colno}).") from None
    except RecursionError:
        raise UnsupportedVariant("JSON is nested too deeply to be a table.") from None
    if not isinstance(doc, list):
        raise UnsupportedVariant("JSON must be an array of objects or an array of arrays to convert to a table.")
    if not doc:
        return Table(None, [])
    if all(isinstance(item, dict) for item in doc):
        keys = {}
        for item in doc:
            for k in item:
                keys.setdefault(k, None)
        header = list(keys)
        if len(header) * len(doc) > limits.max_table_cells:
            raise LimitExceeded("The table has more cells than the allowed maximum.")
        rows = []
        for i, item in enumerate(doc):
            rows.append([_scalar_text(item[k], f"row {i + 1}, key {k!r}") if k in item else "" for k in header])
        return _mark(Table(header, rows), (v for item in doc for v in item.values()))
    if all(isinstance(item, list) for item in doc):
        if sum(len(r) for r in doc) > limits.max_table_cells:
            raise LimitExceeded("The table has more cells than the allowed maximum.")
        rows = [[_scalar_text(v, f"row {i + 1}, column {j + 1}") for j, v in enumerate(r)]
                for i, r in enumerate(doc)]
        return _mark(Table(None, rows), (v for r in doc for v in r))
    raise UnsupportedVariant("JSON array mixes objects and other values; it cannot be converted to a table.")


def _mark(table: Table, values) -> Table:
    if any(not isinstance(v, str) for v in values):
        table.metadata["coerced_scalars"] = True
    return table


def unique_header(header, width, notes=None):
    """Deterministic de-duplication: empty -> column_N, duplicates -> name_2, name_3."""
    out = []
    seen = set()
    changed = False
    full = list(header) + [""] * (width - len(header))
    for i, name in enumerate(full):
        base = name if name != "" else f"column_{i + 1}"
        candidate = base
        k = 2
        while candidate in seen:
            candidate = f"{base}_{k}"
            k += 1
        if candidate != name:
            changed = True
        seen.add(candidate)
        out.append(candidate)
    if changed and notes is not None:
        notes.append("Empty or duplicate column names were renamed (column_N, name_2, ...).")
    return out


def write(table: Table, indent: int = 2, ensure_ascii: bool = False, notes=None) -> str:
    if table.header is not None:
        header = unique_header(table.header, table.width, notes)
        doc = [dict(zip(header, row)) for row in table.padded_rows()]
    else:
        doc = table.padded_rows()
    if table.is_ragged and notes is not None:
        notes.append("Rows have different numbers of fields; short rows were padded with empty values.")
    if indent:
        return json.dumps(doc, indent=indent, ensure_ascii=ensure_ascii) + "\n"
    return json.dumps(doc, separators=(",", ":"), ensure_ascii=ensure_ascii) + "\n"
