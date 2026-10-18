import random
import unittest

from openconverter.codecs import gif
from openconverter.errors import CorruptFile, LimitExceeded
from openconverter.ops import image_ops
from tests.gif_builder import build_gif, lzw_encode
from tests.helpers import FIXTURES, gradient, make

PAL4 = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255)]


def rgb_of(indices, palette):
    out = []
    for i in indices:
        out += palette[i]
    return out


class LzwTests(unittest.TestCase):
    def test_round_trip_various(self):
        rng = random.Random(8)
        for mcs in (2, 3, 4, 8):
            for n in (1, 2, 10, 1000, 20000):
                data = bytes(rng.randrange(1 << mcs) for _ in range(n))
                self.assertEqual(gif.lzw_decode(lzw_encode(data, mcs), mcs, n), data, (mcs, n))

    def test_code_size_growth_to_12_bits_and_deferred_clear(self):
        rng = random.Random(9)
        data = bytes(rng.randrange(256) for _ in range(60000))
        deferred = lzw_encode(data, 8, clear_when_full=False)
        self.assertEqual(gif.lzw_decode(deferred, 8, len(data)), data)
        eager = lzw_encode(data, 8, clear_when_full=True)
        self.assertEqual(gif.lzw_decode(eager, 8, len(data)), data)

    def test_repetitive_kwkwk(self):
        data = bytes(5000)
        self.assertEqual(gif.lzw_decode(lzw_encode(data, 2), 2, 5000), data)

    def test_output_capped(self):
        data = bytes(range(4)) * 100
        self.assertEqual(len(gif.lzw_decode(lzw_encode(data, 2), 2, 50)), 50)

    def test_malformed_codes(self):
        # code 7 right after clear with min size 2 (clear=4, eoi=5): invalid first code
        with self.assertRaises(CorruptFile):
            gif.lzw_decode(bytes([0b00111100]), 2, 10)
        # a code far beyond the table
        with self.assertRaises(CorruptFile):
            gif.lzw_decode(bytes([0xCC, 0x01]), 2, 10)
        with self.assertRaises(CorruptFile):
            gif.lzw_decode(b"\x00", 9, 10)


