"""BigTIFF (and later CCITT, JPEG-in-TIFF) fixtures for the TIFF test, written by libtiff
through ImageMagick/Pillow; expected pixels are ImageMagick's own decode.

Run: python3 make_tiff_extra_fixtures.py ../tests/data/tiff
"""
import os
import subprocess
import sys

import numpy as np
from PIL import Image

out = sys.argv[1]
src = "/tmp/_tiff_extra_src.png"
rng = np.random.default_rng(3)
a = np.zeros((37, 53, 3), np.uint8)
for y in range(37):
    for x in range(53):
        a[y, x] = [x * 4, y * 6, (x * y) % 251]
a[10:20, 10:30] = [200, 30, 30]
Image.fromarray(a).save(src)
Image.fromarray(a[:, :, 0]).save("/tmp/_tiff_extra_gray.png")


def im(*args):
    subprocess.run(["convert", *args], check=True)


def dump(name, depth):
    raw = f"{out}/{name}.rgba"
    im(f"{out}/{name}.tif", "-depth", str(depth), f"rgba:{raw}")


big = [
    ("big_rgb_none", ["-compress", "none"]),
    ("big_rgb_lzw", ["-compress", "lzw"]),
    ("big_rgb_zip", ["-compress", "zip"]),
    ("big_rgb_packbits", ["-compress", "rle"]),
    ("big_msb_rgb_lzw", ["-compress", "lzw", "-endian", "MSB"]),
    ("big_rgb_strips3", ["-compress", "lzw", "-define", "tiff:rows-per-strip=3"]),
    ("big_rgb_tiles", ["-compress", "zip", "-define", "tiff:tile-geometry=16x16"]),
    ("big_rgba", ["-compress", "lzw", "-alpha", "on", "-channel", "A", "-evaluate", "set", "60%"]),
]
for name, opts in big:
    im(src, *opts, f"TIFF64:{out}/{name}.tif")
    dump(name, 8)
im("/tmp/_tiff_extra_gray.png", "-depth", "16", "-compress", "zip", f"TIFF64:{out}/big_gray16.tif")
dump("big_gray16", 16)

# --- CCITT fax -------------------------------------------------------------
def bilevel(kind, w, h):
    r = np.random.default_rng(w * 1000 + h)
    img = np.zeros((h, w), bool)
    if kind == "noise":
        img = r.random((h, w)) > 0.5
    elif kind == "text":
        for _ in range(max(3, w * h // 400)):
            x, y = r.integers(0, w), r.integers(0, h)
            img[y:y + r.integers(1, 6), x:x + r.integers(1, 40)] = True
        img[:, ::17] ^= True
    elif kind == "long":
        img[:, :w // 3] = True
        img[h // 2:, w // 2:] = True
        img[1::3, 5:w - 9] ^= True
    elif kind == "blank":
        pass
    elif kind == "solid":
        img[:] = True
    return img


def fax(name, kind, w, h, comp, tags=None):
    path = f"{out}/{name}.tif"
    img = Image.fromarray(bilevel(kind, w, h))
    img.save(path, compression=comp, tiffinfo=tags or {})
    # ImageMagick's (libtiff's) own decode is the reference.
    im(path, "-depth", "8", f"rgba:{out}/{name}.rgba")


for comp, tag in (("tiff_ccitt", "mh"), ("group3", "g3"), ("group4", "g4")):
    for kind, w, h in (("text", 200, 60), ("noise", 77, 21), ("long", 2600, 12), ("blank", 33, 9),
                       ("solid", 64, 5), ("text", 1, 1), ("text", 7, 3), ("noise", 1700, 9)):
        fax(f"fax_{tag}_{kind}_{w}x{h}", kind, w, h, comp)
    fax(f"fax_{tag}_strips", "text", 150, 40, comp, {278: 7})
fax("fax_g3_2d", "text", 200, 60, "group3", {292: 1})
fax("fax_g3_2d_noise", "noise", 90, 30, "group3", {292: 1})
fax("fax_g3_fill", "text", 150, 20, "group3", {292: 4})
fax("fax_g3_2d_strips", "text", 150, 40, "group3", {292: 1, 278: 6})
# ImageMagick writes white-is-zero, Pillow black-is-zero
img = Image.fromarray(bilevel("text", 180, 50))
img.save("/tmp/_fax_src.png")
for tag, comp in (("fax", "Fax"), ("group4", "Group4")):
    im("/tmp/_fax_src.png", "-monochrome", "-compress", comp, f"{out}/fax_im_{tag}.tif")
    im(f"{out}/fax_im_{tag}.tif", "-depth", "8", f"rgba:{out}/fax_im_{tag}.rgba")
