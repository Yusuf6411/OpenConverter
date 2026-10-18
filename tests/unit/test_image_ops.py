import unittest

from openconverter.errors import BadRequest
from openconverter.models.image import Image
from openconverter.ops import image_ops as ops
from tests.helpers import gradient, make, noise


class ImageModelTests(unittest.TestCase):
    def test_validates_buffer_length(self):
        with self.assertRaises(ValueError):
            Image(2, 2, "RGB", 8, bytearray(11))
        Image(2, 2, "RGB", 8, bytearray(12))
        Image(2, 2, "RGBA", 16, bytearray(32))

    def test_rejects_bad_mode_and_depth(self):
        with self.assertRaises(ValueError):
            Image(1, 1, "CMYK", 8, bytearray(4))
        with self.assertRaises(ValueError):
            Image(1, 1, "L", 4, bytearray(1))
        with self.assertRaises(ValueError):
            Image(0, 1, "L", 8, bytearray(0))


class DepthTests(unittest.TestCase):
    def test_16_to_8_formula(self):
        vals = [0, 1, 128, 129, 257, 32767, 32768, 65534, 65535]
        img = make(len(vals), 1, "L", 16, vals)
        notes = []
        out = ops.to_8bit(img, notes)
        self.assertEqual(list(out.pixels), [(v * 255 + 32767) // 65535 for v in vals])
        self.assertTrue(notes)

    def test_8_to_16_is_times_257(self):
        img = make(4, 1, "L", 8, [0, 1, 128, 255])
        out = ops.to_16bit(img)
        self.assertEqual(list(ops.unpack16(out.pixels)), [0, 257, 128 * 257, 65535])

    def test_round_trip_8_16_8_is_identity(self):
        img = noise(16, 16, "RGBA")
        self.assertEqual(ops.to_8bit(ops.to_16bit(img)).pixels, img.pixels)


class AlphaTests(unittest.TestCase):
    def test_flatten_formula_on_white(self):
        img = make(3, 1, "RGBA", 8, [200, 100, 50, 128, 10, 20, 30, 0, 1, 2, 3, 255])
        notes = []
        out = ops.flatten_alpha(img, (255, 255, 255), notes)
        exp = []
        for c, a in [(200, 128), (100, 128), (50, 128), (10, 0), (20, 0), (30, 0), (1, 255), (2, 255), (3, 255)]:
            exp.append((c * a + 255 * (255 - a) + 127) // 255)
        self.assertEqual(out.mode, "RGB")
        self.assertEqual(list(out.pixels), exp)
        self.assertIn("Alpha flattened onto #FFFFFF.", notes)

    def test_flatten_custom_background(self):
        img = make(1, 1, "RGBA", 8, [0, 0, 0, 0])
        out = ops.flatten_alpha(img, (12, 34, 56))
        self.assertEqual(list(out.pixels), [12, 34, 56])

    def test_flatten_16bit(self):
        img = make(1, 1, "RGBA", 16, [65535, 0, 30000, 32768])
        out = ops.flatten_alpha(img, (0, 0, 0))
        exp = [(c * 32768 + 32767) // 65535 for c in (65535, 0, 30000)]
        self.assertEqual(list(ops.unpack16(out.pixels)), exp)

    def test_flatten_la(self):
        img = make(2, 1, "LA", 8, [100, 0, 100, 255])
        out = ops.flatten_alpha(img, (255, 255, 255))
        self.assertEqual(out.mode, "L")
        self.assertEqual(list(out.pixels), [255, 100])

    def test_opaque_alpha_removed_with_note(self):
        img = make(2, 1, "RGBA", 8, [1, 2, 3, 255, 4, 5, 6, 255])
        notes = []
        out = ops.flatten_alpha(img, notes=notes)
        self.assertEqual(list(out.pixels), [1, 2, 3, 4, 5, 6])
        self.assertTrue(any("fully opaque" in n for n in notes))

    def test_add_alpha(self):
        out = ops.add_alpha(make(1, 1, "RGB", 8, [1, 2, 3]))
        self.assertEqual(list(out.pixels), [1, 2, 3, 255])


class ColourTests(unittest.TestCase):
    def test_bt601_luma(self):
        px = [255, 0, 0, 0, 255, 0, 0, 0, 255, 10, 200, 30]
        out = ops.to_grey(make(4, 1, "RGB", 8, px))
        exp = [(299 * r + 587 * g + 114 * b + 500) // 1000
               for r, g, b in zip(px[0::3], px[1::3], px[2::3])]
        self.assertEqual(list(out.pixels), exp)

    def test_luma_16bit(self):
        out = ops.to_grey(make(1, 1, "RGB", 16, [65535, 1000, 0]))
        self.assertEqual(list(ops.unpack16(out.pixels)), [(299 * 65535 + 587 * 1000 + 500) // 1000])

    def test_grey_to_rgb_replicates(self):
        out = ops.to_colour(make(2, 1, "LA", 8, [7, 100, 9, 200]))
        self.assertEqual(out.mode, "RGBA")
        self.assertEqual(list(out.pixels), [7, 7, 7, 100, 9, 9, 9, 200])

    def test_convert_matrix(self):
        for mode in ("L", "LA", "RGB", "RGBA"):
            for depth in (8, 16):
                src = gradient(5, 3, mode, depth)
                for tmode in ("L", "LA", "RGB", "RGBA"):
                    for tdepth in (8, 16):
                        out = ops.convert(src, tmode, tdepth)
                        self.assertEqual((out.mode, out.bit_depth), (tmode, tdepth))

    def test_parse_colour(self):
        self.assertEqual(ops.parse_colour("#0A0b0C"), (10, 11, 12))
        for bad in ("fff", "#12345", "#GGGGGG", 5):
            with self.assertRaises(BadRequest):
                ops.parse_colour(bad)


class OrientationTests(unittest.TestCase):
    # 3x2 image with pixel values 0..5:  [0 1 2]
    #                                    [3 4 5]
    EXPECTED = {
        1: (3, 2, [0, 1, 2, 3, 4, 5]),
        2: (3, 2, [2, 1, 0, 5, 4, 3]),
        3: (3, 2, [5, 4, 3, 2, 1, 0]),
        4: (3, 2, [3, 4, 5, 0, 1, 2]),
        5: (2, 3, [0, 3, 1, 4, 2, 5]),
        6: (2, 3, [3, 0, 4, 1, 5, 2]),
        7: (2, 3, [5, 2, 4, 1, 3, 0]),
        8: (2, 3, [2, 5, 1, 4, 0, 3]),
    }

    def test_all_orientations(self):
        img = make(3, 2, "L", 8, [0, 1, 2, 3, 4, 5])
        for o, (w, h, px) in self.EXPECTED.items():
            out = ops.apply_orientation(img, o)
            self.assertEqual((out.width, out.height, list(out.pixels)), (w, h, px), f"orientation {o}")

    def test_orientation_multibyte_pixels(self):
        img = make(3, 2, "RGB", 16, [v for p in range(6) for v in (p, p + 100, p + 200)])
        out = ops.apply_orientation(img, 6)
        vals = list(ops.unpack16(out.pixels))
        self.assertEqual(vals[0::3], [3, 0, 4, 1, 5, 2])
        self.assertEqual(vals[1::3], [103, 100, 104, 101, 105, 102])


class ResizeTests(unittest.TestCase):
    def test_box_downscale_by_two(self):
        img = make(4, 2, "L", 8, [0, 10, 20, 30, 40, 50, 60, 70])
        out = ops.resize(img, 2, 1)
        self.assertEqual(list(out.pixels), [25, 45])

    def test_resize_keeps_mode(self):
        out = ops.resize(gradient(33, 17, "RGBA", 16), 7, 5)
        self.assertEqual((out.width, out.height, out.mode, out.bit_depth), (7, 5, "RGBA", 16))

    def test_thumbnail_size(self):
        self.assertEqual(ops.thumbnail_size(1024, 768, 512), (512, 384))
        self.assertEqual(ops.thumbnail_size(100, 2000, 512), (26, 512))
        self.assertEqual(ops.thumbnail_size(10, 10, 512), (10, 10))


if __name__ == "__main__":
    unittest.main()
