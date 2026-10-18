"""Format registry: the single source of truth for formats, routing and options.

Adding a format = adding a FormatSpec (with a reader and/or writer) to a
converter module and listing that module's formats in ``default_registry``.
"""

import threading

from openconverter.converters import image_converter, table_converter, text_converter
from openconverter.converters.base import ConversionResult, FormatSpec
from openconverter.errors import BadRequest, UnsupportedFormat
from openconverter.limits import DEFAULT_LIMITS
from openconverter.models.progress import NULL_PROGRESS

CATEGORIES = {
    "image": "Images",
    "text": "Text",
    "data": "Tabular data",
}


class Registry:
    def __init__(self) -> None:
        self._formats: dict[str, FormatSpec] = {}
        self._lock = threading.Lock()

    def register(self, spec: FormatSpec) -> None:
        with self._lock:
            if spec.id in self._formats:
                raise ValueError(f"format {spec.id!r} already registered")
            self._formats[spec.id] = spec

    def get(self, fmt_id: str) -> FormatSpec:
        try:
            return self._formats[fmt_id]
        except KeyError:
            raise UnsupportedFormat(f"Unknown format {fmt_id!r}.") from None

    def all(self) -> list[FormatSpec]:
        return list(self._formats.values())

    def by_extension(self, ext: str) -> FormatSpec | None:
        ext = ext.lower()
        for spec in self._formats.values():
            if ext in spec.extensions:
                return spec
        return None

    def targets(self, source_id: str) -> list[str]:
        """Writable formats reachable from ``source_id`` (identity included)."""
        src = self.get(source_id)
        if not src.can_read:
            return []
        if src.category == "image":
            return [f.id for f in self._formats.values() if f.category == "image" and f.can_write]
        if src.category == "data":
            return [f.id for f in self._formats.values() if f.category == "data" and f.can_write]
        if src.category == "text":
            return [f.id for f in self._formats.values() if f.category == "text" and f.can_write]
        return []

    def option_schema(self, source_id: str, target_id: str) -> dict:
        schema = dict(self.get(source_id).read_options)
        schema.update(self.get(target_id).write_options)
        return schema

    def validate_options(self, source_id: str, target_id: str, options) -> dict:
        """Validate ``options`` against the combined schema. Unknown keys,
        wrong types and out-of-range values raise BadRequest."""
        if options is None:
            options = {}
        if not isinstance(options, dict):
            raise BadRequest("Options must be a JSON object.")
        schema = self.option_schema(source_id, target_id)
        clean = {}
        for key, value in options.items():
            if key not in schema:
                raise BadRequest(f"Unknown option {key!r} for {source_id} -> {target_id}.")
            clean[key] = validate_value(key, schema[key], value)
        return clean

    def defaults(self, source_id: str, target_id: str) -> dict:
        return {k: v["default"] for k, v in self.option_schema(source_id, target_id).items()}

    def convert(self, data: bytes, source_id: str, target_id: str, options: dict,
                progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> ConversionResult:
        if target_id not in self.targets(source_id):
            raise UnsupportedFormat(f"Cannot convert {source_id} to {target_id}.")
        category = self.get(source_id).category
        if category == "image":
            return image_converter.convert_image(data, source_id, target_id, options, progress, limits)
        if category == "data":
            out, notes, info = table_converter.convert_table(data, source_id, target_id, options, progress, limits)
            return ConversionResult(out, notes, info)
        out, notes, info = text_converter.convert_text(data, options, progress, limits)
        return ConversionResult(out, notes, info)

    def to_json(self, overrides: dict | None = None) -> dict:
        """Serialisable description for GET /api/formats. ``overrides`` maps
        option keys to default values taken from the user's settings."""
        overrides = overrides or {}
        formats = []
        for spec in self._formats.values():
            formats.append({
                "id": spec.id,
                "name": spec.name,
                "category": spec.category,
                "extensions": list(spec.extensions),
                "mime": spec.mime,
                "can_read": spec.can_read,
                "can_write": spec.can_write,
                "ownership": spec.ownership,
                "ownership_note": spec.ownership_note,
                "variants_note": spec.variants_note,
                "read_options": _with_defaults(spec.read_options, overrides),
                "write_options": _with_defaults(spec.write_options, overrides),
                "targets": self.targets(spec.id) if spec.can_read else [],
                "identity_meaningful": spec.identity_meaningful,
            })
        return {"formats": formats, "categories": [{"id": k, "name": v} for k, v in CATEGORIES.items()]}


def _with_defaults(schema: dict, overrides: dict) -> dict:
    out = {}
    for key, opt in schema.items():
        opt = dict(opt)
        if key in overrides:
            opt["default"] = overrides[key]
        out[key] = opt
    return out


def validate_value(key: str, opt: dict, value):
    kind = opt["type"]
    if kind == "int":
        if isinstance(value, bool) or not isinstance(value, int):
            if isinstance(value, str) and value.strip().lstrip("-").isdigit():
                value = int(value)
            else:
                raise BadRequest(f"Option {key!r} must be an integer.")
        if not opt["min"] <= value <= opt["max"]:
            raise BadRequest(f"Option {key!r} must be between {opt['min']} and {opt['max']}.")
        return value
    if kind == "bool":
        if isinstance(value, bool):
            return value
        if value in ("true", "false"):
            return value == "true"
        raise BadRequest(f"Option {key!r} must be true or false.")
    if kind == "choice":
        choices = opt["choices"]
        if value in choices and type(value) is type(choices[0]):
            return value
        if isinstance(value, str) and all(isinstance(c, int) for c in choices) and value.isdigit() \
                and int(value) in choices:
            return int(value)
        raise BadRequest(f"Option {key!r} must be one of {', '.join(map(str, choices))}.")
    if kind == "colour":
        if isinstance(value, str) and len(value) == 7 and value[0] == "#":
            try:
                int(value[1:], 16)
                return value.upper()
            except ValueError:
                pass
        raise BadRequest(f"Option {key!r} must be a colour like #RRGGBB.")
    raise BadRequest(f"Option {key!r} has an unknown type.")


def default_registry() -> Registry:
    reg = Registry()
    for spec in image_converter.IMAGE_FORMATS + text_converter.TEXT_FORMATS + table_converter.TABLE_FORMATS:
        reg.register(spec)
    return reg


REGISTRY = default_registry()
