import http.client
import json
import socket
import time
import unittest

from openconverter.codecs import png
from openconverter.converters.image_converter import decode_image
from openconverter.core import detect
from openconverter.core.registry import REGISTRY
from tests.helpers import FIXTURES, noise
from tests.server_helpers import LiveServer

SOURCES = {
    "png": "images/alpha.png", "jpeg": "images/photo.jpg", "gif": "images/palette.gif", "bmp": "images/alpha.bmp",
    "tga": "images/gradient.tga", "qoi": "images/alpha.qoi", "ppm": "images/rgb16.ppm", "pgm": "images/grey.pgm",
    "pbm": "images/bits.pbm", "csv": "data/people.csv", "tsv": "data/people.tsv", "json": "data/people.json",
    "txt": "text/hello_utf8.txt",
}
SECURITY_HEADERS = ("Content-Security-Policy", "X-Content-Type-Options", "Referrer-Policy",
                    "Cross-Origin-Resource-Policy")


class ServerTestCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv = LiveServer(max_workers=2)

    @classmethod
    def tearDownClass(cls):
        cls.srv.close()

    def assertSecure(self, resp):
        for h in SECURITY_HEADERS:
            self.assertTrue(resp.getheader(h), h)
        self.assertIn("frame-ancestors 'none'", resp.getheader("Content-Security-Policy"))

    def assertError(self, status, body, expected_status, code):
        self.assertEqual(status, expected_status, body)
        self.assertEqual(set(body), {"error"})
        self.assertEqual(body["error"]["code"], code)
        self.assertIsInstance(body["error"]["message"], str)
        self.assertNotIn("Traceback", json.dumps(body))


class BasicApiTests(ServerTestCase):
    def test_health_and_formats(self):
        status, body, resp = self.srv.json("GET", "/api/health")
        self.assertEqual((status, body), (200, {"status": "ok", "version": "1.0.0"}))
        self.assertEqual(resp.getheader("Content-Type"), "application/json; charset=utf-8")
        self.assertEqual(resp.getheader("Cache-Control"), "no-store")
        self.assertSecure(resp)
        status, body, _ = self.srv.json("GET", "/api/formats")
        self.assertEqual({f["id"] for f in body["formats"]}, set(SOURCES))

    def test_static(self):
        resp, data = self.srv.request("GET", "/")
        self.assertEqual(resp.status, 200)
        self.assertTrue(resp.getheader("Content-Type").startswith("text/html"))
        self.assertSecure(resp)
        resp, data = self.srv.request("HEAD", "/index.html")
        self.assertEqual((resp.status, data), (200, b""))
        self.assertGreater(int(resp.getheader("Content-Length")), 0)

    def test_static_traversal(self):
        for path in ("/../README.md", "/%2e%2e/README.md", "/..%2fREADME.md", "/%5c..%5cREADME.md",
                     "/..\\README.md", "/C:/Windows/win.ini", "/index.html%00", "/%252e%252e/README.md",
                     "//etc/passwd", "/./index.html", "/missing.html", "/js/../../README.md", "/%ff"):
            resp, data = self.srv.request("GET", path)
            self.assertEqual(resp.status, 404, path)
            self.assertSecure(resp)
        resp, _ = self.srv.request("POST", "/index.html", b"")
        self.assertEqual(resp.status, 405)

    def test_unknown_api_and_method(self):
        status, body, resp = self.srv.json("GET", "/api/nope")
        self.assertError(status, body, 404, "not_found")
        status, body, _ = self.srv.json("PUT", "/api/health", {})
        self.assertError(status, body, 405, "method_not_allowed")
        for path in ("/api/outputs/../../secret/download", "/api/outputs/ABC/download",
                     "/api/outputs/" + "0" * 32 + "/download", "/api/jobs/" + "z" * 32):
            status, body, _ = self.srv.json("GET", path)
            self.assertError(status, body, 404, "not_found")


