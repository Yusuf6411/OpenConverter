import struct
import unittest

from openconverter.codecs import qoi, tga
from openconverter.errors import CorruptFile
from tests.helpers import checkerboard, gradient, make, noise


def tga_header(itype, w, h, depth, desc=0, cmap=None):
    if cmap:
        first, length, cdepth = cmap
        return struct.pack("<BBBHHBHHHHBB", 0, 1, itype, first, length, cdepth, 0, 0, w, h, depth, desc)
    return struct.pack("<BBBHHBHHHHBB", 0, 0, itype, 0, 0, 0, 0, 0, w, h, depth, desc)


class TgaTests(unittest.TestCase):
    def test_round_trips(self):
        for mode in ("L", "RGB", "RGBA"):
            for rle in (False, True):
                img = noise(9, 5, mode) if not rle else checkerboard(13, 7, 3, mode)
                out = tga.decode(tga.encode(img, rle=rle))
                self.assertEqual((out.mode, out.pixels), (mode, img.pixels), (mode, rle))

    def test_rle_mixed_runs(self):
        img = make(10, 1, "RGB", 8, [1, 1, 1] * 4 + [1, 2, 3, 4, 5, 6] + [9, 9, 9] * 4)
        data = tga.encode(img, rle=True)
        self.assertEqual(tga.decode(data).pixels, img.pixels)

    def test_bottom_left_origin(self):
        data = tga_header(3, 2, 2, 8, desc=0) + bytes([1, 2, 3, 4])
        self.assertEqual(list(tga.decode(data).pixels), [3, 4, 1, 2])

    def test_right_to_left(self):
        data = tga_header(3, 2, 1, 8, desc=0x30) + bytes([1, 2])
        self.assertEqual(list(tga.decode(data).pixels), [2, 1])

    def test_colour_mapped_with_offset(self):
        cmap = bytes([255, 0, 0, 0, 255, 0])  # BGR entries: blue, green
        data = tga_header(1, 3, 1, 8, desc=0x20, cmap=(5, 2, 24)) + cmap + bytes([5, 6, 5])
        img = tga.decode(data)
        self.assertEqual(list(img.pixels), [0, 0, 255, 0, 255, 0, 0, 0, 255])

    def test_colour_mapped_rle(self):
        cmap = bytes([0, 0, 255, 0, 255, 0])
        data = tga_header(9, 4, 1, 8, desc=0x20, cmap=(0, 2, 24)) + cmap + bytes([0x83, 1])
        self.assertEqual(list(tga.decode(data).pixels), [0, 255, 0] * 4)

    def test_16bit_truecolour(self):
        v = (1 << 15) | (31 << 10) | (0 << 5) | 31
        data = tga_header(2, 1, 1, 16, desc=0x21) + struct.pack("<H", v)
        img = tga.decode(data)
        self.assertEqual((img.mode, list(img.pixels)), ("RGBA", [255, 0, 255, 255]))

    def test_rle_packet_crossing_rows(self):
        data = tga_header(11, 2, 2, 8, desc=0x20) + bytes([0x83, 7])
        self.assertEqual(list(tga.decode(data).pixels), [7, 7, 7, 7])

    def test_footer_read(self):
        data = tga.encode(gradient(3, 3, "RGB"))
        self.assertTrue(data.endswith(tga.FOOTER_SIG))
        self.assertEqual(tga.decode(data).metadata["tga_version"], "2.0")
        self.assertTrue(tga.sniff(data))

    def test_errors(self):
        for bad in (b"", b"\x00" * 10, tga_header(2, 4, 4, 24), tga_header(10, 4, 4, 24) + b"\x81",
                    tga_header(2, 1, 1, 7) + b"\x00", tga_header(0, 1, 1, 8),
                    tga_header(1, 1, 1, 8, cmap=(0, 1, 24)) + b"\x00\x00\x00\x05"):
            with self.assertRaises(CorruptFile, msg=bad):
                tga.decode(bad)


class QoiTests(unittest.TestCase):
    def test_round_trips(self):
        for img in (noise(17, 9, "RGB"), noise(17, 9, "RGBA"), gradient(33, 17, "RGB"),
                    gradient(33, 17, "RGBA"), checkerboard(40, 40, 5, "RGBA"),
                    make(100, 1, "RGB", 8, [5, 5, 5] * 100)):
            out = qoi.decode(qoi.encode(img))
            self.assertEqual((out.mode, out.pixels), (img.mode, img.pixels))

    def test_long_runs_split_at_62(self):
        img = make(200, 1, "RGB", 8, [0, 0, 0] * 200)
        data = qoi.encode(img)
        self.assertEqual(qoi.decode(data).pixels, img.pixels)

    def test_hand_made_stream_all_ops(self):
        body = bytes([
            0xFE, 10, 20, 30,          # RGB
            0x40 | (3 << 4) | (2 << 2) | 1,  # DIFF +1, 0, -1
            0x80 | (32 + 5), (8 + 1) << 4 | (8 - 1),  # LUMA vg=5 vr-vg=1 vb-vg=-1
            0xFF, 1, 2, 3, 4,          # RGBA
            0xC0 | 1,                  # RUN 2
        ])
        h0 = (10 * 3 + 20 * 5 + 30 * 7 + 255 * 11) % 64
        body += bytes([h0])            # INDEX back to (10,20,30,255)
        data = qoi.MAGIC + struct.pack(">IIBB", 7, 1, 4, 0) + body + qoi.END_MARKER
        img = qoi.decode(data)
        px = [tuple(img.pixels[i:i + 4]) for i in range(0, 28, 4)]
        self.assertEqual(px, [(10, 20, 30, 255), (11, 20, 29, 255), (17, 25, 33, 255),
                              (1, 2, 3, 4), (1, 2, 3, 4), (1, 2, 3, 4), (10, 20, 30, 255)])

    def test_grey_and_16bit_notes(self):
        notes = []
        out = qoi.decode(qoi.encode(gradient(4, 4, "L", 16), notes=notes))
        self.assertEqual(out.mode, "RGB")
        self.assertTrue(any("16 to 8" in n for n in notes))

    def test_errors(self):
        good = qoi.encode(noise(4, 4, "RGB"))
        for bad in (b"", b"qoif", good[:-1], good[:20] + qoi.END_MARKER,
                    qoi.MAGIC + struct.pack(">IIBB", 1, 1, 5, 0) + qoi.END_MARKER,
                    qoi.MAGIC + struct.pack(">IIBB", 0, 1, 3, 0) + qoi.END_MARKER):
            with self.assertRaises(CorruptFile, msg=bad):
                qoi.decode(bad)


if __name__ == "__main__":
    unittest.main()
