"""Baseline sequential JPEG decoder (ITU-T T.81), Huffman coding, 8-bit."""

import struct

from openconverter.codecs.jpeg.dct import idct_block
from openconverter.codecs.jpeg.huffman import build_decode_table
from openconverter.codecs.jpeg.tables import ZIGZAG
from openconverter.errors import CorruptFile, LimitExceeded, UnsupportedVariant
from openconverter.limits import DEFAULT_LIMITS, check_dimensions
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS
from openconverter.ops import image_ops

UNSUPPORTED_SOF = {
    0xC2: "This JPEG uses progressive encoding, which OpenConverter 1.0 doesn't support yet.",
    0xC3: "Lossless JPEG is not supported.",
    0xC5: "Hierarchical (differential) JPEG is not supported.",
    0xC6: "Hierarchical (differential) JPEG is not supported.",
    0xC7: "Hierarchical (differential) JPEG is not supported.",
    0xC9: "Arithmetic-coded JPEG is not supported.",
    0xCA: "Arithmetic-coded JPEG is not supported.",
    0xCB: "Arithmetic-coded JPEG is not supported.",
    0xCD: "Arithmetic-coded JPEG is not supported.",
    0xCE: "Arithmetic-coded JPEG is not supported.",
    0xCF: "Arithmetic-coded JPEG is not supported.",
}


def sniff(data: bytes) -> bool:
    return len(data) >= 3 and data[0] == 0xFF and data[1] == 0xD8 and data[2] == 0xFF


class _Component:
    __slots__ = ("cid", "h", "v", "tq", "td", "ta", "bw", "bh", "plane", "stride", "pred")

    def __init__(self, cid, h, v, tq):
        self.cid, self.h, self.v, self.tq = cid, h, v, tq
        self.td = self.ta = 0
        self.plane = None
        self.pred = 0


def _parse_exif_orientation(body: bytes):
    if not body.startswith(b"Exif\x00\x00") or len(body) < 14:
        return None
    tiff = body[6:]
    if tiff[:2] == b"II":
        e = "<"
    elif tiff[:2] == b"MM":
        e = ">"
    else:
        return None
    try:
        if struct.unpack_from(e + "H", tiff, 2)[0] != 42:
            return None
        ifd = struct.unpack_from(e + "I", tiff, 4)[0]
        count = struct.unpack_from(e + "H", tiff, ifd)[0]
        for i in range(min(count, 512)):
            off = ifd + 2 + 12 * i
            tag, typ, cnt = struct.unpack_from(e + "HHI", tiff, off)
            if tag == 0x0112 and typ == 3 and cnt >= 1:
                val = struct.unpack_from(e + "H", tiff, off + 8)[0]
                return val if 1 <= val <= 8 else None
    except struct.error:
        return None
    return None


def _scan_segments(data: bytes, pos: int):
    """Split entropy-coded data starting at ``pos`` into restart segments.

    Returns (segments, rst_markers, end_pos). Byte stuffing is removed;
    0xFF fill bytes before markers are skipped.
    """
    segments = []
    markers = []
    n = len(data)
    start = pos
    parts = []
    while True:
        j = data.find(b"\xff", pos)
        if j < 0:
            parts.append(data[start:])
            segments.append(b"".join(parts))
            return segments, markers, n
        k = j + 1
        while k < n and data[k] == 0xFF:
            k += 1
        if k >= n:
            parts.append(data[start:j])
            segments.append(b"".join(parts))
            return segments, markers, n
        m = data[k]
        if m == 0x00:
            parts.append(data[start:j + 1])  # stuffed 0xFF
            start = pos = k + 1
            continue
        parts.append(data[start:j])
        segments.append(b"".join(parts))
        parts = []
        if 0xD0 <= m <= 0xD7:
            markers.append(m - 0xD0)
            start = pos = k + 1
            continue
        return segments, markers, j


