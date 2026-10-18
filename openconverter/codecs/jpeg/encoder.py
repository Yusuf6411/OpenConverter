"""Baseline JPEG encoder (JFIF, Huffman coded, 8-bit)."""

import struct

from openconverter.codecs.jpeg.dct import aan_divisors, fdct_aan
from openconverter.codecs.jpeg.huffman import build_encode_table, optimal_table
from openconverter.codecs.jpeg.tables import (STD_AC_CHROMA, STD_AC_LUMA, STD_CHROMA_QT, STD_DC_CHROMA,
                                              STD_DC_LUMA, STD_LUMA_QT, ZIGZAG, scaled_quant_table)
from openconverter.models.image import Image
from openconverter.models.progress import NULL_PROGRESS
from openconverter.ops import image_ops

SUBSAMPLING = {"4:4:4": (1, 1), "4:2:2": (2, 1), "4:4:0": (1, 2), "4:2:0": (2, 2), "4:1:1": (4, 1)}

# RGB -> YCbCr (JFIF, BT.601 full range) in 16.16 fixed point, as IJG does.
_FIX = 1 << 16
_Y_R = [19595 * v for v in range(256)]
_Y_G = [38470 * v for v in range(256)]
_Y_B = [7471 * v + 32768 for v in range(256)]
_CB_R = [-11059 * v for v in range(256)]
_CB_G = [-21709 * v for v in range(256)]
_CB_B = [32768 * v + (128 << 16) + 32767 for v in range(256)]
_CR_R = [32768 * v + (128 << 16) + 32767 for v in range(256)]
_CR_G = [-27439 * v for v in range(256)]
_CR_B = [-5329 * v for v in range(256)]


def rgb_to_ycc(rgb: bytes):
    r, g, b = rgb[0::3], rgb[1::3], rgb[2::3]
    y = bytes([(_Y_R[x] + _Y_G[yv] + _Y_B[z]) >> 16 for x, yv, z in zip(r, g, b)])
    cb = bytes([(_CB_R[x] + _CB_G[yv] + _CB_B[z]) >> 16 for x, yv, z in zip(r, g, b)])
    cr = bytes([(_CR_R[x] + _CR_G[yv] + _CR_B[z]) >> 16 for x, yv, z in zip(r, g, b)])
    return y, cb, cr


def _pad_plane(plane: bytes, w: int, h: int, pw: int, ph: int) -> bytes:
    """Extend a w x h plane to pw x ph by replicating the right/bottom edges."""
    rows = []
    for y in range(h):
        row = plane[y * w:(y + 1) * w]
        if pw > w:
            row = row + row[-1:] * (pw - w)
        rows.append(row)
    rows.extend([rows[-1]] * (ph - h))
    return b"".join(rows)


