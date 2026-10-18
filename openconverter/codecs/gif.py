"""GIF reader (first frame), with our own LZW decoder."""

import struct

from openconverter.errors import CorruptFile
from openconverter.limits import DEFAULT_LIMITS, check_dimensions
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS

MAX_CODE_BITS = 12


def sniff(data: bytes) -> bool:
    return data[:6] in (b"GIF87a", b"GIF89a")


def lzw_decode(data: bytes, min_code_size: int, max_output: int, progress=NULL_PROGRESS) -> bytes:
    """Decode GIF LZW data (already joined from sub-blocks).

    Stops once ``max_output`` bytes have been produced (extra data is ignored).
    Raises CorruptFile on malformed codes.
    """
    if not 2 <= min_code_size <= 8:
        raise CorruptFile(f"GIF LZW minimum code size {min_code_size} is invalid.")
    clear = 1 << min_code_size
    eoi = clear + 1
    base = [bytes([i]) for i in range(clear)] + [b"", b""]
    table = list(base)
    code_size = min_code_size + 1
    mask = (1 << code_size) - 1
    out = bytearray()
    prev = None
    n = len(data)
    padded = data + bytes(8)
    pos = 0
    bitbuf = 0
    bitcnt = 0
    next_check = 1 << 16
    while len(out) < max_output:
        if bitcnt < code_size:
            if pos >= n:
                break  # ran out of data without EOI
            bitbuf |= int.from_bytes(padded[pos:pos + 4], "little") << bitcnt
            avail = min(4, n - pos)
            pos += 4
            bitcnt += 8 * avail
            if bitcnt < code_size:
                break
        code = bitbuf & mask
        bitbuf >>= code_size
        bitcnt -= code_size
        if code == clear:
            table = list(base)
            code_size = min_code_size + 1
            mask = (1 << code_size) - 1
            prev = None
            continue
        if code == eoi:
            break
        size = len(table)
        if prev is None:
            if code >= clear:
                raise CorruptFile("GIF LZW stream starts with an invalid code.")
            entry = table[code]
        elif code < size:
            entry = table[code]
            if size < 4096:
                table.append(prev + entry[:1])
        elif code == size and size < 4096:
            entry = prev + prev[:1]
            table.append(entry)
        else:
            raise CorruptFile("GIF LZW stream contains an invalid code.")
        out += entry
        prev = entry
        if len(table) == (1 << code_size) and code_size < MAX_CODE_BITS:
            code_size += 1
            mask = (1 << code_size) - 1
        # At 4096 entries the table is full: "deferred clear" - keep 12-bit codes
        # and stop adding entries until the encoder sends a clear code.
        if pos >= next_check:
            next_check = pos + (1 << 16)
            progress.check_cancelled()
            progress.report(pos, n, "decoding")
    if len(out) > max_output:
        del out[max_output:]
    return bytes(out)


def _read_sub_blocks(data: bytes, pos: int):
    parts = []
    n = len(data)
    while True:
        if pos >= n:
            raise CorruptFile("GIF data sub-blocks are truncated.")
        size = data[pos]
        pos += 1
        if size == 0:
            return b"".join(parts), pos
        if pos + size > n:
            raise CorruptFile("GIF data sub-block is truncated.")
        parts.append(data[pos:pos + size])
        pos += size


def _skip_sub_blocks(data: bytes, pos: int) -> int:
    n = len(data)
    while True:
        if pos >= n:
            raise CorruptFile("GIF data sub-blocks are truncated.")
        size = data[pos]
        pos += 1 + size
        if size == 0:
            return pos


def _read_palette(data, pos, flag_byte):
    size = 3 * (2 << (flag_byte & 7))
    raw = data[pos:pos + size]
    if len(raw) < size:
        raise CorruptFile("GIF colour table is truncated.")
    return [tuple(raw[i:i + 3]) for i in range(0, size, 3)], pos + size


