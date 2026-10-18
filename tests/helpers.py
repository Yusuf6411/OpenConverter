"""Deterministic test images and small utilities shared by the test suite."""

import math
import random
import shutil
import tempfile
import threading
from pathlib import Path

from openconverter.models.image import CHANNELS, Image

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "fixtures"


def gradient(w, h, mode="RGB", depth=8) -> Image:
    n = CHANNELS[mode]
    top = 255 if depth == 8 else 65535
    vals = []
    for y in range(h):
        for x in range(w):
            r = (x * top) // max(1, w - 1)
            g = (y * top) // max(1, h - 1)
            b = ((x + y) * top) // max(1, w + h - 2)
            a = ((x * 7 + y * 3) * top // max(1, 7 * w + 3 * h)) if n in (2, 4) else top
            if mode == "L":
                vals.append(r)
            elif mode == "LA":
                vals += [g, a]
            elif mode == "RGB":
                vals += [r, g, b]
            else:
                vals += [r, g, b, a]
    return make(w, h, mode, depth, vals)


def noise(w, h, mode="RGB", depth=8, seed=1234) -> Image:
    rng = random.Random(seed)
    top = 255 if depth == 8 else 65535
    count = w * h * CHANNELS[mode]
    return make(w, h, mode, depth, [rng.randint(0, top) for _ in range(count)])


def photo(w, h, seed=7, mode="RGB") -> Image:
    """Smooth photo-like content: sums of low-frequency waves plus mild noise."""
    rng = random.Random(seed)
    waves = [(rng.uniform(0.5, 3.0), rng.uniform(0.5, 3.0), rng.uniform(0, 6.28)) for _ in range(4)]
    vals = []
    for y in range(h):
        for x in range(w):
            u, v = x / max(1, w), y / max(1, h)
            base = sum(math.sin(6.28 * (fx * u + fy * v) + ph) for fx, fy, ph in waves)
            r = 128 + 50 * base / 2 + rng.uniform(-4, 4)
            g = 128 + 45 * math.sin(6.28 * u * 1.3 + base * 0.5) + rng.uniform(-4, 4)
            b = 128 + 45 * math.cos(6.28 * v * 1.7 - base * 0.4) + rng.uniform(-4, 4)
            px = [max(0, min(255, int(c))) for c in (r, g, b)]
            if mode == "L":
                vals.append((299 * px[0] + 587 * px[1] + 114 * px[2] + 500) // 1000)
            else:
                vals += px
    return make(w, h, mode, 8, vals)


def checkerboard(w, h, cell=4, mode="RGB") -> Image:
    vals = []
    for y in range(h):
        for x in range(w):
            on = ((x // cell) + (y // cell)) % 2
            if mode == "L":
                vals.append(255 if on else 0)
            elif mode == "RGB":
                vals += [255, 0, 0] if on else [0, 0, 255]
            elif mode == "RGBA":
                vals += [255, 0, 0, 255] if on else [0, 0, 255, 0]
            else:
                vals += [255 if on else 0, 255]
    return make(w, h, mode, 8, vals)


def make(w, h, mode, depth, vals) -> Image:
    if depth == 8:
        data = bytearray(vals)
    else:
        data = bytearray()
        for v in vals:
            data += v.to_bytes(2, "big")
    return Image(w, h, mode, depth, data)


def psnr(a: Image, b: Image) -> float:
    assert (a.width, a.height, a.mode) == (b.width, b.height, b.mode)
    se = sum((x - y) ** 2 for x, y in zip(a.pixels, b.pixels))
    if se == 0:
        return float("inf")
    mse = se / len(a.pixels)
    return 10 * math.log10(255 * 255 / mse)


class TempDir:
    """Context-managed per-test temporary directory."""

    def __init__(self):
        self.path = Path(tempfile.mkdtemp(prefix="oc-test-"))

    def __enter__(self):
        return self.path

    def __exit__(self, *exc):
        shutil.rmtree(self.path, ignore_errors=True)


def run_with_timeout(fn, timeout: float):
    """Run ``fn`` in a daemon thread. Returns (finished, result, exception)."""
    box = {}

    def target():
        try:
            box["result"] = fn()
        except BaseException as exc:  # noqa: BLE001 - reported to the caller
            box["exc"] = exc

    t = threading.Thread(target=target, daemon=True)
    t.start()
    t.join(timeout)
    return (not t.is_alive()), box.get("result"), box.get("exc")
