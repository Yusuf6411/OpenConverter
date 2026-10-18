import struct
import unittest

from openconverter.codecs import bmp
from openconverter.errors import CorruptFile, UnsupportedVariant
from tests.helpers import gradient, make, noise


def build_bmp(width, height, bpp, rows, *, compression=0, palette=None, header=40,
              masks=None, top_down=False, pixel_data=None):
    """Hand-craft a BMP. ``rows`` are top-down unpadded row bytes."""
    if pixel_data is None:
        stride = ((width * bpp + 31) // 32) * 4
        ordered = rows if top_down else rows[::-1]
        pixel_data = b"".join(r + bytes(stride - len(r)) for r in ordered)
    pal = b""
    if palette:
        entry = 3 if header == 12 else 4
        pal = b"".join(bytes((b, g, r)) + (b"" if entry == 3 else b"\x00") for r, g, b in palette)
    if header == 12:
        info = struct.pack("<IHHHH", 12, width, height, 1, bpp)
    else:
        h = -height if top_down else height
        info = struct.pack("<IiiHHIIiiII", header, width, h, 1, bpp, compression,
                           len(pixel_data), 2835, 2835, len(palette or []), 0)
        if header > 40:
            m = (masks or [0, 0, 0, 0]) + [0] * 4
            info += struct.pack("<4I", *m[:4])
            info += bytes(header - 56)
        elif masks and compression == 3:
            info += struct.pack("<3I", *masks[:3])
    off = 14 + len(info) + len(pal)
    return struct.pack("<2sIHHI", b"BM", off + len(pixel_data), 0, 0, off) + info + pal + pixel_data


PAL = [(0, 0, 0), (255, 0, 0), (0, 255, 0), (0, 0, 255)] + [(i, i, 255 - i) for i in range(12)]


class BmpReadTests(unittest.TestCase):
    def test_1bit(self):
        data = build_bmp(10, 2, 1, [bytes([0b10110000, 0b01000000]), bytes([0xFF, 0xC0])],
                         palette=[(0, 0, 0), (255, 255, 255)])
        img = bmp.decode(data)
        self.assertEqual(img.mode, "L")  # grey palette -> L
        self.assertEqual(list(img.pixels[:10]), [255, 0, 255, 255, 0, 0, 0, 0, 0, 255])
        self.assertEqual(list(img.pixels[10:]), [255] * 10)

    def test_4bit_and_8bit(self):
        img4 = bmp.decode(build_bmp(3, 1, 4, [bytes([0x12, 0x30])], palette=PAL))
        self.assertEqual(list(img4.pixels), [255, 0, 0, 0, 255, 0, 0, 0, 255])
        img8 = bmp.decode(build_bmp(3, 1, 8, [bytes([1, 2, 3])], palette=PAL))
        self.assertEqual(img8.pixels, img4.pixels)

    def test_os2_core_header(self):
        img = bmp.decode(build_bmp(3, 1, 8, [bytes([3, 2, 1])], palette=PAL, header=12))
        self.assertEqual(list(img.pixels), [0, 0, 255, 0, 255, 0, 255, 0, 0])

    def test_24bit_bottom_up_and_top_down(self):
        rows = [bytes([1, 2, 3, 4, 5, 6]), bytes([7, 8, 9, 10, 11, 12])]
        for td in (False, True):
            img = bmp.decode(build_bmp(2, 2, 24, rows, top_down=td))
            self.assertEqual(list(img.pixels), [3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10])

    def test_16bit_555_default(self):
        v = (31 << 10) | (0 << 5) | 15
        img = bmp.decode(build_bmp(1, 1, 16, [struct.pack("<H", v)]))
        self.assertEqual(list(img.pixels), [255, 0, (15 * 255 + 15) // 31])

    def test_16bit_565_bitfields(self):
        v = (31 << 11) | (63 << 5) | 0
        data = build_bmp(1, 1, 16, [struct.pack("<H", v)], compression=3,
                         masks=[0xF800, 0x07E0, 0x001F])
        self.assertEqual(list(bmp.decode(data).pixels), [255, 255, 0])

    def test_32bit_bi_rgb_ignores_fourth_byte(self):
        img = bmp.decode(build_bmp(1, 1, 32, [bytes([1, 2, 3, 0])]))
        self.assertEqual((img.mode, list(img.pixels)), ("RGB", [3, 2, 1]))

    def test_v5_header_with_alpha(self):
        data = build_bmp(2, 1, 32, [bytes([1, 2, 3, 128, 4, 5, 6, 255])], compression=3, header=124,
                         masks=[0xFF0000, 0xFF00, 0xFF, 0xFF000000])
        img = bmp.decode(data)
        self.assertEqual((img.mode, list(img.pixels)), ("RGBA", [3, 2, 1, 128, 6, 5, 4, 255]))

    def test_v4_odd_bitfields(self):
        # 32-bit with 10-bit channels
        v = (1023 << 20) | (512 << 10) | 0
        data = build_bmp(1, 1, 32, [struct.pack("<I", v)], compression=3, header=108,
                         masks=[0x3FF00000, 0xFFC00, 0x3FF, 0])
        self.assertEqual(list(bmp.decode(data).pixels), [255, (512 * 255 + 511) // 1023, 0])

    def test_rle8(self):
        # bottom-up: first row in stream is the bottom row
        stream = bytes([3, 1, 0, 0,        # bottom row: 1 1 1
                        0, 3, 2, 3, 1, 0,  # absolute: 2 3 1 (+pad)
                        0, 0, 0, 1])
        data = build_bmp(3, 2, 8, None, compression=1, palette=PAL, pixel_data=stream)
        img = bmp.decode(data)
        self.assertEqual(list(img.pixels), [0, 255, 0, 0, 0, 255, 255, 0, 0] + [255, 0, 0] * 3)

    def test_rle8_delta(self):
        stream = bytes([0, 2, 1, 1, 1, 2, 0, 1])
        data = build_bmp(3, 2, 8, None, compression=1, palette=PAL, pixel_data=stream)
        img = bmp.decode(data)
        # delta moves to (1, row 1 from bottom) == top row, x=1
        self.assertEqual(list(img.pixels[3:6]), [0, 255, 0])
        self.assertEqual(list(img.pixels[:3]), [0, 0, 0])

    def test_rle4(self):
        stream = bytes([4, 0x12, 0, 0,     # bottom row: 1 2 1 2
                        0, 3, 0x31, 0x20, 0, 0, 0, 1])  # top: 3 1 2
        data = build_bmp(4, 2, 4, None, compression=2, palette=PAL, pixel_data=stream)
        img = bmp.decode(data)
        top = list(img.pixels[:12])
        self.assertEqual(top[:9], [0, 0, 255, 255, 0, 0, 0, 255, 0])
        self.assertEqual(list(img.pixels[12:]), [255, 0, 0, 0, 255, 0] * 2)

    def test_unsupported_and_corrupt(self):
        jpeg = build_bmp(1, 1, 24, None, compression=4, pixel_data=b"\xff\xd8")
        with self.assertRaises(UnsupportedVariant):
            bmp.decode(jpeg)
        png = build_bmp(1, 1, 24, None, compression=5, pixel_data=b"\x89PNG")
        with self.assertRaises(UnsupportedVariant):
            bmp.decode(png)
        with self.assertRaises(CorruptFile):
            bmp.decode(b"BM" + bytes(10))
        with self.assertRaises(CorruptFile):
            bmp.decode(build_bmp(4, 4, 24, [b"\x00" * 12] * 4)[:-30])
        with self.assertRaises(CorruptFile):
            bmp.decode(build_bmp(1, 1, 8, [bytes([9])], palette=PAL[:2]))


class BmpWriteTests(unittest.TestCase):
    def test_rgb_round_trip_odd_width(self):
        for w in (1, 2, 3, 5, 7):
            img = noise(w, 3, "RGB")
            out = bmp.decode(bmp.encode(img))
            self.assertEqual(out.pixels, img.pixels)

    def test_alpha_written_as_32bit_v4(self):
        img = gradient(5, 4, "RGBA")
        data = bmp.encode(img)
        self.assertEqual(struct.unpack_from("<I", data, 14)[0], 108)
        self.assertEqual(struct.unpack_from("<H", data, 28)[0], 32)
        out = bmp.decode(data)
        self.assertEqual((out.mode, out.pixels), ("RGBA", img.pixels))

    def test_grey_and_16bit_notes(self):
        notes = []
        img = gradient(3, 3, "L", 16)
        out = bmp.decode(bmp.encode(img, notes=notes))
        self.assertEqual(out.mode, "RGB")  # BMP 1.0 writer stores 24-bit
        self.assertEqual(out.pixels[0::3], out.pixels[1::3])
        self.assertTrue(any("16 to 8" in n for n in notes))
        self.assertTrue(any("expanded to RGB" in n for n in notes))

    def test_dpi_preserved(self):
        img = make(1, 1, "RGB", 8, [0, 0, 0])
        img.metadata["dpi"] = (300, 300)
        self.assertEqual(bmp.decode(bmp.encode(img)).metadata["dpi"], (300.0, 300.0))


if __name__ == "__main__":
    unittest.main()
