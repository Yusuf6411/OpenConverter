#!/usr/bin/env python3
"""Builds crates/oc-image/tests/data/webp_lossy: lossy WebP files made by libwebp (through
Pillow and ImageMagick) with the length, CRC-32 and Adler-32 of the RGBA that Pillow decodes from them (NAME.sum),
which the Rust decoder must reproduce exactly. Run from the repository root; needs numpy, Pillow and `convert`."""
import io
import os
import subprocess
import zlib

import numpy as np
from PIL import Image

OUT = "crates/oc-image/tests/data/webp_lossy"
os.makedirs(OUT, exist_ok=True)


def photo(w, h, seed=3):
    rng = np.random.default_rng(seed)
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float64)
    a = np.zeros((h, w, 3))
    for c in range(3):
        for _ in range(5):
            fx, fy = rng.uniform(0.01, 0.08, 2)
            ph = rng.uniform(0, 6.28, 2)
            a[..., c] += 30 * np.sin(fx * xx * 6.28 + ph[0]) * np.cos(fy * yy * 6.28 + ph[1])
        a[..., c] += 128
    a += rng.normal(0, 3, a.shape)
    return np.clip(a, 0, 255).astype(np.uint8)


def with_alpha(rgb, kind, seed=5):
    h, w, _ = rgb.shape
    rng = np.random.default_rng(seed)
    yy, xx = np.mgrid[0:h, 0:w]
    if kind == "ramp":
        a = (xx * 255 // max(w - 1, 1))
    elif kind == "disc":
        a = np.where((xx - w / 2) ** 2 + (yy - h / 2) ** 2 < (min(w, h) / 3) ** 2, 255, 0)
    elif kind == "noise":
        a = rng.integers(0, 256, (h, w))
    else:  # opaque
        a = np.full((h, w), 255)
    return Image.fromarray(np.dstack([rgb, a.astype(np.uint8)]), "RGBA")


cases = []


def add(name, data):
    cases.append((name, data))


def pil(img, **kw):
    b = io.BytesIO()
    img.save(b, "WEBP", lossless=False, **kw)
    return b.getvalue()


def im(img, **defines):
    img.save("/tmp/_lw.png")
    cmd = ["convert", "/tmp/_lw.png"]
    for k, v in defines.items():
        cmd += ["-define", "webp:%s=%s" % (k.replace("_", "-"), v)]
    cmd += ["/tmp/_lw.webp"]
    subprocess.run(cmd, check=True)
    return open("/tmp/_lw.webp", "rb").read()


rgb = photo(120, 90)
add("opaque_120x90", pil(Image.fromarray(rgb), quality=70))
add("odd_37x23", pil(Image.fromarray(photo(37, 23)), quality=50))
add("tiny_1x1", pil(Image.fromarray(photo(1, 1)), quality=50))
add("wide_100x2", pil(Image.fromarray(photo(100, 2)), quality=50))
add("tall_3x70", pil(Image.fromarray(photo(3, 70)), quality=50))
for kind in ("ramp", "disc", "noise"):
    img = with_alpha(rgb, kind)
    add("alpha_%s_default" % kind, pil(img, quality=70, alpha_quality=100))
    for comp in (0, 1):
        for filt in (0, 1, 2, 3):
            add("alpha_%s_c%d_f%d" % (kind, comp, filt),
                im(img, quality=70, alpha_compression=comp, alpha_filtering=min(filt, 2) if filt < 3 else 2,
                   alpha_quality=100) if filt != 3 else
                im(img, quality=70, alpha_compression=comp, alpha_filtering=2, alpha_quality=100))
    add("alpha_%s_q30" % kind, im(img, quality=70, alpha_quality=30))
small = with_alpha(photo(21, 13), "disc")
add("alpha_odd_21x13", pil(small, quality=60, alpha_quality=100))

for name, data in cases:
    open("%s/%s.webp" % (OUT, name), "wb").write(data)
    ref = Image.open(io.BytesIO(data)).convert("RGBA")
    raw = ref.tobytes()
    open("%s/%s.sum" % (OUT, name), "w").write("%d %08x %08x\n" % (len(raw), zlib.crc32(raw), zlib.adler32(raw)))
print(len(cases), "cases")
