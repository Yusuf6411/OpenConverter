"""Image formats: every readable image converts to every writable image
through the Image IR."""

import logging

from openconverter.codecs import bmp, gif, jpeg, netpbm, png, qoi, tga
from openconverter.converters.base import (OWN, ConversionResult, FormatSpec, bool_option, choice_option,
                                           colour_option, int_option)
from openconverter.errors import CorruptFile, OpenConverterError
from openconverter.limits import DEFAULT_LIMITS
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS, SubProgress
from openconverter.ops import image_ops

log = logging.getLogger("openconverter.image")

BACKGROUND = colour_option("Background colour", "#FFFFFF",
                           "Transparent areas are flattened onto this colour.")
ASCII = bool_option("ASCII (plain) encoding", False, "Write P1/P2/P3 text instead of binary.")


def _kind(k):
    return lambda data: netpbm.sniff_kind(data) == k


IMAGE_FORMATS = [
    FormatSpec("png", "PNG", "image", (".png",), "image/png", png.sniff, True, True, OWN,
               "Read: all colour types and bit depths, Adam7, tRNS, PLTE. Write: best-fit colour type, "
               "adaptive filters, levels 0-9.",
               write_options={"compression_level": int_option("Compression level", 0, 9, 6,
                                                              "0 = stored, 9 = smallest (slowest)."),
                              "palette": choice_option("Palette", ["auto", "off", "on"], "auto",
                                                       "Use a colour palette when the image has <= 256 colours.")},
               identity_meaningful=True),
    FormatSpec("jpeg", "JPEG", "image", (".jpg", ".jpeg", ".jpe", ".jfif"), "image/jpeg", jpeg.sniff, True, True,
               OWN, "Baseline (sequential Huffman) 8-bit, greyscale or YCbCr, any sampling, restart markers, "
               "EXIF orientation. Progressive, arithmetic, lossless, 12-bit and CMYK are not supported.",
               write_options={"quality": int_option("Quality", 1, 100, 85, "Higher is better and larger."),
                              "subsampling": choice_option("Chroma subsampling", ["auto", "4:4:4", "4:2:0"], "auto",
                                                           "auto = 4:2:0 below quality 90, else 4:4:4."),
                              "optimize_huffman": bool_option("Optimise Huffman tables", False,
                                                              "Slightly smaller files, slower."),
                              "background": BACKGROUND},
               identity_meaningful=True),
    FormatSpec("gif", "GIF", "image", (".gif",), "image/gif", gif.sniff, True, False, OWN,
               "Read only: first frame, global/local palettes, transparency, interlacing."),
    FormatSpec("bmp", "BMP", "image", (".bmp", ".dib"), "image/bmp", bmp.sniff, True, True, OWN,
               "Read: 1/4/8-bit palettised, 16/24/32-bit, RLE4/RLE8, bit fields, OS/2 and V4/V5 headers. "
               "Write: 24-bit, or 32-bit with alpha."),
    FormatSpec("tga", "TGA", "image", (".tga", ".tpic"), "image/x-tga", tga.sniff, True, True, OWN,
               "Colour-mapped, true-colour and greyscale, uncompressed or RLE, 8/15/16/24/32-bit.",
               write_options={"rle": bool_option("RLE compression", False)},
               identity_meaningful=True),
    FormatSpec("qoi", "QOI", "image", (".qoi",), "image/qoi", qoi.sniff, True, True, OWN,
               "Full QOI specification (RGB/RGBA)."),
    FormatSpec("ppm", "PPM", "image", (".ppm",), "image/x-portable-pixmap", _kind("ppm"), True, True, OWN,
               "P3/P6, maxval 1-65535 (16-bit kept).", write_options={"ascii": ASCII, "background": BACKGROUND},
               identity_meaningful=True),
    FormatSpec("pgm", "PGM", "image", (".pgm",), "image/x-portable-graymap", _kind("pgm"), True, True, OWN,
               "P2/P5, maxval 1-65535 (16-bit kept).", write_options={"ascii": ASCII, "background": BACKGROUND},
               identity_meaningful=True),
    FormatSpec("pbm", "PBM", "image", (".pbm",), "image/x-portable-bitmap", _kind("pbm"), True, True, OWN,
               "P1/P4 1-bit bitmaps (writing thresholds at 50% grey).",
               write_options={"ascii": ASCII, "background": BACKGROUND}, identity_meaningful=True),
]

DECODERS = {"png": png.decode, "jpeg": jpeg.decode, "gif": gif.decode, "bmp": bmp.decode, "tga": tga.decode,
            "qoi": qoi.decode, "ppm": netpbm.decode, "pgm": netpbm.decode, "pbm": netpbm.decode}
ENCODERS = {"png": png.encode, "jpeg": jpeg.encode, "bmp": bmp.encode, "tga": tga.encode, "qoi": qoi.encode,
            "ppm": netpbm.encode_ppm, "pgm": netpbm.encode_pgm, "pbm": netpbm.encode_pbm}

# Relative cost of decoding/encoding (used to weight progress by real work).
DECODE_COST = {"png": 3, "jpeg": 3, "gif": 1, "bmp": 0.5, "tga": 1, "qoi": 1.5, "ppm": 0.3, "pgm": 0.3, "pbm": 0.5}
ENCODE_COST = {"png": 6, "jpeg": 3, "bmp": 0.4, "tga": 0.6, "qoi": 1.5, "ppm": 0.3, "pgm": 0.3, "pbm": 0.5}


