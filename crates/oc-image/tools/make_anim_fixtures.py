"""Animated GIF, APNG and WebP fixtures with the expected composited frames.

Pillow writes the files and decodes them again; the decoded canvas of every frame goes next
to each file as `<name>.rgba` (frames back to back) and `<name>.txt` ("w h count loops" and then
one delay in ms per line). Needs Pillow built with WebP support. Dev-time only.
"""
import os
import sys
from PIL import Image, ImageDraw

out = os.path.join(os.path.dirname(__file__), "..", "tests", "data", "anim")
os.makedirs(out, exist_ok=True)
W, H = 32, 24


def scene(i, n, bg):
    im = Image.new("RGBA", (W, H), bg)
    d = ImageDraw.Draw(im)
    x = 2 + i * (W - 14) // max(1, n - 1)
    d.rectangle([x, 4, x + 9, 13], fill=(200, 30 + 40 * i, 60, 255))
    d.ellipse([W - 12, H - 10 + i % 3, W - 3, H - 2], fill=(20, 180, 220 - 30 * i, 255))
    return im


def write(name, frames, fmt, delays, loop, skip_first=False, **kw):
    path = os.path.join(out, name)
    if loop is not None:
        kw["loop"] = loop
    frames[0].save(path, save_all=True, append_images=frames[1:], duration=delays, **kw)
    im = Image.open(path)
    n = im.n_frames
    raw = b""
    got = []
    for k in range(1 if skip_first else 0, n):
        im.seek(k)
        raw += im.convert("RGBA").tobytes()
        got.append(im.info.get("duration", 0))
    base = os.path.splitext(path)[0]
    with open(base + ".rgba", "wb") as f:
        f.write(raw)
    with open(base + ".txt", "w") as f:
        f.write(f"{W} {H} {len(got)} {im.info.get('loop', 1 if fmt == 'gif' else 0)}\n")
        for g in got:
            f.write(f"{g}\n")
    print(name, n, got[:3], len(raw))


n = 5
opaque = [scene(i, n, (255, 255, 255, 255)) for i in range(n)]
clear = [scene(i, n, (0, 0, 0, 0)) for i in range(n)]
delays = [40, 80, 120, 60, 100]
for disp in (0, 1, 2, 3):
    # Pillow draws "restore previous" on an opaque file onto the background colour, which
    # browsers do not; the transparent variants cover disposal 3.
    if disp != 3:
        write(f"gif_opaque_d{disp}.gif", [f.convert("RGB") for f in opaque], "gif", delays, 0, disposal=disp)
    write(f"gif_clear_d{disp}.gif", clear, "gif", delays, 3, disposal=disp)
write("gif_once.gif", [f.convert("RGB") for f in opaque], "gif", delays, None)
for disp in (0, 1, 2):
    for blend in (0, 1):
        write(f"apng_clear_d{disp}_b{blend}.png", clear, "png", delays, 2, disposal=disp, blend=blend)
write("apng_opaque.png", [f.convert("RGB") for f in opaque], "png", delays, 0)
# Skip the first frame when it equals the default image.
# Pillow counts the hidden default image as a frame; the animation proper starts after it.
write("apng_default_hidden.png", clear, "png", delays, 0, skip_first=True, default_image=True)
write("webp_lossless.webp", clear, "webp", delays, 0, lossless=True)
write("webp_lossless_opaque.webp", opaque, "webp", delays, 4, lossless=True)
write("webp_lossy.webp", opaque, "webp", delays, 0, quality=80)
write("webp_mixed.webp", clear, "webp", delays, 0, lossless=True, minimize_size=True, allow_mixed=True)