class GifDecodeTests(unittest.TestCase):
    def test_basic_global_palette(self):
        idx = bytes([0, 1, 2, 3, 3, 2, 1, 0])
        img = gif.decode(build_gif(4, 2, [{"indices": idx, "mcs": 2}], PAL4))
        self.assertEqual((img.width, img.height, img.mode), (4, 2, "RGB"))
        self.assertEqual(list(img.pixels), rgb_of(idx, PAL4))

    def test_local_palette_and_gif87a(self):
        local = [(9, 9, 9), (200, 100, 50)]
        idx = bytes([1, 0, 1, 1])
        img = gif.decode(build_gif(2, 2, [{"indices": idx, "palette": local, "mcs": 2}], PAL4, version=b"GIF87a"))
        self.assertEqual(list(img.pixels), rgb_of(idx, local))

    def test_transparency(self):
        idx = bytes([0, 1, 2, 3])
        img = gif.decode(build_gif(2, 2, [{"indices": idx, "transparent": 2, "mcs": 2}], PAL4))
        self.assertEqual(img.mode, "RGBA")
        self.assertEqual(list(img.pixels[8:12]), [0, 0, 255, 0])
        self.assertEqual(img.pixels[3], 255)

    def test_interlaced(self):
        w, h = 5, 19
        rng = random.Random(1)
        idx = bytes(rng.randrange(4) for _ in range(w * h))
        img = gif.decode(build_gif(w, h, [{"indices": idx, "interlace": True, "mcs": 2}], PAL4))
        self.assertEqual(list(img.pixels), rgb_of(idx, PAL4))

    def test_multi_frame_first_only(self):
        f1 = {"indices": bytes([0] * 4), "mcs": 2}
        f2 = {"indices": bytes([1] * 4), "mcs": 2}
        f3 = {"indices": bytes([2] * 4), "mcs": 2}
        img = gif.decode(build_gif(2, 2, [f1, f2, f3], PAL4))
        self.assertEqual(list(img.pixels), rgb_of(bytes(4), PAL4))
        self.assertEqual(img.metadata["gif_frames"], 3)
        self.assertTrue(any("3 frames" in n for n in img.metadata["notes"]))

    def test_frame_smaller_than_screen(self):
        fr = {"indices": bytes([1, 1, 1, 1]), "w": 2, "h": 2, "left": 1, "top": 1, "mcs": 2}
        img = gif.decode(build_gif(4, 4, [fr], PAL4))
        self.assertEqual(img.mode, "RGBA")
        self.assertEqual(list(img.pixels[0:4]), [0, 0, 0, 0])
        px = (1 * 4 + 1) * 4
        self.assertEqual(list(img.pixels[px:px + 4]), [0, 255, 0, 255])

    def test_grey_palette_gives_L(self):
        pal = [(0, 0, 0), (128, 128, 128), (255, 255, 255), (64, 64, 64)]
        img = gif.decode(build_gif(2, 2, [{"indices": bytes([0, 1, 2, 3]), "mcs": 2}], pal))
        self.assertEqual((img.mode, list(img.pixels)), ("L", [0, 128, 255, 64]))

    def test_256_colours_large(self):
        pal = [(i, 255 - i, (i * 7) & 255) for i in range(256)]
        rng = random.Random(3)
        idx = bytes(rng.randrange(256) for _ in range(200 * 150))
        img = gif.decode(build_gif(200, 150, [{"indices": idx}], pal))
        self.assertEqual(list(img.pixels[:30]), rgb_of(idx[:10], pal))
        self.assertEqual(img.pixels[-3:], bytes(pal[idx[-1]]))

    def test_errors(self):
        good = build_gif(2, 2, [{"indices": bytes(4), "mcs": 2}], PAL4)
        for bad in (b"", b"GIF89a", good[:20], good[:-6],
                    build_gif(2, 2, [{"indices": bytes([0, 1, 2, 9]), "mcs": 4}], PAL4),
                    build_gif(2, 2, [{"indices": bytes(4), "mcs": 2}], None),
                    b"GIF89a" + bytes([2, 0, 2, 0, 0, 0, 0]) + b"\x3b"):
            with self.assertRaises(CorruptFile, msg=bad[:30]):
                gif.decode(bad)

    def test_absurd_dimensions(self):
        data = bytearray(build_gif(2, 2, [{"indices": bytes(4), "mcs": 2}], PAL4))
        data[6:10] = bytes([0xFF, 0xFF, 0xFF, 0xFF])
        with self.assertRaises(LimitExceeded):
            gif.decode(bytes(data))

    def test_browser_verified_fixture(self):
        """A GIF built by the test builder whose pixels Chromium decoded identically."""
        data = (FIXTURES / "gif" / "interlaced_transparent.gif").read_bytes()
        ref = (FIXTURES / "gif" / "interlaced_transparent.chromium.rgba").read_bytes()
        px = bytearray(image_ops.add_alpha(gif.decode(data)).pixels)
        for i in range(0, len(px), 4):
            if px[i + 3] == 0:  # canvas readback reports fully transparent pixels as 0,0,0,0
                px[i:i + 3] = b"\x00\x00\x00"
        self.assertEqual(bytes(px), ref)


class ThumbnailTests(unittest.TestCase):
    def test_box_reduce_exact(self):
        img = make(4, 2, "RGB", 8, [0, 0, 0, 10, 20, 30, 100, 100, 100, 50, 50, 50,
                                    2, 2, 2, 12, 22, 32, 102, 102, 102, 52, 52, 52])
        out = image_ops.box_reduce(img, 2)
        self.assertEqual((out.width, out.height, list(out.pixels)), (2, 1, [6, 11, 16, 76, 76, 76]))

    def test_thumbnail_limits(self):
        img = gradient(1300, 20, "RGBA", 16)
        th = image_ops.thumbnail(img, 512)
        self.assertLessEqual(max(th.width, th.height), 512)
        self.assertEqual((th.mode, th.bit_depth), ("RGBA", 8))
        small = gradient(40, 30, "L")
        self.assertEqual(image_ops.thumbnail(small, 512).pixels, small.pixels)

    def test_thin_image(self):
        th = image_ops.thumbnail(gradient(3, 2000, "L"), 512)
        self.assertEqual(th.height, 512)


if __name__ == "__main__":
    unittest.main()
