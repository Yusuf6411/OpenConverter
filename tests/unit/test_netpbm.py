import unittest

from openconverter.codecs import netpbm
from openconverter.errors import CorruptFile
from openconverter.ops import image_ops
from tests.helpers import gradient, make, noise


class NetpbmReadTests(unittest.TestCase):
    def test_p1_ascii_with_comments_and_packed_digits(self):
        data = b"P1\n# a comment\n3 2\n1 0 1\n010\n"
        img = netpbm.decode(data)
        self.assertEqual((img.width, img.height, img.mode), (3, 2, "L"))
        self.assertEqual(list(img.pixels), [0, 255, 0, 255, 0, 255])

    def test_p4_binary(self):
        data = b"P4\n10 2\n" + bytes([0b10100000, 0b01000000, 0b11111111, 0b11000000])
        img = netpbm.decode(data)
        self.assertEqual(list(img.pixels[:10]), [0, 255, 0] + [255] * 5 + [255, 0])
        self.assertEqual(list(img.pixels[10:]), [0] * 10)

    def test_p2_maxval_scaling(self):
        img = netpbm.decode(b"P2 4 1 3  0 1 2 3")
        self.assertEqual(list(img.pixels), [0, 85, 170, 255])

    def test_p3_and_p6_equivalent(self):
        a = netpbm.decode(b"P3\n2 1\n255\n1 2 3 4 5 6\n")
        b = netpbm.decode(b"P6\n2 1\n255\n" + bytes([1, 2, 3, 4, 5, 6]))
        self.assertEqual(a.pixels, b.pixels)
        self.assertEqual(a.mode, "RGB")

    def test_16bit_binary(self):
        data = b"P5 2 1 65535\n" + (1000).to_bytes(2, "big") + (65535).to_bytes(2, "big")
        img = netpbm.decode(data)
        self.assertEqual(img.bit_depth, 16)
        self.assertEqual(list(image_ops.unpack16(img.pixels)), [1000, 65535])

    def test_16bit_maxval_scaled(self):
        img = netpbm.decode(b"P2 2 1 1000 0 1000")
        self.assertEqual(img.bit_depth, 16)
        self.assertEqual(list(image_ops.unpack16(img.pixels)), [0, 65535])

    def test_comment_after_maxval(self):
        img = netpbm.decode(b"P5 1 1 255#c\n" + b"\x07")
        self.assertEqual(list(img.pixels), [7])

    def test_errors(self):
        for bad in (b"", b"P", b"P6", b"P6 2 2", b"P6 2 2 255\n\x00", b"P5 1 1 0\n\x00",
                    b"P5 1 1 99999\n\x00", b"P2 2 1 3 0 9", b"P2 2 1 3 0 x", b"P5 0 1 255\n",
                    b"P6 a b c\n", b"P5 1 1 100\n\xff", b"P1 2 1 0 2"):
            with self.assertRaises(CorruptFile, msg=bad):
                netpbm.decode(bad)

    def test_sniff(self):
        self.assertEqual(netpbm.sniff_kind(b"P6\n1 1"), "ppm")
        self.assertEqual(netpbm.sniff_kind(b"P4 1 1"), "pbm")
        self.assertIsNone(netpbm.sniff_kind(b"P7\n"))


class NetpbmRoundTripTests(unittest.TestCase):
    def test_ppm_round_trips(self):
        for depth in (8, 16):
            for ascii_ in (False, True):
                img = noise(7, 5, "RGB", depth)
                out = netpbm.decode(netpbm.encode_ppm(img, ascii=ascii_))
                self.assertEqual((out.mode, out.bit_depth), ("RGB", depth))
                self.assertEqual(out.pixels, img.pixels)

    def test_pgm_round_trips(self):
        for depth in (8, 16):
            for ascii_ in (False, True):
                img = gradient(9, 4, "L", depth)
                out = netpbm.decode(netpbm.encode_pgm(img, ascii=ascii_))
                self.assertEqual(out.pixels, img.pixels)

    def test_pbm_round_trips(self):
        img = make(11, 3, "L", 8, [255 if (i * 7) % 3 else 0 for i in range(33)])
        for ascii_ in (False, True):
            notes = []
            out = netpbm.decode(netpbm.encode_pbm(img, ascii=ascii_, notes=notes))
            self.assertEqual(out.pixels, img.pixels)
            self.assertEqual(notes, [])

    def test_pbm_threshold_noted(self):
        notes = []
        out = netpbm.decode(netpbm.encode_pbm(make(2, 1, "L", 8, [100, 200]), notes=notes))
        self.assertEqual(list(out.pixels), [0, 255])
        self.assertTrue(any("1-bit" in n for n in notes))

    def test_alpha_flattened_with_note(self):
        notes = []
        img = make(1, 1, "RGBA", 8, [0, 0, 0, 0])
        out = netpbm.decode(netpbm.encode_ppm(img, notes=notes))
        self.assertEqual(list(out.pixels), [255, 255, 255])
        self.assertIn("Alpha flattened onto #FFFFFF.", notes)

    def test_ascii_lines_short(self):
        data = netpbm.encode_ppm(noise(40, 3, "RGB", 16), ascii=True)
        self.assertTrue(all(len(line) <= 70 for line in data.split(b"\n")))


if __name__ == "__main__":
    unittest.main()
