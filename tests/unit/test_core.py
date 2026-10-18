import json
import os
import threading
import time
import unittest
from pathlib import Path

from openconverter.config import DEFAULT_SETTINGS, Settings
from openconverter.core import detect
from openconverter.core.files import clean_display_name, store_upload_bytes
from openconverter.core.history import History
from openconverter.core.jobs import CANCELLED, DONE, FAILED, JobManager
from openconverter.core.progress import JobProgress
from openconverter.core.registry import REGISTRY
from openconverter.core.storage import Storage, check_id, new_id
from openconverter.errors import BadRequest, ConversionCancelled, NotFound, UnsupportedFormat
from tests.helpers import FIXTURES, TempDir, gradient, noise
from openconverter.codecs import png


def wait_final(job, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        if job.state in (DONE, FAILED, CANCELLED):
            return job.state
        time.sleep(0.02)
    raise AssertionError("job did not finish")


class RegistryTests(unittest.TestCase):
    def test_targets(self):
        self.assertIn("jpeg", REGISTRY.targets("gif"))
        self.assertNotIn("gif", REGISTRY.targets("png"))
        self.assertEqual(set(REGISTRY.targets("csv")), {"csv", "tsv", "json"})
        self.assertEqual(REGISTRY.targets("txt"), ["txt"])

    def test_ownership(self):
        own = {f.id: f.ownership for f in REGISTRY.all()}
        self.assertEqual(own.pop("txt"), "PARTIAL")
        self.assertEqual(own.pop("json"), "PARTIAL")
        self.assertEqual(set(own.values()), {"OWN"})

    def test_option_validation(self):
        v = REGISTRY.validate_options
        self.assertEqual(v("png", "jpeg", {"quality": 50}), {"quality": 50})
        self.assertEqual(v("png", "jpeg", {"background": "#abcdef"}), {"background": "#ABCDEF"})
        for bad in ({"quality": 0}, {"quality": 101}, {"quality": "high"}, {"quality": True},
                    {"nope": 1}, {"subsampling": "4:1:1"}, {"background": "red"},
                    {"optimize_huffman": "yes"}):
            with self.assertRaises(BadRequest, msg=bad):
                v("png", "jpeg", bad)
        with self.assertRaises(BadRequest):
            v("png", "png", {"quality": 80})  # jpeg option on a png target
        with self.assertRaises(BadRequest):
            v("png", "jpeg", [1, 2])
        self.assertEqual(v("csv", "json", {"indent": 4, "has_header": False}), {"indent": 4, "has_header": False})
        with self.assertRaises(BadRequest):
            v("csv", "json", {"indent": 3})

    def test_json_description(self):
        doc = REGISTRY.to_json({"quality": 70})
        jpeg = next(f for f in doc["formats"] if f["id"] == "jpeg")
        self.assertEqual(jpeg["write_options"]["quality"]["default"], 70)
        self.assertEqual(len(doc["formats"]), 13)
        json.dumps(doc)

    def test_unsupported_pair(self):
        with self.assertRaises(UnsupportedFormat):
            REGISTRY.convert(b"", "png", "csv", {})


class DetectTests(unittest.TestCase):
    def test_images_by_content(self):
        for name, fmt in (("alpha.png", "png"), ("photo.jpg", "jpeg"), ("palette.gif", "gif"),
                          ("alpha.bmp", "bmp"), ("gradient.tga", "tga"), ("alpha.qoi", "qoi"),
                          ("gradient.ppm", "ppm"), ("grey.pgm", "pgm"), ("bits.pbm", "pbm")):
            data = (FIXTURES / "images" / name).read_bytes()
            self.assertEqual(detect.detect(data, "misleading.txt"), fmt)
            self.assertTrue(detect.extension_mismatch(fmt, "misleading.txt"))
            self.assertFalse(detect.extension_mismatch(fmt, name))

    def test_text_kinds(self):
        self.assertEqual(detect.detect(b"a,b\n1,2\n", "x.csv"), "csv")
        self.assertEqual(detect.detect(b"a,b\n1,2\n", ""), "csv")
        self.assertEqual(detect.detect(b"a\tb\n1\t2\n", ""), "tsv")
        self.assertEqual(detect.detect(b'[{"a": 1}]', ""), "json")
        self.assertEqual(detect.detect(b"just some words\nand more", ""), "txt")
        self.assertEqual(detect.detect(b"a,b\n1,2\n", "notes.txt"), "txt")
        self.assertEqual(detect.detect("hi".encode("utf-16"), "x.txt"), "txt")
        self.assertEqual(detect.detect(b"", "empty.txt"), "txt")

    def test_unsupported(self):
        for data in (b"%PDF-1.7 ...", b"PK\x03\x04....", b"\x00\x01\x02\x03\x04" * 20, b"RIFF\x00\x00WEBP"):
            with self.assertRaises(UnsupportedFormat):
                detect.detect(data, "file.bin")

    def test_display_name(self):
        self.assertEqual(clean_display_name("..\\..\\evil/na\u0000me.png"), "name.png")
        self.assertEqual(clean_display_name(""), "file")


class StorageTests(unittest.TestCase):
    def test_ids_validated(self):
        with TempDir() as d:
            st = Storage(d)
            for bad in ("../x", "ABC", "0" * 31, "g" * 32, "0" * 32 + "/", None):
                with self.assertRaises(NotFound):
                    st.upload_path(bad)
            check_id(new_id())

    def test_store_output_atomic_and_meta(self):
        with TempDir() as d:
            st = Storage(d)
            oid = st.store_output(b"data", {"output_name": "a.png"})
            self.assertEqual(st.output_path(oid).read_bytes(), b"data")
            self.assertEqual(st.output_meta(oid)["size"], 4)
            self.assertEqual(list(st.temp.iterdir()), [])

    def test_cleanup_respects_protected_and_age(self):
        with TempDir() as d:
            st = Storage(d)
            old = st.store_output(b"1", {})
            keep = st.store_output(b"2", {})
            fresh = st.store_output(b"3", {})
            past = time.time() - 48 * 3600
            for oid in (old, keep):
                os.utime(st.output_path(oid), (past, past))
            st.cleanup(24, protected={keep})
            self.assertFalse(st.output_exists(old))
            self.assertTrue(st.output_exists(keep))
            self.assertTrue(st.output_exists(fresh))


class SettingsTests(unittest.TestCase):
    def test_defaults_and_persistence(self):
        with TempDir() as d:
            s = Settings(d / "settings.json")
            self.assertEqual(s.get(), DEFAULT_SETTINGS)
            s.update({"default_jpeg_quality": 60, "theme": "dark"})
            self.assertEqual(Settings(d / "settings.json").get()["default_jpeg_quality"], 60)

    def test_validation(self):
        with TempDir() as d:
            s = Settings(d / "settings.json")
            for bad in ({"unknown": 1}, {"default_jpeg_quality": 0}, {"theme": "blue"},
                        {"alpha_background": "#12345"}, {"max_upload_mb": "x"}):
                with self.assertRaises(BadRequest, msg=bad):
                    s.update(bad)

    def test_corrupt_file_gives_defaults(self):
        with TempDir() as d:
            (d / "settings.json").write_text("{not json", encoding="utf-8")
            self.assertEqual(Settings(d / "settings.json").get(), DEFAULT_SETTINGS)
            (d / "settings.json").write_text('{"default_png_level": 99, "theme": "dark"}', encoding="utf-8")
            self.assertEqual(Settings(d / "settings.json").get()["theme"], "dark")
            self.assertEqual(Settings(d / "settings.json").get()["default_png_level"], 6)


class HistoryTests(unittest.TestCase):
    def test_append_delete_clear_expired(self):
        with TempDir() as d:
            h = History(d / "history.jsonl")
            h.append({"id": "a", "status": "done", "output_id": "x"})
            h.append({"id": "b", "status": "failed"})
            with open(d / "history.jsonl", "a", encoding="utf-8") as fh:
                fh.write("garbage line\n")
            items = h.entries(output_exists=lambda oid: False)
            self.assertEqual([i["id"] for i in items], ["b", "a"])
            self.assertTrue(items[1]["expired"])
            self.assertTrue(h.delete("a"))
            self.assertFalse(h.delete("a"))
            self.assertEqual([i["id"] for i in h.entries()], ["b"])
            h.clear()
            self.assertEqual(h.entries(), [])

    def test_concurrent_appends(self):
        with TempDir() as d:
            h = History(d / "history.jsonl")
            threads = [threading.Thread(target=lambda i=i: [h.append({"id": f"{i}-{k}"}) for k in range(50)])
                       for i in range(8)]
            for t in threads:
                t.start()
            for t in threads:
                t.join()
            self.assertEqual(len(h.entries()), 400)


class ProgressTests(unittest.TestCase):
    def test_monotonic_and_throttled(self):
        class FakeJob:
            progress = 0.0
            stage = "queued"
            lock = threading.Lock()
            cancel_event = threading.Event()
        job = FakeJob()
        events = []
        clock = [0.0]
        p = JobProgress(job, lambda j, k: events.append(j.progress), clock=lambda: clock[0])
        p.report(50, 100, "decoding")
        p.report(40, 100, "decoding")
        self.assertEqual(job.progress, 50)
        for i in range(100):
            p.report(50 + i // 4, 100, "decoding")
        self.assertEqual(len(events), 1)  # throttled: no time has passed
        clock[0] = 0.2
        p.report(80, 100, "encoding")
        self.assertEqual(events[-1], 80)
        p.report(200, 100, "encoding")
        self.assertEqual(job.progress, 100)
        job.cancel_event.set()
        with self.assertRaises(ConversionCancelled):
            p.check_cancelled()


class JobTests(unittest.TestCase):
    def setUp(self):
        self.tmp = TempDir()
        self.dir = self.tmp.__enter__()
        self.storage = Storage(self.dir)
        self.settings = Settings(self.dir / "settings.json")
        self.history = History(self.dir / "history.jsonl")
        self.jobs = JobManager(self.storage, self.settings, self.history, max_workers=2)

    def tearDown(self):
        self.jobs.shutdown()
        self.tmp.__exit__(None, None, None)

    def upload(self, data, name):
        return store_upload_bytes(self.storage, data, name)

    def test_conversion_and_history(self):
        f = self.upload((FIXTURES / "images" / "alpha.png").read_bytes(), "pic.png")
        job = self.jobs.submit(f["file_id"], "jpeg", {})
        self.assertEqual(wait_final(job), DONE)
        snap = job.snapshot()
        self.assertEqual(snap["progress"], 100)
        res = snap["result"]
        self.assertEqual(res["output_name"], "pic.jpg")
        self.assertTrue(any("Alpha flattened" in n for n in res["notes"]))
        self.assertTrue(self.storage.output_exists(res["output_id"]))
        self.assertEqual(job.options["quality"], 85)
        hist = self.history.entries(self.storage.output_exists)
        self.assertEqual(hist[0]["status"], "done")
        self.assertFalse(hist[0]["expired"])

    def test_settings_defaults_applied(self):
        self.settings.update({"default_jpeg_quality": 42, "alpha_background": "#000000"})
        f = self.upload((FIXTURES / "images" / "alpha.png").read_bytes(), "pic.png")
        job = self.jobs.submit(f["file_id"], "jpeg", {})
        wait_final(job)
        self.assertEqual(job.options["quality"], 42)
        self.assertIn("Alpha flattened onto #000000.", job.result["notes"])

    def test_failure_is_typed(self):
        data = bytearray((FIXTURES / "images" / "photo.png").read_bytes())
        f = self.upload(bytes(data[:len(data) // 2]), "broken.png")
        job = self.jobs.submit(f["file_id"], "bmp", {})
        self.assertEqual(wait_final(job), FAILED)
        self.assertEqual(job.error["code"], "corrupt_file")

    def test_rejections(self):
        f = self.upload(b"a,b\n1,2\n", "t.csv")
        with self.assertRaises(UnsupportedFormat):
            self.jobs.submit(f["file_id"], "png", {})
        with self.assertRaises(BadRequest):
            self.jobs.submit(f["file_id"], "json", {"bogus": 1})
        with self.assertRaises(NotFound):
            self.jobs.submit(new_id(), "json", {})
        with self.assertRaises(NotFound):
            self.jobs.submit("../../etc", "json", {})

    def test_cancel_running_and_queued_leave_no_output(self):
        big = png.encode(noise(700, 700, "RGB"), compression_level=0)
        f = self.upload(big, "big.png")
        jobs = [self.jobs.submit(f["file_id"], "png", {"compression_level": 9}) for _ in range(4)]
        # Wait for the first to be running, then cancel everything.
        end = time.time() + 30
        while jobs[0].state != "running" and time.time() < end:
            time.sleep(0.01)
        time.sleep(0.3)
        for j in jobs:
            self.jobs.cancel(j.id)
        for j in jobs:
            self.assertEqual(wait_final(j), CANCELLED)
        self.assertEqual(list(self.storage.outputs.iterdir()), [])
        self.assertEqual([p for p in self.storage.temp.iterdir()], [])

    def test_concurrent_jobs(self):
        files = [self.upload(png.encode(gradient(40 + i, 30, "RGB")), f"g{i}.png") for i in range(6)]
        jobs = [self.jobs.submit(f["file_id"], t, {}) for f in files for t in ("jpeg", "bmp", "qoi")]
        for j in jobs:
            self.assertEqual(wait_final(j), DONE, j.error)
        ids = {j.result["output_id"] for j in jobs}
        self.assertEqual(len(ids), len(jobs))

    def test_events(self):
        f = self.upload((FIXTURES / "images" / "photo.png").read_bytes(), "p.png")
        job = self.jobs.submit(f["file_id"], "jpeg", {})
        q = self.jobs.subscribe(job.id)
        seen = []
        while True:
            kind, snap = q.get(timeout=30)
            seen.append(snap)
            if snap["state"] in (DONE, FAILED, CANCELLED):
                break
        progress = [s["progress"] for s in seen]
        self.assertEqual(progress, sorted(progress))
        self.assertEqual(seen[-1]["state"], DONE)


if __name__ == "__main__":
    unittest.main()
