"""QOI - "Quite OK Image" format, full specification (qoiformat.org, v1.0)."""

import struct
import sys
from array import array

from openconverter.errors import CorruptFile
from openconverter.limits import DEFAULT_LIMITS, check_dimensions
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS
from openconverter.ops import image_ops

MAGIC = b"qoif"
END_MARKER = b"\x00\x00\x00\x00\x00\x00\x00\x01"
OP_INDEX, OP_DIFF, OP_LUMA, OP_RUN = 0x00, 0x40, 0x80, 0xC0
OP_RGB, OP_RGBA = 0xFE, 0xFF
_WORD = next(code for code in ("I", "L") if array(code).itemsize == 4)
_LITTLE = sys.byteorder == "little"
CHUNK = 1 << 16  # pixels between progress/cancel checks


_MUL = {k: bytes((k * v) & 63 for v in range(256)) for k in (3, 5, 7, 11)}
_PLUS128 = bytes((v + 128) & 255 for v in range(256))


def _lane_sum_mod64(planes) -> bytes:
    n = len(planes[0])
    total = 0
    for p in planes:
        total += int.from_bytes(p, "little")
    mask = int.from_bytes(b"\x3f" * n, "little") if n else 0
    return (total & mask).to_bytes(n, "little")


def _diff_plus128(plane: bytes, first_prev: int) -> bytes:
    """(plane[i] - plane[i-1] + 128) mod 256 for every i (plane[-1] = first_prev)."""
    n = len(plane)
    if n == 0:
        return b""
    shifted = bytes([first_prev]) + plane[:-1]
    hi = int.from_bytes(b"\x80" * n, "little")
    lo = hi ^ int.from_bytes(b"\xff" * n, "little")
    a = int.from_bytes(plane, "little")
    b = int.from_bytes(shifted, "little")
    diff = ((a | hi) - (b & lo)) ^ ((a ^ b ^ hi) & hi)  # byte-lane subtraction mod 256
    return diff.to_bytes(n, "little").translate(_PLUS128)


def sniff(data: bytes) -> bool:
    return len(data) >= 14 and data[:4] == MAGIC


def _words_to_rgba(words: array) -> bytearray:
    """Pixels packed as r | g<<8 | b<<16 | a<<24 -> RGBA bytes."""
    if not _LITTLE:
        words.byteswap()
    return bytearray(words.tobytes())


