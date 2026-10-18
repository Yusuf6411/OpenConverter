import io
import unittest

from openconverter.errors import BadRequest, Forbidden, LimitExceeded
from openconverter.server import multipart, security


class TrickleReader(io.RawIOBase):
    """Returns at most ``step`` bytes per read to exercise boundary splitting."""

    def __init__(self, data, step):
        self.data = data
        self.pos = 0
        self.step = step

    def read(self, n=-1):
        n = min(n if n >= 0 else self.step, self.step)
        out = self.data[self.pos:self.pos + n]
        self.pos += len(out)
        return out


def parse(body, boundary=b"BOUND", step=7, **kw):
    r = multipart.MultipartReader(TrickleReader(body, step), len(body), boundary, **kw)
    out = []
    for part in r.parts():
        out.append((part.name, part.filename, b"".join(r.read_part())))
    return out


def body(*parts, boundary="BOUND", preamble="", epilogue="", pad=""):
    out = preamble
    for headers, data in parts:
        out += f"--{boundary}{pad}\r\n" + "".join(h + "\r\n" for h in headers) + "\r\n" + data + "\r\n"
    out += f"--{boundary}--{epilogue}"
    return out.encode("utf-8")


class MultipartTests(unittest.TestCase):
    def test_basic_and_split_boundaries(self):
        payload = "x" * 1000 + "\r\n--BOUN" + "y" * 500  # contains a near-boundary
        b = body((['Content-Disposition: form-data; name="files"; filename="a.bin"'], payload),
                 (['Content-Disposition: form-data; name="note"'], "hello"))
        for step in (1, 3, 7, 64, 100000):
            self.assertEqual(parse(b, step=step), [("files", "a.bin", payload.encode()), ("note", None, b"hello")])

    def test_preamble_epilogue_padding(self):
        b = body((['Content-Disposition: form-data; name="f"; filename="z"'], "data"),
                 preamble="ignored preamble\r\n", epilogue="\r\ntrailing junk", pad="  \t")
        self.assertEqual(parse(b), [("f", "z", b"data")])

    def test_quoted_and_rfc5987_filenames(self):
        b = body((['Content-Disposition: form-data; name="f"; filename="we\\"ird; name.png"'], "1"),
                 (["Content-Disposition: form-data; name=\"f\"; filename*=UTF-8''na%C3%AFve.txt"], "2"),
                 (["Content-Disposition: form-data; name=\"f\"; filename=\"plain.txt\"; filename*=bogus"], "3"))
        names = [p[1] for p in parse(b)]
        self.assertEqual(names, ['we"ird; name.png', "naïve.txt", "plain.txt"])

    def test_header_folding(self):
        b = body((["Content-Disposition: form-data;", ' name="f"; filename="folded.txt"'], "ok"))
        self.assertEqual(parse(b), [("f", "folded.txt", b"ok")])

    def test_limits(self):
        many = body(*[(['Content-Disposition: form-data; name="f"; filename="a"'], "x")] * 5)
        with self.assertRaises(LimitExceeded):
            parse(many, max_parts=3)
        huge_header = body((["Content-Disposition: form-data; name=\"f\"; filename=\"" + "a" * 20000 + "\""], "x"))
        with self.assertRaises(LimitExceeded):
            parse(huge_header)

    def test_malformed(self):
        for bad in (b"no boundary at all", b"--BOUND\r\nContent-Disposition: form-data\r\n\r\nunterminated",
                    b"--BOUND\r\nBadHeader\r\n\r\nx\r\n--BOUND--",
                    b"--BOUND\r\nContent-Disposition: attachment\r\n\r\nx\r\n--BOUND--"):
            with self.assertRaises(BadRequest, msg=bad):
                parse(bad)

    def test_boundary_validation(self):
        self.assertEqual(multipart.get_boundary('multipart/form-data; boundary="a b"'), b"a b")
        for bad in ("text/plain", "multipart/form-data", "multipart/form-data; boundary=" + "x" * 71,
                    "multipart/form-data; boundary=bad\x01"):
            with self.assertRaises(BadRequest):
                multipart.get_boundary(bad)


class FilenameTests(unittest.TestCase):
    def test_safe_filename(self):
        cases = {
            "../../etc/passwd": "passwd",
            "C:\\Windows\\system32\\evil.png": "evil.png",
            "CON": "_CON", "con.txt": "_con.txt", "LPT9.jpg": "_LPT9.jpg", "COM1": "_COM1",
            "name. . .": "name", "trailing   ": "trailing", "": "download", "..": "download",
            "a\x00b\x1fc.png": "abc.png", "q<u>o:t\"e|s?*.png": "q_u_o_t_e_s__.png",
            "e\u0301.png": "\u00e9.png",
        }
        for raw, expected in cases.items():
            self.assertEqual(security.safe_filename(raw), expected, raw)
        long = "x" * 300 + ".jpeg"
        out = security.safe_filename(long)
        self.assertLessEqual(len(out), security.MAX_NAME)
        self.assertTrue(out.endswith(".jpeg"))

    def test_content_disposition(self):
        cd = security.content_disposition("Grüße €.png")
        self.assertTrue(cd.startswith('attachment; filename="Gru_e _.png"') or 'filename="' in cd)
        self.assertIn("filename*=UTF-8''Gr%C3%BC%C3%9Fe%20%E2%82%AC.png", cd)
        self.assertNotIn("\n", cd)


class HostOriginTests(unittest.TestCase):
    def test_hosts(self):
        allowed = security.allowed_hosts("127.0.0.1", 8080)
        security.check_host("127.0.0.1:8080", allowed)
        security.check_host("LOCALHOST:8080", allowed)
        for bad in (None, "", "127.0.0.1", "127.0.0.1:8081", "example.com:8080", "127.0.0.1:8080.evil"):
            with self.assertRaises(Forbidden):
                security.check_host(bad, allowed)
        explicit = security.allowed_hosts("192.168.1.5", 9000)
        security.check_host("192.168.1.5:9000", explicit)

    def test_csrf(self):
        allowed = security.allowed_hosts("127.0.0.1", 8080)
        security.check_csrf({"Origin": "http://127.0.0.1:8080", "X-OpenConverter": "1"}, allowed)
        for headers in ({"Origin": "http://127.0.0.1:8080"}, {"X-OpenConverter": "1"},
                        {"Origin": "http://127.0.0.1:8080/x", "X-OpenConverter": "1"},
                        {"Origin": "http://evil:8080", "X-OpenConverter": "1"},
                        {"Origin": "http://127.0.0.1:8080", "X-OpenConverter": "true"}):
            with self.assertRaises(Forbidden, msg=headers):
                security.check_csrf(headers, allowed)


if __name__ == "__main__":
    unittest.main()
