"""Writes 12-bit and lossless JPEG fixtures with libjpeg-turbo 3 (via imagecodecs) and the
samples it decodes from them, scaled the way our decoder presents them. Dev-time only.

    pip install imagecodecs
    python3 make_jpeg_deep_fixtures.py crates/oc-image/tests/data/jpeg

Files: deep_*.jpg with deep_*.gray / .rgb (8-bit output) or .gray16 / .rgb16 (big-endian
16-bit output). The Pt and restart cases come from a small lossless writer in this file,
because the library does not expose them; the library still decodes them for the answer.
"""
import os
import struct
import sys

import imagecodecs as ic
import numpy as np
from PIL import Image

out = sys.argv[1]
base = np.array(Image.open(os.path.join(out, "big_444.jpg")).convert("RGB"))
crop = np.ascontiguousarray(base[:61, :83])
small = np.ascontiguousarray(base[:21, :37])
green = np.ascontiguousarray(crop[..., 1])


def rescale(v, p, to):
    v = v.astype(np.uint32)
    if p >= to:
        return v >> (p - to)
    res = v << (to - p)
    shift = to - p
    while shift > 0:
        s = min(shift, p)
        res |= (v >> (p - s)) << (shift - s)
        shift -= s
    return res


def save(name, jpg, samples, p):
    """samples: decoded array at precision p, shape (h, w) or (h, w, 3)."""
    open(os.path.join(out, name + ".jpg"), "wb").write(jpg)
    kind = "gray" if samples.ndim == 2 else "rgb"
    if p <= 8:
        data = rescale(samples, p, 8).astype(np.uint8).tobytes()
        ext = kind
    else:
        data = rescale(samples, p, 16).astype(">u2").tobytes()
        ext = kind + "16"
    open(os.path.join(out, f"{name}.{ext}"), "wb").write(data)
    print(name, len(jpg), samples.shape, p)


# ---- 12-bit DCT
g12 = (green.astype(np.uint16) * 16 + 7)
c12 = crop.astype(np.uint16) * 16 + 5
for q in (50, 95):
    j = ic.jpeg_encode(g12, level=q, bitspersample=12, colorspace="gray")
    save(f"deep_dct12_gray_q{q}", j, ic.jpeg_decode(j).reshape(g12.shape), 12)
    j = ic.jpeg_encode(c12, level=q, bitspersample=12)
    save(f"deep_dct12_ycc_q{q}", j, ic.jpeg_decode(j), 12)
for ss in ("444", "422", "440"):
    j = ic.jpeg_encode(c12, level=90, bitspersample=12, subsampling=ss)
    save(f"deep_dct12_ycc_{ss}", j, ic.jpeg_decode(j), 12)
j = ic.jpeg_encode(c12, level=90, bitspersample=12, colorspace="rgb", outcolorspace="rgb")
save("deep_dct12_rgb", j, ic.jpeg_decode(j), 12)
j = ic.jpeg_encode(np.ascontiguousarray(small[..., 0].astype(np.uint16) * 16), level=80, bitspersample=12, colorspace="gray")
save("deep_dct12_small", j, ic.jpeg_decode(j).reshape(small.shape[:2]), 12)

# ---- lossless from the library: every predictor, several precisions
for pred in range(1, 8):
    j = ic.jpeg_encode(green, lossless=True, predictor=pred, bitspersample=8)
    save(f"deep_ll8_gray_p{pred}", j, ic.jpeg_decode(j).reshape(crop.shape[:2]), 8)
