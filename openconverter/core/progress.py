"""Job-aware Progress implementation: monotonic, throttled, cancellable."""

import time

from openconverter.errors import ConversionCancelled
from openconverter.models.progress import NULL_PROGRESS, Progress, SubProgress  # noqa: F401 (re-export)

MIN_INTERVAL = 0.1  # at most 10 notifications per second


class JobProgress(Progress):
    """Receives codec progress for one job and forwards it to the job record.

    Values are clamped to 0..100 and never decrease; notifications to
    listeners are throttled, but stage changes are always delivered.
    """

    def __init__(self, job, notify, clock=time.monotonic) -> None:
        self.job = job
        self.notify = notify
        self.clock = clock
        self._last_emit = 0.0
        self._last_stage = None

    def report(self, done: int, total: int, stage: str) -> None:
        frac = 0.0 if total <= 0 else done / total
        pct = max(0.0, min(100.0, frac * 100.0))
        job = self.job
        with job.lock:
            if pct < job.progress:
                pct = job.progress
            job.progress = round(pct, 1)
            stage_changed = stage != job.stage
            job.stage = stage
        now = self.clock()
        if stage_changed or now - self._last_emit >= MIN_INTERVAL:
            self._last_emit = now
            self.notify(job, "progress")

    def check_cancelled(self) -> None:
        if self.job.cancel_event.is_set():
            raise ConversionCancelled()
