"""Decoder resource limits (checked before any large allocation)."""

from dataclasses import dataclass

from openconverter.errors import CorruptFile, LimitExceeded

HARD_MAX_PIXELS = 100_000_000


@dataclass(frozen=True)
class Limits:
    max_pixels: int = 64_000_000
    max_side: int = 30_000
    max_png_chunk: int = 1 << 28  # 256 MiB, well below the PNG 2**31-1 ceiling
    max_jpeg_components: int = 4
    max_table_cells: int = 20_000_000
    max_csv_field: int = 16 * 1024 * 1024
    max_text_bytes: int = 512 * 1024 * 1024


DEFAULT_LIMITS = Limits()


def check_dimensions(width: int, height: int, limits: Limits = DEFAULT_LIMITS) -> None:
    """Validate declared image dimensions before allocating a pixel buffer."""
    if width <= 0 or height <= 0:
        raise CorruptFile(f"Invalid image dimensions {width}x{height}.")
    if width > limits.max_side or height > limits.max_side:
        raise LimitExceeded(
            f"Image dimensions {width}x{height} exceed the maximum side of {limits.max_side} px.")
    if width * height > min(limits.max_pixels, HARD_MAX_PIXELS):
        raise LimitExceeded(
            f"Image has {width * height:,} pixels, more than the limit of "
            f"{min(limits.max_pixels, HARD_MAX_PIXELS):,}.")
