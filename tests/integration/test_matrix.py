"""Acceptance matrix (spec §6.2), generated from the registry: every required
conversion is run on fixtures and the output is re-decoded with our codecs."""

import json
import unittest

from openconverter.codecs import text as textcodec
from openconverter.converters.image_converter import decode_image
from openconverter.converters.table_converter import read_table
from openconverter.core import detect
from openconverter.core.registry import REGISTRY
from openconverter.ops import image_ops
from tests.helpers import FIXTURES, psnr

IMAGE_SOURCES = {
    "png": ["alpha.png", "rgb16.png", "photo.png", "tiny_1x1.png", "odd_7x13.png"],
    "jpeg": ["photo.jpg", "odd_33x17.jpg"],
    "gif": ["palette.gif"],
    "bmp": ["alpha.bmp", "odd_17x1.bmp"],
    "tga": ["gradient.tga"],
    "qoi": ["alpha.qoi"],
    "ppm": ["gradient.ppm", "rgb16.ppm"],
    "pgm": ["grey.pgm"],
    "pbm": ["bits.pbm"],
}
LOSSLESS = {"png", "bmp", "tga", "qoi"}
REQUIRED_IMAGE = {"ppm", "pgm", "bmp", "png", "jpeg", "tga", "qoi"}


def expected_after_rules(src, target, background=(255, 255, 255)):
    """Apply the documented writer rules to the source image."""
    img = src
    if target in ("ppm", "pgm", "pbm", "jpeg"):
        img = image_ops.flatten_alpha(img, background)
    if target in ("pgm", "pbm"):
        img = image_ops.to_grey(img)
    if target == "pbm":
        img = image_ops.to_8bit(img)
        img = image_ops.Image(img.width, img.height, "L", 8,
                              bytearray(0 if v < 128 else 255 for v in img.pixels))
    if target in ("bmp", "tga", "qoi", "jpeg"):
        img = image_ops.to_8bit(img)
    return img


def rgba8(img):
    img = image_ops.to_8bit(img)
    return image_ops.convert(img, "RGBA", 8)


class ImageMatrixTests(unittest.TestCase):
    def test_required_pairs_are_routable(self):
        for src in REQUIRED_IMAGE | {"gif"}:
            targets = set(REGISTRY.targets(src))
            self.assertTrue(REQUIRED_IMAGE <= targets, src)

    def test_matrix(self):
        image_formats = [f.id for f in REGISTRY.all() if f.category == "image"]
        for src_fmt in image_formats:
            spec = REGISTRY.get(src_fmt)
            if not spec.can_read:
                continue
            self.assertIn(src_fmt, IMAGE_SOURCES, f"add a fixture for new format {src_fmt}")
            for name in IMAGE_SOURCES[src_fmt]:
                data = (FIXTURES / "images" / name).read_bytes()
                self.assertEqual(detect.detect(data, name), src_fmt, name)
                src = decode_image(data, src_fmt)
                for target in REGISTRY.targets(src_fmt):
                    with self.subTest(src=name, target=target):
                        opts = REGISTRY.defaults(src_fmt, target)
                        result = REGISTRY.convert(data, src_fmt, target, opts)
                        self.assertEqual(detect.detect(result.data, "x"), target)
                        out = decode_image(result.data, target)
                        self.assertEqual((out.width, out.height), (src.width, src.height))
                        expected = expected_after_rules(src, target)
                        if target == "jpeg":
                            ref = image_ops.to_colour(expected) if out.mode == "RGB" else expected
                            if out.mode == "L":
                                ref = image_ops.to_grey(ref)
                            if src.width * src.height >= 64:
                                # Default q85 4:2:0 is a sanity check (synthetic stripes lose
                                # chroma); quality 95 at 4:4:4 must be faithful for every source.
                                self.assertGreaterEqual(psnr(ref, out), 14)
                                hq = REGISTRY.convert(data, src_fmt, "jpeg",
                                                      dict(opts, quality=95, subsampling="4:4:4"))
                                hq_img = decode_image(hq.data, "jpeg")
                                self.assertGreaterEqual(psnr(ref, hq_img), 30)
                                if name.startswith("photo"):
                                    self.assertGreaterEqual(psnr(ref, out), 30)
                        else:
                            self.assertEqual(rgba8(out).pixels, rgba8(expected).pixels)
                        if target in ("png", "ppm") and src.bit_depth == 16 and not (
                                target == "ppm" and src.has_alpha):
                            self.assertEqual(out.bit_depth, 16, "16-bit must be preserved")
                        if target in LOSSLESS and src.has_alpha and not image_ops.is_opaque(src):
                            self.assertTrue(out.has_alpha, "alpha must be preserved")


class TableMatrixTests(unittest.TestCase):
    def test_all_six_pairs(self):
        sources = {"csv": "people.csv", "tsv": "people.tsv", "json": "people.json"}
        data_formats = [f.id for f in REGISTRY.all() if f.category == "data"]
        for src, name in sources.items():
            data = (FIXTURES / "data" / name).read_bytes()
            self.assertEqual(detect.detect(data, name), src)
            table = read_table(data, src, {})
            for target in REGISTRY.targets(src):
                self.assertIn(target, data_formats)
                with self.subTest(src=src, target=target):
                    result = REGISTRY.convert(data, src, target, REGISTRY.defaults(src, target))
                    back = read_table(result.data, target, {})
                    self.assertEqual(back.header, table.header)
                    self.assertEqual(back.padded_rows(), table.padded_rows())
                    if target == "json":
                        json.loads(result.data)


class TextMatrixTests(unittest.TestCase):
    def test_txt_to_txt(self):
        for name in ("hello_utf8.txt", "hello_utf16le_bom.txt", "cp1252.txt"):
            data = (FIXTURES / "text" / name).read_bytes()
            self.assertEqual(detect.detect(data, name), "txt")
            original, _ = textcodec.decode_text(data)
            for enc in ("utf-8", "utf-16le-bom", "utf-16be"):
                for nl in ("lf", "crlf", "preserve"):
                    out = REGISTRY.convert(data, "txt", "txt", {"to_encoding": enc, "newline": nl})
                    decoded, _ = textcodec.decode_text(out.data)
                    self.assertEqual(textcodec.normalise_newlines(decoded, "lf"),
                                     textcodec.normalise_newlines(original, "lf"), (name, enc, nl))


if __name__ == "__main__":
    unittest.main()
