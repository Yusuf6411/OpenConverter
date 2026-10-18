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


if __name__ == "__main__":
    unittest.main()
