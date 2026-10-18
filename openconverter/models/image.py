"""Image intermediate representation."""

from dataclasses import dataclass, field

CHANNELS = {"L": 1, "LA": 2, "RGB": 3, "RGBA": 4}


@dataclass
class Image:
    """Row-major, interleaved, top-down pixels with no row padding.

    16-bit samples are stored big-endian. ``metadata`` carries source_format,
    dpi, gamma, orientation_applied, original_bit_depth, notes, ...
    """

    width: int
    height: int
    mode: str
    bit_depth: int
    pixels: bytearray
    metadata: dict = field(default_factory=dict)

    def __post_init__(self) -> None:
        if self.mode not in CHANNELS:
            raise ValueError(f"unknown image mode {self.mode!r}")
        if self.bit_depth not in (8, 16):
            raise ValueError(f"unsupported bit depth {self.bit_depth}")
        if self.width <= 0 or self.height <= 0:
            raise ValueError("image dimensions must be positive")
        if not isinstance(self.pixels, bytearray):
            self.pixels = bytearray(self.pixels)
        expected = self.width * self.height * self.channels * (self.bit_depth // 8)
        if len(self.pixels) != expected:
            raise ValueError(f"pixel buffer has {len(self.pixels)} bytes, expected {expected}")

    @property
    def channels(self) -> int:
        return CHANNELS[self.mode]

    @property
    def has_alpha(self) -> bool:
        return self.mode in ("LA", "RGBA")

    @property
    def is_grey(self) -> bool:
        return self.mode in ("L", "LA")

    @property
    def bytes_per_pixel(self) -> int:
        return self.channels * (self.bit_depth // 8)

    @property
    def row_bytes(self) -> int:
        return self.width * self.bytes_per_pixel

    def copy(self) -> "Image":
        return Image(self.width, self.height, self.mode, self.bit_depth,
                     bytearray(self.pixels), dict(self.metadata))
