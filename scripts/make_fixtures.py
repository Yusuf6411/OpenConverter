"""Regenerate the deterministic test fixtures in tests/fixtures/ (no downloads).

Usage:  python scripts/make_fixtures.py
Images are generated with our own encoders from seeded generators, so the
output is byte-identical on every run.
"""

import json
import math
import random
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from openconverter.codecs import bmp, jpeg, netpbm, png, qoi, tga  # noqa: E402
from openconverter.models.image import Image  # noqa: E402
from tests.gif_builder import build_gif  # noqa: E402

OUT = ROOT / "tests" / "fixtures"


def gradient(w, h, mode="RGB"):
    px = bytearray()
    for y in range(h):
        for x in range(w):
            r = x * 255 // max(1, w - 1)
            g = y * 255 // max(1, h - 1)
            b = (x + y) * 255 // max(1, w + h - 2)
            a = 255 - (x * 255 // max(1, w - 1)) // 2
            px += {"L": bytes([g]), "LA": bytes([g, a]), "RGB": bytes([r, g, b]), "RGBA": bytes([r, g, b, a])}[mode]
    return Image(w, h, mode, 8, px)


def photo(w, h, seed=7):
    rng = random.Random(seed)
    waves = [(rng.uniform(0.5, 3.0), rng.uniform(0.5, 3.0), rng.uniform(0, 6.28)) for _ in range(4)]
    px = bytearray()
    for y in range(h):
        for x in range(w):
            u, v = x / w, y / h
            base = sum(math.sin(6.28 * (fx * u + fy * v) + ph) for fx, fy, ph in waves)
            r = 128 + 25 * base + rng.uniform(-4, 4)
            g = 128 + 45 * math.sin(6.28 * u * 1.3 + base * 0.5) + rng.uniform(-4, 4)
            b = 128 + 45 * math.cos(6.28 * v * 1.7 - base * 0.4) + rng.uniform(-4, 4)
            px += bytes(max(0, min(255, int(c))) for c in (r, g, b))
    return Image(w, h, "RGB", 8, px)


def rgb16(w, h):
    px = bytearray()
    for y in range(h):
        for x in range(w):
            for v in (x * 65535 // (w - 1), y * 65535 // (h - 1), (x * y * 37) % 65536):
                px += v.to_bytes(2, "big")
    return Image(w, h, "RGB", 16, px)


def write(rel, data):
    path = OUT / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return path


def main():
    g = gradient(33, 17)
    ga = gradient(33, 17, "RGBA")
    grey = gradient(33, 17, "L")
    p = photo(64, 48)
    files = {
        "images/gradient.png": png.encode(g),
        "images/alpha.png": png.encode(ga),
        "images/rgb16.png": png.encode(rgb16(17, 9)),
        "images/photo.png": png.encode(p),
        "images/photo.jpg": jpeg.encode(p, quality=90),
        "images/gradient.bmp": bmp.encode(g),
        "images/alpha.bmp": bmp.encode(ga),
        "images/gradient.tga": tga.encode(g, rle=True),
        "images/alpha.qoi": qoi.encode(ga),
        "images/gradient.ppm": netpbm.encode_ppm(g),
        "images/grey.pgm": netpbm.encode_pgm(grey),
        "images/rgb16.ppm": netpbm.encode_ppm(rgb16(17, 9)),
        "images/bits.pbm": netpbm.encode_pbm(Image(17, 5, "L", 8, bytearray(
            255 if (x * 3 + y) % 4 else 0 for y in range(5) for x in range(17)))),
        "images/tiny_1x1.png": png.encode(gradient(1, 1)),
        "images/odd_7x13.png": png.encode(gradient(7, 13, "RGBA")),
        "images/odd_17x1.bmp": bmp.encode(gradient(17, 1)),
        "images/odd_33x17.jpg": jpeg.encode(gradient(33, 17), quality=95),
    }
    pal = [(i * 16, 255 - i * 16, (i * 64) & 255) for i in range(16)]
    idx = bytes((x // 3 + y) % 16 for y in range(13) for x in range(19))
    files["images/palette.gif"] = build_gif(19, 13, [{"indices": idx, "transparent": 3, "mcs": 4},
                                                     {"indices": bytes(19 * 13), "mcs": 4}], pal)
    files["data/people.csv"] = ('name,city,notes\r\nAda,London,"likes, commas"\r\n'
                                'Grace,"New\nYork","said ""hi"""\r\nLinus,Helsinki,\r\n').encode("utf-8")
    files["data/people.tsv"] = "name\tcity\nAda\tLondon\nGrace\tArlington\n".encode("utf-8")
    files["data/people.json"] = json.dumps([{"name": "Ada", "age": 36, "active": True},
                                            {"name": "Grace", "age": None}], indent=2).encode("utf-8")
    files["text/hello_utf8.txt"] = "Hello, wörld – ½ €\nSecond line\n".encode("utf-8")
    files["text/hello_utf16le_bom.txt"] = b"\xff\xfe" + "Hello\r\nUTF-16\r\n".encode("utf-16-le")
    files["text/cp1252.txt"] = "Café – naïve\r\n".encode("cp1252")
    total = 0
    for rel, data in files.items():
        write(rel, data)
        total += len(data)
    print(f"wrote {len(files)} fixtures, {total} bytes")


if __name__ == "__main__":
    main()
