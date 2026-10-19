"""PSD fixtures written by ImageMagick, with the pixels Pillow (8-bit) or ImageMagick (16-bit)
reads back from the merged image. Dev-time only.

`<name>.rgba` holds RGBA (8 bits, or 16 bits big-endian when the name ends in 16); `<name>.txt`
holds "width height bits".
"""
import os
import subprocess
import tempfile
from PIL import Image

out = os.path.join(os.path.dirname(__file__), "..", "tests", "data", "psd")
os.makedirs(out, exist_ok=True)
w, h = 23, 17
src = Image.new("RGBA", (w, h))
px = src.load()
for y in range(h):
    for x in range(w):
        px[x, y] = (x * 10 % 256, y * 14 % 256, (x * y) % 256, 255 if x > 4 else x * 50)


def run(*args):
    subprocess.run(args, check=True)


def record(name, bits=8):
    path = os.path.join(out, name)
    base = path.rsplit(".", 1)[0]
    if bits == 8 and not name.endswith(".psb"):
        rgba = Image.open(path).convert("RGBA")
        data = rgba.tobytes()
    else:
        # Pillow reads neither 16-bit files nor PSB. ImageMagick's own merged image
        # (frame 0) loses colour under low alpha at 16 bits; its layer (frame 1) holds the
        # same data ImageMagick wrote into the merged section, so that is the reference.
        data = subprocess.run(["convert", path + "[1]", "-alpha", "on", "-depth", str(bits), "-endian", "MSB", "rgba:-"],
                              check=True, capture_output=True).stdout
    open(base + ".rgba", "wb").write(data)
    open(base + ".txt", "w").write(f"{w} {h} {bits}\n")
    print(name, bits, len(data))


with tempfile.TemporaryDirectory() as t:
    rgba, rgb, gray = (os.path.join(t, n) for n in ("rgba.png", "rgb.png", "gray.png"))
    src.save(rgba)
    src.convert("RGB").save(rgb)
    src.convert("L").save(gray)
    for comp in ("None", "RLE"):
        for name, f in (("rgb", rgb), ("rgba", rgba), ("gray", gray)):
            n = f"{name}_{comp.lower()}.psd"
            run("convert", f, "-compress", comp, os.path.join(out, n))
            record(n)
    run("convert", rgb, "-colorspace", "CMYK", "-compress", "RLE", os.path.join(out, "cmyk.psd"))
    record("cmyk.psd")
    run("convert", rgb, "-colors", "16", "-type", "palette", "-compress", "RLE", os.path.join(out, "indexed.psd"))
    record("indexed.psd")
    run("convert", rgb, "-depth", "16", "-compress", "RLE", os.path.join(out, "rgb16.psd"))
    record("rgb16.psd", 16)
    run("convert", rgba, "-depth", "16", "-compress", "None", os.path.join(out, "rgba16.psd"))
    record("rgba16.psd", 16)
    run("convert", rgb, "-compress", "RLE", os.path.join(out, "big.psb"))
    record("big.psb")
