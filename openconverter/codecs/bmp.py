"""Windows/OS2 bitmap (BMP) reader and writer."""

import struct
import sys
from array import array

from openconverter.errors import CorruptFile, UnsupportedVariant
from openconverter.limits import DEFAULT_LIMITS, check_dimensions
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS
from openconverter.ops import image_ops

BI_RGB, BI_RLE8, BI_RLE4, BI_BITFIELDS, BI_JPEG, BI_PNG, BI_ALPHABITFIELDS = range(7)
_LITTLE = sys.byteorder == "little"


def sniff(data: bytes) -> bool:
    if len(data) < 18 or data[:2] != b"BM":
        return False
    return struct.unpack_from("<I", data, 14)[0] in (12, 16, 40, 52, 56, 64, 108, 124)


def _expand_bits(row: bytes, bpp: int, width: int) -> bytes:
    """Unpack 1/2/4-bit palette indices (MSB first) into one byte per pixel."""
    if bpp == 8:
        return row[:width]
    if bpp == 4:
        out = bytearray(len(row) * 2)
        out[0::2] = row.translate(_HI_NIBBLE)
        out[1::2] = row.translate(_LO_NIBBLE)
        return bytes(out[:width])
    if bpp == 2:
        return b"".join(_EXPAND2[b] for b in row)[:width]
    return b"".join(_EXPAND1[b] for b in row)[:width]


_HI_NIBBLE = bytes(b >> 4 for b in range(256))
_LO_NIBBLE = bytes(b & 15 for b in range(256))
_EXPAND1 = [bytes((b >> (7 - i)) & 1 for i in range(8)) for b in range(256)]
_EXPAND2 = [bytes((b >> (6 - 2 * i)) & 3 for i in range(4)) for b in range(256)]


def _mask_shift_bits(mask: int):
    if mask == 0:
        return 0, 0
    shift = (mask & -mask).bit_length() - 1
    bits = (mask >> shift).bit_length()
    if (mask >> shift) != (1 << bits) - 1:
        raise CorruptFile("BMP bit-field mask is not contiguous.")
    return shift, bits