def decode_image(data: bytes, fmt: str, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> Image:
    """Run a decoder, converting stray internal exceptions into CorruptFile."""
    try:
        return DECODERS[fmt](data, progress, limits)
    except OpenConverterError:
        raise
    except (MemoryError, RecursionError) as exc:
        log.exception("decoder %s failed with %s", fmt, type(exc).__name__)
        raise CorruptFile("The file could not be decoded (it appears to be damaged).") from None
    except Exception:  # noqa: BLE001 - a codec bug must not crash the job
        log.exception("unexpected error in %s decoder", fmt)
        raise CorruptFile("The file could not be decoded (it appears to be damaged).") from None


def convert_image(data: bytes, source: str, target: str, options: dict, progress=NULL_PROGRESS,
                  limits=DEFAULT_LIMITS) -> ConversionResult:
    dc = DECODE_COST[source]
    ec = ENCODE_COST[target]
    total = dc + ec + 0.2
    d_end = dc / total
    c_end = (dc + 0.1) / total
    e_end = (dc + 0.1 + ec) / total
    notes = []
    if source == target and not options_change_output(target, options):
        notes.append("No change expected: converting to the same format with default options.")
    progress.report(0, 1000, "decoding")
    img = decode_image(data, source, SubProgress(progress, 0.0, d_end, "decoding"), limits)
    notes.extend(img.metadata.get("notes", []))
    progress.check_cancelled()
    progress.report(int(c_end * 1000), 1000, "converting")
    opts = dict(options)
    if "background" in opts:
        opts["background"] = image_ops.parse_colour(opts["background"])
    out = ENCODERS[target](img, progress=SubProgress(progress, c_end, e_end, "encoding"), notes=notes, **opts)
    progress.report(int(e_end * 1000), 1000, "finalizing")
    if out == data:
        notes.append("No change: the output is identical to the input.")
    info = {"width": img.width, "height": img.height, "mode": img.mode, "bit_depth": img.bit_depth}
    return ConversionResult(out, list(dict.fromkeys(notes)), info)


def options_change_output(fmt: str, options: dict) -> bool:
    spec = next(f for f in IMAGE_FORMATS if f.id == fmt)
    return spec.identity_meaningful and bool(spec.write_options)


def thumbnail_png(img: Image, max_side: int = 512) -> bytes:
    """PNG preview made with our own resize and encoder."""
    small = image_ops.thumbnail(img, max_side)
    return png.encode(small, compression_level=3)


def probe(data: bytes, fmt: str) -> dict:
    """Declared image dimensions from the header only (cheap; no pixel decoding).

    Returns {} when the header cannot be read; full validation happens when
    the file is actually converted.
    """
    import struct
    try:
        if fmt == "png" and data[12:16] == b"IHDR":
            w, h = struct.unpack_from(">II", data, 16)
        elif fmt == "gif":
            w, h = struct.unpack_from("<HH", data, 6)
        elif fmt == "bmp":
            hsize = struct.unpack_from("<I", data, 14)[0]
            if hsize == 12:
                w, h = struct.unpack_from("<HH", data, 18)
            else:
                w, h = struct.unpack_from("<ii", data, 18)
                h = abs(h)
        elif fmt == "tga":
            w, h = struct.unpack_from("<HH", data, 12)
        elif fmt == "qoi":
            w, h = struct.unpack_from(">II", data, 4)
        elif fmt in ("ppm", "pgm", "pbm"):
            (w, h, *_), _ = netpbm._read_header(data[:4096] + b"\n", 2)
        elif fmt == "jpeg":
            pos = 2
            w = h = 0
            while pos + 9 < len(data):
                if data[pos] != 0xFF:
                    return {}
                marker = data[pos + 1]
                if marker == 0xFF:
                    pos += 1
                    continue
                length = struct.unpack_from(">H", data, pos + 2)[0]
                if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
                    h, w = struct.unpack_from(">HH", data, pos + 5)
                    break
                pos += 2 + length
            if not w:
                return {}
        else:
            return {}
    except (struct.error, CorruptFile, ValueError):
        return {}
    if w <= 0 or h <= 0:
        return {}
    return {"width": w, "height": h, "may_have_alpha": _may_have_alpha(data, fmt)}


def _may_have_alpha(data: bytes, fmt: str) -> bool:
    """Best-effort header check used by the UI to decide whether to offer a
    background colour (the conversion itself always uses the real pixels)."""
    import struct
    try:
        if fmt == "png":
            if data[25] in (4, 6):
                return True
            return b"tRNS" in data[33:data.find(b"IDAT")] if b"IDAT" in data else b"tRNS" in data
        if fmt == "qoi":
            return data[12] == 4
        if fmt == "gif":
            gce = b"\x21\xf9\x04"
            return any(data[i + 3] & 1 for i in _find_all(data, gce) if i + 3 < len(data))
        if fmt == "bmp":
            hsize = struct.unpack_from("<I", data, 14)[0]
            bpp = struct.unpack_from("<H", data, 28)[0] if hsize >= 40 else 0
            return bpp == 32 and hsize >= 56
        if fmt == "tga":
            depth, desc = data[16], data[17]
            return bool(desc & 0x0F) or depth == 32
    except (IndexError, struct.error):
        return False
    return False


def _find_all(data: bytes, needle: bytes):
    i = data.find(needle)
    while i >= 0:
        yield i
        i = data.find(needle, i + 1)
