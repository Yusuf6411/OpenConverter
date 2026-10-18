"""Live-server helpers for integration tests (ephemeral ports, temp data dirs)."""

import http.client
import json
import time
import uuid

from openconverter.server.app import App
from tests.helpers import TempDir


class LiveServer:
    def __init__(self, max_workers=2):
        self.tmp = TempDir()
        self.data_dir = self.tmp.__enter__()
        self.app = App(self.data_dir, "127.0.0.1", 0, max_workers=max_workers)
        self.app.bind()
        self.port = self.app.port
        self.thread = self.app.serve_in_thread()
        self.origin = f"http://127.0.0.1:{self.port}"

    def close(self):
        self.app.shutdown()
        self.thread.join(timeout=10)
        self.tmp.__exit__(None, None, None)

    # ---- raw requests ----
    def request(self, method, path, body=None, headers=None, host=None, csrf=True, timeout=30):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=timeout)
        h = {"Host": host or f"127.0.0.1:{self.port}"}
        if csrf and method not in ("GET", "HEAD"):
            h["Origin"] = self.origin
            h["X-OpenConverter"] = "1"
        if headers:
            h.update(headers)
        h = {k: v for k, v in h.items() if v is not None}
        conn.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
        for k, v in h.items():
            conn.putheader(k, v)
        if body is not None:
            conn.putheader("Content-Length", str(len(body)))
        conn.endheaders()
        if body is not None:
            conn.send(body)
        resp = conn.getresponse()
        data = resp.read()
        conn.close()
        return resp, data

    def json(self, method, path, obj=None, **kw):
        body = None
        headers = kw.pop("headers", {}) or {}
        if obj is not None:
            body = json.dumps(obj).encode("utf-8")
            headers.setdefault("Content-Type", "application/json")
        resp, data = self.request(method, path, body, headers=headers, **kw)
        return resp.status, (json.loads(data) if data else None), resp

    # ---- multipart ----
    @staticmethod
    def multipart(files, boundary=None):
        boundary = boundary or "----oc" + uuid.uuid4().hex
        out = bytearray()
        for name, data in files:
            out += f"--{boundary}\r\n".encode()
            out += f'Content-Disposition: form-data; name="files"; filename="{name}"\r\n'.encode("utf-8")
            out += b"Content-Type: application/octet-stream\r\n\r\n"
            out += data + b"\r\n"
        out += f"--{boundary}--\r\n".encode()
        return bytes(out), f"multipart/form-data; boundary={boundary}"

    def upload(self, files):
        body, ctype = self.multipart(files)
        resp, data = self.request("POST", "/api/files", body, headers={"Content-Type": ctype})
        return resp.status, json.loads(data)

    def convert(self, data, name, target, options=None, timeout=120):
        status, up = self.upload([(name, data)])
        assert status == 200, up
        entry = up["files"][0]
        assert "error" not in entry, entry
        status, job, _ = self.json("POST", "/api/jobs", {"file_id": entry["file_id"], "target_format": target,
                                                         "options": options or {}})
        assert status == 201, job
        return self.wait(job["job_id"], timeout), entry

    def wait(self, job_id, timeout=120):
        end = time.time() + timeout
        while time.time() < end:
            status, snap, _ = self.json("GET", f"/api/jobs/{job_id}")
            if snap["state"] in ("done", "failed", "cancelled"):
                return snap
            time.sleep(0.05)
        raise AssertionError("job did not finish")

    def download(self, output_id):
        resp, data = self.request("GET", f"/api/outputs/{output_id}/download")
        return resp, data
