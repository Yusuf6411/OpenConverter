#!/usr/bin/env python3
"""Interop soak: big and awkward inputs through our codecs and CPython's/zstandard's.
Usage: interop.py <codec> [files...]   (builds examples/codec_tool first)"""
import subprocess, sys, os, tempfile, bz2, lzma, random
import zstandard

root = os.path.join(os.path.dirname(__file__), "..", "..", "..")
subprocess.check_call(["cargo", "build", "-q", "--release", "-p", "oc-compress", "--example", "codec_tool"], cwd=root)
tool = os.path.join(root, "target", "release", "examples", "codec_tool")
codec = sys.argv[1]
files = sys.argv[2:] or [sys.executable, "/usr/bin/git", os.path.join(root, "target", "release", "openconverter")]
r = random.Random(1)
blobs = [open(f, "rb").read() for f in files if os.path.exists(f)]
blobs.append(bytes(r.randrange(256) for _ in range(300000)))
blobs.append(b"".join(r.choice([b"abc", b"abd", b"xyz\n", b"\x00" * r.randrange(1, 300)]) for _ in range(100000)))
enc = {"bzip2": lambda d: bz2.compress(d, 9), "xz": lambda d: lzma.compress(d, preset=6),
       "zstd": lambda d: zstandard.ZstdCompressor(level=r.choice([1, 3, 10, 19]), write_checksum=True).compress(d)}[codec]
for i, d in enumerate(blobs):
    with tempfile.TemporaryDirectory() as t:
        src, dst = os.path.join(t, "s"), os.path.join(t, "d")
        open(src, "wb").write(enc(d))
        subprocess.check_call([tool, codec, "d", "0", src, dst])
        assert open(dst, "rb").read() == d, f"{codec} decode mismatch on blob {i}"
print(codec, "decode ok on", len(blobs), "blobs")

# Encoder direction: our output must be readable by the reference implementation.
dec = {"bzip2": bz2.decompress, "xz": lzma.decompress,
       "zstd": lambda b: zstandard.ZstdDecompressor().decompressobj().decompress(b)}[codec]
for level in {"bzip2": [1, 9], "xz": [0, 6, 9], "zstd": [1, 3, 9, 19]}[codec]:
    for i, d in enumerate(blobs):
        with tempfile.TemporaryDirectory() as t:
            src, dst = os.path.join(t, "s"), os.path.join(t, "d")
            open(src, "wb").write(d)
            subprocess.check_call([tool, codec, "c", str(level), src, dst])
            comp = open(dst, "rb").read()
            assert dec(comp) == d, f"{codec} level {level}: reference could not read our output for blob {i}"
            print(f"{codec} level {level} blob {i}: {len(d)} -> {len(comp)}")
print(codec, "encode ok")
