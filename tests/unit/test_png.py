import random
import struct
import unittest
import zlib  # tests only: inspect IDAT independently

from openconverter.codecs import png
from openconverter.codecs.checksums import crc32
from openconverter.errors import CorruptFile, LimitExceeded, UnsupportedVariant
from openconverter.limits import Limits
from openconverter.ops import image_ops
from tests.helpers import gradient, make, noise, photo

SCALE = {1: 255, 2: 85, 4: 17, 8: 1}


def pack_rows(values, width, height, depth):
    """Pack per-sample ints into PNG rows (MSB first, rows padded to bytes)."""
    spp = len(values) // (width * height)
    out = bytearray()
    for y in range(height):
        row = values[y * width * spp:(y + 1) * width * spp]
        if depth == 16:
            for v in row:
                out += v.to_bytes(2, "big")
        elif depth == 8:
            out += bytes(row)
        else:
            nbits = len(row) * depth
            acc = 0
            for v in row:
                acc = (acc << depth) | v
            pad = (-nbits) % 8
            out += (acc << pad).to_bytes((nbits + pad) // 8, "big")
    return bytes(out)


def chunk(ctype, body):
    return struct.pack(">I", len(body)) + ctype + body + struct.pack(">I", crc32(body, crc32(ctype)))


class PngDecodeMatrixTests(unittest.TestCase):
    """Every colour type x valid bit depth x {interlaced, not} x all filters."""

    def expected_image(self, ctype, depth, values, w, h, palette):
        if ctype == 3:
            out = []
            for v in values:
                out += palette[v]
            return make(w, h, "RGB", 8, out)
        mode = {0: "L", 2: "RGB", 4: "LA", 6: "RGBA"}[ctype]
        if depth < 8:
            values = [v * SCALE[depth] for v in values]
        return make(w, h, mode, 16 if depth == 16 else 8, values)

    def test_matrix(self):
        rng = random.Random(77)
        palette = [(rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255)) for _ in range(256)]
        for ctype, depths in png.VALID_DEPTHS.items():
            for depth in depths:
                for w, h in ((1, 1), (7, 13), (17, 5)):
                    spp = png.CHANNELS[ctype]
                    top = (1 << depth) - 1
                    values = [rng.randint(0, top) for _ in range(w * h * spp)]
                    raw = pack_rows(values, w, h, depth)
                    plte = b"".join(bytes(p) for p in palette[:1 << depth]) if ctype == 3 else None
                    expected = self.expected_image(ctype, depth, values, w, h, palette)
                    for interlace in (False, True):
                        for ftype in range(5):
                            data = png.encode_raw(w, h, ctype, depth, raw, filter_type=ftype,
                                                  interlace=interlace, plte=plte)
                            img = png.decode(data)
                            label = f"ctype={ctype} depth={depth} {w}x{h} interlace={interlace} filter={ftype}"
                            self.assertEqual((img.mode, img.bit_depth), (expected.mode, expected.bit_depth), label)
                            self.assertEqual(img.pixels, expected.pixels, label)

    def test_adam7_larger_image(self):
        img = noise(37, 29, "RGBA", 8, seed=3)
        data = png.encode_raw(37, 29, 6, 8, bytes(img.pixels), filter_type=4, interlace=True)
        self.assertEqual(png.decode(data).pixels, img.pixels)


