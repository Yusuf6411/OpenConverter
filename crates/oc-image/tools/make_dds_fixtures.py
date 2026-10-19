"""DDS fixtures written by Pillow and ImageMagick, with the RGBA Pillow decodes from each.

Dev-time only. `<name>.rgba` is the decoded image, `<name>.txt` holds "width height".
"""
import os
import subprocess
import tempfile
from PIL import Image

out = os.path.join(os.path.dirname(__file__), "..", "tests", "data", "dds")
os.makedirs(out, exist_ok=True)


def picture(w, h, alpha):
    im = Image.new("RGBA", (w, h))
    px = im.load()
    for y in range(h):
        for x in range(w):
            a = 255 if not alpha else (0 if x < w // 6 else min(255, 40 + y * 255 // max(1, h - 1)))
            px[x, y] = (x * 255 // max(1, w - 1), y * 255 // max(1, h - 1), (x * 7 + y * 13) % 256, a)
    return im


def record(name):
    path = os.path.join(out, name)
    im = Image.open(path)
    rgba = im.convert("RGBA")
    open(path.rsplit(".", 1)[0] + ".rgba", "wb").write(rgba.tobytes())
    open(path.rsplit(".", 1)[0] + ".txt", "w").write(f"{rgba.width} {rgba.height}\n")
    print(name, im.mode, rgba.size, os.path.getsize(path))


for (w, h) in ((16, 12), (13, 9)):
    sfx = f"{w}x{h}"
    rgba = picture(w, h, True)
    rgb = picture(w, h, False).convert("RGB")
    rgba.save(os.path.join(out, f"rgba8_{sfx}.dds"))
    record(f"rgba8_{sfx}.dds")
    rgb.save(os.path.join(out, f"rgb8_{sfx}.dds"))
    record(f"rgb8_{sfx}.dds")
    rgb.convert("L").save(os.path.join(out, f"l8_{sfx}.dds"))
    record(f"l8_{sfx}.dds")
    rgba.convert("LA").save(os.path.join(out, f"la8_{sfx}.dds"))
    record(f"la8_{sfx}.dds")
    for fmt in ("DXT1", "DXT3", "DXT5", "BC2", "BC3"):
        rgba.save(os.path.join(out, f"{fmt.lower()}_{sfx}.dds"), pixel_format=fmt)
        record(f"{fmt.lower()}_{sfx}.dds")
    rgb.save(os.path.join(out, f"bc5_{sfx}.dds"), pixel_format="BC5")
    record(f"bc5_{sfx}.dds")
    # ImageMagick's squish-based compressor, for blocks Pillow's encoder does not produce.
    with tempfile.TemporaryDirectory() as t:
        src = os.path.join(t, "s.png")
        rgba.save(src)
        for fmt in ("dxt1", "dxt5"):
            dst = os.path.join(out, f"im_{fmt}_{sfx}.dds")
            subprocess.run(["convert", src, "-define", f"dds:compression={fmt}", dst], check=True)
            record(f"im_{fmt}_{sfx}.dds")
