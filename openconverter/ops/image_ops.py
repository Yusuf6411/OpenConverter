"""Pixel operations on the Image IR: depth/mode conversion, alpha flattening,
orientation and resizing.

Every rule that changes the image appends a human-readable note so the user
can see exactly what happened (see docs/FORMATS.md, "Conversion rules").
"""

import sys
from array import array

from openconverter.errors import BadRequest
from openconverter.models.image import Image

_LITTLE = sys.byteorder == "little"

WHITE = (255, 255, 255)


# --------------------------------------------------------------------------
# helpers
# --------------------------------------------------------------------------

def parse_colour(value: str) -> tuple[int, int, int]:
    """Parse ``#RRGGBB`` into an (r, g, b) tuple."""
    if not isinstance(value, str) or len(value) != 7 or value[0] != "#":
        raise BadRequest(f"Invalid colour {value!r}; expected #RRGGBB.")
    try:
        return int(value[1:3], 16), int(value[3:5], 16), int(value[5:7], 16)
    except ValueError:
        raise BadRequest(f"Invalid colour {value!r}; expected #RRGGBB.") from None


def format_colour(rgb: tuple[int, int, int]) -> str:
    return "#{:02X}{:02X}{:02X}".format(*rgb)


def unpack16(buf) -> array:
    """Big-endian 16-bit bytes -> array('H') of native ints."""
    arr = array("H")
    arr.frombytes(bytes(buf))
    if _LITTLE:
        arr.byteswap()
    return arr


def pack16(values) -> bytearray:
    """Iterable of ints (0..65535) -> big-endian bytes."""
    arr = values if isinstance(values, array) and values.typecode == "H" else array("H", values)
    if _LITTLE:
        arr = array("H", arr)
        arr.byteswap()
    return bytearray(arr.tobytes())


def _note(notes, text):
    if notes is not None and text not in notes:
        notes.append(text)


_LUT_16_TO_8 = None


