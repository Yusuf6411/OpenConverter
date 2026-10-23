import os
import tempfile
import threading
import time
import unittest

import openconverter as oc

PPM = b"P6\n4 2\n255\n" + bytes(range(24))


class Basics(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.TemporaryDirectory()
        self.addCleanup(self.d.cleanup)
        self.src = os.path.join(self.d.name, "a.ppm")
        with open(self.src, "wb") as f:
            f.write(PPM)

    def path(self, name):
        return os.path.join(self.d.name, name)

    def test_convert_and_info(self):
        r = oc.convert(self.src, self.path("a.png"))
        self.assertEqual(r.output_format, "png")
        self.assertGreater(r.bytes_out, 0)
        i = oc.info(self.path("a.png"))
        self.assertEqual((i["format"], i["width"], i["height"]), ("png", 4, 2))

    def test_options_and_pathlike(self):
        import pathlib

        r = oc.convert(pathlib.Path(self.src), pathlib.Path(self.path("a.jpg")), quality=50)
        self.assertEqual(r.output_format, "jpeg")

    def test_bad_option_is_an_error_with_a_code(self):
        with self.assertRaises(oc.ConversionError) as c:
            oc.convert(self.src, self.path("a.png"), nonsense=1)
        self.assertTrue(c.exception.code)

    def test_overwrite(self):
        oc.convert(self.src, self.path("a.png"))
        with self.assertRaises(oc.ConversionError) as c:
            oc.convert(self.src, self.path("a.png"))
        self.assertEqual(c.exception.code, "output_exists")
        oc.convert(self.src, self.path("a.png"), overwrite=True)

    def test_unknown_input(self):
        with self.assertRaises(oc.ConversionError):
            oc.convert(self.path("missing.xyz"), self.path("b.png"))

    def test_missing_file_info(self):
        with self.assertRaises(OSError):
            oc.info(self.path("nope.png"))

    def test_formats(self):
        f = {x["id"]: x for x in oc.formats()}
        self.assertIn("png", f)
        self.assertIn("png", f["png"]["extensions"])
        self.assertTrue(f["png"]["read"] and f["png"]["write"])

    def test_notes_are_dicts(self):
        r = oc.convert(self.src, self.path("a.gif"))
        for n in r.notes:
            self.assertIn(n["severity"], ("info", "warning", "loss"))

    def test_gil_is_released(self):
        big = self.path("big.csv")
        with open(big, "w") as f:
            f.write("a,b,c\n" + "1,2,3\n" * 60000)
        ticks = []
        stop = threading.Event()

        def tick():
            while not stop.is_set():
                ticks.append(time.monotonic())
                time.sleep(0.001)

        t = threading.Thread(target=tick)
        t.start()
        try:
            oc.convert(big, self.path("big.json"))
        finally:
            stop.set()
            t.join()
        self.assertGreater(len(ticks), 3)


if __name__ == "__main__":
    unittest.main()
