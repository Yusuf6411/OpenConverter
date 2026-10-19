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

# --- JPEG in TIFF ------------------------------------------------------------
photo = np.zeros((70, 90, 3), np.uint8)
for y in range(70):
    for x in range(90):
        photo[y, x] = [(x * 3 + y) % 256, (y * 4) % 256, 128 + int(100 * np.sin(x / 7.0 + y / 11.0))]
Image.fromarray(photo).save("/tmp/_jpeg_src.png")
Image.fromarray(photo[:, :, 1]).save("/tmp/_jpeg_gray.png")
variants = [
    ("jpg_rgb_ycc", ["-compress", "JPEG", "-quality", "90"]),
    ("jpg_rgb_ycc_q40", ["-compress", "JPEG", "-quality", "40"]),
    ("jpg_rgb_strips", ["-compress", "JPEG", "-quality", "85", "-define", "tiff:rows-per-strip=16"]),
    ("jpg_rgb_tiles", ["-compress", "JPEG", "-quality", "85", "-define", "tiff:tile-geometry=32x32"]),
    ("jpg_rgb_direct", ["-compress", "JPEG", "-quality", "90", "-define", "tiff:photometric=rgb"]),
    ("jpg_rgb_444", ["-compress", "JPEG", "-quality", "90", "-sampling-factor", "1x1"]),
    ("jpg_big", ["-compress", "JPEG", "-quality", "85", "-endian", "MSB"]),
]
for name, opts in variants:
    im("/tmp/_jpeg_src.png", *opts, f"{out}/{name}.tif")
    dump(name, 8)
im("/tmp/_jpeg_gray.png", "-compress", "JPEG", "-quality", "90", f"{out}/jpg_gray.tif")
dump("jpg_gray", 8)
im("/tmp/_jpeg_src.png", "-compress", "JPEG", "-quality", "90", f"TIFF64:{out}/jpg_bigtiff.tif")
dump("jpg_bigtiff", 8)
# Pillow's libtiff
Image.fromarray(photo).save(f"{out}/jpg_pillow.tif", compression="jpeg", quality=80)
im(f"{out}/jpg_pillow.tif", "-depth", "8", f"rgba:{out}/jpg_pillow.rgba")

# YCbCr (photometric 6) with the JPEG stream carried whole in the strip, no JPEGTables:
# built by hand, decoded by libtiff for the reference.
import io
import struct


def tiff_with_jpeg(path, jpeg_bytes, w, h, photometric, subsample):
    entries = []

    def short(tag, vals):
        entries.append((tag, 3, vals))

    def long(tag, vals):
        entries.append((tag, 4, vals))

    n_entries = 11
    data_off = 8 + 2 + n_entries * 12 + 4
    bps_off = data_off
    sub_off = bps_off + 6
    jpeg_off = sub_off + 4
    ents = [
        (256, 3, 1, w), (257, 3, 1, h), (258, 3, 3, bps_off), (259, 3, 1, 7), (262, 3, 1, photometric),
        (273, 4, 1, jpeg_off), (277, 3, 1, 3), (278, 3, 1, h), (279, 4, 1, len(jpeg_bytes)),
        (284, 3, 1, 1), (530, 3, 2, subsample),
    ]
    assert len(ents) == n_entries
    out_b = b"II*\0" + struct.pack("<I", 8) + struct.pack("<H", n_entries)
    for tag, typ, cnt, val in ents:
        if tag == 530:
            out_b += struct.pack("<HHIHH", tag, typ, cnt, *val)
        elif typ == 3 and cnt == 1:
            out_b += struct.pack("<HHIHH", tag, typ, cnt, val, 0)
        else:
            out_b += struct.pack("<HHII", tag, typ, cnt, val)
    out_b += struct.pack("<I", 0)
    out_b += struct.pack("<HHH", 8, 8, 8) + b"\0\0\0\0" + jpeg_bytes
    open(path, "wb").write(out_b)


for tag, sub, name in (("420", (2, 2), "2x2"), ("444", (1, 1), "1x1"), ("422", (2, 1), "2x1")):
    buf = io.BytesIO()
    Image.fromarray(photo).save(buf, format="JPEG", quality=88, subsampling={"420": 2, "444": 0, "422": 1}[tag])
    tiff_with_jpeg(f"{out}/jpg_ycbcr_{tag}.tif", buf.getvalue(), 90, 70, 6, sub)
    dump(f"jpg_ycbcr_{tag}", 8)
