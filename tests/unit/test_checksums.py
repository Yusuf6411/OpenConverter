import random
import unittest
import zlib  # tests only: cross-check our checksums

from openconverter.codecs.checksums import CRC32, adler32, crc32


def samples():
    rng = random.Random(42)
    yield b""
    yield b"a"
    yield b"abc"
    yield b"123456789"
    yield bytes(range(256)) * 3
    yield b"\x00" * 100_001
    yield b"\xff" * 70_001
    yield bytes(rng.getrandbits(8) for _ in range(3_000_003))
    for n in range(1, 20):
        yield bytes(rng.getrandbits(8) for _ in range(n))


class ChecksumTests(unittest.TestCase):
    def test_crc32_matches_zlib(self):
        for data in samples():
            self.assertEqual(crc32(data), zlib.crc32(data), len(data))

    def test_crc32_known_value(self):
        self.assertEqual(crc32(b"123456789"), 0xCBF43926)

    def test_adler32_matches_zlib(self):
        for data in samples():
            self.assertEqual(adler32(data), zlib.adler32(data), len(data))

    def test_incremental(self):
        rng = random.Random(1)
        data = bytes(rng.getrandbits(8) for _ in range(200_000))
        c, a = 0, 1
        pos = 0
        while pos < len(data):
            step = rng.randint(0, 9000)
            c = crc32(data[pos:pos + step], c)
            a = adler32(data[pos:pos + step], a)
            pos += step
        self.assertEqual(c, zlib.crc32(data))
        self.assertEqual(a, zlib.adler32(data))
        self.assertEqual(CRC32().update(data[:5]).update(data[5:]).value, zlib.crc32(data))

    def test_accepts_memoryview_and_bytearray(self):
        data = bytearray(b"hello world")
        self.assertEqual(crc32(memoryview(data)), zlib.crc32(data))
        self.assertEqual(adler32(data), zlib.adler32(data))


if __name__ == "__main__":
    unittest.main()