class SecurityTests(ServerTestCase):
    def test_host_header_rejected(self):
        for host in ("evil.example", f"evil.example:{self.srv.port}", "127.0.0.1:1", f"127.0.0.2:{self.srv.port}"):
            for path in ("/api/health", "/", "/api/jobs/" + "0" * 32 + "/events",
                         "/api/outputs/" + "0" * 32 + "/download"):
                resp, data = self.srv.request("GET", path, host=host)
                self.assertEqual(resp.status, 403, (host, path))
                self.assertEqual(json.loads(data)["error"]["code"], "forbidden")
                self.assertSecure(resp)
        for host in (f"localhost:{self.srv.port}", f"[::1]:{self.srv.port}"):
            resp, _ = self.srv.request("GET", "/api/health", host=host)
            self.assertEqual(resp.status, 200, host)

    def test_csrf(self):
        body = json.dumps({"theme": "dark"}).encode()
        ct = {"Content-Type": "application/json"}
        cases = [
            ({}, 403),  # no Origin, no Referer, no header
            ({"Origin": "http://evil.example", "X-OpenConverter": "1"}, 403),
            ({"Origin": f"http://127.0.0.1:{self.srv.port}"}, 403),  # missing X-OpenConverter
            ({"Origin": f"https://127.0.0.1:{self.srv.port}", "X-OpenConverter": "1"}, 403),
            ({"Origin": "null", "X-OpenConverter": "1"}, 403),
            ({"Referer": "http://evil.example/page", "X-OpenConverter": "1"}, 403),
            ({"Referer": f"http://127.0.0.1:{self.srv.port}/", "X-OpenConverter": "1"}, 200),
            ({"Origin": f"http://localhost:{self.srv.port}", "X-OpenConverter": "1"}, 200),
        ]
        for headers, expected in cases:
            resp, data = self.srv.request("PUT", "/api/settings", body, headers=dict(ct, **headers), csrf=False)
            self.assertEqual(resp.status, expected, headers)
            self.assertSecure(resp)
        self.srv.json("PUT", "/api/settings", {"theme": "system"})

    def test_json_endpoint_hygiene(self):
        resp, data = self.srv.request("PUT", "/api/settings", b'{"theme":"dark"}', headers={"Content-Type": "text/plain"})
        self.assertEqual(resp.status, 400)
        resp, data = self.srv.request("PUT", "/api/settings", b"{bad json", headers={"Content-Type": "application/json"})
        self.assertEqual(resp.status, 400)
        big = b'{"theme": "' + b"x" * (1024 * 1024 + 10) + b'"}'
        resp, data = self.srv.request("PUT", "/api/settings", big, headers={"Content-Type": "application/json"})
        self.assertEqual(resp.status, 413)

    def test_chunked_rejected(self):
        conn = http.client.HTTPConnection("127.0.0.1", self.srv.port, timeout=10)
        conn.putrequest("POST", "/api/jobs", skip_host=True)
        conn.putheader("Host", f"127.0.0.1:{self.srv.port}")
        conn.putheader("Origin", self.srv.origin)
        conn.putheader("X-OpenConverter", "1")
        conn.putheader("Content-Type", "application/json")
        conn.putheader("Transfer-Encoding", "chunked")
        conn.endheaders()
        conn.send(b"2\r\n{}\r\n0\r\n\r\n")
        resp = conn.getresponse()
        self.assertEqual(resp.status, 400)
        conn.close()

    def test_malformed_request_line_gets_json(self):
        with socket.create_connection(("127.0.0.1", self.srv.port), timeout=10) as s:
            s.sendall(b"GARBAGE\r\n\r\n")
            data = s.recv(65536)
        self.assertIn(b"400", data.split(b"\r\n")[0])
        self.assertIn(b"Content-Security-Policy", data)

    def test_slow_client_does_not_hang_forever(self):
        app = self.srv.app
        from openconverter.server.app import Handler
        self.assertLessEqual(Handler.timeout, 60)
        self.assertIsNotNone(app)


