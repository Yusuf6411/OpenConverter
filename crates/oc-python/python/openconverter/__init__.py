"""OpenConverter: file conversion with no external tools. See `convert`, `info` and `formats`."""
from ._native import ConversionError, Result, __version__, convert, formats, info

__all__ = ["ConversionError", "Result", "__version__", "convert", "formats", "info"]