def decode(data: bytes, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> Image:
    data = bytes(data)
    if len(data) < 14 or data[:4] != MAGIC:
        raise CorruptFile("Not a QOI file (missing 'qoif' magic).")
    width, height, channels, colorspace = struct.unpack_from(">IIBB", data, 4)
    if channels not in (3, 4):
        raise CorruptFile(f"QOI channel count {channels} is invalid.")
    if colorspace not in (0, 1):
        raise CorruptFile(f"QOI colour space {colorspace} is invalid.")
    check_dimensions(width, height, limits)
    total = width * height
    end = len(data) - 8
    if end < 14 or data[end:] != END_MARKER:
        raise CorruptFile("QOI file is truncated (end marker missing).")
    # Padding lets the op loop read without bounds checks; overruns are
    # detected after each chunk by comparing p with ``end``.
    buf = data[:end] + bytes(16)
    pixels = []
    push = pixels.append
    extend = pixels.extend
    index = [0] * 64
    r = g = b = 0
    a = 255
    v = 255 << 24
    p = 14
    i = 0
    progress.report(0, total, "decoding")
    while i < total:
        stop = min(total, i + CHUNK)
        while i < stop:
            b1 = buf[p]
            p += 1
            if b1 < 0x40:                                    # OP_INDEX
                v = index[b1]
                r = v & 255
                g = (v >> 8) & 255
                b = (v >> 16) & 255
                a = v >> 24
                push(v)
                i += 1
                continue
            if b1 < 0x80:                                    # OP_DIFF
                r = (r + ((b1 >> 4) & 3) - 2) & 255
                g = (g + ((b1 >> 2) & 3) - 2) & 255
                b = (b + (b1 & 3) - 2) & 255
            elif b1 < 0xC0:                                  # OP_LUMA
                b2 = buf[p]
                p += 1
                vg = b1 - 0xA0                                # (b1 & 0x3f) - 32
                r = (r + vg - 8 + (b2 >> 4)) & 255
                g = (g + vg) & 255
                b = (b + vg - 8 + (b2 & 15)) & 255
            elif b1 == OP_RGB:
                r = buf[p]
                g = buf[p + 1]
                b = buf[p + 2]
                p += 3
            elif b1 == OP_RGBA:
                r = buf[p]
                g = buf[p + 1]
                b = buf[p + 2]
                a = buf[p + 3]
                p += 4
            else:                                            # OP_RUN of the previous pixel
                run = b1 - 0xBF
                index[(r * 3 + g * 5 + b * 7 + a * 11) & 63] = v
                extend([v] * run)
                i += run
                continue
            v = r | (g << 8) | (b << 16) | (a << 24)
            index[(r * 3 + g * 5 + b * 7 + a * 11) & 63] = v
            push(v)
            i += 1
        if p > end:
            raise CorruptFile("QOI pixel data is truncated.")
        progress.check_cancelled()
        progress.report(min(i, total), total, "decoding")
    if p > end:
        raise CorruptFile("QOI pixel data is truncated.")
    del pixels[total:]
    out = _words_to_rgba(array(_WORD, pixels))
    meta = {"source_format": "qoi", "qoi_colorspace": "linear" if colorspace else "sRGB"}
    if channels == 3:
        rgb = bytearray(total * 3)
        rgb[0::3] = out[0::4]
        rgb[1::3] = out[1::4]
        rgb[2::3] = out[2::4]
        return Image(width, height, "RGB", 8, rgb, meta)
    return Image(width, height, "RGBA", 8, out, meta)


def encode(img: Image, progress=NULL_PROGRESS, notes=None, background=image_ops.WHITE, **_) -> bytes:
    has_alpha = img.has_alpha
    img = image_ops.convert(img, "RGBA" if has_alpha else "RGB", 8, background, notes)
    w, h = img.width, img.height
    total = w * h
    src = bytes(img.pixels)
    if has_alpha:
        rgba = src
    else:
        buf = bytearray(b"\xff" * (total * 4))
        buf[0::4] = src[0::3]
        buf[1::4] = src[1::3]
        buf[2::4] = src[2::3]
        rgba = bytes(buf)
    words = array(_WORD)
    words.frombytes(rgba)
    if not _LITTLE:
        words.byteswap()  # values are always r | g<<8 | b<<16 | a<<24
    R, G, B, A = rgba[0::4], rgba[1::4], rgba[2::4], rgba[3::4]
    # Per-pixel colour hashes, computed for the whole image with byte-lane
    # arithmetic: each lane holds (k * v) & 63, so four lanes sum below 256.
    hashes = _lane_sum_mod64([R.translate(_MUL[3]), G.translate(_MUL[5]), B.translate(_MUL[7]),
                              A.translate(_MUL[11])])
    # Signed differences to the previous pixel, offset by 128 (prev of pixel 0 is 0,0,0,255).
    DR = _diff_plus128(R, 0)
    DG = _diff_plus128(G, 0)
    DB = _diff_plus128(B, 0)
    out = bytearray(MAGIC + struct.pack(">IIBB", w, h, 4 if has_alpha else 3, 0))
    append = out.append
    extend = out.extend
    index = [0] * 64
    pa = 255
    prev = 255 << 24
    run = 0
    for start in range(0, total, CHUNK):
        stop = start + CHUNK
        for v, r, g, b, a, hsh, dr, dg, db in zip(words[start:stop], R[start:stop], G[start:stop],
                                                  B[start:stop], A[start:stop], hashes[start:stop],
                                                  DR[start:stop], DG[start:stop], DB[start:stop]):
            if v == prev:
                run += 1
                if run == 62:
                    append(0xFD)  # OP_RUN | 61
                    run = 0
                continue
            if run:
                append(OP_RUN | (run - 1))
                run = 0
            if index[hsh] == v:
                append(hsh)  # OP_INDEX
            else:
                index[hsh] = v
                if a == pa:
                    if 126 <= dr <= 129 and 126 <= dg <= 129 and 126 <= db <= 129:
                        append(OP_DIFF | ((dr - 126) << 4) | ((dg - 126) << 2) | (db - 126))
                    else:
                        vg_r = dr - dg
                        vg_b = db - dg
                        if -8 <= vg_r <= 7 and 96 <= dg <= 159 and -8 <= vg_b <= 7:
                            extend((OP_LUMA | (dg - 96), ((vg_r + 8) << 4) | (vg_b + 8)))
                        else:
                            extend((OP_RGB, r, g, b))
                else:
                    extend((OP_RGBA, r, g, b, a))
            prev = v
            pa = a
        progress.check_cancelled()
        progress.report(min(stop, total), total, "encoding")
    if run:
        append(OP_RUN | (run - 1))
    out += END_MARKER
    return bytes(out)