def _downsample(plane: bytes, w: int, h: int, fx: int, fy: int) -> bytes:
    """Box-average a padded plane by integer factors (w, h divisible)."""
    if fx == 1 and fy == 1:
        return plane
    ow = w // fx
    out = []
    count = fx * fy
    half = count // 2
    for oy in range(h // fy):
        acc = [half] * ow
        for dy in range(fy):
            row = plane[(oy * fy + dy) * w:(oy * fy + dy + 1) * w]
            for dx in range(fx):
                acc = [a + v for a, v in zip(acc, row[dx::fx])]
        out.append(bytes([a // count for a in acc]))
    return b"".join(out)


class _Writer:
    """MSB-first entropy-coded segment writer."""

    __slots__ = ("out", "acc", "n")

    def __init__(self):
        self.out = bytearray()
        self.acc = 0
        self.n = 0

    def flush_segment(self) -> bytes:
        """Pad with 1-bits to a byte boundary and return the stuffed bytes."""
        pad = (-self.n) % 8
        if pad:
            self.acc = (self.acc << pad) | ((1 << pad) - 1)
            self.n += pad
        if self.n:
            self.out += self.acc.to_bytes(self.n // 8, "big")
        data = bytes(self.out).replace(b"\xff", b"\xff\x00")
        self.out = bytearray()
        self.acc = 0
        self.n = 0
        return data


def _segment(marker: int, body: bytes) -> bytes:
    return struct.pack(">BBH", 0xFF, marker, len(body) + 2) + body


def _dht(tc, th, table) -> bytes:
    bits, vals = table
    return bytes([(tc << 4) | th]) + bytes(bits) + bytes(vals)


def encode(img: Image, progress=NULL_PROGRESS, notes=None, quality=85, subsampling="auto",
           optimize_huffman=False, background=image_ops.WHITE, restart_interval=0,
           colour_transform=True, **_) -> bytes:
    """Encode baseline JPEG. ``colour_transform=False`` stores RGB directly
    (Adobe APP14 transform 0); it exists for decoder tests."""
    quality = int(quality)
    if not 1 <= quality <= 100:
        raise ValueError("quality must be 1..100")
    if img.has_alpha:
        img = image_ops.flatten_alpha(img, background, notes)
    if img.bit_depth == 16:
        img = image_ops.to_8bit(img, notes)
    grey = img.mode == "L"
    w, h = img.width, img.height
    if grey:
        planes = [bytes(img.pixels)]
        factors = [(1, 1)]
    else:
        if subsampling == "auto":
            subsampling = "4:2:0" if quality < 90 else "4:4:4"
        if subsampling not in SUBSAMPLING:
            raise ValueError(f"unsupported subsampling {subsampling!r}")
        rgb = bytes(img.pixels)
        planes = list(rgb_to_ycc(rgb)) if colour_transform else [rgb[0::3], rgb[1::3], rgb[2::3]]
        hy, vy = SUBSAMPLING[subsampling]
        factors = [(hy, vy), (1, 1), (1, 1)]
        if notes is not None:
            notes.append(f"Chroma subsampling {subsampling}.")
    hmax = max(f[0] for f in factors)
    vmax = max(f[1] for f in factors)
    mcux = (w + 8 * hmax - 1) // (8 * hmax)
    mcuy = (h + 8 * vmax - 1) // (8 * vmax)
    pw, ph = mcux * 8 * hmax, mcuy * 8 * vmax
    comp_planes = []
    for plane, (hf, vf) in zip(planes, factors):
        padded = _pad_plane(plane, w, h, pw, ph)
        comp_planes.append((_downsample(padded, pw, ph, hmax // hf, vmax // vf), pw * hf // hmax))
    qt_luma = scaled_quant_table(STD_LUMA_QT, quality)
    qt_chroma = scaled_quant_table(STD_CHROMA_QT, quality)
    qts = [qt_luma, qt_chroma]
    divs = [aan_divisors(qt_luma), aan_divisors(qt_chroma)]
    comp_q = [0] + [1 if colour_transform else 0] * (len(factors) - 1)

    # --- transform + quantise every block, in MCU order ---
    zz = ZIGZAG
    blocks = []  # list of (component index, zig-zag quantised coefficients)
    total_mcus = mcux * mcuy
    report_every = max(1, mcux)
    for my in range(mcuy):
        for mx in range(mcux):
            for ci, (hf, vf) in enumerate(factors):
                plane, stride = comp_planes[ci]
                div = divs[comp_q[ci]]
                dc_shift = 1024.0 / qts[comp_q[ci]][0]
                for by in range(vf):
                    for bx in range(hf):
                        base = (my * vf + by) * 8 * stride + (mx * hf + bx) * 8
                        blk = list(b"".join(plane[base + y * stride:base + y * stride + 8] for y in range(8)))
                        fdct_aan(blk)
                        q = [int(c * d + 16384.5) - 16384 for c, d in zip(blk, div)]
                        q[0] = int(blk[0] * div[0] - dc_shift + 16384.5) - 16384
                        blocks.append((ci, [q[z] for z in zz]))
        if (my * mcux) % report_every == 0:
            progress.check_cancelled()
            progress.report(my * mcux, total_mcus * 2, "encoding")

    # --- Huffman tables ---
    ncomp = len(factors)
    if optimize_huffman:
        dc_freq = [[0] * 256, [0] * 256]
        ac_freq = [[0] * 256, [0] * 256]
        preds = [0] * ncomp
        for ci, zq in blocks:
            t = comp_q[ci]
            diff = zq[0] - preds[ci]
            preds[ci] = zq[0]
            dc_freq[t][abs(diff).bit_length()] += 1
            run = 0
            for v in zq[1:]:
                if v == 0:
                    run += 1
                    continue
                while run > 15:
                    ac_freq[t][0xF0] += 1
                    run -= 16
                ac_freq[t][(run << 4) | abs(v).bit_length()] += 1
                run = 0
            if run:
                ac_freq[t][0x00] += 1
        dc_tabs = [optimal_table(dc_freq[0]), optimal_table(dc_freq[1])]
        ac_tabs = [optimal_table(ac_freq[0]), optimal_table(ac_freq[1])]
    else:
        dc_tabs = [STD_DC_LUMA, STD_DC_CHROMA]
        ac_tabs = [STD_AC_LUMA, STD_AC_CHROMA]
    dc_enc = [build_encode_table(*t) for t in dc_tabs]
    ac_enc = [build_encode_table(*t) for t in ac_tabs]

    # --- entropy coding ---
    wr = _Writer()
    scan = bytearray()
    preds = [0] * ncomp
    blocks_per_mcu = sum(hf * vf for hf, vf in factors)
    acc = 0
    n = 0
    out = wr.out
    mcu_count = 0
    rst = 0
    for bi, (ci, zq) in enumerate(blocks):
        t = comp_q[ci]
        de = dc_enc[t]
        ae = ac_enc[t]
        diff = zq[0] - preds[ci]
        preds[ci] = zq[0]
        s = abs(diff).bit_length()
        code, ln = de[s]
        acc = (acc << ln) | code
        n += ln
        if s:
            acc = (acc << s) | (diff if diff >= 0 else diff + (1 << s) - 1)
            n += s
        run = 0
        for v in zq[1:]:
            if v == 0:
                run += 1
                continue
            while run > 15:
                code, ln = ae[0xF0]
                acc = (acc << ln) | code
                n += ln
                run -= 16
            s = abs(v).bit_length()
            code, ln = ae[(run << 4) | s]
            acc = (((acc << ln) | code) << s) | (v if v >= 0 else v + (1 << s) - 1)
            n += ln + s
            run = 0
            if n >= 32:
                n -= 32
                out += (acc >> n).to_bytes(4, "big")
                acc &= (1 << n) - 1
        if run:
            code, ln = ae[0x00]
            acc = (acc << ln) | code
            n += ln
        if n >= 32:
            n -= 32
            out += (acc >> n).to_bytes(4, "big")
            acc &= (1 << n) - 1
        if (bi + 1) % blocks_per_mcu == 0:
            mcu_count += 1
            if (mcu_count % report_every) == 0:
                progress.check_cancelled()
                progress.report(total_mcus + mcu_count, total_mcus * 2, "encoding")
            if restart_interval and mcu_count % restart_interval == 0 and mcu_count < total_mcus:
                wr.acc, wr.n = acc, n
                scan += wr.flush_segment()
                out = wr.out
                acc = n = 0
                scan += bytes((0xFF, 0xD0 + rst))
                rst = (rst + 1) & 7
                preds = [0] * ncomp
    wr.acc, wr.n = acc, n
    scan += wr.flush_segment()

    # --- headers ---
    parts = [b"\xff\xd8"]
    dpi = img.metadata.get("dpi")
    if dpi and 0 < dpi[0] < 65536 and 0 < dpi[1] < 65536:
        density = struct.pack(">BHH", 1, round(dpi[0]), round(dpi[1]))
    else:
        density = struct.pack(">BHH", 0, 1, 1)
    if colour_transform or grey:
        parts.append(_segment(0xE0, b"JFIF\x00\x01\x01" + density + b"\x00\x00"))
    else:
        parts.append(_segment(0xEE, b"Adobe\x00\x64\x00\x00\x00\x00\x00"))
    nq = 1 if grey or not colour_transform else 2
    dqt = b"".join(bytes([i]) + bytes(qts[i][z] for z in zz) for i in range(nq))
    parts.append(_segment(0xDB, dqt))
    sof = struct.pack(">BHHB", 8, h, w, ncomp)
    for ci, (hf, vf) in enumerate(factors):
        cid = ci + 1 if colour_transform or grey else b"RGB"[ci]
        sof += bytes([cid, (hf << 4) | vf, comp_q[ci]])
    parts.append(_segment(0xC0, sof))
    dht = b"".join(_dht(0, i, dc_tabs[i]) + _dht(1, i, ac_tabs[i]) for i in range(nq))
    parts.append(_segment(0xC4, dht))
    if restart_interval:
        parts.append(_segment(0xDD, struct.pack(">H", restart_interval)))
    sos = bytes([ncomp])
    for ci in range(ncomp):
        cid = ci + 1 if colour_transform or grey else b"RGB"[ci]
        sos += bytes([cid, (comp_q[ci] << 4) | comp_q[ci]])
    sos += b"\x00\x3f\x00"
    parts.append(_segment(0xDA, sos))
    parts.append(bytes(scan))
    parts.append(b"\xff\xd9")
    progress.report(1, 1, "encoding")
    return b"".join(parts)
