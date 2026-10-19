"""OpenEXR fixtures for tests/exr.rs, written and read back by the OpenEXR library.

The expected pixels are the library's own decode run through the same display mapping
(clip to [0, 1], un-premultiply alpha, sRGB) in float64.

Run: python3 make_exr_fixtures.py ../tests/data/exr
(needs the OpenEXR Python package at fixture-making time only)
"""
import os
import sys

import numpy as np
import OpenEXR

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
rng = np.random.default_rng(9)

COMP = {
    "none": OpenEXR.NO_COMPRESSION,
    "rle": OpenEXR.RLE_COMPRESSION,
    "zips": OpenEXR.ZIPS_COMPRESSION,
    "zip": OpenEXR.ZIP_COMPRESSION,
    "piz": OpenEXR.PIZ_COMPRESSION,
    "pxr24": OpenEXR.PXR24_COMPRESSION,
    "b44": OpenEXR.B44_COMPRESSION,
    "b44a": OpenEXR.B44A_COMPRESSION,
}


def scene(h, w, kind="mixed"):
    y, x = np.mgrid[0:h, 0:w].astype(np.float64)
    base = np.stack([x / max(w - 1, 1) * 3.0, y / max(h - 1, 1), (np.sin(x / 5.0) + 1) / 2], -1)
    if kind == "noise":
        base = rng.random((h, w, 3)) * 1.5 - 0.1
    elif kind == "flat":
        base = np.full((h, w, 3), 0.25)
    elif kind == "wide":  # values over many orders of magnitude, so the 16-bit wavelet path is used
        base = 10.0 ** (rng.random((h, w, 3)) * 8 - 5)
    base[: h // 4, : w // 3] = 0.0
    return base


def srgb(v):
    v = np.clip(v, 0, 1)
    return np.where(v <= 0.0031308, 12.92 * v, 1.055 * np.power(v, 1 / 2.4) - 0.055)


def reference(file, path):
    f = OpenEXR.File(path)
    ch = f.channels()
    if "RGB" in ch or "RGBA" in ch:
        name = "RGBA" if "RGBA" in ch else "RGB"
        a = ch[name].pixels.astype(np.float64)
    elif "Y" in ch:
        a = ch["Y"].pixels.astype(np.float64)[..., None]
    else:
        parts = [ch[n].pixels.astype(np.float64) for n in "RGB"]
        a = np.stack(parts, -1)
        if "A" in ch:
            a = np.concatenate([a, ch["A"].pixels.astype(np.float64)[..., None]], -1)
    h, w, n = a.shape
    has_alpha = n in (2, 4)
    colour = a[..., : n - 1] if has_alpha else a
    if has_alpha:
        al = a[..., -1:]
        with np.errstate(divide="ignore", invalid="ignore"):
            colour = np.where(al > 0, colour / al, colour)
    px = np.floor(srgb(colour) * 255 + 0.5).astype(np.uint8)
    if has_alpha:
        px = np.concatenate([px, np.floor(np.clip(al, 0, 1) * 255 + 0.5).astype(np.uint8)], -1)
    return w, h, px


def write(name, channels, comp, tiles=None, dtype_note=""):
    header = {"compression": COMP[comp], "type": OpenEXR.scanlineimage}
    if tiles:
        header["type"] = OpenEXR.tiledimage
        td = OpenEXR.TileDescription()
        td.xSize, td.ySize = tiles
        td.mode, td.roundingMode = OpenEXR.ONE_LEVEL, OpenEXR.ROUND_DOWN
        header["tiles"] = td
    path = f"{out}/{name}.exr"
    with OpenEXR.File(header, channels) as f:
        f.write(path)
    w, h, px = reference(None, path)
    open(f"{out}/{name}.px", "wb").write(px.tobytes())
    open(f"{out}/{name}.txt", "w").write(f"{w} {h} {px.shape[2]}\n")


for comp in COMP:
    base = scene(29, 37)
    write(f"rgb_half_{comp}", {"RGB": base.astype(np.float16)}, comp)
    write(f"rgb_float_{comp}", {"RGB": base.astype(np.float32)}, comp)
    rgba = np.concatenate([base, rng.random((29, 37, 1))], -1)
    write(f"rgba_half_{comp}", {"RGBA": rgba.astype(np.float16)}, comp)
    write(f"gray_half_{comp}", {"Y": base[..., 0].astype(np.float16)}, comp)
for comp in ("piz", "zip", "pxr24", "b44", "rle"):
    for (h, w) in ((1, 1), (70, 5), (3, 100), (100, 33), (64, 64), (17, 130)):
        write(f"size_{comp}_{w}x{h}", {"RGB": scene(h, w).astype(np.float16)}, comp)
    write(f"noise_{comp}", {"RGB": scene(40, 50, "noise").astype(np.float16)}, comp)
    write(f"flat_{comp}", {"RGB": scene(33, 47, "flat").astype(np.float16)}, comp)
write("wide_piz", {"RGB": scene(40, 50, "wide").astype(np.float16)}, "piz")
write("wide_zip_float", {"RGB": scene(40, 50, "wide").astype(np.float32)}, "zip")
for comp in ("none", "zip", "piz", "rle", "pxr24", "b44"):
    write(f"tiled_{comp}", {"RGB": scene(45, 70).astype(np.float16)}, comp, tiles=(16, 16))
    write(f"tiled_rect_{comp}", {"RGBA": np.concatenate([scene(45, 70), rng.random((45, 70, 1))], -1).astype(np.float16)}, comp, tiles=(32, 8))