class PngTransparencyTests(unittest.TestCase):
    def test_trns_grey(self):
        data = png.encode_raw(3, 1, 0, 8, bytes([10, 20, 10]), trns=struct.pack(">H", 10))
        img = png.decode(data)
        self.assertEqual((img.mode, list(img.pixels)), ("LA", [10, 0, 20, 255, 10, 0]))

    def test_trns_grey_low_depth(self):
        raw = pack_rows([0, 1, 2, 3], 4, 1, 2)
        img = png.decode(png.encode_raw(4, 1, 0, 2, raw, trns=struct.pack(">H", 2)))
        self.assertEqual(list(img.pixels), [0, 255, 85, 255, 170, 0, 255, 255])

    def test_trns_rgb_8_and_16(self):
        img = png.decode(png.encode_raw(2, 1, 2, 8, bytes([1, 2, 3, 4, 5, 6]),
                                        trns=struct.pack(">HHH", 4, 5, 6)))
        self.assertEqual(list(img.pixels), [1, 2, 3, 255, 4, 5, 6, 0])
        raw16 = b"".join(v.to_bytes(2, "big") for v in (1, 2, 3, 400, 500, 600))
        img16 = png.decode(png.encode_raw(2, 1, 2, 16, raw16, trns=struct.pack(">HHH", 400, 500, 600)))
        self.assertEqual(img16.mode, "RGBA")
        self.assertEqual(list(image_ops.unpack16(img16.pixels)), [1, 2, 3, 65535, 400, 500, 600, 0])

    def test_trns_palette(self):
        plte = bytes([255, 0, 0, 0, 255, 0, 0, 0, 255])
        img = png.decode(png.encode_raw(3, 1, 3, 8, bytes([0, 1, 2]), plte=plte, trns=bytes([0, 128])))
        self.assertEqual(list(img.pixels), [255, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255])

    def test_grey_palette_becomes_L(self):
        plte = bytes([0, 0, 0, 128, 128, 128])
        img = png.decode(png.encode_raw(2, 1, 3, 1, bytes([0b01000000]), plte=plte))
        self.assertEqual((img.mode, list(img.pixels)), ("L", [0, 128]))


class PngChunkTests(unittest.TestCase):
    def build(self, extra_before_idat=b"", corrupt_crc=False):
        img = gradient(4, 4, "RGB")
        data = png.encode(img)
        ihdr_end = 8 + 25
        out = data[:ihdr_end] + extra_before_idat + data[ihdr_end:]
        if corrupt_crc:
            out = bytearray(out)
            out[ihdr_end + 8 + 2] ^= 0xFF  # flip a byte inside the next chunk's body
            out = bytes(out)
        return out

    def test_metadata_chunks(self):
        extra = chunk(b"gAMA", struct.pack(">I", 45455)) + chunk(b"pHYs", struct.pack(">IIB", 11811, 11811, 1))
        img = png.decode(self.build(extra))
        self.assertAlmostEqual(img.metadata["gamma"], 0.45455)
        self.assertEqual(img.metadata["dpi"], (300.0, 300.0))

    def test_unknown_ancillary_ignored(self):
        img = png.decode(self.build(chunk(b"teXt", b"Comment\x00hello") + chunk(b"zzZz", b"abc")))
        self.assertEqual(img.width, 4)

    def test_unknown_critical_fails(self):
        with self.assertRaises(UnsupportedVariant):
            png.decode(self.build(chunk(b"ABCD", b"")))

    def test_crc_mismatch(self):
        with self.assertRaises(CorruptFile):
            png.decode(self.build(chunk(b"teXt", b"Comment\x00hello"), corrupt_crc=True))

    def test_structural_errors(self):
        good = png.encode(gradient(4, 4, "RGB"))
        cases = [
            b"", good[:8], good[:20], good[:-20],
            png.SIGNATURE + chunk(b"IDAT", b"x") + chunk(b"IEND", b""),
            png.SIGNATURE + chunk(b"IHDR", struct.pack(">IIBBBBB", 4, 4, 3, 2, 0, 0, 0)),
            png.SIGNATURE + chunk(b"IHDR", struct.pack(">IIBBBBB", 0, 4, 8, 2, 0, 0, 0)) +
            chunk(b"IDAT", zlib.compress(b"")) + chunk(b"IEND", b""),
            png.SIGNATURE + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 3, 0, 0, 0)) +
            chunk(b"IDAT", zlib.compress(b"\x00\x00")) + chunk(b"IEND", b""),
            png.SIGNATURE + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 1, 8, 0, 0, 0, 0)) +
            chunk(b"IDAT", zlib.compress(b"\x09\x00\x00")) + chunk(b"IEND", b""),
            png.SIGNATURE + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 1, 8, 0, 0, 0, 0)) +
            chunk(b"IDAT", zlib.compress(b"\x00\x00")) + chunk(b"IEND", b""),
        ]
        for i, bad in enumerate(cases):
            with self.assertRaises(CorruptFile, msg=i):
                png.decode(bad)

    def test_dimension_limits(self):
        ihdr = chunk(b"IHDR", struct.pack(">IIBBBBB", 40000, 40000, 8, 2, 0, 0, 0))
        with self.assertRaises(LimitExceeded):
            png.decode(png.SIGNATURE + ihdr + chunk(b"IDAT", b"") + chunk(b"IEND", b""))
        small = Limits(max_pixels=10)
        with self.assertRaises(LimitExceeded):
            png.decode(png.encode(gradient(4, 4, "RGB")), limits=small)

    def test_bomb_guard(self):
        ihdr = chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 0, 0, 0, 0))
        idat = chunk(b"IDAT", zlib.compress(bytes(10_000_000)))
        with self.assertRaises(LimitExceeded):
            png.decode(png.SIGNATURE + ihdr + idat + chunk(b"IEND", b""))


