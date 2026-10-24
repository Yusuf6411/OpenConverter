"""Writes arithmetic-coded JPEG fixtures with libjpeg-turbo's cjpeg and the pixels djpeg
decodes from them. Dev-time only. Needs cjpeg and djpeg (libjpeg-turbo-progs) and Pillow.

    python3 make_jpeg_arith_fixtures.py crates/oc-image/tests/data/jpeg
"""
import os
import subprocess
import sys
import tempfile

from PIL import Image

out = sys.argv[1]
src = Image.open(os.path.join(out, "big_444.jpg")).convert("RGB")
variants = {
    "arith_444": ["-sample", "1x1"],
    "arith_420": ["-sample", "2x2"],
    "arith_422": ["-sample", "2x1"],
    "arith_prog_444": ["-progressive", "-sample", "1x1"],
    "arith_prog_420": ["-progressive", "-sample", "2x2"],
    "arith_q10": ["-quality", "10"],
    "arith_q100": ["-quality", "100", "-sample", "1x1"],
    "arith_restart": ["-restart", "7", "-sample", "2x2"],
    "arith_prog_restart": ["-progressive", "-restart", "5B"],
}
with tempfile.TemporaryDirectory() as t:
    ppm = os.path.join(t, "s.ppm")
    src.save(ppm)
    small = src.crop((0, 0, 37, 21))
    small_ppm = os.path.join(t, "small.ppm")
    small.save(small_ppm)
    gray_pgm = os.path.join(t, "g.pgm")
    src.convert("L").save(gray_pgm)
    jobs = [(n, ppm, a) for n, a in variants.items()]
    jobs.append(("arith_small", small_ppm, ["-sample", "2x2"]))
    jobs.append(("arith_prog_small", small_ppm, ["-progressive", "-sample", "2x2"]))
    jobs.append(("arith_gray", gray_pgm, ["-grayscale"]))
    jobs.append(("arith_prog_gray", gray_pgm, ["-grayscale", "-progressive"]))
    for name, inp, args in jobs:
        jpg = os.path.join(out, name + ".jpg")
        with open(jpg, "wb") as f:
            subprocess.run(["cjpeg", "-arithmetic", *args, inp], stdout=f, check=True)
        dec = subprocess.run(["djpeg", "-pnm", jpg], capture_output=True, check=True).stdout
        im = Image.open(__import__("io").BytesIO(dec))
        ext = "gray" if im.mode == "L" else "rgb"
        open(os.path.join(out, f"{name}.{ext}"), "wb").write(im.tobytes())
        print(name, os.path.getsize(jpg), im.mode, im.size)