def _decode_scan_segment(seg, comps, mcus, mcux, interleaved, tables, qts, progress, mcu_index, total_mcus,
                         restart_counter):
    """Decode ``mcus`` MCUs from one restart segment into component planes."""
    data = seg + b"\xff" * 8
    limit = len(seg) + 4
    pos = 0
    bits = 0
    nbits = 0
    zz = ZIGZAG
    from_bytes = int.from_bytes
    for c in comps:
        c.pred = 0
    report_every = max(1, total_mcus // 50)
    for m in range(mcus):
        idx = mcu_index + m
        if interleaved:
            my, mx = divmod(idx, mcux)
            units = []
            for c in comps:
                for by in range(c.v):
                    for bx in range(c.h):
                        units.append((c, (my * c.v + by), (mx * c.h + bx)))
        else:
            c = comps[0]
            my, mx = divmod(idx, mcux)
            units = [(c, my, mx)]
        for c, brow, bcol in units:
            dct_t = tables[0][c.td]
            act = tables[1][c.ta]
            if dct_t is None or act is None:
                raise CorruptFile("JPEG scan uses a Huffman table that was not defined.")
            q = qts[c.tq]
            # --- DC ---
            if nbits < 32:
                if pos > limit:
                    raise CorruptFile("JPEG entropy-coded data is truncated.")
                bits = ((bits & ((1 << nbits) - 1)) << 32) | from_bytes(data[pos:pos + 4], "big")
                pos += 4
                nbits += 32
            e = dct_t[(bits >> (nbits - 16)) & 0xFFFF]
            if not e:
                raise CorruptFile("Invalid JPEG Huffman code.")
            nbits -= e & 31
            s = e >> 5
            if s:
                if s > 11:
                    raise CorruptFile("Invalid JPEG DC coefficient size.")
                v = (bits >> (nbits - s)) & ((1 << s) - 1)
                nbits -= s
                if v < (1 << (s - 1)):
                    v -= (1 << s) - 1
                c.pred += v
            dc = c.pred * q[0]
            # --- AC ---
            nz = []
            k = 1
            while k < 64:
                if nbits < 32:
                    if pos > limit:
                        raise CorruptFile("JPEG entropy-coded data is truncated.")
                    bits = ((bits & ((1 << nbits) - 1)) << 32) | from_bytes(data[pos:pos + 4], "big")
                    pos += 4
                    nbits += 32
                e = act[(bits >> (nbits - 16)) & 0xFFFF]
                if not e:
                    raise CorruptFile("Invalid JPEG Huffman code.")
                nbits -= e & 31
                rs = e >> 5
                s = rs & 15
                r = rs >> 4
                if s == 0:
                    if r == 15:
                        k += 16
                        continue
                    break
                k += r
                if k > 63:
                    raise CorruptFile("JPEG AC coefficient run overflows the block.")
                v = (bits >> (nbits - s)) & ((1 << s) - 1)
                nbits -= s
                if v < (1 << (s - 1)):
                    v -= (1 << s) - 1
                nat = zz[k]
                nz.append((nat, v * q[nat]))
                k += 1
            if k > 64:
                raise CorruptFile("JPEG AC coefficient run overflows the block.")
            block = idct_block(nz, dc)
            stride = c.stride
            base = brow * 8 * stride + bcol * 8
            plane = c.plane
            if base + 7 * stride + 8 > len(plane):
                raise CorruptFile("JPEG block lies outside the image.")
            for y in range(8):
                o = base + y * stride
                plane[o:o + 8] = block[y * 8:y * 8 + 8]
        if (idx % report_every) == 0:
            progress.check_cancelled()
            progress.report(idx, total_mcus, "decoding")


def decode(data: bytes, progress=NULL_PROGRESS, limits=DEFAULT_LIMITS) -> Image:
    data = bytes(data)
    if not sniff(data):
        raise CorruptFile("Not a JPEG file (missing SOI marker).")
    n = len(data)
    pos = 2
    qts = [None] * 4
    tables = [[None] * 4, [None] * 4]
    comps = None
    frame = None
    restart_interval = 0
    adobe_transform = None
    jfif = False
    orientation = None
    meta = {"source_format": "jpeg"}
    scans_done = 0
    decoded_any = False
    while True:
        # find next marker
        if pos >= n:
            if decoded_any:
                break
            raise CorruptFile("JPEG file is truncated (no image data).")
        if data[pos] != 0xFF:
            raise CorruptFile("JPEG marker expected.")
        while pos < n and data[pos] == 0xFF:
            pos += 1
        if pos >= n:
            raise CorruptFile("JPEG file is truncated.")
        marker = data[pos]
        pos += 1
        if marker == 0xD9:  # EOI
            break
        if marker in (0x01,) or 0xD0 <= marker <= 0xD7:
            continue
        if pos + 2 > n:
            raise CorruptFile("JPEG marker segment is truncated.")
        seglen = struct.unpack_from(">H", data, pos)[0]
        if seglen < 2 or pos + seglen > n:
            raise CorruptFile("JPEG marker segment is truncated.")
        body = data[pos + 2:pos + seglen]
        pos += seglen
        if marker in UNSUPPORTED_SOF:
            raise UnsupportedVariant(UNSUPPORTED_SOF[marker])
        if marker == 0xDE:
            raise UnsupportedVariant("Hierarchical (differential) JPEG is not supported.")
        if marker in (0xC0, 0xC1):
            if frame is not None:
                raise CorruptFile("JPEG has more than one frame header.")
            if len(body) < 6:
                raise CorruptFile("JPEG frame header is truncated.")
            precision, height, width, ncomp = struct.unpack_from(">BHHB", body, 0)
            if precision != 8:
                raise UnsupportedVariant(f"{precision}-bit JPEG is not supported (only 8-bit).")
            if height == 0:
                raise UnsupportedVariant("JPEG files that define their height later (DNL) are not supported.")
            check_dimensions(width, height, limits)
            if ncomp > limits.max_jpeg_components:
                raise LimitExceeded(f"JPEG declares {ncomp} components, more than allowed.")
            if ncomp == 4:
                raise UnsupportedVariant("CMYK/YCCK (4-component) JPEG is not supported.")
            if ncomp not in (1, 3):
                raise UnsupportedVariant(f"JPEG with {ncomp} components is not supported.")
            if len(body) < 6 + 3 * ncomp:
                raise CorruptFile("JPEG frame header is truncated.")
            comps = []
            for i in range(ncomp):
                cid, hv, tq = body[6 + 3 * i:9 + 3 * i]
                h, v = hv >> 4, hv & 15
                if not 1 <= h <= 4 or not 1 <= v <= 4 or tq > 3:
                    raise CorruptFile("JPEG component parameters are invalid.")
                comps.append(_Component(cid, h, v, tq))
            if len({c.cid for c in comps}) != ncomp:
                raise CorruptFile("JPEG component identifiers are not unique.")
            frame = (width, height, marker)
            hmax = max(c.h for c in comps)
            vmax = max(c.v for c in comps)
            for c in comps:
                if hmax % c.h or vmax % c.v:
                    raise UnsupportedVariant("JPEG uses non-integer chroma subsampling ratios.")
            mcux = (width + 8 * hmax - 1) // (8 * hmax)
            mcuy = (height + 8 * vmax - 1) // (8 * vmax)
            for c in comps:
                c.bw = mcux * c.h
                c.bh = mcuy * c.v
                c.stride = c.bw * 8
                c.plane = bytearray(c.bw * 8 * c.bh * 8)
            meta["jpeg_sampling"] = [(c.h, c.v) for c in comps]
            continue
        if marker == 0xC4:  # DHT
            p = 0
            while p < len(body):
                if p + 17 > len(body):
                    raise CorruptFile("JPEG Huffman table is truncated.")
                tc, th = body[p] >> 4, body[p] & 15
                if tc > 1 or th > 3:
                    raise CorruptFile("JPEG Huffman table class/index is invalid.")
                counts = list(body[p + 1:p + 17])
                total = sum(counts)
                vals = list(body[p + 17:p + 17 + total])
                if len(vals) < total:
                    raise CorruptFile("JPEG Huffman table is truncated.")
                tables[tc][th] = build_decode_table(counts, vals)
                p += 17 + total
            continue
        if marker == 0xDB:  # DQT
            p = 0
            while p < len(body):
                pq, tq = body[p] >> 4, body[p] & 15
                if pq > 1 or tq > 3:
                    raise CorruptFile("JPEG quantisation table is invalid.")
                size = 64 * (2 if pq else 1)
                raw = body[p + 1:p + 1 + size]
                if len(raw) < size:
                    raise CorruptFile("JPEG quantisation table is truncated.")
                if pq:
                    zvals = struct.unpack(">64H", raw)
                else:
                    zvals = list(raw)
                nat = [0] * 64
                for i in range(64):
                    nat[ZIGZAG[i]] = zvals[i]
                qts[tq] = nat
                p += 1 + size
            continue
        if marker == 0xDD:  # DRI
            if len(body) != 2:
                raise CorruptFile("JPEG restart interval segment is invalid.")
            restart_interval = struct.unpack(">H", body)[0]
            continue
        if marker == 0xE0 and body.startswith(b"JFIF\x00") and len(body) >= 12:
            jfif = True
            units, xd, yd = struct.unpack_from(">BHH", body, 7)
            if units == 1 and xd and yd:
                meta["dpi"] = (float(xd), float(yd))
            elif units == 2 and xd and yd:
                meta["dpi"] = (round(xd * 2.54, 1), round(yd * 2.54, 1))
            continue
        if marker == 0xE1:
            o = _parse_exif_orientation(body)
            if o is not None:
                orientation = o
            continue
        if marker == 0xEE and body.startswith(b"Adobe") and len(body) >= 12:
            adobe_transform = body[11]
            continue
        if marker == 0xDA:  # SOS
            if frame is None:
                raise CorruptFile("JPEG scan appears before the frame header.")
            ns = body[0] if body else 0
            if not 1 <= ns <= 4 or len(body) < 1 + 2 * ns + 3:
                raise CorruptFile("JPEG scan header is invalid.")
            scomps = []
            for i in range(ns):
                cs, t = body[1 + 2 * i], body[2 + 2 * i]
                match = [c for c in comps if c.cid == cs]
                if not match:
                    raise CorruptFile("JPEG scan refers to an unknown component.")
                c = match[0]
                c.td, c.ta = t >> 4, t & 15
                if c.td > 3 or c.ta > 3:
                    raise CorruptFile("JPEG scan Huffman table selector is invalid.")
                scomps.append(c)
            ss, se, ahal = body[1 + 2 * ns:4 + 2 * ns]
            if ss != 0 or se != 63 or ahal != 0:
                raise CorruptFile("JPEG baseline scan has invalid spectral selection.")
            for c in scomps:
                if qts[c.tq] is None:
                    raise CorruptFile("JPEG component uses an undefined quantisation table.")
            width, height, _ = frame
            hmax = max(c.h for c in comps)
            vmax = max(c.v for c in comps)
            if ns > 1:
                mcux = (width + 8 * hmax - 1) // (8 * hmax)
                mcuy = (height + 8 * vmax - 1) // (8 * vmax)
                if sum(c.h * c.v for c in scomps) > 10:
                    raise CorruptFile("JPEG MCU has more than 10 blocks.")
                interleaved = True
            else:
                c = scomps[0]
                cw = (width * c.h + hmax - 1) // hmax
                ch = (height * c.v + vmax - 1) // vmax
                mcux = (cw + 7) // 8
                mcuy = (ch + 7) // 8
                interleaved = False
            total = mcux * mcuy
            segments, markers, end = _scan_segments(data, pos)
            pos = end
            interval = restart_interval or total
            needed = (total + interval - 1) // interval
            for i, mk in enumerate(markers[:needed - 1]):
                if mk != i % 8:
                    raise CorruptFile("JPEG restart markers are out of sequence.")
            if len(segments) < needed:
                raise CorruptFile("JPEG scan is truncated (missing restart intervals).")
            done = 0
            for i in range(needed):
                count = min(interval, total - done)
                _decode_scan_segment(segments[i], scomps, count, mcux, interleaved, tables, qts,
                                     progress, done, total, i)
                done += count
            scans_done += 1
            decoded_any = True
            covered = {c.cid for c in scomps}
            meta.setdefault("_scanned", set()).update(covered)
            continue
        # APPn, COM, and anything else with a length: skip.
    if comps is None or not decoded_any:
        raise CorruptFile("JPEG has no decodable image data.")
    if len(meta.pop("_scanned", set())) != len(comps):
        raise CorruptFile("JPEG does not contain data for every component.")
    width, height, sof = frame
    if sof == 0xC1:
        meta["jpeg_process"] = "extended sequential"
    progress.report(1, 1, "decoding")
    img = _assemble(comps, width, height, adobe_transform, jfif, meta)
    if orientation and orientation != 1:
        img = image_ops.apply_orientation(img, orientation)
        img.metadata.setdefault("notes", []).append(
            f"EXIF orientation {orientation} applied (image rotated/flipped upright).")
    return img


def _fancy_upsample(c, fx, fy, out_w, out_h):
    """Triangle ("fancy") upsampling for 2:1 horizontal, vertical or both,
    matching the IJG/libjpeg-turbo filter: 3/4 nearer sample + 1/4 further."""
    stride = c.stride
    plane = c.plane
    cw = (out_w + fx - 1) // fx
    ch = (out_h + fy - 1) // fy
    src = [list(plane[y * stride:y * stride + cw]) for y in range(ch)]
    rows = []
    if fx == 1:  # 1:2 vertical only (h1v2)
        for y in range(ch):
            near = src[y]
            up = src[y - 1] if y > 0 else near
            down = src[y + 1] if y + 1 < ch else near
            rows.append(bytes([(3 * a + b + 1) >> 2 for a, b in zip(near, up)])[:out_w])
            rows.append(bytes([(3 * a + b + 2) >> 2 for a, b in zip(near, down)])[:out_w])
    elif fy == 1:
        for c0 in src:
            left = [c0[0]] + c0[:-1]
            right = c0[1:] + [c0[-1]]
            out = bytearray(2 * cw)
            out[0::2] = bytes([(3 * a + b + 1) >> 2 for a, b in zip(c0, left)])
            out[1::2] = bytes([(3 * a + b + 2) >> 2 for a, b in zip(c0, right)])
            rows.append(bytes(out[:out_w]))
    else:
        for y in range(ch):
            near = src[y]
            for far in (src[y - 1] if y > 0 else near, src[y + 1] if y + 1 < ch else near):
                col = [3 * a + b for a, b in zip(near, far)]
                left = [col[0]] + col[:-1]
                right = col[1:] + [col[-1]]
                out = bytearray(2 * cw)
                out[0::2] = bytes([(3 * a + b + 8) >> 4 for a, b in zip(col, left)])
                out[1::2] = bytes([(3 * a + b + 7) >> 4 for a, b in zip(col, right)])
                rows.append(bytes(out[:out_w]))
    return b"".join(rows[:out_h])


def _upsample(c, hmax, vmax, out_w, out_h):
    """Return the component's samples at full resolution, cropped to out_w x out_h."""
    fx, fy = hmax // c.h, vmax // c.v
    if (fx, fy) in ((2, 1), (2, 2), (1, 2)):
        return _fancy_upsample(c, fx, fy, out_w, out_h)
    stride = c.stride
    plane = c.plane
    cw = (out_w + fx - 1) // fx
    ch = (out_h + fy - 1) // fy
    rows = []
    for y in range(ch):
        row = bytes(plane[y * stride:y * stride + cw])
        if fx > 1:
            wide = bytearray(cw * fx)
            for k in range(fx):
                wide[k::fx] = row
            row = bytes(wide[:out_w])
        else:
            row = row[:out_w]
        for _ in range(fy):
            rows.append(row)
    return b"".join(rows[:out_h])


def _ycc_tables():
    cr_r = [round(1.402 * (i - 128)) for i in range(256)]
    cb_b = [round(1.772 * (i - 128)) for i in range(256)]
    cr_g = [-0.714136 * (i - 128) for i in range(256)]
    cb_g = [-0.344136 * (i - 128) for i in range(256)]
    return cr_r, cb_b, cr_g, cb_g


_YCC = _ycc_tables()
_CLAMP = bytes(min(255, max(0, i - 512)) for i in range(1280))


def ycc_to_rgb(Y, Cb, Cr) -> bytearray:
    cr_r, cb_b, cr_g, cb_g = _YCC
    clamp = _CLAMP
    n = len(Y)
    out = bytearray(n * 3)
    out[0::3] = bytes([clamp[y + cr_r[cr] + 512] for y, cr in zip(Y, Cr)])
    out[1::3] = bytes([clamp[int(y + cb_g[cb] + cr_g[cr] + 512.5)] for y, cb, cr in zip(Y, Cb, Cr)])
    out[2::3] = bytes([clamp[y + cb_b[cb] + 512] for y, cb in zip(Y, Cb)])
    return out


def _assemble(comps, width, height, adobe_transform, jfif, meta) -> Image:
    hmax = max(c.h for c in comps)
    vmax = max(c.v for c in comps)
    planes = [_upsample(c, hmax, vmax, width, height) for c in comps]
    if len(comps) == 1:
        return Image(width, height, "L", 8, bytearray(planes[0]), meta)
    ids = bytes(c.cid for c in comps)
    rgb_stored = (adobe_transform == 0) or (adobe_transform is None and not jfif and ids == b"RGB")
    if rgb_stored:
        out = bytearray(width * height * 3)
        for i in range(3):
            out[i::3] = planes[i]
        meta["jpeg_colour"] = "RGB"
        return Image(width, height, "RGB", 8, out, meta)
    meta["jpeg_colour"] = "YCbCr"
    return Image(width, height, "RGB", 8, ycc_to_rgb(planes[0], planes[1], planes[2]), meta)