class UploadTests(ServerTestCase):
    def test_upload_multiple_and_bad_file(self):
        png_data = (FIXTURES / "images/alpha.png").read_bytes()
        status, body = self.srv.upload([("a.png", png_data), ("weird.bin", b"\x00\x01\x02" * 50),
                                        ("b.jpg", png_data), ("empty.png", b"")])
        self.assertEqual(status, 200)
        a, bad, b, empty = body["files"]
        self.assertEqual((a["detected_format"], a["width"], a["height"]), ("png", 33, 17))
        self.assertIn("jpeg", a["available_targets"])
        self.assertFalse(a["extension_mismatch"])
        self.assertEqual(bad["error"]["code"], "unsupported_format")
        self.assertTrue(b["extension_mismatch"])
        self.assertEqual(empty["error"]["code"], "bad_request")

    def test_upload_limit_413_and_no_leftovers(self):
        self.srv.json("PUT", "/api/settings", {"max_upload_mb": 1})
        try:
            before = set(self.srv.app.storage.uploads.iterdir())
            body, ctype = self.srv.multipart([("big.png", b"\x89PNG" + bytes(2 * 1024 * 1024))])
            resp, data = self.srv.request("POST", "/api/files", body, headers={"Content-Type": ctype})
            self.assertEqual(resp.status, 413)
            self.assertEqual(json.loads(data)["error"]["code"], "limit_exceeded")
            # streamed case: declared length within the slack, actual part over the limit
            body, ctype = self.srv.multipart([("a.png", bytes(600 * 1024)), ("b.png", bytes(600 * 1024))])
            resp, data = self.srv.request("POST", "/api/files", body, headers={"Content-Type": ctype})
            self.assertEqual(resp.status, 413)
            self.assertEqual(set(self.srv.app.storage.uploads.iterdir()), before)
        finally:
            self.srv.json("PUT", "/api/settings", {"max_upload_mb": 200})

    def test_multipart_errors(self):
        resp, data = self.srv.request("POST", "/api/files", b"x", headers={"Content-Type": "text/plain"})
        self.assertEqual(resp.status, 400)
        resp, data = self.srv.request("POST", "/api/files", b"no boundary here",
                                      headers={"Content-Type": "multipart/form-data; boundary=abc"})
        self.assertEqual(resp.status, 400)
        resp, data = self.srv.request("POST", "/api/files", b"--abc--\r\n",
                                      headers={"Content-Type": "multipart/form-data; boundary=abc"})
        self.assertEqual(resp.status, 400)  # no files

    def test_rfc5987_filename(self):
        boundary = "XyZ"
        png_data = (FIXTURES / "images/alpha.png").read_bytes()
        body = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"fallback.png\"; "
                f"filename*=UTF-8''%E2%82%AC%20price.png\r\n\r\n").encode() + png_data + f"\r\n--{boundary}--\r\n".encode()
        resp, data = self.srv.request("POST", "/api/files", body,
                                      headers={"Content-Type": f"multipart/form-data; boundary={boundary}"})
        self.assertEqual(json.loads(data)["files"][0]["original_name"], "€ price.png")

    def test_preview(self):
        status, body = self.srv.upload([("p.png", png.encode(noise(700, 300, "RGBA")))])
        fid = body["files"][0]["file_id"]
        resp, data = self.srv.request("GET", f"/api/files/{fid}/preview")
        self.assertEqual(resp.status, 200)
        self.assertEqual(resp.getheader("Content-Type"), "image/png")
        thumb = png.decode(data)
        self.assertLessEqual(max(thumb.width, thumb.height), 512)
        status, body = self.srv.upload([("t.csv", b"a,b\n1,2\n")])
        resp, data = self.srv.request("GET", f"/api/files/{body['files'][0]['file_id']}/preview")
        self.assertEqual(resp.status, 415)