def _channel_lut(bits: int):
    top = (1 << bits) - 1
    return bytes((v * 255 + top // 2) // top for v in range(top + 1))


def _extract_fields(vals, masks, has_alpha):
    """Pixel words -> list of 8-bit channel planes (R, G, B[, A])."""
    planes = []
    for mask in masks[:4 if has_alpha else 3]:
        shift, bits = _mask_shift_bits(mask)
        if bits == 0:
            planes.append(bytes(len(vals)))
            continue
        top = (1 << bits) - 1
        if bits <= 16:
            lut = _channel_lut(bits)
            planes.append(bytes([lut[(v >> shift) & top] for v in vals]))
        else:
            planes.append(bytes([(((v >> shift) & top) * 255 + top // 2) // top for v in vals]))
    return planes


def _rle_decode(data: bytes, pos: int, width: int, height: int, bpp: int, progress) -> bytearray:
    """Decode BI_RLE8 / BI_RLE4 into palette indices, bottom-up row order."""
    out = bytearray(width * height)  # undefined pixels take palette index 0
    x = y = 0
    n = len(data)
    rows_done = 0
    while True:
        if pos + 2 > n:
            raise CorruptFile("BMP RLE data is truncated.")
        count, value = data[pos], data[pos + 1]
        pos += 2
        if count:
            if y >= height:
                raise CorruptFile("BMP RLE data runs past the last row.")
            run = min(count, width - x)
            if bpp == 8:
                out[y * width + x:y * width + x + run] = bytes([value]) * run
            else:
                pair = bytes([value >> 4, value & 15])
                out[y * width + x:y * width + x + run] = (pair * ((run + 1) // 2))[:run]
            x += count
            if x > width:
                x = width
            continue
        if value == 0:          # end of line
            x = 0
            y += 1
            rows_done += 1
            if (rows_done & 63) == 0:
                progress.check_cancelled()
                progress.report(min(y, height), height, "decoding")
        elif value == 1:        # end of bitmap
            break
        elif value == 2:        # delta
            if pos + 2 > n:
                raise CorruptFile("BMP RLE delta is truncated.")
            x += data[pos]
            y += data[pos + 1]
            pos += 2
            if x > width or y > height:
                raise CorruptFile("BMP RLE delta moves outside the image.")
        else:                   # absolute run of `value` pixels
            if bpp == 8:
                nbytes = value
                chunk = data[pos:pos + nbytes]
                pix = chunk
            else:
                nbytes = (value + 1) // 2
                chunk = data[pos:pos + nbytes]
                pix = _expand_bits(chunk, 4, value)
            if len(chunk) < nbytes:
                raise CorruptFile("BMP RLE absolute run is truncated.")
            pos += nbytes + (nbytes & 1)
            if y >= height:
                raise CorruptFile("BMP RLE data runs past the last row.")
            run = min(value, width - x)
            out[y * width + x:y * width + x + run] = pix[:run]
            x += value
            if x > width:
                x = width
        if y > height:
            raise CorruptFile("BMP RLE data runs past the last row.")
    return out


def decode(data: bytes, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> Image:
    data = bytes(data)
    if len(data) < 26 or data[:2] != b"BM":
        raise CorruptFile("Not a BMP file (missing 'BM' signature).")
    off_bits = struct.unpack_from("<I", data, 10)[0]
    hsize = struct.unpack_from("<I", data, 14)[0]
    if hsize not in (12, 16, 40, 52, 56, 64, 108, 124):
        raise UnsupportedVariant(f"BMP header size {hsize} is not supported.")
    if 14 + hsize > len(data):
        raise CorruptFile("BMP header is truncated.")
    masks = None
    meta = {"source_format": "bmp"}
    if hsize == 12:
        width, height, planes, bpp = struct.unpack_from("<HHHH", data, 18)
        compression = BI_RGB
        top_down = False
        pal_entry = 3
        colours_used = 0
        meta["bmp_header"] = "OS/2 BITMAPCOREHEADER"
    else:
        width, height, planes, bpp = struct.unpack_from("<iiHH", data, 18)
        compression = BI_RGB
        colours_used = 0
        if hsize >= 20:
            compression = struct.unpack_from("<I", data, 30)[0]
        if hsize >= 36:
            xppm, yppm, colours_used = struct.unpack_from("<iiI", data, 38)
            if xppm > 0 and yppm > 0:
                meta["dpi"] = (round(xppm * 0.0254, 1), round(yppm * 0.0254, 1))
        top_down = height < 0
        height = abs(height)
        pal_entry = 4
        if hsize in (16, 64):
            meta["bmp_header"] = "OS/2 2.x"
            if compression == 3:
                raise UnsupportedVariant("OS/2 Huffman 1D compressed BMP files are not supported.")
            if compression == 4:
                raise UnsupportedVariant("OS/2 RLE24 compressed BMP files are not supported.")
        else:
            meta["bmp_header"] = {40: "BITMAPINFOHEADER", 52: "BITMAPV2INFOHEADER",
                                  56: "BITMAPV3INFOHEADER", 108: "BITMAPV4HEADER",
                                  124: "BITMAPV5HEADER"}[hsize]
        if compression == BI_JPEG:
            raise UnsupportedVariant("BMP files containing JPEG data are not supported.")
        if compression == BI_PNG:
            raise UnsupportedVariant("BMP files containing PNG data are not supported.")
        if compression in (BI_BITFIELDS, BI_ALPHABITFIELDS):
            nmasks = 4 if compression == BI_ALPHABITFIELDS else 3
            if hsize >= 52:
                masks = list(struct.unpack_from("<4I", data, 54)) if hsize >= 56 else \
                    list(struct.unpack_from("<3I", data, 54)) + [0]
            else:
                if 14 + hsize + 4 * nmasks > len(data):
                    raise CorruptFile("BMP bit-field masks are truncated.")
                masks = list(struct.unpack_from(f"<{nmasks}I", data, 14 + hsize)) + [0] * (4 - nmasks)
        elif compression not in (BI_RGB, BI_RLE8, BI_RLE4):
            raise UnsupportedVariant(f"BMP compression type {compression} is not supported.")
    if planes != 1:
        raise CorruptFile("BMP planes field must be 1.")
    if bpp not in (1, 2, 4, 8, 16, 24, 32):
        raise UnsupportedVariant(f"BMP bit depth {bpp} is not supported.")
    if compression == BI_RLE8 and bpp != 8 or compression == BI_RLE4 and bpp != 4:
        raise CorruptFile("BMP RLE compression does not match the bit depth.")
    if compression in (BI_BITFIELDS, BI_ALPHABITFIELDS) and bpp not in (16, 32):
        raise CorruptFile("BMP bit-field compression requires 16 or 32 bits per pixel.")
    if top_down and compression in (BI_RLE8, BI_RLE4):
        raise CorruptFile("Top-down BMP files cannot be RLE compressed.")
    check_dimensions(width, height, limits)
    meta["original_bit_depth"] = bpp

    palette = None
    if bpp <= 8:
        pal_start = 14 + hsize
        if masks is not None and hsize < 52:
            pal_start += 12
        count = colours_used or (1 << bpp)
        if count > 256:
            raise CorruptFile("BMP palette is larger than 256 entries.")
        max_count = (off_bits - pal_start) // pal_entry if off_bits > pal_start else 0
        count = min(count, max_count) if off_bits else count
        if count <= 0 or pal_start + count * pal_entry > len(data):
            raise CorruptFile("BMP palette is missing or truncated.")
        raw = data[pal_start:pal_start + count * pal_entry]
        palette = [(raw[i + 2], raw[i + 1], raw[i]) for i in range(0, len(raw), pal_entry)]

    if off_bits >= len(data) or off_bits < 14 + hsize:
        raise CorruptFile("BMP pixel data offset is invalid.")
    progress.report(0, height, "decoding")

    if compression in (BI_RLE8, BI_RLE4):
        indices = _rle_decode(data, off_bits, width, height, bpp, progress)
        rows = [indices[y * width:(y + 1) * width] for y in range(height - 1, -1, -1)]
        index_data = b"".join(rows)
        meta["bmp_compression"] = "RLE8" if compression == BI_RLE8 else "RLE4"
        return _apply_palette(index_data, palette, width, height, meta)

    stride = ((width * bpp + 31) // 32) * 4
    need = stride * height
    pix = data[off_bits:off_bits + need]
    if len(pix) < need:
        # Some writers omit the padding of the very last row.
        last_row = (width * bpp + 7) // 8
        if len(pix) < need - stride + last_row:
            raise CorruptFile("BMP pixel data is truncated.")
        pix = pix + bytes(need - len(pix))
    order = range(height) if top_down else range(height - 1, -1, -1)
    row_bytes = (width * bpp + 7) // 8
    rows = []
    for i, y in enumerate(order):
        rows.append(pix[y * stride:y * stride + row_bytes])
        if (i & 255) == 0:
            progress.check_cancelled()
            progress.report(i, height, "decoding")
    if bpp <= 8:
        index_data = b"".join(_expand_bits(r, bpp, width) for r in rows)
        return _apply_palette(index_data, palette, width, height, meta)
    raw = b"".join(rows)
    n = width * height
    if bpp == 24:
        out = bytearray(n * 3)
        out[0::3] = raw[2::3]
        out[1::3] = raw[1::3]
        out[2::3] = raw[0::3]
        return Image(width, height, "RGB", 8, out, meta)
    if bpp == 32 and (masks is None or masks[:3] == [0xFF0000, 0xFF00, 0xFF]):
        has_alpha = masks is not None and masks[3] == 0xFF000000
        out = bytearray(n * (4 if has_alpha else 3))
        c = 4 if has_alpha else 3
        out[0::c] = raw[2::4]
        out[1::c] = raw[1::4]
        out[2::c] = raw[0::4]
        if has_alpha:
            out[3::4] = raw[3::4]
            return image_ops.drop_alpha_if_opaque(Image(width, height, "RGBA", 8, out, meta))
        return Image(width, height, "RGB", 8, out, meta)
    if masks is None:  # 16-bit BI_RGB defaults to X1R5G5B5
        masks = [0x7C00, 0x03E0, 0x001F, 0]
    vals = array("H" if bpp == 16 else ("I" if array("I").itemsize == 4 else "L"))
    vals.frombytes(raw)
    if not _LITTLE:
        vals.byteswap()
    has_alpha = masks[3] != 0
    planes_ = _extract_fields(vals, masks, has_alpha)
    c = 4 if has_alpha else 3
    out = bytearray(n * c)
    for i, plane in enumerate(planes_):
        out[i::c] = plane
    meta["bmp_masks"] = [hex(m) for m in masks]
    img = Image(width, height, "RGBA" if has_alpha else "RGB", 8, out, meta)
    return image_ops.drop_alpha_if_opaque(img) if has_alpha else img


def _apply_palette(index_data: bytes, palette, width, height, meta) -> Image:
    if max(index_data, default=0) >= len(palette):
        raise CorruptFile("BMP pixel refers to a palette entry that does not exist.")
    pad = [(0, 0, 0)] * (256 - len(palette))
    full = palette + pad
    if all(r == g == b for r, g, b in palette):
        lut = bytes(r for r, _, _ in full)
        return Image(width, height, "L", 8, bytearray(index_data.translate(lut)), meta)
    out = bytearray(width * height * 3)
    for ch in range(3):
        out[ch::3] = index_data.translate(bytes(entry[ch] for entry in full))
    return Image(width, height, "RGB", 8, out, meta)


# --------------------------------------------------------------------------
# writer
# --------------------------------------------------------------------------

def encode(img: Image, progress=NULL_PROGRESS, notes=None, background=image_ops.WHITE, **_) -> bytes:
    img = image_ops.drop_alpha_if_opaque(img)
    has_alpha = img.has_alpha
    img = image_ops.convert(img, "RGBA" if has_alpha else "RGB", 8, background, notes)
    w, h = img.width, img.height
    dpi = img.metadata.get("dpi")
    ppm = (round(dpi[0] / 0.0254), round(dpi[1] / 0.0254)) if dpi else (2835, 2835)
    src = bytes(img.pixels)
    progress.report(0, h, "encoding")
    if has_alpha:
        bgra = bytearray(len(src))
        bgra[0::4] = src[2::4]
        bgra[1::4] = src[1::4]
        bgra[2::4] = src[0::4]
        bgra[3::4] = src[3::4]
        stride = w * 4
        rows = [bytes(bgra[y * stride:(y + 1) * stride]) for y in range(h - 1, -1, -1)]
        pixel_data = b"".join(rows)
        header_size = 108
        info = struct.pack("<IiiHHIIiiII", header_size, w, h, 1, 32, BI_BITFIELDS,
                           len(pixel_data), ppm[0], ppm[1], 0, 0)
        info += struct.pack("<4I", 0x00FF0000, 0x0000FF00, 0x000000FF, 0xFF000000)
        info += struct.pack("<I", 0x73524742)  # LCS_sRGB
        info += bytes(36 + 12)                  # endpoints + gamma (unused for sRGB)
    else:
        bgr = bytearray(len(src))
        bgr[0::3] = src[2::3]
        bgr[1::3] = src[1::3]
        bgr[2::3] = src[0::3]
        row = w * 3
        pad = bytes((4 - row % 4) % 4)
        rows = []
        for i, y in enumerate(range(h - 1, -1, -1)):
            rows.append(bytes(bgr[y * row:(y + 1) * row]) + pad)
            if (i & 255) == 0:
                progress.check_cancelled()
        pixel_data = b"".join(rows)
        header_size = 40
        info = struct.pack("<IiiHHIIiiII", header_size, w, h, 1, 24, BI_RGB,
                           len(pixel_data), ppm[0], ppm[1], 0, 0)
    off = 14 + len(info)
    file_header = struct.pack("<2sIHHI", b"BM", off + len(pixel_data), 0, 0, off)
    progress.report(h, h, "encoding")
    return file_header + info + pixel_data
