import re
import subprocess
import sys
import unittest
from pathlib import Path

import openconverter

ROOT = Path(__file__).resolve().parents[2]


class VersionTests(unittest.TestCase):
    def test_version_is_1_0_0(self):
        self.assertEqual(openconverter.__version__, "1.0.0")

    def test_pyproject_reads_version_dynamically(self):
        text = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
        self.assertRegex(text, r'dynamic\s*=\s*\["version"\]')
        self.assertIn('attr = "openconverter.__version__"', text)
        self.assertIsNone(re.search(r'^version\s*=\s*"', text, re.M))

    def test_module_version_flag(self):
        out = subprocess.run([sys.executable, "-m", "openconverter", "--version"],
                             cwd=ROOT, capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 0)
        self.assertIn("1.0.0", out.stdout)



def _verifier():
    import importlib.util
    spec = importlib.util.spec_from_file_location("verify_release", ROOT / "scripts" / "verify_release.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class StaticPolicyTests(unittest.TestCase):
    """Meta tests (spec §15): the shipped code honours the hard constraints."""

    @classmethod
    def setUpClass(cls):
        cls.v = _verifier()

    def test_only_stdlib_imports(self):
        self.assertEqual(self.v.check_stdlib_only(), [])

    def test_no_forbidden_modules(self):
        self.assertEqual(self.v.check_forbidden(), [])
        for mod in ("zlib", "gzip", "zipfile", "bz2", "lzma", "cgi"):
            self.assertIn(mod, self.v.FORBIDDEN)

    def test_no_remote_urls(self):
        self.assertEqual(self.v.check_remote_urls(), [])

    def test_no_inline_script_or_style(self):
        self.assertEqual(self.v.check_inline_web(), [])

    def test_detectors_catch_violations(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "x.py"
            p.write_text("import zlib\nimport numpy\nfrom os import path\n", encoding="utf-8")
            mods = {m for m, _ in self.v.imported_modules(p)}
            self.assertEqual(mods, {"zlib", "numpy", "os"})
        self.assertTrue(self.v.URL_RE.search("fetch('https://cdn.example.com/x.js')"))


if __name__ == "__main__":
    unittest.main()
