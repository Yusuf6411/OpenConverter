"""Netpbm: PBM (P1/P4), PGM (P2/P5) and PPM (P3/P6)."""

import re

from openconverter.errors import CorruptFile, UnsupportedVariant
from openconverter.limits import DEFAULT_LIMITS, check_dimensions
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS
from openconverter.ops import image_ops

_WS = b" \t\r\n\x0b\x0c"
_COMMENT = re.compile(rb"#[^\r\n]*")


def sniff(data: bytes) -> bool:
    return len(data) >= 3 and data[0:1] == b"P" and data[1:2] in b"123456" and data[2:3] in _WS


def sniff_kind(data: bytes) -> str | None:
    """'pbm' | 'pgm' | 'ppm' for a Netpbm file, else None."""
    if not sniff(data):
        return None
    return {b"1": "pbm", b"4": "pbm", b"2": "pgm", b"5": "pgm", b"3": "ppm", b"6": "ppm"}[data[1:2]]


def _read_header(data: bytes, count: int):
    """Read ``count`` integer header fields after the magic.

    Returns (values, offset of the byte after the single whitespace that
    terminates the last field).
    """
    pos = 2
    n = len(data)
    values = []
    while len(values) < count:
        # skip whitespace and comments
        while pos < n:
            ch = data[pos]
            if ch in _WS:
                pos += 1
            elif ch == 0x23:  # '#'
                while pos < n and data[pos] not in b"\r\n":
                    pos += 1
            else:
                break
        start = pos
        while pos < n and 0x30 <= data[pos] <= 0x39:
            pos += 1
        if pos == start:
            raise CorruptFile("Netpbm header is truncated or contains a non-numeric field.")
        if pos - start > 9:
            raise CorruptFile("Netpbm header field is too large.")
        values.append(int(data[start:pos]))
        if pos < n and data[pos] not in _WS and data[pos] != 0x23:
            raise CorruptFile("Netpbm header field is followed by an invalid character.")
    if pos >= n:
        raise CorruptFile("Netpbm file has no pixel data.")
    if data[pos] == 0x23:
        # A comment directly after the last field: skip it and its newline.
        while pos < n and data[pos] not in b"\r\n":
            pos += 1
    return values, pos + 1


