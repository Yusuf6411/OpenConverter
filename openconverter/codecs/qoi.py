"""QOI - "Quite OK Image" format, full specification (qoiformat.org, v1.0)."""

import struct

from openconverter.errors import CorruptFile
from openconverter.limits import DEFAULT_LIMITS, check_dimensions
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS
from openconverter.ops import image_ops

MAGIC = b"qoif"
END_MARKER = b"\x00\x00\x00\x00\x00\x00\x00\x01"
OP_INDEX, OP_DIFF, OP_LUMA, OP_RUN = 0x00, 0x40, 0x80, 0xC0
OP_RGB, OP_RGBA = 0xFE, 0xFF


def sniff(data: bytes) -> bool:
    return len(data) >= 14 and data[:4] == MAGIC


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
    out = bytearray(total * 4)
    index = [0] * 64
    r = g = b = 0
    a = 255
    p = 14
    i = 0
    o = 0
    report_every = max(width * 64, 1 << 16)
    next_report = report_every
    progress.report(0, total, "decoding")
    while i < total:
        if p >= end:
            raise CorruptFile("QOI pixel data is truncated.")
        b1 = data[p]
        p += 1
        if b1 == OP_RGB:
            if p + 3 > end:
                raise CorruptFile("QOI pixel data is truncated.")
            r, g, b = data[p], data[p + 1], data[p + 2]
            p += 3
            run = 1
        elif b1 == OP_RGBA:
            if p + 4 > end:
                raise CorruptFile("QOI pixel data is truncated.")
            r, g, b, a = data[p], data[p + 1], data[p + 2], data[p + 3]
            p += 4
            run = 1
        else:
            tag = b1 & 0xC0
            if tag == OP_INDEX:
                v = index[b1]
                r, g, b, a = v >> 24, (v >> 16) & 255, (v >> 8) & 255, v & 255
                run = 1
            elif tag == OP_DIFF:
                r = (r + ((b1 >> 4) & 3) - 2) & 255
                g = (g + ((b1 >> 2) & 3) - 2) & 255
                b = (b + (b1 & 3) - 2) & 255
                run = 1
            elif tag == OP_LUMA:
                if p >= end:
                    raise CorruptFile("QOI pixel data is truncated.")
                b2 = data[p]
                p += 1
                vg = (b1 & 0x3F) - 32
                r = (r + vg - 8 + ((b2 >> 4) & 15)) & 255
                g = (g + vg) & 255
                b = (b + vg - 8 + (b2 & 15)) & 255
                run = 1
            else:
                run = (b1 & 0x3F) + 1
        index[(r * 3 + g * 5 + b * 7 + a * 11) & 63] = (r << 24) | (g << 16) | (b << 8) | a
        if run == 1:
            out[o] = r
            out[o + 1] = g
            out[o + 2] = b
            out[o + 3] = a
            o += 4
            i += 1
        else:
            if i + run > total:
                run = total - i
            out[o:o + 4 * run] = bytes((r, g, b, a)) * run
            o += 4 * run
            i += run
        if i >= next_report:
            next_report += report_every
            progress.check_cancelled()
            progress.report(i, total, "decoding")
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
    out = bytearray(MAGIC + struct.pack(">IIBB", w, h, 4 if has_alpha else 3, 0))
    index = [0] * 64
    pr, pg, pb, pa = 0, 0, 0, 255
    prev = 0x000000FF
    run = 0
    append = out.append
    words = memoryview(rgba).cast("B")
    report_every = max(w * 64, 1 << 16)
    for i in range(total):
        o = i * 4
        r, g, b, a = words[o], words[o + 1], words[o + 2], words[o + 3]
        v = (r << 24) | (g << 16) | (b << 8) | a
        if v == prev:
            run += 1
            if run == 62:
                append(OP_RUN | 61)
                run = 0
            continue
        if run:
            append(OP_RUN | (run - 1))
            run = 0
        hsh = (r * 3 + g * 5 + b * 7 + a * 11) & 63
        if index[hsh] == v:
            append(OP_INDEX | hsh)
        else:
            index[hsh] = v
            if a == pa:
                vr = ((r - pr + 128) & 255) - 128
                vg = ((g - pg + 128) & 255) - 128
                vb = ((b - pb + 128) & 255) - 128
                vg_r = vr - vg
                vg_b = vb - vg
                if -2 <= vr <= 1 and -2 <= vg <= 1 and -2 <= vb <= 1:
                    append(OP_DIFF | ((vr + 2) << 4) | ((vg + 2) << 2) | (vb + 2))
                elif -8 <= vg_r <= 7 and -32 <= vg <= 31 and -8 <= vg_b <= 7:
                    append(OP_LUMA | (vg + 32))
                    append(((vg_r + 8) << 4) | (vg_b + 8))
                else:
                    out += bytes((OP_RGB, r, g, b))
            else:
                out += bytes((OP_RGBA, r, g, b, a))
        prev = v
        pr, pg, pb, pa = r, g, b, a
        if i % report_every == 0:
            progress.check_cancelled()
            progress.report(i, total, "encoding")
    if run:
        append(OP_RUN | (run - 1))
    out += END_MARKER
    progress.report(total, total, "encoding")
    return bytes(out)