for p in (2, 5, 10, 12, 14, 16):
    shifted = (green.astype(np.uint32) * ((1 << p) - 1) // 255).astype(np.uint8 if p <= 8 else np.uint16)
    pred = 1 + p % 7
    j = ic.jpeg_encode(shifted, lossless=True, predictor=pred, bitspersample=p)
    save(f"deep_ll{p}_gray", j, ic.jpeg_decode(j).reshape(shifted.shape), p)
for pred in (1, 4, 7):
    j = ic.jpeg_encode(crop, lossless=True, predictor=pred, bitspersample=8, colorspace="rgb", outcolorspace="rgb")
    save(f"deep_ll8_rgb_p{pred}", j, ic.jpeg_decode(j), 8)
j = ic.jpeg_encode(crop, lossless=True, predictor=2, bitspersample=8)
save("deep_ll8_ycc_p2", j, ic.jpeg_decode(j), 8)
c16 = crop.astype(np.uint16) * 257
j = ic.jpeg_encode(c16, lossless=True, predictor=6, bitspersample=16, colorspace="rgb", outcolorspace="rgb")
save("deep_ll16_rgb_p6", j, ic.jpeg_decode(j), 16)


# ---- our own lossless writer: point transform and restart intervals
def sof3_file(samples, p, pred, pt, restart, ncomp):
    h, w = samples.shape[:2]
    planes = [samples] if ncomp == 1 else [samples[..., c] for c in range(3)]
    seg = lambda m, b: b"\xff" + bytes([m]) + struct.pack(">H", len(b) + 2) + b
    o = b"\xff\xd8"
    o += seg(0xC3, bytes([p]) + struct.pack(">HH", h, w) + bytes([ncomp]) + b"".join(bytes([c + 1, 0x11, 0]) for c in range(ncomp)))
    # 17 five-bit codes: 00000..10000 (the all-ones code stays unused)
    o += seg(0xC4, bytes([0x00, 0, 0, 0, 0, 17] + [0] * 11) + bytes(range(17)))
    if restart:
        o += seg(0xDD, struct.pack(">H", restart))
    o += seg(0xDA, bytes([ncomp]) + b"".join(bytes([c + 1, 0x00]) for c in range(ncomp)) + bytes([pred, 0, pt]))
    bits = []

    def put(v, n):
        for i in range(n - 1, -1, -1):
            bits.append((v >> i) & 1)

    def flush_marker(rst):
        while len(bits) % 8:
            bits.append(1)
        by = bytearray()
        for i in range(0, len(bits), 8):
            b = 0
            for k in range(8):
                b = (b << 1) | bits[i + k]
            by.append(b)
            if b == 0xFF:
                by.append(0)
        bits.clear()
        return bytes(by) + (b"\xff" + bytes([0xD0 + rst]) if rst is not None else b"")

    data = b""
    rst = 0
    start = 0
    vals = [pl.astype(np.int64) >> pt for pl in planes]
    for pix in range(h * w):
        if restart and pix and pix % restart == 0:
            data += flush_marker(rst)
            rst = (rst + 1) % 8
            start = pix
        y, x = divmod(pix, w)
        for pl in vals:
            if pix == start:
                pr = 1 << (p - pt - 1)
            elif y == start // w:
                pr = int(pl[y, x - 1])
            elif x == 0:
                pr = int(pl[y - 1, x])
            else:
                ra, rb, rc = int(pl[y, x - 1]), int(pl[y - 1, x]), int(pl[y - 1, x - 1])
                pr = [0, ra, rb, rc, ra + rb - rc, ra + ((rb - rc) >> 1), rb + ((ra - rc) >> 1), (ra + rb) >> 1][pred]
            diff = ((int(pl[y, x]) - pr + 32768) & 0xFFFF) - 32768
            if diff == -32768:
                diff = 32768
            if diff == 32768:
                put(16, 5)
            else:
                s = abs(diff).bit_length()
                put(s, 5)
                if s:
                    put(diff if diff >= 0 else diff - 1, s)
    data += flush_marker(None)
    return o + data + b"\xff\xd9"


g8 = green
for name, (samp, p, pred, pt, restart, nc) in {
    "deep_ll8_pt2": (g8, 8, 1, 2, 0, 1),
    "deep_ll8_pt1_p5": (g8, 8, 5, 1, 0, 1),
    "deep_ll12_pt3": ((g8.astype(np.uint16) * 16), 12, 4, 3, 0, 1),
    "deep_ll8_restart_row": (g8, 8, 1, 0, 83, 1),
    "deep_ll8_restart_row_p7": (g8, 8, 7, 0, 83 * 3, 1),
    "deep_ll8_restart_mid": (g8, 8, 4, 0, 50, 1),
    "deep_ll8_rgb_restart": (crop, 8, 6, 0, 83, 3),
    "deep_ll8_rgb_pt1": (crop, 8, 3, 1, 0, 3),
}.items():
    j = sof3_file(samp, p, pred, pt, restart, nc)
    try:
        dec = ic.jpeg_decode(j)
    except Exception as e:
        print(name, "library refused:", e)
        continue
    save(name, j, dec.reshape(samp.shape), p)
