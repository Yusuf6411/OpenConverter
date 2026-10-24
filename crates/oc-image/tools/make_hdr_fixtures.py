"""Radiance HDR fixtures for tests/hdr.rs.

Pixel bytes are written here with Bruce Walter's float2rgbe and the several scanline
codings (flat, old run marker, new RLE), in all eight orientations plus XYZE; the
expected output is computed from the RGBE bytes in float64 (clip, sRGB). A few files
are written by ImageMagick (needs `convert`); for those the expected output is
computed from the file's own bytes in the same way.

Run: python3 make_hdr_fixtures.py ../tests/data/hdr
"""
import math
import os
import random
import subprocess
import sys

out = sys.argv[1]
os.makedirs(out, exist_ok=True)


def float2rgbe(r, g, b):
    v = max(r, g, b)
    if v < 1e-32:
        return (0, 0, 0, 0)
    f, e = math.frexp(v)
    s = f * 256.0 / v
    return (int(r * s), int(g * s), int(b * s), e + 128)


def rgbe2lin(q):
    if q[3] == 0:
        return (0.0, 0.0, 0.0)
    f = math.ldexp(1.0, q[3] - 136)
    return (q[0] * f, q[1] * f, q[2] * f)


def xyz2lin(c):
    x, y, z = c
    return (
        3.240970 * x - 1.537383 * y - 0.498611 * z,
        -0.969244 * x + 1.875968 * y + 0.041555 * z,
        0.055630 * x - 0.203977 * y + 1.056972 * z,
    )


def srgb(v):
    v = min(max(v, 0.0), 1.0)
    s = 12.92 * v if v <= 0.0031308 else 1.055 * v ** (1 / 2.4) - 0.055
    return int(s * 255 + 0.5)


def expected(w, h, pixels, xyz=False):
    """pixels[y][x] is an RGBE quadruple in display orientation."""
    res = bytearray()
    for row in pixels:
        for q in row:
            c = rgbe2lin(q)
            if xyz:
                c = xyz2lin(c)
            res += bytes(srgb(v) for v in c)
    return bytes(res)


def rle_plane(plane):
    o = bytearray()
    i = 0
    n = len(plane)
    while i < n:
        run = 1
        while i + run < n and plane[i + run] == plane[i] and run < 127:
            run += 1
        if run >= 3:
            o += bytes([128 + run, plane[i]])
            i += run
        else:
            j = i
            while j < n and j - i < 128:
                if j + 2 < n and plane[j] == plane[j + 1] == plane[j + 2]:
                    break
                j += 1
            if j == i:
                j = i + 1
            o += bytes([j - i]) + bytes(plane[i:j])
            i = j
    return bytes(o)


def scan_bytes(scan, mode):
    n = len(scan)
    if mode == "flat":
        return b"".join(bytes(q) for q in scan)
    if mode == "old":
        o = bytearray()
        i = 0
        while i < n:
            run = 1
            while i + run < n and scan[i + run] == scan[i]:
                run += 1
            o += bytes(scan[i])
            rest = run - 1
            if rest:
                # Markers (1, 1, 1, c): the first count is c, the next c << 8, and so on.
                digits = []
                while rest:
                    digits.append(rest & 255)
                    rest >>= 8
                for d in digits:
                    o += bytes([1, 1, 1, d])
            i += run
        return bytes(o)
    assert mode == "rle"
    o = bytearray([2, 2, n >> 8, n & 255])
    for c in range(4):
        o += rle_plane([q[c] for q in scan])
    return bytes(o)


def write(name, w, h, pixels, mode="rle", res=None, xyz=False, extra=b""):
    """pixels[y][x]; res is (axis1, axis2) strings for the resolution line."""
    res = res or ("-Y", "+X")
    fmt = b"32-bit_rle_xyze" if xyz else b"32-bit_rle_rgbe"
    head = b"#?RADIANCE\n# made by make_hdr_fixtures\n" + extra + b"FORMAT=" + fmt + b"\n\n"
    a1, a2 = res
    rows = a1[1] == "Y"
    fwd1 = a1[0] == "-" if rows else a1[0] == "+"
    fwd2 = a2[0] == "+" if rows else a2[0] == "-"
    if rows:
        head += f"{a1} {h} {a2} {w}\n".encode()
        order = range(h) if fwd1 else range(h - 1, -1, -1)
        scans = [list(pixels[y]) if fwd2 else list(reversed(pixels[y])) for y in order]
    else:
        head += f"{a1} {w} {a2} {h}\n".encode()
        order = range(w) if fwd1 else range(w - 1, -1, -1)
        scans = []
        for x in order:
            col = [pixels[y][x] for y in range(h)]
            scans.append(col if fwd2 else list(reversed(col)))
    body = b"".join(scan_bytes(s, mode) for s in scans)
    open(f"{out}/{name}.hdr", "wb").write(head + body)
    open(f"{out}/{name}.rgb", "wb").write(expected(w, h, pixels, xyz))
    open(f"{out}/{name}.txt", "w").write(f"{w} {h}\n")


