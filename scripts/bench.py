"""Performance measurements for docs/FORMATS.md (spec §9).

Usage:  python scripts/bench.py [--quick] [--json out.json]

Images are photo-like (smooth waves plus +-4 noise, seeded) so results are
reproducible. Each operation is timed once with time.perf_counter() on the
current machine; the table printed at the end is what docs/FORMATS.md shows.
"""

import argparse
import json
import math
import os
import platform
import random
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from openconverter.codecs import bmp, jpeg, netpbm, png, qoi, tga  # noqa: E402
from openconverter.core.registry import REGISTRY  # noqa: E402
from openconverter.models.image import Image  # noqa: E402


def photo(w, h, seed=7):
    cache = Path(tempfile.gettempdir()) / f"openconverter-bench-{w}x{h}-{seed}.rgb"
    if cache.is_file() and cache.stat().st_size == w * h * 3:
        return Image(w, h, "RGB", 8, bytearray(cache.read_bytes()))
    rng = random.Random(seed)
    waves = [(rng.uniform(0.5, 3.0), rng.uniform(0.5, 3.0), rng.uniform(0, 6.28)) for _ in range(4)]
    px = bytearray()
    for y in range(h):
        v = y / h
        for x in range(w):
            u = x / w
            base = sum(math.sin(6.28 * (fx * u + fy * v) + ph) for fx, fy, ph in waves)
            r = 128 + 25 * base + rng.uniform(-4, 4)
            g = 128 + 45 * math.sin(6.28 * u * 1.3 + base * 0.5) + rng.uniform(-4, 4)
            b = 128 + 45 * math.cos(6.28 * v * 1.7 - base * 0.4) + rng.uniform(-4, 4)
            px += bytes((max(0, min(255, int(r))), max(0, min(255, int(g))), max(0, min(255, int(b)))))
    cache.write_bytes(px)
    return Image(w, h, "RGB", 8, px)


def timed(fn):
    t = time.perf_counter()
    out = fn()
    return time.perf_counter() - t, out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="1024x768 only")
    ap.add_argument("--json", help="write results to this file")
    args = ap.parse_args()
    sizes = [(1024, 768)] if args.quick else [(1024, 768), (1920, 1080)]
    rows = []
    machine = {"python": platform.python_version(), "platform": platform.platform(),
               "processor": platform.processor() or platform.machine(), "cpus": os.cpu_count()}
    print(json.dumps(machine))
    for w, h in sizes:
        label = f"{w}x{h}"
        img = photo(w, h)
        t, png_data = timed(lambda: png.encode(img, compression_level=6))
        rows.append(("PNG write (level 6)", label, t, len(png_data)))
        print(rows[-1], flush=True)
        t, _ = timed(lambda: png.decode(png_data))
        rows.append(("PNG read", label, t, len(png_data)))
        print(rows[-1], flush=True)
        t, jpg = timed(lambda: jpeg.encode(img, quality=85))
        rows.append(("JPEG encode (q85, 4:2:0)", label, t, len(jpg)))
        print(rows[-1], flush=True)
        t, _ = timed(lambda: jpeg.decode(jpg))
        rows.append(("JPEG decode", label, t, len(jpg)))
        print(rows[-1], flush=True)
        t, res = timed(lambda: REGISTRY.convert(png_data, "png", "jpeg", REGISTRY.defaults("png", "jpeg")))
        rows.append(("PNG -> JPG (full conversion)", label, t, len(res.data)))
        print(rows[-1], flush=True)
        t, res = timed(lambda: REGISTRY.convert(jpg, "jpeg", "png", REGISTRY.defaults("jpeg", "png")))
        rows.append(("JPG -> PNG (full conversion)", label, t, len(res.data)))
        print(rows[-1], flush=True)
        if (w, h) == (1920, 1080) or args.quick:
            for name, enc, dec in (("PPM", netpbm.encode_ppm, netpbm.decode), ("BMP", bmp.encode, bmp.decode),
                                   ("TGA", tga.encode, tga.decode), ("QOI", qoi.encode, qoi.decode)):
                t1, data = timed(lambda: enc(img))
                t2, _ = timed(lambda: dec(data))
                rows.append((f"{name} write + read", label, t1 + t2, len(data)))
                print(rows[-1], flush=True)
    print()
    print(f"Machine: {machine['processor']}, {machine['cpus']} logical CPUs, {machine['platform']}, "
          f"Python {machine['python']}")
    print()
    print("| Operation | Image | Time | Output size |")
    print("|---|---|---|---|")
    for op, label, t, size in rows:
        print(f"| {op} | {label} | {t:.1f} s | {size:,} B |")
    if args.json:
        Path(args.json).write_text(json.dumps({"machine": machine, "rows": rows}, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
