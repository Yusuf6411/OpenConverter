"""PNG reader and writer (ISO/IEC 15948), using our own DEFLATE and CRC-32."""

import struct

from openconverter.codecs import deflate
from openconverter.codecs.checksums import crc32
from openconverter.errors import CorruptFile, LimitExceeded, UnsupportedVariant
from openconverter.limits import DEFAULT_LIMITS, check_dimensions
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS, SubProgress
from openconverter.ops import image_ops

SIGNATURE = b"\x89PNG\r\n\x1a\n"
VALID_DEPTHS = {0: (1, 2, 4, 8, 16), 2: (8, 16), 3: (1, 2, 4, 8), 4: (8, 16), 6: (8, 16)}
CHANNELS = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}
ADAM7 = ((0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2))
LARGE_IMAGE_PIXELS = 4_000_000
LARGE_IMAGE_CHAIN = 4


def sniff(data: bytes) -> bool:
    return data[:8] == SIGNATURE


# --------------------------------------------------------------------------
# SWAR helpers: whole-row byte-lane arithmetic on Python big integers
# --------------------------------------------------------------------------

class _Lanes:
    """Masks for byte-lane arithmetic on rows of ``n`` bytes (little-endian ints)."""

    __slots__ = ("n", "full", "hi", "lo")
    _cache: dict = {}

    def __new__(cls, n):
        obj = cls._cache.get(n)
        if obj is None:
            obj = super().__new__(cls)
            obj.n = n
            obj.full = (1 << (8 * n)) - 1
            obj.hi = int.from_bytes(b"\x80" * n, "little")
            obj.lo = int.from_bytes(b"\x7f" * n, "little")
            if len(cls._cache) > 64:
                cls._cache.clear()
            cls._cache[n] = obj
        return obj

    def add(self, a, b):
        lo = self.lo
        return ((a & lo) + (b & lo)) ^ ((a ^ b) & self.hi)

    def sub(self, a, b):
        hi = self.hi
        return ((a | hi) - (b & self.lo)) ^ ((a ^ b ^ hi) & hi)

    def avg(self, a, b):
        return (a & b) + (((a ^ b) >> 1) & self.lo)


def _to_int(row) -> int:
    return int.from_bytes(row, "little")


def _to_bytes(value: int, n: int) -> bytes:
    return value.to_bytes(n, "little")


_ABS = bytes(v if v < 128 else 256 - v for v in range(256))


# --------------------------------------------------------------------------
# unfiltering
# --------------------------------------------------------------------------

