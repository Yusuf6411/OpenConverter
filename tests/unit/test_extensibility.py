"""The "How to add a new format" skeleton in docs/ARCHITECTURE.md must work."""

import re
import textwrap
import types
import unittest

from openconverter.converters import image_converter
from openconverter.converters.base import OWN, FormatSpec
from openconverter.core.registry import Registry, default_registry
from tests.helpers import FIXTURES, ROOT


def load_skeleton():
    doc = (ROOT / "docs" / "ARCHITECTURE.md").read_text(encoding="utf-8")
    blocks = [textwrap.dedent(b) for b in re.findall(r"```python\n(.*?)```", doc, re.S)]
    codec_src = next(b for b in blocks if "MAGIC = b\"FOO1\"" in b)
    module = types.ModuleType("foo_skeleton")
    exec(compile(codec_src, "ARCHITECTURE.md:foo", "exec"), module.__dict__)
    return module


class SkeletonTests(unittest.TestCase):
    def test_documented_format_skeleton_registers_and_converts(self):
        foo = load_skeleton()
        spec = FormatSpec("foo", "FOO", "image", (".foo",), "image/x-foo", foo.sniff, True, True, OWN,
                          "8-bit RGB only.", write_options={"background": image_converter.BACKGROUND})
        reg = Registry()
        for s in default_registry().all():
            reg.register(s)
        reg.register(spec)
        patches = {"DECODERS": foo.decode, "ENCODERS": foo.encode, "DECODE_COST": 0.5, "ENCODE_COST": 0.5}
        try:
            for name, value in patches.items():
                getattr(image_converter, name)["foo"] = value
            self.assertIn("foo", reg.targets("png"))
            self.assertIn("png", reg.targets("foo"))
            png_data = (FIXTURES / "images" / "alpha.png").read_bytes()
            out = reg.convert(png_data, "png", "foo", reg.defaults("png", "foo"))
            self.assertTrue(foo.sniff(out.data))
            self.assertIn("Alpha flattened onto #FFFFFF.", out.notes)
            back = reg.convert(out.data, "foo", "png", reg.defaults("foo", "png"))
            img = image_converter.decode_image(back.data, "png")
            self.assertEqual((img.width, img.height), (33, 17))
            doc = reg.to_json()
            self.assertIn("foo", [f["id"] for f in doc["formats"]])
        finally:
            for name in patches:
                getattr(image_converter, name).pop("foo", None)


if __name__ == "__main__":
    unittest.main()
