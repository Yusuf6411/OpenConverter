#!/usr/bin/env python3
"""Compressed fixtures for the decoder tests, made by CPython's bz2/lzma and the
zstandard, lz4 and brotli modules (dev-time only). Usage: make_fixtures.py [codec...]"""
import bz2, lzma, os, sys, random, zlib

root = os.path.join(os.path.dirname(__file__), "..", "tests", "data")

def samples():
    r = random.Random(7)
    text = (b"The quick brown fox jumps over the lazy dog. " * 50 + b"Pack my box with five dozen liquor jugs.\n" * 30)
    words = [bytes(r.choice(b"abcdefghij") for _ in range(r.randint(2, 8))) for _ in range(200)]
    prose = b" ".join(r.choice(words) for _ in range(6000))
    noise = bytes(r.randrange(256) for _ in range(20000))
    runs = b"".join(bytes([r.randrange(256)]) * r.choice([1, 1, 3, 4, 5, 255, 256, 600]) for _ in range(300))
    sparse = bytes(70000) + b"x" + bytes(70000)
    tabular = b"".join(b"%d,%s,%d\n" % (i, r.choice(words), i * i) for i in range(3000))
    return {"empty": b"", "one": b"a", "text": text, "prose": prose, "noise": noise,
            "runs": runs, "sparse": sparse, "tabular": tabular}

def put(codec, name, raw, comp, ext):
    d = os.path.join(root, codec)
    os.makedirs(d, exist_ok=True)
    open(os.path.join(d, f"{name}.{ext}"), "wb").write(comp)
    if not os.path.exists(os.path.join(d, f"{name}.raw")):
        open(os.path.join(d, f"{name}.raw"), "wb").write(raw)

want = set(sys.argv[1:]) or {"bzip2", "xz", "lzma", "zstd", "lz4", "brotli"}
for name, raw in samples().items():
    if "bzip2" in want:
        for lv in (1, 9):
            put("bzip2", f"{name}-{lv}", raw, bz2.compress(raw, lv), "bz2")
    if "xz" in want:
        for preset, check in ((0, lzma.CHECK_CRC32), (6, lzma.CHECK_CRC64), (9 | lzma.PRESET_EXTREME, lzma.CHECK_SHA256), (3, lzma.CHECK_NONE)):
            put("xz", f"{name}-{preset}-{check}", raw, lzma.compress(raw, format=lzma.FORMAT_XZ, check=check, preset=preset), "xz")
        for filt, tag in (([{"id": lzma.FILTER_DELTA, "dist": 4}, {"id": lzma.FILTER_LZMA2, "preset": 4}], "delta"),
                          ([{"id": lzma.FILTER_X86}, {"id": lzma.FILTER_LZMA2, "preset": 4}], "x86")):
            put("xz", f"{name}-{tag}", raw, lzma.compress(raw, format=lzma.FORMAT_XZ, filters=filt), "xz")
    if "lzma" in want:
        for preset in (1, 6):
            put("lzma", f"{name}-{preset}", raw, lzma.compress(raw, format=lzma.FORMAT_ALONE, preset=preset), "lzma")
    if "zstd" in want:
        import zstandard as z
        for lv in (-5, 1, 3, 9, 19):
            put("zstd", f"{name}-{lv}", raw, z.ZstdCompressor(level=lv).compress(raw), "zst")
    if "lz4" in want:
        import lz4.frame, lz4.block
        for lv in (0, 9):
            put("lz4", f"{name}-{lv}", raw, lz4.frame.compress(raw, compression_level=lv), "lz4")
        put("lz4", f"{name}-nocontent", raw, lz4.frame.compress(raw, block_linked=False, content_checksum=True, block_checksum=True), "lz4")
print("fixtures written")
