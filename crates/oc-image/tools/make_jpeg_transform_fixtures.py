"""Fixtures for lossless JPEG transforms, made with cjpeg and jpegtran from libjpeg-turbo.

Dev-time only. For every source `<name>.jpg` and operation `<op>`, `<name>__<op>.rgb` (or
`.gray`) holds what djpeg decodes from jpegtran's output, and `<name>__<op>.txt` its size.
"""
import os
import subprocess
import tempfile
from PIL import Image

out = os.path.join(os.path.dirname(__file__), "..", "tests", "data", "jpeg", "transform")
os.makedirs(out, exist_ok=True)


def picture(w, h, gray):
    im = Image.new("RGB", (w, h))
    px = im.load()
    for y in range(h):
        for x in range(w):
            # An asymmetric pattern, so a wrong mirror or turn cannot go unnoticed.
            px[x, y] = ((x * 5 + y * y // 3) % 256, (y * 7 + x * x // 5) % 256, (x * y + 11 * x) % 256)
    return im.convert("L") if gray else im


SOURCES = [
    # name, size, gray, cjpeg flags
    ("t444", (48, 32), False, ["-sample", "1x1"]),
    ("t420", (48, 32), False, ["-sample", "2x2"]),
    ("t422", (48, 32), False, ["-sample", "2x1"]),
    ("t440", (48, 32), False, ["-sample", "1x2"]),
    ("tgray", (40, 24), True, []),
    ("t420_prog", (48, 32), False, ["-sample", "2x2", "-progressive"]),
    ("t420_odd", (53, 37), False, ["-sample", "2x2"]),
    ("t444_odd", (21, 13), False, ["-sample", "1x1"]),
]
OPS = {
    "flip_h": ["-flip", "horizontal"],
    "flip_v": ["-flip", "vertical"],
    "transpose": ["-transpose"],
    "transverse": ["-transverse"],
    "rot90": ["-rotate", "90"],
    "rot180": ["-rotate", "180"],
    "rot270": ["-rotate", "270"],
    "crop": ["-crop", "16x16+16+8"],
}

for name, size, gray, flags in SOURCES:
    with tempfile.TemporaryDirectory() as t:
        src = os.path.join(t, "s.ppm")
        picture(*size, gray).save(src)
        jpg = os.path.join(out, name + ".jpg")
        subprocess.run(["cjpeg", "-quality", "80", *flags, "-outfile", jpg, src], check=True)
        for op, args in OPS.items():
            if op == "crop" and size[0] < 40:
                continue
            res = os.path.join(t, "r.jpg")
            subprocess.run(["jpegtran", "-trim", *args, "-outfile", res, jpg], check=True)
            pnm = subprocess.run(["djpeg", "-pnm", res], check=True, capture_output=True).stdout
            # P5/P6 header: magic, width height, maxval.
            parts = pnm.split(b"\n", 3)
            w, h = map(int, parts[1].split())
            stem = os.path.join(out, f"{name}__{op}")
            open(stem + (".gray" if gray else ".rgb"), "wb").write(parts[3])
            open(stem + ".txt", "w").write(f"{w} {h}\n")
print(len(os.listdir(out)))