class ConversionApiTests(ServerTestCase):
    def test_every_required_conversion_end_to_end(self):
        for src, rel in SOURCES.items():
            data = (FIXTURES / rel).read_bytes()
            for target in REGISTRY.targets(src):
                with self.subTest(src=src, target=target):
                    snap, entry = self.srv.convert(data, rel.split("/")[-1], target)
                    self.assertEqual(snap["state"], "done", snap)
                    resp, out = self.srv.download(snap["result"]["output_id"])
                    self.assertEqual(resp.status, 200)
                    self.assertEqual(len(out), snap["result"]["size"])
                    self.assertIn("attachment;", resp.getheader("Content-Disposition"))
                    self.assertEqual(detect.detect(out, snap["result"]["output_name"]), target)
                    if REGISTRY.get(target).category == "image":
                        img = decode_image(out, target)
                        self.assertEqual((img.width, img.height), (entry["width"], entry["height"]))

    def test_options_validation(self):
        status, body = self.srv.upload([("a.png", (FIXTURES / "images/alpha.png").read_bytes())])
        fid = body["files"][0]["file_id"]
        for opts in ({"bogus": 1}, {"quality": 0}, {"quality": 101}, {"quality": "x"}, {"compression_level": 5}):
            status, body, _ = self.srv.json("POST", "/api/jobs", {"file_id": fid, "target_format": "jpeg",
                                                                  "options": opts})
            self.assertError(status, body, 400, "bad_request")
        status, body, _ = self.srv.json("POST", "/api/jobs", {"file_id": fid, "target_format": "csv"})
        self.assertError(status, body, 415, "unsupported_format")
        status, body, _ = self.srv.json("POST", "/api/jobs", {"file_id": fid, "target_format": "jpeg", "x": 1})
        self.assertError(status, body, 400, "bad_request")
        status, body, _ = self.srv.json("POST", "/api/jobs", {"file_id": "0" * 32, "target_format": "jpeg"})
        self.assertError(status, body, 404, "not_found")

    def test_failure_reports_typed_error(self):
        data = (FIXTURES / "images/photo.png").read_bytes()
        snap, _ = self.srv.convert(data[:len(data) - 300], "trunc.png", "jpeg")
        self.assertEqual(snap["state"], "failed")
        self.assertEqual(snap["error"]["code"], "corrupt_file")

    def test_sse_progress_monotonic(self):
        status, body = self.srv.upload([("n.png", png.encode(noise(300, 200, "RGB"), compression_level=1))])
        fid = body["files"][0]["file_id"]
        status, job, _ = self.srv.json("POST", "/api/jobs", {"file_id": fid, "target_format": "png",
                                                             "options": {"compression_level": 6}})
        conn = http.client.HTTPConnection("127.0.0.1", self.srv.port, timeout=60)
        conn.request("GET", f"/api/jobs/{job['job_id']}/events", headers={"Host": f"127.0.0.1:{self.srv.port}"})
        resp = conn.getresponse()
        self.assertEqual(resp.status, 200)
        self.assertTrue(resp.getheader("Content-Type").startswith("text/event-stream"))
        self.assertSecure(resp)
        raw = resp.read().decode("utf-8")
        conn.close()
        events = []
        for block in raw.split("\n\n"):
            data_lines = [ln[6:] for ln in block.split("\n") if ln.startswith("data: ")]
            if data_lines:
                events.append(json.loads("".join(data_lines)))
        progress = [e["progress"] for e in events]
        self.assertEqual(progress, sorted(progress))
        self.assertEqual(events[-1]["state"], "done")
        self.assertEqual(events[-1]["progress"], 100)
        self.assertGreaterEqual(len(events), 3)

    def test_cancel_queued_and_running(self):
        big = png.encode(noise(800, 800, "RGB"), compression_level=0)
        status, body = self.srv.upload([("big.png", big)])
        fid = body["files"][0]["file_id"]
        jobs = []
        for _ in range(4):  # two workers: two run, two wait in the queue
            status, job, _ = self.srv.json("POST", "/api/jobs", {"file_id": fid, "target_format": "png",
                                                                 "options": {"compression_level": 9}})
            jobs.append(job["job_id"])
        deadline = time.time() + 30
        while time.time() < deadline:
            states = [self.srv.json("GET", f"/api/jobs/{j}")[1]["state"] for j in jobs]
            if states.count("running") >= 1:
                break
            time.sleep(0.05)
        self.assertIn("queued", states)
        before = set(self.srv.app.storage.outputs.iterdir())
        for j in jobs:
            status, snap, _ = self.srv.json("POST", f"/api/jobs/{j}/cancel")
            self.assertEqual(status, 200)
        for j in jobs:
            self.assertEqual(self.srv.wait(j)["state"], "cancelled")
        self.assertEqual(set(self.srv.app.storage.outputs.iterdir()), before)
        self.assertEqual(list(self.srv.app.storage.temp.glob("*.part")), [])

    def test_concurrent_jobs(self):
        import threading
        data = (FIXTURES / "images/photo.png").read_bytes()
        results = []

        def worker(t):
            results.append(self.srv.convert(data, "p.png", t)[0]["state"])
        threads = [threading.Thread(target=worker, args=(t,)) for t in ("jpeg", "bmp", "qoi", "tga", "ppm", "png")]
        for t in threads:
            t.start()
        for t in threads:
            t.join(120)
        self.assertEqual(results, ["done"] * 6)

    def test_download_name_sanitised(self):
        data = (FIXTURES / "images/alpha.png").read_bytes()
        snap, _ = self.srv.convert(data, 'CON..\\evil"<>.png', "bmp")
        resp, _ = self.srv.download(snap["result"]["output_id"])
        cd = resp.getheader("Content-Disposition")
        self.assertTrue(cd.startswith('attachment; filename="'), cd)
        self.assertNotIn("\\", cd)
        self.assertIn("filename*=UTF-8''", cd)
        snap, _ = self.srv.convert(data, "CON.png", "bmp")
        resp, _ = self.srv.download(snap["result"]["output_id"])
        self.assertIn('filename="_CON.bmp"', resp.getheader("Content-Disposition"))


