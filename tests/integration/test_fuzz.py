"""Robustness suite (spec §12): seeded, reproducible malformed inputs for every
decoder. Each case must finish quickly and raise only our typed errors."""

import random
import struct
import unittest

from openconverter.codecs import bmp, deflate, delimited, gif, jpeg, jsontable, netpbm, png, qoi, tga
from openconverter.codecs import text as textcodec
from openconverter.errors import OpenConverterError
from openconverter.limits import Limits
from tests.helpers import FIXTURES, run_with_timeout

LIMITS = Limits(max_pixels=4_000_000, max_side=8000)
CASE_TIMEOUT = 20.0


def _zlib(data):
    return deflate.zlib_decompress(data, 1 << 22)


def _text(data):
    return textcodec.decode_text(data)


def _csv(data):
    text, _ = textcodec.decode_text(data)
    return delimited.parse(text, ",", LIMITS)


def _json(data):
    text, _ = textcodec.decode_text(data)
    return jsontable.read(text, LIMITS)


DECODERS = {
    "png": (lambda d: png.decode(d, limits=LIMITS), ["images/alpha.png", "images/rgb16.png", "images/photo.png"]),
    "jpeg": (lambda d: jpeg.decode(d, limits=LIMITS), ["images/photo.jpg", "jpeg/ours_p_rst.jpg", "jpeg/ours_grey.jpg"]),
    "gif": (lambda d: gif.decode(d, limits=LIMITS), ["images/palette.gif", "gif/interlaced_transparent.gif"]),
    "bmp": (lambda d: bmp.decode(d, limits=LIMITS), ["images/alpha.bmp", "images/gradient.bmp", "images/odd_17x1.bmp"]),
    "tga": (lambda d: tga.decode(d, limits=LIMITS), ["images/gradient.tga"]),
    "qoi": (lambda d: qoi.decode(d, limits=LIMITS), ["images/alpha.qoi"]),
    "netpbm": (lambda d: netpbm.decode(d, limits=LIMITS), ["images/gradient.ppm", "images/grey.pgm",
                                                            "images/bits.pbm", "images/rgb16.ppm"]),
    "zlib": (_zlib, None),
    "text": (_text, ["text/hello_utf8.txt", "text/hello_utf16le_bom.txt"]),
    "csv": (_csv, ["data/people.csv"]),
    "json": (_json, ["data/people.json"]),
}


def zlib_samples():
    rng = random.Random(3)
    raw = bytes(rng.randrange(4) for _ in range(3000)) + b"hello world " * 50
    return [deflate.zlib_compress(raw, 6), deflate.zlib_compress(raw, 0), deflate.zlib_compress(b"abc" * 1000, 9)]


class FuzzTests(unittest.TestCase):
    def check(self, name, fn, data, label):
        finished, _result, exc = run_with_timeout(lambda: fn(data), CASE_TIMEOUT)
        self.assertTrue(finished, f"{name}: {label} hung (> {CASE_TIMEOUT}s)")
        if exc is not None and not isinstance(exc, OpenConverterError):
            raise AssertionError(f"{name}: {label} raised untyped {type(exc).__name__}: {exc}") from exc

    def samples(self, name, files):
        if name == "zlib":
            return zlib_samples()
        return [(FIXTURES / f).read_bytes() for f in files]

    def test_empty_and_one_byte(self):
        for name, (fn, _files) in DECODERS.items():
            for data in (b"", b"\x00", b"\xff", b"P", b"B", b"G", b"q", b"\x89"):
                self.check(name, fn, data, f"input {data!r}")

    def test_truncation(self):
        for name, (fn, files) in DECODERS.items():
            for sample in self.samples(name, files):
                n = len(sample)
                offsets = range(n) if n <= 300 else sorted(set(random.Random(n).sample(range(n), 120)) | {1, 2, 8, 16, n - 1})
                for cut in offsets:
                    self.check(name, fn, sample[:cut], f"truncated at {cut}/{n}")

    def test_byte_flips(self):
        rng = random.Random(1234)
        for name, (fn, files) in DECODERS.items():
            for sample in self.samples(name, files):
                for i in range(150):
                    data = bytearray(sample)
                    for _ in range(rng.randint(1, 4)):
                        data[rng.randrange(len(data))] = rng.randrange(256)
                    self.check(name, fn, bytes(data), f"flip case {i}")

    def test_garbage_with_valid_magic(self):
        rng = random.Random(99)
        magics = {"png": png.SIGNATURE + b"\x00\x00\x00\x0dIHDR", "jpeg": b"\xff\xd8\xff", "gif": b"GIF89a",
                  "bmp": b"BM", "tga": b"\x00\x00\x02", "qoi": b"qoif", "netpbm": b"P6\n", "zlib": b"\x78\x9c",
                  "text": b"\xef\xbb\xbf", "csv": b"a,b\n", "json": b"[{"}
        for name, (fn, _files) in DECODERS.items():
            for i in range(120):
                body = bytes(rng.randrange(256) for _ in range(rng.randint(0, 600)))
                self.check(name, fn, magics[name] + body, f"garbage case {i}")

    def test_absurd_dimensions(self):
        cases = {
            "png": png.SIGNATURE + struct.pack(">I4sIIBBBBB", 13, b"IHDR", 0xFFFFFFF, 0xFFFFFFF, 8, 2, 0, 0, 0) + b"\0" * 4,
            "bmp": b"BM" + struct.pack("<IHHIIiiHH", 0, 0, 0, 54, 40, 2 ** 30, 2 ** 30, 1, 24) + bytes(30),
            "tga": struct.pack("<BBBHHBHHHHBB", 0, 0, 2, 0, 0, 0, 0, 0, 65535, 65535, 24, 0x20) + bytes(30),
            "qoi": b"qoif" + struct.pack(">IIBB", 2 ** 31, 2 ** 31, 4, 0) + bytes(8),
            "netpbm": b"P6\n999999999 999999999\n255\n" + bytes(10),
            "gif": b"GIF89a" + struct.pack("<HHBBB", 65535, 65535, 0, 0, 0) + b"\x3b",
            "jpeg": b"\xff\xd8\xff\xc0\x00\x11\x08\xff\xff\xff\xff\x03\x01\x22\x00\x02\x11\x01\x03\x11\x01\xff\xd9",
        }
        for name, data in cases.items():
            fn = DECODERS[name][0]
            finished, _r, exc = run_with_timeout(lambda: fn(data), CASE_TIMEOUT)
            self.assertTrue(finished, name)
            self.assertIsInstance(exc, OpenConverterError, f"{name}: {exc!r}")

    def test_inflate_random_streams(self):
        rng = random.Random(7)
        for i in range(300):
            blob = bytes(rng.randrange(256) for _ in range(rng.randint(1, 300)))
            self.check("inflate", lambda d: deflate.inflate(d, 1 << 20), blob, f"raw case {i}")


if __name__ == "__main__":
    unittest.main()
