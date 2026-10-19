#!/usr/bin/env python3
"""Builds the lossy WebP fixtures of crates/oc-vp8/tests/data.

Each NAME.webp is made by libwebp (through Pillow or ImageMagick) from a synthetic image;
NAME.yuv is libwebp's decoded Y, U and V planes (WebPDecodeYUV), tightly packed, which the
Rust decoder must reproduce exactly. Run from the repository root; needs numpy, Pillow,
ImageMagick's `convert` and the system libwebp.
"""
import ctypes
import ctypes.util
import io
import os
import subprocess
import sys

import numpy as np
from PIL import Image

OUT = "crates/oc-vp8/tests/data"
os.makedirs(OUT, exist_ok=True)
lib = ctypes.CDLL(ctypes.util.find_library("webp") or "libwebp.so.7")
lib.WebPDecodeYUV.restype = ctypes.POINTER(ctypes.c_uint8)
lib.WebPDecodeYUV.argtypes = [ctypes.c_char_p, ctypes.c_size_t] + [ctypes.c_void_p] * 6


def libwebp_yuv(data):
    w, h, st, uvst = (ctypes.c_int() for _ in range(4))
    u, v = ctypes.POINTER(ctypes.c_uint8)(), ctypes.POINTER(ctypes.c_uint8)()
    y = lib.WebPDecodeYUV(data, len(data), ctypes.byref(w), ctypes.byref(h),
                          ctypes.byref(u), ctypes.byref(v), ctypes.byref(st), ctypes.byref(uvst))
    assert y, "libwebp could not decode"
    W, H, S, UVS = w.value, h.value, st.value, uvst.value
    cw, ch = (W + 1) // 2, (H + 1) // 2
    out = bytearray()
    for plane, stride, pw, ph in ((y, S, W, H), (u, UVS, cw, ch), (v, UVS, cw, ch)):
        raw = ctypes.string_at(plane, stride * ph)
        for r in range(ph):
            out += raw[r * stride:r * stride + pw]
    return W, H, bytes(out)


def synth(kind, w, h, seed=1):
    rng = np.random.default_rng(seed)
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float64)
    if kind == "gradient":
        r = xx / max(w - 1, 1) * 255
        g = yy / max(h - 1, 1) * 255
        b = (xx + yy) / max(w + h - 2, 1) * 255
        a = np.stack([r, g, b], -1)
    elif kind == "noise":
        a = rng.integers(0, 256, (h, w, 3)).astype(np.float64)
    elif kind == "photo":
        a = np.zeros((h, w, 3))
        for c in range(3):
            for _ in range(6):
                fx, fy = rng.uniform(0.01, 0.08, 2)
                ph = rng.uniform(0, 6.28, 2)
                a[..., c] += 28 * np.sin(fx * xx * 6.28 + ph[0]) * np.cos(fy * yy * 6.28 + ph[1])
            a[..., c] += 128
        for _ in range(8):
            cx, cy, rad = rng.uniform(0, w), rng.uniform(0, h), rng.uniform(1, max(w, h, 6) / 3)
            col = rng.uniform(0, 255, 3)
            m = ((xx - cx) ** 2 + (yy - cy) ** 2) < rad * rad
            a[m] = 0.5 * a[m] + 0.5 * col
        a += rng.normal(0, 4, a.shape)
    elif kind == "edges":
        a = np.zeros((h, w, 3)) + 235
        a[(xx // 8 + yy // 8) % 2 == 0] = 20
        a[:, ::7] = [200, 30, 30]
        a[::5, :] = [30, 30, 200]
        a += rng.normal(0, 1.5, a.shape)
    elif kind == "flat":
        a = np.zeros((h, w, 3)) + [90, 160, 200]
    else:
        raise ValueError(kind)
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8), "RGB")


def pil_webp(img, **kw):
    b = io.BytesIO()
    img.save(b, "WEBP", lossless=False, **kw)
    return b.getvalue()


def im_webp(img, **defines):
    src = "/tmp/_fx_src.png"
    img.save(src)
    cmd = ["convert", src]
    for k, v in defines.items():
        cmd += ["-define", "webp:%s=%s" % (k.replace("_", "-"), v)]
    cmd += ["/tmp/_fx.webp"]
    subprocess.run(cmd, check=True)
    return open("/tmp/_fx.webp", "rb").read()


cases = []
for kind, w, h in [("gradient", 64, 64), ("noise", 48, 48), ("photo", 200, 150), ("edges", 128, 96),
                   ("flat", 40, 40), ("photo", 1, 1), ("photo", 2, 3), ("photo", 17, 33),
                   ("photo", 15, 15), ("photo", 16, 16), ("noise", 31, 17), ("photo", 33, 1), ("photo", 1, 33)]:
    img = synth(kind, w, h)
    for q in (5, 40, 75, 100):
        if (w * h > 3000) or q in (5, 75):
            cases.append(("%s_%dx%d_q%d" % (kind, w, h, q), pil_webp(img, quality=q, method=4)))
img = synth("photo", 200, 150)
for m in (0, 2, 6):
    cases.append(("photo_m%d" % m, pil_webp(img, quality=60, method=m)))
for name, d in [
    ("seg1", dict(segments=1)), ("seg2", dict(segments=2)), ("seg3", dict(segments=3)),
    ("part1", dict(partitions=1)), ("part2", dict(partitions=2)), ("part3", dict(partitions=3)),
    ("simple", dict(filter_type=0, filter_strength=40)), ("strong", dict(filter_type=1, filter_strength=70)),
    ("nofilter", dict(filter_strength=0)), ("sharp7", dict(filter_sharpness=7, filter_strength=60)),
    ("sharp3", dict(filter_sharpness=3, filter_strength=30)), ("sns0", dict(sns_strength=0)),
    ("sns100", dict(sns_strength=100, segments=4)), ("autofilter", dict(auto_filter="true")),
]:
    cases.append(("photo_" + name, im_webp(img, quality=55, **d)))
for kind in ("noise", "edges"):
    cases.append((kind + "_im_seg4", im_webp(synth(kind, 96, 80), quality=70, segments=4, sns_strength=80)))

total = 0
for name, data in cases:
    W, H, yuv = libwebp_yuv(data)
    open("%s/%s.webp" % (OUT, name), "wb").write(data)
    open("%s/%s.yuv" % (OUT, name), "wb").write(yuv)
    total += len(data) + len(yuv)
print(len(cases), "cases,", total // 1024, "KiB")
