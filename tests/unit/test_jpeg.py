import random
import struct
import unittest

from openconverter.codecs import jpeg
from openconverter.codecs.jpeg import dct, huffman, tables
from openconverter.errors import CorruptFile, UnsupportedVariant
from openconverter.models.image import Image
from openconverter.ops import image_ops
from tests.helpers import FIXTURES, gradient, make, photo, psnr

JPEG_FIXTURES = FIXTURES / "jpeg"


def segments(data):
    """Split a JPEG into [(marker, segment_bytes)] up to and including SOS; then ('scan', rest)."""
    out = []
    pos = 2
    while True:
        marker = data[pos + 1]
        length = struct.unpack_from(">H", data, pos + 2)[0]
        out.append((marker, data[pos:pos + 2 + length]))
        pos += 2 + length
        if marker == 0xDA:
            out.append(("scan", data[pos:]))
            return out


def join(segs):
    return b"\xff\xd8" + b"".join(s for _, s in segs)


def app1_orientation(value, big_endian=False):
    e = ">" if big_endian else "<"
    tiff = (b"MM" if big_endian else b"II") + struct.pack(e + "HI", 42, 8)
    tiff += struct.pack(e + "H", 1) + struct.pack(e + "HHIHH", 0x0112, 3, 1, value, 0) + struct.pack(e + "I", 0)
    body = b"Exif\x00\x00" + tiff
    return b"\xff\xe1" + struct.pack(">H", len(body) + 2) + body


def max_diff(a: Image, b: Image):
    return max(abs(x - y) for x, y in zip(a.pixels, b.pixels))


class DctTests(unittest.TestCase):
    def check(self, coefs):
        nz = [(k, float(c)) for k, c in enumerate(coefs) if k and c]
        ours = dct.idct_block(nz, float(coefs[0]))
        ref = [min(255, max(0, int(round(v + 128)))) for v in dct.reference_idct([float(c) for c in coefs])]
        return max(abs(a - b) for a, b in zip(ours, ref))

    def test_idct_accuracy_random_blocks(self):
        rng = random.Random(1)
        worst = 0
        for _ in range(300):
            coefs = [0] * 64
            for k in rng.sample(range(64), rng.randint(1, 64)):
                coefs[k] = rng.randint(-300, 300)
            worst = max(worst, self.check(coefs))
        self.assertLessEqual(worst, 1)

    def test_idct_accuracy_extreme_blocks(self):
        rng = random.Random(2)
        blocks = [[1023] * 64, [-1024] * 64, [0] * 64, [2040] + [0] * 63, [-2040] + [0] * 63]
        blocks += [[rng.choice((-1024, 1023)) for _ in range(64)] for _ in range(50)]
        for coefs in blocks:
            self.assertLessEqual(self.check(coefs), 1)

    def test_fdct_aan_matches_reference(self):
        rng = random.Random(3)
        scale = [1.0 / (dct._AAN[v] * dct._AAN[u] * 8.0) for v in range(8) for u in range(8)]
        for _ in range(50):
            samples = [rng.randint(-128, 127) for _ in range(64)]
            ours = dct.fdct_aan([float(s) for s in samples])
            ref = dct.reference_fdct(samples)
            for a, s, r in zip(ours, scale, ref):
                self.assertAlmostEqual(a * s, r, places=3)


class HuffmanTests(unittest.TestCase):
    def test_standard_tables_valid(self):
        for bits, vals in (tables.STD_DC_LUMA, tables.STD_DC_CHROMA, tables.STD_AC_LUMA, tables.STD_AC_CHROMA):
            enc = huffman.build_encode_table(bits, vals)
            self.assertEqual(len(enc), len(vals))
            table = huffman.build_decode_table(bits, vals)
            for sym, (code, ln) in enc.items():
                self.assertEqual(table[code << (16 - ln)], (sym << 5) | ln)

    def test_optimal_table_limits(self):
        rng = random.Random(4)
        freqs = [0] * 256
        for i in range(162):
            freqs[i] = int(2 ** rng.uniform(0, 24))
        bits, vals = huffman.optimal_table(freqs)
        self.assertEqual(sum(bits), 162)
        codes = huffman.code_lengths_and_codes(bits, vals)
        self.assertLessEqual(max(ln for _, _, ln in codes), 16)
        for _, code, ln in codes:
            self.assertNotEqual(code, (1 << ln) - 1, "all-ones code must not be used")

    def test_over_subscribed_rejected(self):
        with self.assertRaises(CorruptFile):
            huffman.build_decode_table([3] + [0] * 15, [0, 1, 2])

    def test_quality_scaling(self):
        self.assertEqual(tables.scaled_quant_table(tables.STD_LUMA_QT, 50), tables.STD_LUMA_QT)
        self.assertEqual(set(tables.scaled_quant_table(tables.STD_LUMA_QT, 100)), {1})
        q1 = tables.scaled_quant_table(tables.STD_LUMA_QT, 1)
        self.assertEqual(max(q1), 255)
        self.assertEqual(tables.scaled_quant_table([16], 25), [32])


class QualityGateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.photo = photo(96, 64)

    def test_psnr_gates(self):
        q90 = jpeg.decode(jpeg.encode(self.photo, quality=90))
        q75 = jpeg.decode(jpeg.encode(self.photo, quality=75))
        self.assertGreaterEqual(psnr(self.photo, q90), 35)
        self.assertGreaterEqual(psnr(self.photo, q75), 30)

    def test_monotonic(self):
        prev_p, prev_s = 0, 0
        for q in (20, 50, 75, 90, 100):
            data = jpeg.encode(self.photo, quality=q, subsampling="4:4:4")
            p = psnr(self.photo, jpeg.decode(data))
            self.assertGreaterEqual(p, prev_p, q)
            self.assertGreater(len(data), prev_s, q)
            prev_p, prev_s = p, len(data)

    def test_optimized_huffman_smaller_and_identical_pixels(self):
        a = jpeg.encode(self.photo, quality=85)
        b = jpeg.encode(self.photo, quality=85, optimize_huffman=True)
        self.assertLess(len(b), len(a))
        self.assertEqual(jpeg.decode(a).pixels, jpeg.decode(b).pixels)


class StructureTests(unittest.TestCase):
    def test_default_subsampling(self):
        img = photo(32, 32)
        notes = []
        jpeg.encode(img, quality=85, notes=notes)
        self.assertIn("Chroma subsampling 4:2:0.", notes)
        notes = []
        jpeg.encode(img, quality=95, notes=notes)
        self.assertIn("Chroma subsampling 4:4:4.", notes)

    def test_odd_sizes_all_subsamplings(self):
        for w, h in ((1, 1), (7, 13), (17, 1), (33, 17)):
            img = gradient(w, h, "RGB")
            for ss in ("4:4:4", "4:2:0", "4:2:2", "4:4:0", "4:1:1"):
                out = jpeg.decode(jpeg.encode(img, quality=95, subsampling=ss))
                self.assertEqual((out.width, out.height, out.mode), (w, h, "RGB"))
                # 4:1:1 keeps a quarter of the horizontal chroma: steep 7-px ramps suffer.
                self.assertGreaterEqual(psnr(img, out), 20 if ss == "4:1:1" else 28, (w, h, ss))

    def test_greyscale_single_component(self):
        img = gradient(33, 17, "L")
        data = jpeg.encode(img, quality=90)
        sof = [s for m, s in segments(data) if m == 0xC0][0]
        self.assertEqual(sof[9], 1)
        out = jpeg.decode(data)
        self.assertEqual(out.mode, "L")
        self.assertGreater(psnr(img, out), 40)

    def test_restart_markers(self):
        img = photo(64, 48)
        plain = jpeg.decode(jpeg.encode(img, quality=80))
        data = jpeg.encode(img, quality=80, restart_interval=3)
        self.assertIn(b"\xff\xdd", data)
        self.assertIn(b"\xff\xd0", data)
        self.assertEqual(jpeg.decode(data).pixels, plain.pixels)

    def test_restart_marker_out_of_sequence(self):
        data = bytearray(jpeg.encode(photo(64, 48), quality=80, restart_interval=1))
        i = data.index(b"\xff\xd1")
        data[i + 1] = 0xD3
        with self.assertRaises(CorruptFile):
            jpeg.decode(bytes(data))

    def test_fill_bytes_and_split_tables(self):
        img = photo(24, 16)
        data = jpeg.encode(img, quality=80)
        segs = segments(data)
        out = []
        for m, s in segs:
            if m == 0xDB:  # split the two DQT tables into separate segments, one 16-bit
                body = s[4:]
                t0, t1 = body[:65], body[65:]
                t1_16 = bytes([0x11]) + b"".join(struct.pack(">H", v) for v in t1[1:])
                out.append((m, b"\xff\xdb" + struct.pack(">H", 67) + t0))
                out.append((m, b"\xff\xff\xff\xdb" + struct.pack(">H", 2 + len(t1_16)) + t1_16))
            elif m == 0xC4:
                out.append((m, b"\xff\xff" + s))
            else:
                out.append((m, s))
        modified = join(out)
        modified = modified[:-2] + b"\xff\xff\xff\xd9"
        self.assertEqual(jpeg.decode(modified).pixels, jpeg.decode(data).pixels)

    def test_adobe_rgb_transform_zero(self):
        img = photo(24, 16)
        data = jpeg.encode(img, quality=95, colour_transform=False)
        out = jpeg.decode(data)
        self.assertEqual(out.metadata["jpeg_colour"], "RGB")
        self.assertGreater(psnr(img, out), 35)

    def test_alpha_and_16bit_notes(self):
        notes = []
        jpeg.encode(gradient(8, 8, "RGBA", 16), quality=80, notes=notes)
        self.assertTrue(any("flattened" in n for n in notes))
        self.assertTrue(any("16 to 8" in n for n in notes))

    def test_jfif_density(self):
        img = gradient(8, 8, "RGB")
        img.metadata["dpi"] = (300, 300)
        self.assertEqual(jpeg.decode(jpeg.encode(img)).metadata["dpi"], (300.0, 300.0))


