"""Background conversion jobs on a thread pool."""

import logging
import os
import queue
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import PurePath

from openconverter.core.progress import JobProgress
from openconverter.core.registry import REGISTRY
from openconverter.core.storage import check_id, new_id
from openconverter.errors import (BadRequest, ConversionCancelled, NotFound, OpenConverterError,
                                  UnsupportedFormat)
from openconverter.limits import DEFAULT_LIMITS

log = logging.getLogger("openconverter.jobs")

QUEUED, RUNNING, DONE, FAILED, CANCELLED = "queued", "running", "done", "failed", "cancelled"
FINAL = (DONE, FAILED, CANCELLED)


class Job:
    def __init__(self, job_id, file_id, source, target, options, original_name, input_size):
        self.id = job_id
        self.file_id = file_id
        self.source = source
        self.target = target
        self.options = options
        self.original_name = original_name
        self.input_size = input_size
        self.state = QUEUED
        self.progress = 0.0
        self.stage = "queued"
        self.message = ""
        self.result = None
        self.error = None
        self.created = time.time()
        self.started = None
        self.finished = None
        self.lock = threading.Lock()
        self.cancel_event = threading.Event()
        self.future = None

    def snapshot(self) -> dict:
        with self.lock:
            return {
                "job_id": self.id,
                "file_id": self.file_id,
                "source_format": self.source,
                "target_format": self.target,
                "original_name": self.original_name,
                "state": self.state,
                "progress": self.progress,
                "stage": self.stage,
                "message": self.message,
                "result": self.result,
                "error": self.error,
                "created": self.created,
            }


def output_name(original_name: str, target: str) -> str:
    stem = PurePath(original_name or "converted").stem or "converted"
    return stem + REGISTRY.get(target).primary_extension


