import json
import socket
import subprocess
import sys
import time
import unittest
import urllib.request

from openconverter.codecs import jpeg, png
from tests.helpers import FIXTURES, ROOT, TempDir


def run(*args, timeout=180):
    return subprocess.run([sys.executable, "-m", "openconverter", *map(str, args)], cwd=ROOT,
                          capture_output=True, text=True, timeout=timeout, encoding="utf-8")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class CliConvertTests(unittest.TestCase):
    def test_png_to_jpeg_with_options(self):
        with TempDir() as d:
            out = d / "out.jpg"
            r = run("convert", FIXTURES / "images/alpha.png", out, "--quality", "70", "--subsampling", "4:4:4",
                    "--background", "#000000")
            self.assertEqual(r.returncode, 0, r.stderr)
            img = jpeg.decode(out.read_bytes())
            self.assertEqual((img.width, img.height), (33, 17))
            self.assertIn("Alpha flattened onto #000000.", r.stdout)

    def test_source_detected_by_content(self):
        with TempDir() as d:
            src = d / "actually_png.jpg"
            src.write_bytes((FIXTURES / "images/alpha.png").read_bytes())
            r = run("convert", src, d / "x.qoi")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("(png)", r.stdout)

    def test_csv_to_json_indent(self):
        with TempDir() as d:
            out = d / "people.json"
            r = run("convert", FIXTURES / "data/people.csv", out, "--indent", "4")
            self.assertEqual(r.returncode, 0, r.stderr)
            data = json.loads(out.read_text(encoding="utf-8"))
            self.assertEqual(data[1]["city"], "New\nYork")
            self.assertIn('\n    {', out.read_text(encoding="utf-8"))

    def test_txt_encoding_and_newline(self):
        with TempDir() as d:
            out = d / "out.txt"
            r = run("convert", FIXTURES / "text/hello_utf8.txt", out, "--to-encoding", "utf-16le", "--newline", "crlf")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(out.read_bytes(), "Hello, wörld – ½ €\r\nSecond line\r\n".encode("utf-16-le"))

    def test_to_flag_and_overwrite(self):
        with TempDir() as d:
            out = d / "noext"
            r = run("convert", FIXTURES / "images/alpha.png", out, "--to", "png", "--level", "9")
            self.assertEqual(r.returncode, 0, r.stderr)
            png.decode(out.read_bytes())
            r = run("convert", FIXTURES / "images/alpha.png", out, "--to", "png")
            self.assertEqual(r.returncode, 2)
            self.assertIn("--force", r.stderr)
            r = run("convert", FIXTURES / "images/alpha.png", out, "--to", "bmp", "--force")
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_usage_errors(self):
        with TempDir() as d:
            self.assertEqual(run("convert", d / "missing.png", d / "o.jpg").returncode, 2)
            self.assertEqual(run("convert", FIXTURES / "images/alpha.png", d / "o.unknown").returncode, 2)
            r = run("convert", FIXTURES / "images/alpha.png", d / "o.jpg", "--quality", "500")
            self.assertEqual(r.returncode, 2)
            self.assertIn("between 1 and 100", r.stderr)
            r = run("convert", FIXTURES / "images/alpha.png", d / "o.png", "--quality", "50")
            self.assertEqual(r.returncode, 2)
            self.assertIn("does not apply", r.stderr)
            self.assertEqual(run("convert").returncode, 2)

    def test_conversion_errors_exit_1(self):
        with TempDir() as d:
            bad = d / "bad.png"
            data = (FIXTURES / "images/photo.png").read_bytes()
            bad.write_bytes(data[:len(data) // 2])
            r = run("convert", bad, d / "o.jpg")
            self.assertEqual(r.returncode, 1)
            self.assertIn("error:", r.stderr)
            self.assertFalse((d / "o.jpg").exists())
            r = run("convert", FIXTURES / "data/people.csv", d / "o.png")
            self.assertEqual(r.returncode, 1)
            self.assertIn("cannot convert csv to png", r.stderr)

    def test_formats(self):
        r = run("formats")
        self.assertEqual(r.returncode, 0)
        self.assertIn("jpeg", r.stdout)
        self.assertIn("PARTIAL", r.stdout)
        r = run("formats", "--json")
        doc = json.loads(r.stdout)
        self.assertEqual(len(doc["formats"]), 13)

    def test_version(self):
        r = run("--version")
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "OpenConverter 1.0.0"))


class CliServeTests(unittest.TestCase):
    def test_serve_starts_and_answers(self):
        port = free_port()
        with TempDir() as d:
            proc = subprocess.Popen([sys.executable, "-m", "openconverter", "serve", "--no-browser", "--port", str(port),
                                     "--data-dir", str(d)], cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    text=True, encoding="utf-8")
            try:
                deadline = time.time() + 30
                body = None
                while time.time() < deadline:
                    try:
                        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health", timeout=2) as resp:
                            body = json.loads(resp.read())
                            break
                    except OSError:
                        time.sleep(0.2)
                self.assertEqual(body, {"status": "ok", "version": "1.0.0"})
                self.assertTrue((d / "logs").is_dir())
            finally:
                proc.terminate()
                out, err = proc.communicate(timeout=30)
            self.assertIn(f"http://127.0.0.1:{port}/", out)

    def test_busy_port_message(self):
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            s.listen(1)
            port = s.getsockname()[1]
            with TempDir() as d:
                r = run("serve", "--no-browser", "--port", port, "--data-dir", d, timeout=60)
        self.assertEqual(r.returncode, 1)
        self.assertIn("--port", r.stderr)

    def test_non_loopback_warning(self):
        with TempDir() as d:
            # An unassigned TEST-NET address cannot be bound, so the server exits right after the warning.
            r = run("serve", "--no-browser", "--host", "192.0.2.1", "--port", free_port(), "--data-dir", d, timeout=60)
        self.assertIn("WARNING", r.stderr)
        self.assertEqual(r.returncode, 1)


if __name__ == "__main__":
    unittest.main()