def decode(data: bytes, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> Image:
    data = bytes(data)
    if not sniff(data):
        raise CorruptFile("Not a GIF file (bad signature).")
    if len(data) < 13:
        raise CorruptFile("GIF header is truncated.")
    sw, sh, flags, _bg, _aspect = struct.unpack_from("<HHBBB", data, 6)
    check_dimensions(sw, sh, limits)
    pos = 13
    gct = None
    if flags & 0x80:
        gct, pos = _read_palette(data, pos, flags)
    transparent = None
    frame = None
    frames = 0
    n = len(data)
    while pos < n:
        block = data[pos]
        pos += 1
        if block == 0x3B:  # trailer
            break
        if block == 0x21:
            if pos >= n:
                raise CorruptFile("GIF extension is truncated.")
            label = data[pos]
            pos += 1
            if label == 0xF9 and frame is None:
                body, pos = _read_sub_blocks(data, pos)
                if len(body) >= 4 and body[0] & 1:
                    transparent = body[3]
            else:
                pos = _skip_sub_blocks(data, pos)
            continue
        if block == 0x2C:
            if pos + 9 > n:
                raise CorruptFile("GIF image descriptor is truncated.")
            left, top, fw, fh, fflags = struct.unpack_from("<HHHHB", data, pos)
            pos += 9
            palette = gct
            if fflags & 0x80:
                palette, pos = _read_palette(data, pos, fflags)
            if pos >= n:
                raise CorruptFile("GIF image data is truncated.")
            mcs = data[pos]
            pos += 1
            frames += 1
            if frame is None:
                lzw, pos = _read_sub_blocks(data, pos)
                frame = (left, top, fw, fh, bool(fflags & 0x40), palette, mcs, lzw, transparent)
            else:
                pos = _skip_sub_blocks(data, pos)
            continue
        if frame is not None:
            break  # junk after the first frame: stop counting
        raise CorruptFile(f"GIF contains an unknown block type 0x{block:02X}.")
    if frame is None:
        raise CorruptFile("GIF contains no image.")
    left, top, fw, fh, interlaced, palette, mcs, lzw, transparent = frame
    if fw == 0 or fh == 0:
        raise CorruptFile("GIF frame has zero size.")
    check_dimensions(fw, fh, limits)
    if palette is None:
        raise CorruptFile("GIF frame has no colour table.")
    total = fw * fh
    indices = lzw_decode(lzw, mcs, total, progress)
    if len(indices) < total:
        raise CorruptFile("GIF image data is truncated.")
    if interlaced:
        order = list(range(0, fh, 8)) + list(range(4, fh, 8)) + list(range(2, fh, 4)) + list(range(1, fh, 2))
        rows = [None] * fh
        for i, y in enumerate(order):
            rows[y] = indices[i * fw:(i + 1) * fw]
        indices = b"".join(rows)
    used = set(indices)
    used.discard(transparent)
    if used and max(used) >= len(palette):
        raise CorruptFile("GIF pixel refers to a colour that is not in the palette.")
    meta = {"source_format": "gif", "gif_frames": frames, "original_bit_depth": 8}
    notes = []
    if frames > 1:
        notes.append(f"GIF has {frames} frames; only the first frame was converted.")
    full = list(palette) + [(0, 0, 0)] * (256 - len(palette))
    grey = all(r == g == b for r, g, b in palette)
    covers = left == 0 and top == 0 and fw >= sw and fh >= sh
    has_alpha = transparent is not None or not covers
    if fw > sw - left or fh > sh - top:
        # Frame extends past the logical screen: crop to the screen.
        fw_c, fh_c = max(0, min(fw, sw - left)), max(0, min(fh, sh - top))
    else:
        fw_c, fh_c = fw, fh
    c = (2 if grey else 4) if has_alpha else (1 if grey else 3)
    alpha_t = bytes(0 if i == transparent else 255 for i in range(256))
    planes = []
    if grey:
        planes.append(indices.translate(bytes(e[0] for e in full)))
    else:
        for ch in range(3):
            planes.append(indices.translate(bytes(e[ch] for e in full)))
    if has_alpha:
        planes.append(indices.translate(alpha_t))
    frame_px = bytearray(total * c)
    for i, p in enumerate(planes):
        frame_px[i::c] = p
    if covers and fw == sw and fh == sh:
        pixels = frame_px
    else:
        # Composite the first frame onto a transparent logical screen.
        pixels = bytearray(sw * sh * c)
        for y in range(fh_c):
            dst = ((top + y) * sw + left) * c
            src = y * fw * c
            pixels[dst:dst + fw_c * c] = frame_px[src:src + fw_c * c]
        if not covers:
            notes.append("Area outside the first GIF frame is transparent.")
    if notes:
        meta["notes"] = notes
    mode = {1: "L", 2: "LA", 3: "RGB", 4: "RGBA"}[c]
    progress.report(1, 1, "decoding")
    return Image(sw, sh, mode, 8, pixels, meta)