random.seed(11)


def scene(w, h, hot=4.0):
    px = []
    for y in range(h):
        row = []
        for x in range(w):
            r = (x + 1) / w * hot
            g = (y + 1) / h
            b = ((x * 7 + y * 3) % 13) / 13.0 * 0.5
            if (x + y) % 11 == 0:
                r = g = b = 0.0
            row.append(float2rgbe(r, g, b))
        px.append(row)
    return px


def flat_runs(w, h):
    px = []
    for y in range(h):
        row = []
        x = 0
        while x < w:
            n = random.choice([1, 1, 2, 5, 9, 20, 130, 300])
            q = float2rgbe(random.random(), random.random() * 2, random.random() * 0.1)
            row += [q] * min(n, w - x)
            x += n
        px.append(row)
    return px


for mode in ("rle", "flat"):
    write(f"scene_{mode}", 40, 13, scene(40, 13), mode)
write("runs_rle", 700, 6, flat_runs(700, 6), "rle")
write("runs_flat", 300, 4, flat_runs(300, 4), "flat")
write("runs_old", 700, 6, flat_runs(700, 6), "old")
write("runs_old_small", 40, 5, flat_runs(40, 5), "old")
write("tiny", 1, 1, [[float2rgbe(0.5, 0.25, 2.0)]], "flat")
narrow = scene(7, 5)
narrow[0][0] = (2, 2, 0, 7)  # looks like the start of a new-style scanline, but 7 pixels are too few
write("narrow_flat", 7, 5, narrow, "flat")
write("wide_flat", 32768, 1, flat_runs(32768, 1)[:1], "flat")  # too wide for the new coding
for res in [("-Y", "+X"), ("-Y", "-X"), ("+Y", "+X"), ("+Y", "-X"),
            ("+X", "-Y"), ("+X", "+Y"), ("-X", "-Y"), ("-X", "+Y")]:
    n = "".join(c if c.isalpha() else ("p" if c == "+" else "m") for c in "".join(res))
    write(f"orient_{n}", 21, 10, scene(21, 10), "rle" if res[0][1] == "Y" else "flat", res)
write("exposure", 16, 8, scene(16, 8), extra=b"EXPOSURE=2.0\nSOFTWARE=test\n")
xyz_px = [[float2rgbe(0.3 + x * 0.01, 0.2 + y * 0.02, 0.1) for x in range(20)] for y in range(9)]
write("xyze", 20, 9, xyz_px, "rle", xyz=True)

# ImageMagick's own files (it writes RGBE of the pixel values as they are).
from PIL import Image
im = Image.new("RGB", (37, 11))
im.putdata([((x * 7) % 256, (y * 23) % 256, (x * y) % 256) for y in range(11) for x in range(37)])
im.save("/tmp/_hdr_src.png")
for name, size in (("im_small", None),):
    subprocess.run(["convert", "/tmp/_hdr_src.png", f"{out}/{name}.hdr"], check=True)
    data = open(f"{out}/{name}.hdr", "rb").read()
    # The writer's output is new-style RLE; decode it with the reference reader.
    i = data.index(b"\n\n") + 2
    j = data.index(b"\n", i) + 1
    toks = data[i:j].split()
    h, w = int(toks[1]), int(toks[3])
    pos = j
    rows = []
    for _ in range(h):
        assert data[pos:pos + 2] == b"\x02\x02"
        pos += 4
        planes = []
        for _c in range(4):
            row = []
            while len(row) < w:
                n = data[pos]
                pos += 1
                if n > 128:
                    row += [data[pos]] * (n - 128)
                    pos += 1
                else:
                    row += list(data[pos:pos + n])
                    pos += n
            planes.append(row)
        rows.append([tuple(planes[c][x] for c in range(4)) for x in range(w)])
    open(f"{out}/{name}.rgb", "wb").write(expected(w, h, rows))
    open(f"{out}/{name}.txt", "w").write(f"{w} {h}\n")