class JobManager:
    def __init__(self, storage, settings, history, max_workers=None, limits=DEFAULT_LIMITS) -> None:
        self.storage = storage
        self.settings = settings
        self.history = history
        self.limits = limits
        self.max_workers = max_workers or min(4, os.cpu_count() or 1)
        self.pool = ThreadPoolExecutor(max_workers=self.max_workers, thread_name_prefix="oc-job")
        self._jobs: dict[str, Job] = {}
        self._lock = threading.Lock()
        self._subscribers: dict[str, list[queue.Queue]] = {}
        self._closed = False

    # ---- submission ----
    def submit(self, file_id: str, target: str, options) -> Job:
        check_id(file_id)
        meta = self.storage.upload_meta(file_id)
        source = meta.get("detected_format")
        if not source:
            raise UnsupportedFormat("The uploaded file's format is not supported.")
        if not isinstance(target, str):
            raise BadRequest("target_format must be a string.")
        REGISTRY.get(target)
        if target not in REGISTRY.targets(source):
            raise UnsupportedFormat(f"Cannot convert {source} to {target}.")
        clean = REGISTRY.validate_options(source, target, options)
        # Fill unspecified options from the user's settings, then schema defaults.
        schema = REGISTRY.option_schema(source, target)
        defaults = self.settings.option_defaults()
        full = {k: (clean[k] if k in clean else defaults.get(k, opt["default"])) for k, opt in schema.items()}
        job = Job(new_id(), file_id, source, target, full, meta.get("original_name", ""), meta.get("size", 0))
        with self._lock:
            if self._closed:
                raise OpenConverterError("The server is shutting down.")
            self._jobs[job.id] = job
        job.future = self.pool.submit(self._run, job)
        self._emit(job, "state")
        return job

    def get(self, job_id: str) -> Job:
        check_id(job_id)
        with self._lock:
            job = self._jobs.get(job_id)
        if job is None:
            raise NotFound("Job not found.")
        return job

    def list(self) -> list[Job]:
        with self._lock:
            return list(self._jobs.values())

    def active_ids(self) -> set[str]:
        """File ids and output ids that cleanup must not touch."""
        ids = set()
        with self._lock:
            for job in self._jobs.values():
                if job.state not in FINAL:
                    ids.add(job.file_id)
                    ids.add(job.id)
        return ids

    def cancel(self, job_id: str) -> Job:
        job = self.get(job_id)
        with job.lock:
            state = job.state
            if state in FINAL:
                return job
            job.cancel_event.set()
        if state == QUEUED and job.future is not None and job.future.cancel():
            self._finish(job, CANCELLED, error={"code": "cancelled", "message": "The conversion was cancelled."})
        return job

    # ---- events ----
    def subscribe(self, job_id: str) -> queue.Queue:
        job = self.get(job_id)
        q = queue.Queue(maxsize=1000)
        with self._lock:
            self._subscribers.setdefault(job.id, []).append(q)
        q.put(("state", job.snapshot()))
        return q

    def unsubscribe(self, job_id: str, q: queue.Queue) -> None:
        with self._lock:
            subs = self._subscribers.get(job_id, [])
            if q in subs:
                subs.remove(q)

    def _emit(self, job: Job, kind: str) -> None:
        snap = job.snapshot()
        with self._lock:
            subs = list(self._subscribers.get(job.id, []))
        for q in subs:
            try:
                q.put_nowait((kind, snap))
            except queue.Full:
                pass

    # ---- execution ----
    def _run(self, job: Job) -> None:
        with job.lock:
            job.state = RUNNING
            job.started = time.time()
            job.stage = "decoding"
        self._emit(job, "state")
        progress = JobProgress(job, self._emit)
        t0 = time.perf_counter()
        try:
            progress.check_cancelled()
            data = self.storage.read_upload(job.file_id)
            result = REGISTRY.convert(data, job.source, job.target, job.options, progress, self.limits)
            progress.check_cancelled()
            progress.report(99, 100, "finalizing")
            name = output_name(job.original_name, job.target)
            output_id = self.storage.store_output(result.data, {
                "original_name": job.original_name, "output_name": name, "format": job.target,
                "mime": REGISTRY.get(job.target).mime, "job_id": job.id})
            duration_ms = int((time.perf_counter() - t0) * 1000)
            res = {"output_id": output_id, "output_name": name, "size": len(result.data),
                   "input_size": job.input_size, "duration_ms": duration_ms, "notes": result.notes,
                   "info": result.info}
            if job.cancel_event.is_set():
                # Cancelled at the very last moment: do not leave the output behind.
                self.storage.delete_output(output_id)
                raise ConversionCancelled()
            with job.lock:
                job.result = res
                job.progress = 100.0
            self._finish(job, DONE)
        except ConversionCancelled:
            self._finish(job, CANCELLED, error={"code": "cancelled", "message": "The conversion was cancelled."})
        except OpenConverterError as exc:
            log.info("job %s failed: %s", job.id, exc.code)
            self._finish(job, FAILED, error={"code": exc.code, "message": exc.message})
        except Exception:  # noqa: BLE001
            log.exception("job %s crashed", job.id)
            self._finish(job, FAILED, error={"code": "internal_error",
                                            "message": "An unexpected error occurred during conversion."})

    def _finish(self, job: Job, state: str, error=None) -> None:
        with job.lock:
            if job.state in FINAL:
                return
            job.state = state
            job.stage = state
            job.error = error
            job.finished = time.time()
            res = job.result or {}
        self.history.append({
            "id": job.id, "job_id": job.id, "original_name": job.original_name,
            "output_name": res.get("output_name") or output_name(job.original_name, job.target),
            "source_format": job.source, "target_format": job.target, "input_size": job.input_size,
            "output_size": res.get("size"), "duration_ms": res.get("duration_ms"), "status": state,
            "timestamp": job.finished, "notes": res.get("notes", []), "output_id": res.get("output_id"),
            "error_code": (error or {}).get("code"),
        })
        self._emit(job, "state")

    def shutdown(self, wait: bool = True) -> None:
        with self._lock:
            self._closed = True
            jobs = list(self._jobs.values())
        for job in jobs:
            if job.state not in FINAL:
                self.cancel(job.id)
        self.pool.shutdown(wait=wait, cancel_futures=True)
        for job in jobs:
            if job.state == QUEUED:
                self._finish(job, CANCELLED, error={"code": "cancelled", "message": "Server shut down."})