def _scale_lut(maxval: int, top: int):
    return [(v * top + maxval // 2) // maxval for v in range(maxval + 1)]


def decode(data: bytes, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> Image:
    data = bytes(data)
    if not sniff(data):
        raise CorruptFile("Not a Netpbm file (missing P1-P6 magic number).")
    kind = data[1] - 0x30
    bitmap = kind in (1, 4)
    fields, pos = _read_header(data, 2 if bitmap else 3)
    width, height = fields[0], fields[1]
    maxval = 1 if bitmap else fields[2]
    if not 1 <= maxval <= 65535:
        raise CorruptFile(f"Netpbm maxval {maxval} is outside 1..65535.")
    check_dimensions(width, height, limits)
    channels = 3 if kind in (3, 6) else 1
    mode = "RGB" if channels == 3 else "L"
    depth = 16 if maxval > 255 else 8
    count = width * height * channels
    meta = {"source_format": ("pbm", "pgm", "ppm")[(kind - 1) % 3], "maxval": maxval,
            "netpbm_encoding": "ascii" if kind <= 3 else "binary"}
    progress.report(0, height, "decoding")

    if kind == 4:
        row_len = (width + 7) // 8
        need = row_len * height
        raster = data[pos:pos + need]
        if len(raster) < need:
            raise CorruptFile("PBM pixel data is truncated.")
        # Expand bits: 1 = black (0), 0 = white (255).
        lut = [bytes(0 if (b >> (7 - i)) & 1 else 255 for i in range(8)) for b in range(256)]
        out = bytearray()
        for y in range(height):
            row = raster[y * row_len:(y + 1) * row_len]
            out += b"".join(lut[b] for b in row)[:width]
            if (y & 255) == 0:
                progress.check_cancelled()
                progress.report(y, height, "decoding")
        meta["original_bit_depth"] = 1
        progress.report(height, height, "decoding")
        return Image(width, height, "L", 8, out, meta)

    if kind in (5, 6):
        bps = 2 if depth == 16 else 1
        need = count * bps
        raster = data[pos:pos + need]
        if len(raster) < need:
            raise CorruptFile("Netpbm pixel data is truncated.")
        progress.check_cancelled()
        if depth == 8:
            if max(raster, default=0) > maxval:
                raise CorruptFile("Netpbm sample exceeds the declared maxval.")
            if maxval == 255:
                pixels = bytearray(raster)
            else:
                lut = _scale_lut(maxval, 255) + [0] * (255 - maxval)
                pixels = bytearray(raster.translate(bytes(lut)))
        else:
            vals = image_ops.unpack16(raster)
            if max(vals, default=0) > maxval:
                raise CorruptFile("Netpbm sample exceeds the declared maxval.")
            if maxval != 65535:
                vals = [(v * 65535 + maxval // 2) // maxval for v in vals]
            pixels = image_ops.pack16(vals)
        if maxval not in (255, 65535):
            meta["original_maxval_scaled"] = True
        progress.report(height, height, "decoding")
        return Image(width, height, mode, depth, pixels, meta)

    # ASCII variants (P1, P2, P3): comments are tolerated anywhere.
    body = _COMMENT.sub(b" ", data[pos - 1:])
    if kind == 1:
        digits = bytes(c for c in body if c not in _WS)
        if len(digits) < count:
            raise CorruptFile("PBM pixel data is truncated.")
        digits = digits[:count]
        if digits.strip(b"01"):
            raise CorruptFile("PBM pixel data contains a value other than 0 or 1.")
        meta["original_bit_depth"] = 1
        progress.report(height, height, "decoding")
        return Image(width, height, "L", 8,
                     bytearray(digits.translate(bytes.maketrans(b"01", b"\xff\x00"))), meta)
    tokens = body.split()
    if len(tokens) < count:
        raise CorruptFile("Netpbm pixel data is truncated.")
    try:
        vals = [int(t) for t in tokens[:count]]
    except ValueError:
        raise CorruptFile("Netpbm pixel data contains a non-numeric value.") from None
    if max(vals, default=0) > maxval:
        raise CorruptFile("Netpbm sample exceeds the declared maxval.")
    top = 65535 if depth == 16 else 255
    if maxval != top:
        lut = _scale_lut(maxval, top)
        vals = [lut[v] for v in vals]
    pixels = bytearray(vals) if depth == 8 else image_ops.pack16(vals)
    progress.report(height, height, "decoding")
    return Image(width, height, mode, depth, pixels, meta)


# --------------------------------------------------------------------------
# writers
# --------------------------------------------------------------------------

def _ascii_lines(values, per_line: int) -> bytes:
    lines = [" ".join(map(str, values[i:i + per_line])) for i in range(0, len(values), per_line)]
    return ("\n".join(lines) + "\n").encode("ascii")


def encode_pbm(img: Image, progress=NULL_PROGRESS, notes=None, ascii=False,
               background=image_ops.WHITE, **_):
    img = image_ops.convert(img, "L", 8, background, notes)
    w, h = img.width, img.height
    px = bytes(img.pixels)
    # Threshold at 50 %: dark pixels (< 128) become black (bit 1).
    bits = px.translate(bytes(1 if v < 128 else 0 for v in range(256)))
    if notes is not None:
        if px.translate(None, b"\x00\xff"):
            notes.append("Converted to 1-bit black and white (threshold at 50% grey).")
    header = f"P{1 if ascii else 4}\n{w} {h}\n".encode("ascii")
    if ascii:
        digits = bits.translate(bytes.maketrans(b"\x00\x01", b"01"))
        lines = [digits[i:i + 70] for i in range(0, len(digits), 70)]
        return header + b"\n".join(lines) + b"\n"
    out = bytearray(header)
    row_len = (w + 7) // 8
    pad = b"0" * (row_len * 8 - w)
    to_digit = bytes.maketrans(b"\x00\x01", b"01")
    for y in range(h):
        digits = bits[y * w:(y + 1) * w].translate(to_digit) + pad
        out += int(digits, 2).to_bytes(row_len, "big")
        if (y & 127) == 0:
            progress.check_cancelled()
            progress.report(y, h, "encoding")
    return bytes(out)


def _encode_graymap(img: Image, magic_ascii: int, mode: str, progress, notes, ascii, background):
    target_depth = img.bit_depth
    img = image_ops.convert(img, mode, target_depth, background, notes)
    maxval = 65535 if img.bit_depth == 16 else 255
    magic = magic_ascii if ascii else magic_ascii + 3
    header = f"P{magic}\n{img.width} {img.height}\n{maxval}\n".encode("ascii")
    progress.report(0, 1, "encoding")
    if not ascii:
        return header + bytes(img.pixels)
    vals = list(img.pixels) if img.bit_depth == 8 else list(image_ops.unpack16(img.pixels))
    per_line = 17 if maxval == 255 else 11
    return header + _ascii_lines(vals, per_line)


def encode_pgm(img: Image, progress=NULL_PROGRESS, notes=None, ascii=False,
               background=image_ops.WHITE, **_):
    return _encode_graymap(img, 2, "L", progress, notes, ascii, background)


def encode_ppm(img: Image, progress=NULL_PROGRESS, notes=None, ascii=False,
               background=image_ops.WHITE, **_):
    return _encode_graymap(img, 3, "RGB", progress, notes, ascii, background)


def unsupported_pam(data: bytes):
    if data[:2] == b"P7":
        raise UnsupportedVariant("PAM (P7) files are not supported in OpenConverter 1.0.")