class HistorySettingsTests(ServerTestCase):
    def test_history_round_trip(self):
        data = (FIXTURES / "images/alpha.png").read_bytes()
        snap, _ = self.srv.convert(data, "h.png", "qoi")
        status, body, _ = self.srv.json("GET", "/api/history")
        entry = next(e for e in body["entries"] if e["id"] == snap["job_id"])
        self.assertEqual((entry["status"], entry["expired"], entry["target_format"]), ("done", False, "qoi"))
        self.srv.app.storage.delete_output(snap["result"]["output_id"])
        status, body, _ = self.srv.json("GET", "/api/history")
        self.assertTrue(next(e for e in body["entries"] if e["id"] == snap["job_id"])["expired"])
        status, body, _ = self.srv.json("DELETE", f"/api/history/{snap['job_id']}")
        self.assertEqual(status, 200)
        status, body, _ = self.srv.json("DELETE", f"/api/history/{snap['job_id']}")
        self.assertEqual(status, 404)
        status, body, _ = self.srv.json("DELETE", "/api/history")
        self.assertEqual(self.srv.json("GET", "/api/history")[1]["entries"], [])

    def test_settings_round_trip(self):
        status, body, _ = self.srv.json("GET", "/api/settings")
        self.assertEqual(body["default_jpeg_quality"], 85)
        status, body, _ = self.srv.json("PUT", "/api/settings", {"default_jpeg_quality": 70, "theme": "dark"})
        self.assertEqual((status, body["default_jpeg_quality"]), (200, 70))
        status, fm, _ = self.srv.json("GET", "/api/formats")
        jpeg = next(f for f in fm["formats"] if f["id"] == "jpeg")
        self.assertEqual(jpeg["write_options"]["quality"]["default"], 70)
        for bad in ({"nope": 1}, {"default_jpeg_quality": 500}, {"theme": 3}):
            status, body, _ = self.srv.json("PUT", "/api/settings", bad)
            self.assertError(status, body, 400, "bad_request")
        self.srv.json("PUT", "/api/settings", {"default_jpeg_quality": 85, "theme": "system"})


if __name__ == "__main__":
    unittest.main()
