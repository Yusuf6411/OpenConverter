"""ICNS fixtures: files with the old RLE RGB entries (built here) and a PNG-entry file
written by Pillow, each with the RGBA that Pillow reads back at the largest size.

Dev-time only; Pillow is the independent reader the expected pixels come from.
"""
import os
import struct
from PIL import Image

out = os.path.join(os.path.dirname(__file__), "..", "tests", "data", "icns")
os.makedirs(out, exist_ok=True)


def picture(n):
    im = Image.new("RGBA", (n, n))
    px = im.load()
    for y in range(n):
        for x in range(n):
            px[x, y] = ((x * 255 // n), (y * 255 // n), 90 if (x // 4 + y // 4) % 2 else 200,
                        0 if x < n // 8 else (255 if y < n * 3 // 4 else 128))
    return im


def rle(plane):
    """ICNS run-length coding: runs of 3-130 equal bytes, literals of 1-128."""
    o = bytearray()
    i = 0
    while i < len(plane):
        run = 1
        while i + run < len(plane) and plane[i + run] == plane[i] and run < 130:
            run += 1
        if run >= 3:
            o += bytes([run + 125, plane[i]])
            i += run
        else:
            j = i
            lit = bytearray()
            while j < len(plane) and len(lit) < 128:
                r = 1
                while j + r < len(plane) and plane[j + r] == plane[j] and r < 3:
                    r += 1
                if r >= 3:
                    break
                lit.append(plane[j])
                j += 1
            o += bytes([len(lit) - 1]) + lit
            i = j
    return bytes(o)


def element(kind, body):
    return kind + struct.pack(">I", len(body) + 8) + body


def legacy(n, kind, mask_kind):
    im = picture(n)
    data = im.tobytes()
    planes = [bytes(data[c::4]) for c in range(3)]
    mask = bytes(data[3::4])
    body = b"".join(rle(p) for p in planes)
    if n == 128:
        body = b"\0\0\0\0" + body
    return element(kind, body) + element(mask_kind, mask)


def write(name, elements):
    body = b"".join(elements)
    data = b"icns" + struct.pack(">I", len(body) + 8) + body
    path = os.path.join(out, name)
    open(path, "wb").write(data)
    im = Image.open(path)
    big = max(im.info["sizes"], key=lambda s: s[0] * s[2])
    im.size = big[:2]
    rgba = im.convert("RGBA")
    open(path[:-5] + ".rgba", "wb").write(rgba.tobytes())
    open(path[:-5] + ".txt", "w").write(f"{rgba.width} {rgba.height}\n")
    print(name, rgba.size, len(data))


write("legacy16.icns", [legacy(16, b"is32", b"s8mk")])
write("legacy32.icns", [legacy(32, b"il32", b"l8mk")])
write("legacy48.icns", [legacy(48, b"ih32", b"h8mk")])
write("legacy128.icns", [legacy(128, b"it32", b"t8mk")])
write("legacy_all.icns", [legacy(16, b"is32", b"s8mk"), legacy(32, b"il32", b"l8mk"),
                         legacy(48, b"ih32", b"h8mk"), legacy(128, b"it32", b"t8mk")])
# PNG entries, including a Retina one (ic12 is 32 points at 2x = 64 pixels).
import io


def png_entry(kind, n):
    buf = io.BytesIO()
    picture(n).save(buf, "PNG")
    return element(kind, buf.getvalue())


write("png_entries.icns", [png_entry(b"icp4", 16), png_entry(b"icp5", 32), png_entry(b"ic07", 128)])
write("png_retina.icns", [png_entry(b"icp5", 32), png_entry(b"ic12", 64)])