def _lut_16_to_8() -> bytes:
    global _LUT_16_TO_8
    if _LUT_16_TO_8 is None:
        _LUT_16_TO_8 = bytes((v * 255 + 32767) // 65535 for v in range(65536))
    return _LUT_16_TO_8


_FLATTEN_TABLES: dict[int, bytes] = {}


def _flatten_table(bg: int) -> bytes:
    """table[(a << 8) | c] = (c*a + bg*(255-a) + 127) // 255."""
    table = _FLATTEN_TABLES.get(bg)
    if table is None:
        table = bytes((c * a + bg * (255 - a) + 127) // 255
                      for a in range(256) for c in range(256))
        _FLATTEN_TABLES[bg] = table
    return table


# --------------------------------------------------------------------------
# bit depth
# --------------------------------------------------------------------------

def to_8bit(img: Image, notes=None) -> Image:
    if img.bit_depth == 8:
        return img
    lut = _lut_16_to_8()
    out = bytearray(map(lut.__getitem__, unpack16(img.pixels)))
    _note(notes, "Reduced from 16 to 8 bits per channel.")
    meta = dict(img.metadata)
    meta.setdefault("original_bit_depth", 16)
    return Image(img.width, img.height, img.mode, 8, out, meta)


def to_16bit(img: Image, notes=None) -> Image:
    if img.bit_depth == 16:
        return img
    src = img.pixels
    out = bytearray(len(src) * 2)
    out[0::2] = src
    out[1::2] = src  # v * 257 == (v << 8) | v
    _note(notes, "Expanded from 8 to 16 bits per channel (v x 257).")
    return Image(img.width, img.height, img.mode, 16, out, dict(img.metadata))


# --------------------------------------------------------------------------
# alpha
# --------------------------------------------------------------------------

def _split(img: Image):
    """Return per-channel sample sequences (bytes for 8-bit, arrays for 16-bit)."""
    n = img.channels
    if img.bit_depth == 8:
        buf = bytes(img.pixels)
        return [buf[i::n] for i in range(n)]
    vals = unpack16(img.pixels)
    return [vals[i::n] for i in range(n)]


def _merge(planes, bit_depth: int) -> bytearray:
    n = len(planes)
    count = len(planes[0])
    if bit_depth == 8:
        out = bytearray(count * n)
        for i, plane in enumerate(planes):
            out[i::n] = plane
        return out
    vals = array("H", bytes(count * n * 2))
    for i, plane in enumerate(planes):
        vals[i::n] = plane if isinstance(plane, array) else array("H", plane)
    return pack16(vals)


def is_opaque(img: Image) -> bool:
    if not img.has_alpha:
        return True
    alpha = _split(img)[-1]
    top = 255 if img.bit_depth == 8 else 65535
    return min(alpha) == top


def flatten_alpha(img: Image, background=WHITE, notes=None) -> Image:
    """Composite straight alpha onto ``background`` (an (r, g, b) tuple)."""
    if not img.has_alpha:
        return img
    planes = _split(img)
    alpha = planes.pop()
    colour = planes
    new_mode = "L" if img.mode == "LA" else "RGB"
    if img.mode == "LA":
        # Greyscale background: BT.601 luma of the chosen colour.
        r, g, b = background
        bgs = [(299 * r + 587 * g + 114 * b + 500) // 1000]
    else:
        bgs = list(background)
    top = 255 if img.bit_depth == 8 else 65535
    if min(alpha) == top:
        _note(notes, "Alpha channel removed (the image was fully opaque).")
        return Image(img.width, img.height, new_mode, img.bit_depth,
                     _merge(colour, img.bit_depth), dict(img.metadata))
    out_planes = []
    if img.bit_depth == 8:
        for plane, bg in zip(colour, bgs):
            t = _flatten_table(bg)
            out_planes.append(bytes([t[(a << 8) | c] for c, a in zip(plane, alpha)]))
    else:
        for plane, bg in zip(colour, bgs):
            bg16 = bg * 257
            out_planes.append(array("H", [(c * a + bg16 * (65535 - a) + 32767) // 65535
                                          for c, a in zip(plane, alpha)]))
    _note(notes, f"Alpha flattened onto {format_colour(tuple(background))}.")
    return Image(img.width, img.height, new_mode, img.bit_depth,
                 _merge(out_planes, img.bit_depth), dict(img.metadata))


def add_alpha(img: Image, notes=None) -> Image:
    if img.has_alpha:
        return img
    planes = _split(img)
    count = img.width * img.height
    if img.bit_depth == 8:
        planes.append(b"\xff" * count)
    else:
        planes.append(array("H", [65535]) * count)
    _note(notes, "Opaque alpha channel added.")
    return Image(img.width, img.height, "LA" if img.mode == "L" else "RGBA", img.bit_depth,
                 _merge(planes, img.bit_depth), dict(img.metadata))


def drop_alpha_if_opaque(img: Image) -> Image:
    """Remove an alpha channel that carries no information (lossless)."""
    if img.has_alpha and is_opaque(img):
        planes = _split(img)[:-1]
        return Image(img.width, img.height, "L" if img.mode == "LA" else "RGB",
                     img.bit_depth, _merge(planes, img.bit_depth), dict(img.metadata))
    return img


# --------------------------------------------------------------------------
# colour <-> grey
# --------------------------------------------------------------------------

_LUMA = ([299 * v for v in range(256)], [587 * v for v in range(256)],
         [114 * v + 500 for v in range(256)])


def to_grey(img: Image, notes=None) -> Image:
    if img.is_grey:
        return img
    planes = _split(img)
    r, g, b = planes[0], planes[1], planes[2]
    if img.bit_depth == 8:
        lr, lg, lb = _LUMA
        luma = bytes([(lr[x] + lg[y] + lb[z]) // 1000 for x, y, z in zip(r, g, b)])
    else:
        luma = array("H", [(299 * x + 587 * y + 114 * z + 500) // 1000 for x, y, z in zip(r, g, b)])
    out = [luma] + planes[3:]
    _note(notes, "Converted to greyscale (BT.601 luma).")
    return Image(img.width, img.height, "LA" if img.has_alpha else "L", img.bit_depth,
                 _merge(out, img.bit_depth), dict(img.metadata))


def is_greyscale_content(img: Image) -> bool:
    """True if every pixel of an RGB(A) image has R == G == B."""
    if img.is_grey:
        return True
    planes = _split(img)
    return planes[0] == planes[1] and planes[1] == planes[2]


def to_colour(img: Image, notes=None) -> Image:
    if not img.is_grey:
        return img
    planes = _split(img)
    out = [planes[0], planes[0], planes[0]] + planes[1:]
    _note(notes, "Greyscale expanded to RGB.")
    return Image(img.width, img.height, "RGBA" if img.has_alpha else "RGB", img.bit_depth,
                 _merge(out, img.bit_depth), dict(img.metadata))


# --------------------------------------------------------------------------
# general conversion
# --------------------------------------------------------------------------

def convert(img: Image, mode: str, bit_depth: int, background=WHITE, notes=None) -> Image:
    """Convert ``img`` to exactly ``mode``/``bit_depth``, noting each rule applied."""
    if bit_depth == 8 and img.bit_depth == 16:
        img = to_8bit(img, notes)
    want_alpha = mode in ("LA", "RGBA")
    want_grey = mode in ("L", "LA")
    if img.has_alpha and not want_alpha:
        img = flatten_alpha(img, background, notes)
    if want_grey and not img.is_grey:
        img = to_grey(img, notes)
    elif not want_grey and img.is_grey:
        img = to_colour(img, notes)
    if want_alpha and not img.has_alpha:
        img = add_alpha(img, notes)
    if bit_depth == 16 and img.bit_depth == 8:
        img = to_16bit(img, notes)
    assert img.mode == mode and img.bit_depth == bit_depth
    return img


# --------------------------------------------------------------------------
# orientation (EXIF 1..8)
# --------------------------------------------------------------------------

def apply_orientation(img: Image, orientation: int) -> Image:
    """Return ``img`` transformed so that EXIF ``orientation`` becomes 1."""
    if orientation == 1 or orientation not in range(1, 9):
        return img
    w, h, bpp = img.width, img.height, img.bytes_per_pixel
    src = bytes(img.pixels)
    swap = orientation >= 5
    nw, nh = (h, w) if swap else (w, h)
    out = bytearray(len(src))
    for k in range(bpp):
        plane = src[k::bpp]
        if orientation == 2:
            rows = [plane[y * w:(y + 1) * w][::-1] for y in range(h)]
        elif orientation == 3:
            rows = [plane[::-1]]
        elif orientation == 4:
            rows = [plane[y * w:(y + 1) * w] for y in range(h - 1, -1, -1)]
        elif orientation == 5:   # transpose
            rows = [plane[x::w] for x in range(w)]
        elif orientation == 6:   # rotate 90 clockwise
            rows = [plane[x::w][::-1] for x in range(w)]
        elif orientation == 7:   # transverse
            rows = [plane[x::w][::-1] for x in range(w - 1, -1, -1)]
        else:                    # 8: rotate 90 counter-clockwise
            rows = [plane[x::w] for x in range(w - 1, -1, -1)]
        out[k::bpp] = b"".join(rows)
    meta = dict(img.metadata)
    meta["orientation_applied"] = orientation
    return Image(nw, nh, img.mode, img.bit_depth, out, meta)


# --------------------------------------------------------------------------
# resizing
# --------------------------------------------------------------------------

def _resize_axis_weights(src: int, dst: int):
    """Box-filter (area-average) contributions for one axis, as integer weights.

    Returns a list over destination indices of [(src_index, weight), ...] with
    weights summing to ``src`` (the scale denominator).
    """
    result = []
    for d in range(dst):
        start = d * src           # in units of 1/dst source pixels
        end = start + src
        contrib = []
        s = start // dst
        while s * dst < end:
            lo = max(start, s * dst)
            hi = min(end, (s + 1) * dst)
            if hi > lo:
                contrib.append((s, hi - lo))
            s += 1
        result.append(contrib)
    return result


def resize(img: Image, new_w: int, new_h: int, progress=None) -> Image:
    """Area-average (box filter) resize. Exact for downscaling; nearest-like
    replication when upscaling. Works per channel at the image's bit depth."""
    if new_w <= 0 or new_h <= 0:
        raise ValueError("target size must be positive")
    if (new_w, new_h) == (img.width, img.height):
        return img.copy()
    n = img.channels
    w, h = img.width, img.height
    xw = _resize_axis_weights(w, new_w)
    yw = _resize_axis_weights(h, new_h)
    if img.bit_depth == 8:
        vals = img.pixels
    else:
        vals = unpack16(img.pixels)
    stride = w * n
    # Horizontal pass: each source row -> list of new_w*n sums (weight denom w).
    hrows = []
    for y in range(h):
        base = y * stride
        row = vals[base:base + stride]
        out_row = []
        for contrib in xw:
            for c in range(n):
                out_row.append(sum(row[s * n + c] * wt for s, wt in contrib))
        hrows.append(out_row)
        if progress is not None and (y & 63) == 0:
            progress.check_cancelled()
    denom = w * h
    half = denom // 2
    out = []
    for contrib in yw:
        acc = [0] * (new_w * n)
        for s, wt in contrib:
            r = hrows[s]
            for i in range(new_w * n):
                acc[i] += r[i] * wt
        out.extend((v + half) // denom for v in acc)
    if img.bit_depth == 8:
        data = bytearray(out)
    else:
        data = pack16(out)
    return Image(new_w, new_h, img.mode, img.bit_depth, data, dict(img.metadata))


def box_reduce(img: Image, f: int, progress=None) -> Image:
    """Downscale an 8-bit image by an integer factor ``f`` (area average).

    Any remainder columns/rows (fewer than ``f``) at the right/bottom edge are
    dropped. Uses slice arithmetic so the work stays in C loops.
    """
    if img.bit_depth != 8:
        raise ValueError("box_reduce works on 8-bit images")
    if f <= 1:
        return img.copy()
    n = img.channels
    w2, h2 = img.width // f, img.height // f
    if w2 == 0 or h2 == 0:
        raise ValueError("image is smaller than the reduction factor")
    src = bytes(img.pixels)
    stride = img.width * n
    span = w2 * f * n
    step = f * n
    ff = f * f
    half = ff // 2
    out = bytearray(w2 * h2 * n)
    out_stride = w2 * n
    for oy in range(h2):
        sums = [[half] * w2 for _ in range(n)]
        for dy in range(f):
            off = (oy * f + dy) * stride
            row = src[off:off + span]
            for c in range(n):
                acc = sums[c]
                for k in range(f):
                    acc = list(map(int.__add__, acc, row[k * n + c::step]))
                sums[c] = acc
        base = oy * out_stride
        for c in range(n):
            out[base + c:base + out_stride:n] = bytes([v // ff for v in sums[c]])
        if progress is not None and (oy & 15) == 0:
            progress.check_cancelled()
    return Image(w2, h2, img.mode, 8, out, dict(img.metadata))


def thumbnail(img: Image, max_side: int = 512, progress=None) -> Image:
    """8-bit preview no larger than ``max_side`` on either axis."""
    img = to_8bit(img)
    longest = max(img.width, img.height)
    if longest <= max_side:
        return img
    f = -(-longest // max_side)
    if img.width // f == 0 or img.height // f == 0:
        # Extremely thin image: fall back to the exact area-average resize.
        tw, th = thumbnail_size(img.width, img.height, max_side)
        return resize(img, tw, th, progress)
    return box_reduce(img, f, progress)


def thumbnail_size(width: int, height: int, max_side: int) -> tuple[int, int]:
    if width <= max_side and height <= max_side:
        return width, height
    if width >= height:
        return max_side, max(1, round(height * max_side / width))
    return max(1, round(width * max_side / height)), max_side