def _unfilter_rows(raw: bytes, rows: int, row_len: int, bpp: int, progress, report_base, report_total):
    """Reverse PNG filtering. Returns the concatenated reconstructed rows."""
    lanes = _Lanes(row_len)
    out = bytearray()
    prior = bytes(row_len)
    prior_int = 0
    pos = 0
    check_every = max(1, 16384 // max(1, row_len))  # ~every 16 KiB of row data
    shift = 8 * bpp
    for y in range(rows):
        ftype = raw[pos]
        filt = raw[pos + 1:pos + 1 + row_len]
        pos += 1 + row_len
        if ftype == 0:
            cur = filt
            cur_int = None
        elif ftype == 1:      # Sub: prefix sum per lane with stride bpp (log-step doubling)
            x = _to_int(filt)
            s = shift
            full = lanes.full
            while s < 8 * row_len:
                x = lanes.add(x, (x << s) & full)
                s <<= 1
            cur_int = x
            cur = _to_bytes(x, row_len)
        elif ftype == 2:      # Up
            cur_int = lanes.add(_to_int(filt), prior_int)
            cur = _to_bytes(cur_int, row_len)
        elif ftype == 3:      # Average
            r = bytearray(filt)
            for i in range(min(bpp, row_len)):
                r[i] = (r[i] + (prior[i] >> 1)) & 255
            for i in range(bpp, row_len):
                r[i] = (r[i] + ((r[i - bpp] + prior[i]) >> 1)) & 255
            cur = bytes(r)
            cur_int = None
        elif ftype == 4:      # Paeth
            r = bytearray(filt)
            for i in range(min(bpp, row_len)):
                r[i] = (r[i] + prior[i]) & 255
            for i in range(bpp, row_len):
                a = r[i - bpp]
                b = prior[i]
                c = prior[i - bpp]
                pa = b - c
                pb = a - c
                pc = pa + pb
                if pa < 0:
                    pa = -pa
                if pb < 0:
                    pb = -pb
                if pc < 0:
                    pc = -pc
                if pa <= pb and pa <= pc:
                    pred = a
                elif pb <= pc:
                    pred = b
                else:
                    pred = c
                r[i] = (r[i] + pred) & 255
            cur = bytes(r)
            cur_int = None
        else:
            raise CorruptFile(f"Invalid PNG filter type {ftype}.")
        out += cur
        prior = cur
        prior_int = cur_int if cur_int is not None else _to_int(cur)
        if y % check_every == 0:
            progress.check_cancelled()
            progress.report(report_base + y, report_total, "decoding")
    return out


# --------------------------------------------------------------------------
# sample unpacking
# --------------------------------------------------------------------------

def _unpack_tables(depth):
    per = 8 // depth
    mask = (1 << depth) - 1
    return [bytes((b >> (8 - depth * (i + 1))) & mask for i in range(per)) for b in range(256)]


_UNPACK = {d: _unpack_tables(d) for d in (1, 2, 4)}
_SCALE = {1: 255, 2: 85, 4: 17}


def _unpack_row(row: bytes, depth: int, count: int) -> bytes:
    if depth == 8:
        return row
    table = _UNPACK[depth]
    return b"".join(table[b] for b in row)[:count]


# --------------------------------------------------------------------------
# decoder
# --------------------------------------------------------------------------

def decode(data: bytes, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> Image:
    data = bytes(data)
    if data[:8] != SIGNATURE:
        raise CorruptFile("Not a PNG file (bad signature).")
    pos = 8
    n = len(data)
    ihdr = None
    palette = None
    trns = None
    idat = []
    meta = {"source_format": "png"}
    seen_iend = False
    first = True
    while pos < n:
        if pos + 12 > n:
            raise CorruptFile("PNG chunk header is truncated.")
        length, ctype = struct.unpack_from(">I4s", data, pos)
        if length > 0x7FFFFFFF:
            raise CorruptFile("PNG chunk length is invalid.")
        body_start = pos + 8
        body_end = body_start + length
        if body_end + 4 > n:
            raise CorruptFile(f"PNG {ctype.decode('latin-1')!r} chunk is truncated.")
        if length > limits.max_png_chunk:
            raise LimitExceeded("PNG chunk is larger than the allowed maximum.")
        body = data[body_start:body_end]
        (stored_crc,) = struct.unpack_from(">I", data, body_end)
        if crc32(body, crc32(ctype)) != stored_crc:
            raise CorruptFile(f"PNG {ctype.decode('latin-1')!r} chunk has a CRC mismatch.")
        pos = body_end + 4
        if not all(65 <= c <= 90 or 97 <= c <= 122 for c in ctype):
            raise CorruptFile("PNG chunk type is invalid.")
        if first:
            if ctype != b"IHDR":
                raise CorruptFile("PNG does not start with an IHDR chunk.")
            first = False
        if ctype == b"IHDR":
            if ihdr is not None or length != 13:
                raise CorruptFile("PNG IHDR chunk is invalid.")
            ihdr = struct.unpack(">IIBBBBB", body)
        elif ctype == b"PLTE":
            if length % 3 or not 3 <= length <= 768:
                raise CorruptFile("PNG palette has an invalid length.")
            palette = [tuple(body[i:i + 3]) for i in range(0, length, 3)]
        elif ctype == b"tRNS":
            trns = body
        elif ctype == b"IDAT":
            idat.append(body)
        elif ctype == b"IEND":
            seen_iend = True
            break
        elif ctype == b"gAMA" and length == 4:
            meta["gamma"] = struct.unpack(">I", body)[0] / 100000
        elif ctype == b"pHYs" and length == 9:
            ppx, ppy, unit = struct.unpack(">IIB", body)
            if unit == 1 and ppx and ppy:
                meta["dpi"] = (round(ppx * 0.0254, 1), round(ppy * 0.0254, 1))
        elif ctype == b"acTL":
            meta["animated"] = True
        elif not (ctype[0] & 0x20):
            raise UnsupportedVariant(f"PNG uses an unknown critical chunk {ctype.decode('latin-1')!r}.")
    if ihdr is None:
        raise CorruptFile("PNG has no IHDR chunk.")
    if not idat:
        raise CorruptFile("PNG has no image data (IDAT).")
    if not seen_iend:
        meta["notes"] = ["PNG has no IEND chunk; the image data was read anyway."]
    width, height, depth, ctype_, comp, filt, interlace = ihdr
    if ctype_ not in VALID_DEPTHS or depth not in VALID_DEPTHS[ctype_]:
        raise CorruptFile(f"PNG colour type {ctype_} with bit depth {depth} is invalid.")
    if comp != 0 or filt != 0:
        raise CorruptFile("PNG compression or filter method is invalid.")
    if interlace not in (0, 1):
        raise CorruptFile("PNG interlace method is invalid.")
    check_dimensions(width, height, limits)
    if ctype_ == 3 and palette is None:
        raise CorruptFile("Palette PNG has no PLTE chunk.")
    channels = CHANNELS[ctype_]
    bits_pp = channels * depth
    bpp = max(1, bits_pp // 8)
    passes = []
    if interlace:
        for x0, y0, dx, dy in ADAM7:
            pw = (width - x0 + dx - 1) // dx
            ph = (height - y0 + dy - 1) // dy
            if pw > 0 and ph > 0:
                passes.append((x0, y0, dx, dy, pw, ph))
    else:
        passes.append((0, 0, 1, 1, width, height))
    expected = sum(ph * (1 + (pw * bits_pp + 7) // 8) for *_, pw, ph in passes)
    meta.update({"png_colour_type": ctype_, "original_bit_depth": depth, "interlaced": bool(interlace)})

    progress.report(0, 1, "decoding")
    inflate_progress = SubProgress(progress, 0.0, 0.55)
    raw = deflate.zlib_decompress(b"".join(idat), expected + 4096, inflate_progress)
    if len(raw) < expected:
        raise CorruptFile("PNG image data is truncated.")
    row_progress = SubProgress(progress, 0.55, 1.0)
    total_rows = sum(ph for *_, ph in passes)

    # Output sample layout before colour expansion: one byte per sample for
    # depth <= 8, two (big-endian) for 16.
    sample_bytes = 2 if depth == 16 else 1
    px_bytes = channels * sample_bytes
    full = bytearray(width * height * px_bytes) if interlace else None
    offset = 0
    rows_done = 0
    plain = None
    for x0, y0, dx, dy, pw, ph in passes:
        row_len = (pw * bits_pp + 7) // 8
        size = ph * (1 + row_len)
        recon = _unfilter_rows(raw[offset:offset + size], ph, row_len, bpp,
                               row_progress, rows_done, total_rows)
        offset += size
        rows_done += ph
        if depth < 8:
            samples = b"".join(_unpack_row(recon[y * row_len:(y + 1) * row_len], depth, pw)
                               for y in range(ph))
        else:
            samples = bytes(recon)
        if not interlace:
            plain = samples
            break
        stride = width * px_bytes
        pstride = pw * px_bytes
        for py in range(ph):
            y = y0 + py * dy
            src = samples[py * pstride:(py + 1) * pstride]
            base = y * stride + x0 * px_bytes
            for k in range(px_bytes):
                full[base + k:base + stride - x0 * px_bytes:dx * px_bytes] = src[k::px_bytes]
    samples = plain if not interlace else bytes(full)
    progress.report(1, 1, "decoding")
    return _to_image(samples, width, height, ctype_, depth, palette, trns, meta)


def _to_image(samples, width, height, ctype, depth, palette, trns, meta) -> Image:
    n = width * height
    if ctype == 3:
        if max(samples, default=0) >= len(palette):
            raise CorruptFile("PNG pixel refers to a palette entry that does not exist.")
        alphas = None
        if trns is not None:
            if len(trns) > len(palette):
                raise CorruptFile("PNG tRNS chunk is longer than the palette.")
            alphas = list(trns) + [255] * (len(palette) - len(trns))
        full = palette + [(0, 0, 0)] * (256 - len(palette))
        meta["palette_size"] = len(palette)
        grey = all(r == g == b for r, g, b in palette)
        if grey and alphas is None:
            return Image(width, height, "L", 8,
                         bytearray(samples.translate(bytes(e[0] for e in full))), meta)
        c = (2 if grey else 4) if alphas is not None else 3
        out = bytearray(n * c)
        if grey:
            out[0::2] = samples.translate(bytes(e[0] for e in full))
        else:
            for ch in range(3):
                out[ch::c] = samples.translate(bytes(e[ch] for e in full))
        if alphas is not None:
            alphas += [255] * (256 - len(alphas))
            out[c - 1::c] = samples.translate(bytes(alphas))
            return Image(width, height, "LA" if grey else "RGBA", 8, out, meta)
        return Image(width, height, "RGB", 8, out, meta)

    out_depth = 16 if depth == 16 else 8
    mode = {0: "L", 2: "RGB", 4: "LA", 6: "RGBA"}[ctype]
    if trns is not None and ctype in (0, 2):
        alpha = _colour_key_alpha(samples, n, ctype, depth, trns)
        if ctype == 0 and depth < 8:
            samples = _scale_low_depth(samples, depth)
        if alpha is None:  # key colour cannot occur at this bit depth
            return Image(width, height, mode, out_depth, bytearray(samples), meta)
        c = (1 if ctype == 0 else 3) * (2 if depth == 16 else 1)
        a = 2 if depth == 16 else 1
        px = c + a
        out = bytearray(n * px)
        for k in range(c):
            out[k::px] = samples[k::c]
        for k in range(a):
            out[c + k::px] = alpha
        return Image(width, height, "LA" if ctype == 0 else "RGBA", out_depth, out, meta)
    if ctype == 0 and depth < 8:
        samples = _scale_low_depth(samples, depth)
    return Image(width, height, mode, out_depth, bytearray(samples), meta)


def _colour_key_alpha(samples, n, ctype, depth, trns):
    """Alpha plane (0 where the pixel equals the tRNS key colour, else 255)."""
    if ctype == 0:
        if len(trns) != 2:
            raise CorruptFile("PNG tRNS chunk has the wrong length for greyscale.")
        key_val = struct.unpack(">H", trns)[0]
        if depth <= 8:
            if key_val >= (1 << depth):
                return None
            table = bytearray([255]) * 256
            table[key_val] = 0
            return samples.translate(bytes(table))
        key = trns
    else:
        if len(trns) != 6:
            raise CorruptFile("PNG tRNS chunk has the wrong length for RGB.")
        if depth == 16:
            key = trns
        else:
            r, g, b = struct.unpack(">HHH", trns)
            if max(r, g, b) > 255:
                return None
            key = bytes((r, g, b))
    step = len(key)
    return bytes([0 if samples[i:i + step] == key else 255 for i in range(0, n * step, step)])


def _scale_low_depth(samples, depth):
    scale = _SCALE[depth]
    top = (1 << depth) - 1
    return samples.translate(bytes(min(v, top) * scale for v in range(256)))


# --------------------------------------------------------------------------
# encoder
# --------------------------------------------------------------------------

def _chunk(ctype: bytes, body: bytes) -> bytes:
    return struct.pack(">I", len(body)) + ctype + body + struct.pack(">I", crc32(body, crc32(ctype)))


class _Wide:
    """Masks for 16-bit-lane arithmetic on rows of ``n`` samples."""

    __slots__ = ("ones", "full", "k512", "k1024")
    _cache: dict = {}

    def __new__(cls, n):
        obj = cls._cache.get(n)
        if obj is None:
            obj = super().__new__(cls)
            obj.ones = int.from_bytes(b"\x01\x00" * n, "little")
            obj.full = (1 << (16 * n)) - 1
            obj.k512 = obj.ones * 512
            obj.k1024 = obj.ones * 1024
            if len(cls._cache) > 64:
                cls._cache.clear()
            cls._cache[n] = obj
        return obj


def _spread(row: bytes) -> int:
    wide = bytearray(2 * len(row))
    wide[0::2] = row
    return int.from_bytes(wide, "little")


def _lane_abs(v: int, bias: int, bit: int, w: _Wide) -> int:
    """|lane - bias| for lanes holding bias + d (no borrows cross lanes)."""
    s = (v >> bit) & w.ones          # 1 where lane >= bias
    m = (s << 16) - s                # 0xFFFF in those lanes
    pos = v & m
    neg = v & (w.full ^ m)
    kb = (w.k512 if bias == 512 else w.k1024)
    return (pos - (kb & m)) + ((kb & (w.full ^ m)) - neg)


def _lane_ge(x: int, y: int, w: _Wide) -> int:
    """1 in lanes where x >= y (values < 1024)."""
    return ((x + w.k1024 - y) >> 10) & w.ones


def paeth_predict_row(a_row: bytes, b_row: bytes, c_row: bytes) -> bytes:
    """Paeth predictor for every byte of a row at once (PNG spec 9.4)."""
    n = len(a_row)
    if n == 0:
        return b""
    w = _Wide(n)
    a, b, c = _spread(a_row), _spread(b_row), _spread(c_row)
    pa = _lane_abs(b + w.k512 - c, 512, 9, w)           # |b - c|
    pb = _lane_abs(a + w.k512 - c, 512, 9, w)           # |a - c|
    pc = _lane_abs(a + b + w.k1024 - 2 * c, 1024, 10, w)  # |a + b - 2c|
    choose_a = _lane_ge(pb, pa, w) & _lane_ge(pc, pa, w)
    choose_b = (w.ones ^ choose_a) & _lane_ge(pc, pb, w)
    choose_c = w.ones ^ choose_a ^ choose_b
    ma = (choose_a << 16) - choose_a
    mb = (choose_b << 16) - choose_b
    mc = (choose_c << 16) - choose_c
    pred = (a & ma) | (b & mb) | (c & mc)
    return pred.to_bytes(2 * n, "little")[0::2]


def _filter_rows(raw: bytes, rows: int, row_len: int, bpp: int, adaptive: bool, progress) -> bytes:
    """Apply per-row adaptive filtering (minimum sum of absolute differences)."""
    if not adaptive:
        out = bytearray()
        for y in range(rows):
            out.append(0)
            out += raw[y * row_len:(y + 1) * row_len]
        return bytes(out)
    lanes = _Lanes(row_len)
    full = lanes.full
    shift = 8 * bpp
    out = bytearray()
    prior = bytes(row_len)
    prior_int = 0
    abs_t = _ABS
    check_every = max(1, 16384 // max(1, row_len))
    for y in range(rows):
        row = raw[y * row_len:(y + 1) * row_len]
        x = _to_int(row)
        left = (x << shift) & full
        up_left = (prior_int << shift) & full
        candidates = [
            row,
            _to_bytes(lanes.sub(x, left), row_len),
            _to_bytes(lanes.sub(x, prior_int), row_len),
            _to_bytes(lanes.sub(x, lanes.avg(left, prior_int)), row_len),
        ]
        # Paeth predictor, evaluated for the whole row with 16-bit SWAR lanes
        pred = paeth_predict_row(_to_bytes(left, row_len), prior, _to_bytes(up_left, row_len))
        candidates.append(_to_bytes(lanes.sub(x, _to_int(pred)), row_len))
        best = min(range(5), key=lambda f: sum(candidates[f].translate(abs_t)))
        out.append(best)
        out += candidates[best]
        prior = row
        prior_int = x
        if y % check_every == 0:
            progress.check_cancelled()
            progress.report(y, rows, "filtering")
    return bytes(out)


def _pack_samples(samples: bytes, width: int, height: int, depth: int) -> bytes:
    """Pack one-byte-per-sample rows into ``depth``-bit samples (MSB first)."""
    if depth == 8:
        return samples
    per = 8 // depth
    row_len = (width + per - 1) // per
    out = bytearray()
    for y in range(height):
        row = samples[y * width:(y + 1) * width]
        row = row + bytes(row_len * per - width)
        acc = 0
        for v in row:
            acc = (acc << depth) | v
        out += acc.to_bytes(row_len, "big")
    return bytes(out)


def _plan(img: Image, palette_opt: str, notes):
    """Choose the PNG colour type/bit depth. Returns (ctype, depth, samples, plte, trns)."""
    if img.bit_depth == 8 and palette_opt != "off":
        colours = _unique_colours(img, limit=256)
        if colours is not None:
            plan = _palette_plan(img, colours, palette_opt)
            if plan is not None:
                return plan
        elif palette_opt == "on" and notes is not None:
            notes.append("Palette not used: the image has more than 256 colours.")
    if img.mode == "L" and img.bit_depth == 8 and palette_opt != "on":
        values = set(img.pixels)
        for depth in (1, 2, 4):
            scale = _SCALE[depth]
            if all(v % scale == 0 for v in values):
                samples = bytes(img.pixels).translate(bytes((v // scale) & 255 for v in range(256)))
                return 0, depth, _pack_samples(samples, img.width, img.height, depth), None, None
    ctype = {"L": 0, "LA": 4, "RGB": 2, "RGBA": 6}[img.mode]
    return ctype, img.bit_depth, bytes(img.pixels), None, None


def _unique_colours(img: Image, limit: int):
    c = img.channels
    px = bytes(img.pixels)
    seen = set()
    if c == 1:
        seen = set(px)
        return sorted((v,) for v in seen) if len(seen) <= limit else None
    step = c
    for i in range(0, len(px), step * 4096):
        chunk = px[i:i + step * 4096]
        seen.update(chunk[j:j + step] for j in range(0, len(chunk), step))
        if len(seen) > limit:
            return None
    return sorted(tuple(s) for s in seen)


def _palette_plan(img: Image, colours, palette_opt):
    c = img.channels
    if img.is_grey and img.mode == "L" and palette_opt != "on":
        return None  # greyscale is already one byte per pixel (low depths handled separately)
    count = len(colours)
    depth = 1 if count <= 2 else 2 if count <= 4 else 4 if count <= 16 else 8
    w, h = img.width, img.height
    pal_raw = h * ((w * depth + 7) // 8) + 3 * count + (count if img.has_alpha else 0)
    true_raw = w * h * c
    if palette_opt == "auto" and pal_raw >= true_raw:
        return None
    # Order: entries with alpha < 255 first so tRNS can be short.
    def rgba(col):
        if c == 1:
            return (col[0], col[0], col[0], 255)
        if c == 2:
            return (col[0], col[0], col[0], col[1])
        if c == 3:
            return (col[0], col[1], col[2], 255)
        return col
    entries = sorted(colours, key=lambda col: (rgba(col)[3] == 255, col))
    index = {bytes(col): i for i, col in enumerate(entries)}
    px = bytes(img.pixels)
    idx = bytes(index[px[i:i + c]] for i in range(0, len(px), c)) if c > 1 else \
        px.translate(bytes(index.get(bytes([v]), 0) for v in range(256)))
    plte = b"".join(bytes(rgba(col)[:3]) for col in entries)
    alphas = [rgba(col)[3] for col in entries]
    trns = None
    if any(a < 255 for a in alphas):
        last = max(i for i, a in enumerate(alphas) if a < 255)
        trns = bytes(alphas[:last + 1])
    return 3, depth, _pack_samples(idx, w, h, depth), plte, trns


def encode(img: Image, progress=NULL_PROGRESS, notes=None, compression_level=6,
           palette="auto", **_) -> bytes:
    if palette is True:
        palette = "on"
    elif palette is False:
        palette = "auto"
    level = int(compression_level)
    if not 0 <= level <= 9:
        raise ValueError("compression_level must be 0..9")
    w, h = img.width, img.height
    plan_progress = SubProgress(progress, 0.0, 0.05)
    plan_progress.report(0, 1, "encoding")
    ctype, depth, samples, plte, trns = _plan(img, palette, notes)
    channels = CHANNELS[ctype]
    bits_pp = channels * depth
    row_len = (w * bits_pp + 7) // 8
    bpp = max(1, bits_pp // 8)
    adaptive = ctype != 3 and depth >= 8
    filtered = _filter_rows(samples, h, row_len, bpp, adaptive, SubProgress(progress, 0.05, 0.25))
    max_chain = None
    if w * h > LARGE_IMAGE_PIXELS and level > 1:
        max_chain = LARGE_IMAGE_CHAIN
        if notes is not None:
            notes.append("Fast mode: large image, reduced compression search depth.")
    compressed = deflate.zlib_compress(filtered, level, SubProgress(progress, 0.25, 1.0), max_chain)
    parts = [SIGNATURE, _chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, ctype, 0, 0, 0))]
    if img.metadata.get("gamma"):
        parts.append(_chunk(b"gAMA", struct.pack(">I", round(img.metadata["gamma"] * 100000))))
    if plte is not None:
        parts.append(_chunk(b"PLTE", plte))
    if trns is not None:
        parts.append(_chunk(b"tRNS", trns))
    dpi = img.metadata.get("dpi")
    if dpi:
        parts.append(_chunk(b"pHYs", struct.pack(">IIB", round(dpi[0] / 0.0254), round(dpi[1] / 0.0254), 1)))
    for off in range(0, len(compressed), 1 << 20):
        parts.append(_chunk(b"IDAT", compressed[off:off + (1 << 20)]))
    parts.append(_chunk(b"IEND", b""))
    progress.report(1, 1, "encoding")
    return b"".join(parts)


# --------------------------------------------------------------------------
# test/diagnostic helper: encode with a forced filter and optional interlace
# --------------------------------------------------------------------------

def encode_raw(width, height, ctype, depth, samples: bytes, *, filter_type=0, interlace=False,
               plte=None, trns=None, level=6) -> bytes:
    """Build a PNG from already-packed sample rows (tests use this to exercise
    every filter, colour type, bit depth and Adam7)."""
    channels = CHANNELS[ctype]
    bits_pp = channels * depth
    bpp = max(1, bits_pp // 8)
    full_row = (width * bits_pp + 7) // 8

    def filter_image(rows_bytes, pw, ph):
        row_len = (pw * bits_pp + 7) // 8
        out = bytearray()
        prior = bytes(row_len)
        for y in range(ph):
            row = rows_bytes[y * row_len:(y + 1) * row_len]
            out.append(filter_type)
            out += _filter_one(row, prior, bpp, filter_type)
            prior = row
        return bytes(out)

    if not interlace:
        payload = filter_image(samples, width, height)
    else:
        payload = b""
        for x0, y0, dx, dy in ADAM7:
            pw = (width - x0 + dx - 1) // dx
            ph = (height - y0 + dy - 1) // dy
            if pw <= 0 or ph <= 0:
                continue
            rows = []
            for py in range(ph):
                y = y0 + py * dy
                rows.append(_extract_bits(samples[y * full_row:(y + 1) * full_row], bits_pp, x0, dx, pw))
            payload += filter_image(b"".join(rows), pw, ph)
    parts = [SIGNATURE, _chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, depth, ctype, 0, 0,
                                                    1 if interlace else 0))]
    if plte is not None:
        parts.append(_chunk(b"PLTE", plte))
    if trns is not None:
        parts.append(_chunk(b"tRNS", trns))
    parts.append(_chunk(b"IDAT", deflate.zlib_compress(payload, level)))
    parts.append(_chunk(b"IEND", b""))
    return b"".join(parts)


def _filter_one(row, prior, bpp, ftype):
    out = bytearray(len(row))
    for i in range(len(row)):
        a = row[i - bpp] if i >= bpp else 0
        b = prior[i]
        c = prior[i - bpp] if i >= bpp else 0
        if ftype == 0:
            p = 0
        elif ftype == 1:
            p = a
        elif ftype == 2:
            p = b
        elif ftype == 3:
            p = (a + b) >> 1
        else:
            pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
            p = a if pa <= pb and pa <= pc else b if pb <= pc else c
        out[i] = (row[i] - p) & 255
    return out


def _extract_bits(row: bytes, bits_pp: int, x0: int, dx: int, count: int) -> bytes:
    if bits_pp % 8 == 0:
        b = bits_pp // 8
        return b"".join(row[(x0 + i * dx) * b:(x0 + i * dx + 1) * b] for i in range(count))
    total_bits = len(row) * 8
    val = int.from_bytes(row, "big")
    acc = 0
    for i in range(count):
        x = x0 + i * dx
        shift = total_bits - (x + 1) * bits_pp
        acc = (acc << bits_pp) | ((val >> shift) & ((1 << bits_pp) - 1))
    nbits = count * bits_pp
    pad = (-nbits) % 8
    return (acc << pad).to_bytes((nbits + pad) // 8, "big")