class PngEncodeTests(unittest.TestCase):
    def test_round_trip_every_mode_and_depth(self):
        for mode in ("L", "LA", "RGB", "RGBA"):
            for depth in (8, 16):
                img = noise(19, 11, mode, depth, seed=depth)
                out = png.decode(png.encode(img))
                self.assertEqual((out.mode, out.bit_depth, out.pixels), (mode, depth, img.pixels), mode)

    def test_levels(self):
        img = photo(48, 32)
        sizes = []
        for level in range(10):
            data = png.encode(img, compression_level=level)
            self.assertEqual(png.decode(data).pixels, img.pixels)
            sizes.append(len(data))
        self.assertLess(sizes[9], sizes[0])

    def test_palette_auto(self):
        img = make(16, 16, "RGB", 8, [v for i in range(256) for v in ((i % 5) * 50, 0, 255)])
        data = png.encode(img)
        self.assertEqual(data[8 + 8 + 9], 3)  # colour type in IHDR
        self.assertEqual(png.decode(data).pixels, img.pixels)

    def test_palette_with_alpha(self):
        img = make(4, 1, "RGBA", 8, [255, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255, 0, 0, 255, 255])
        data = png.encode(img, palette="on")
        out = png.decode(data)
        self.assertEqual((out.mode, out.pixels), ("RGBA", img.pixels))

    def test_palette_off_and_too_many_colours(self):
        img = make(16, 16, "RGB", 8, [v for i in range(256) for v in (i, 0, 0)])
        self.assertEqual(png.encode(img, palette="off")[8 + 8 + 9], 2)
        notes = []
        big = noise(40, 40, "RGB")
        self.assertEqual(png.encode(big, palette="on", notes=notes)[8 + 8 + 9], 2)
        self.assertTrue(notes)

    def test_bilevel_grey_written_1bit(self):
        img = make(10, 2, "L", 8, [0, 255] * 10)
        data = png.encode(img)
        self.assertEqual((data[8 + 8 + 8], data[8 + 8 + 9]), (1, 0))
        self.assertEqual(png.decode(data).pixels, img.pixels)

    def test_idat_accepted_by_zlib(self):
        data = png.encode(photo(32, 32))
        pos, idat = 8, b""
        while pos < len(data):
            ln, ct = struct.unpack_from(">I4s", data, pos)
            if ct == b"IDAT":
                idat += data[pos + 8:pos + 8 + ln]
            pos += 12 + ln
        self.assertEqual(len(zlib.decompress(idat)), 32 * (1 + 32 * 3))

    def test_dpi_and_gamma_written(self):
        img = gradient(3, 3, "RGB")
        img.metadata.update(dpi=(72, 72), gamma=0.45455)
        out = png.decode(png.encode(img))
        self.assertEqual(out.metadata["dpi"], (72.0, 72.0))
        self.assertAlmostEqual(out.metadata["gamma"], 0.45455)

    def test_all_filters_selected_somewhere(self):
        # Adaptive filtering should pick different filters on varied content.
        img = photo(64, 64)
        data = png.encode(img)
        pos, idat = 8, b""
        while pos < len(data):
            ln, ct = struct.unpack_from(">I4s", data, pos)
            if ct == b"IDAT":
                idat += data[pos + 8:pos + 8 + ln]
            pos += 12 + ln
        raw = zlib.decompress(idat)
        filters = {raw[y * (1 + 64 * 3)] for y in range(64)}
        self.assertGreaterEqual(len(filters), 2)


if __name__ == "__main__":
    unittest.main()