class OrientationTests(unittest.TestCase):
    def test_exif_orientation_1_to_8(self):
        img = gradient(16, 8, "RGB")
        base = jpeg.encode(img, quality=95, subsampling="4:4:4")
        upright = jpeg.decode(base)
        for o in range(1, 9):
            for be in (False, True):
                data = base[:2] + app1_orientation(o, be) + base[2:]
                out = jpeg.decode(data)
                expected = image_ops.apply_orientation(upright, o)
                self.assertEqual((out.width, out.height), (expected.width, expected.height), o)
                self.assertEqual(out.pixels, expected.pixels, o)
                if o != 1:
                    self.assertEqual(out.metadata["orientation_applied"], o)


class InteropFixtureTests(unittest.TestCase):
    """Fixtures decoded by Chromium (libjpeg-turbo) in the browser pane."""

    def test_chromium_encoded_jpegs(self):
        files = sorted(JPEG_FIXTURES.glob("chromium_*.jpg"))
        self.assertEqual(len(files), 3)
        for f in files:
            ref_bytes = f.with_suffix(".rgb").read_bytes()
            out = jpeg.decode(f.read_bytes())
            ref = Image(out.width, out.height, "RGB", 8, bytearray(ref_bytes))
            self.assertLessEqual(max_diff(out, ref), 4, f.name)

    def test_our_jpegs_match_chromium_decode(self):
        files = sorted(JPEG_FIXTURES.glob("ours_*.jpg"))
        self.assertEqual(len(files), 4)
        for f in files:
            out = jpeg.decode(f.read_bytes())
            if out.mode == "L":
                out = image_ops.to_colour(out)
            ref = Image(out.width, out.height, "RGB", 8,
                        bytearray((f.parent / (f.stem + ".chromium.rgb")).read_bytes()))
            self.assertLessEqual(max_diff(out, ref), 4, f.name)


class UnsupportedAndCorruptTests(unittest.TestCase):
    def sof_variant(self, marker, precision=8, ncomp=3):
        body = struct.pack(">BHHB", precision, 8, 8, ncomp) + b"".join(bytes([i + 1, 0x11, 0]) for i in range(ncomp))
        return b"\xff\xd8" + bytes([0xFF, marker]) + struct.pack(">H", len(body) + 2) + body + b"\xff\xd9"

    def test_progressive_stub(self):
        with self.assertRaises(UnsupportedVariant) as cm:
            jpeg.decode(self.sof_variant(0xC2))
        self.assertIn("progressive", str(cm.exception))

    def test_other_variants(self):
        for marker in (0xC3, 0xC5, 0xC9, 0xCA, 0xCB, 0xCD):
            with self.assertRaises(UnsupportedVariant):
                jpeg.decode(self.sof_variant(marker))
        with self.assertRaises(UnsupportedVariant):
            jpeg.decode(self.sof_variant(0xC0, precision=12))
        with self.assertRaises(UnsupportedVariant):
            jpeg.decode(self.sof_variant(0xC0, ncomp=4))

    def test_truncated(self):
        data = jpeg.encode(photo(32, 32), quality=80)
        for cut in (3, 10, 100, len(data) // 2, len(data) - 40):
            with self.assertRaises(CorruptFile, msg=cut):
                jpeg.decode(data[:cut])

    def test_garbage(self):
        for bad in (b"", b"\xff\xd8", b"\xff\xd8\xff", b"\xff\xd8\xff\xd9", b"\xff\xd8\x00\x00"):
            with self.assertRaises(CorruptFile):
                jpeg.decode(bad)

    def test_sof1_extended_accepted(self):
        data = bytearray(jpeg.encode(gradient(8, 8, "RGB"), quality=90))
        i = data.index(b"\xff\xc0")
        data[i + 1] = 0xC1
        out = jpeg.decode(bytes(data))
        self.assertEqual(out.metadata["jpeg_process"], "extended sequential")


if __name__ == "__main__":
    unittest.main()
