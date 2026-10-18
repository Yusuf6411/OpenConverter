"""The tiny progress/cancellation interface codecs depend on.

Codecs call ``report`` from row/MCU/block loops and ``check_cancelled`` every few
rows. The job layer (core/progress.py) provides the real implementation; this
module only defines the contract and a no-op default for the CLI and tests.
"""


class Progress:
    def report(self, done: int, total: int, stage: str) -> None:
        """Report ``done`` of ``total`` units of work for the current stage."""

    def check_cancelled(self) -> None:
        """Raise ConversionCancelled if the job was cancelled."""


class NullProgress(Progress):
    pass


NULL_PROGRESS = NullProgress()


class SubProgress(Progress):
    """Maps a sub-task's 0..total onto a fraction [start, end] of a parent's scale."""

    def __init__(self, parent: Progress, start: float, end: float) -> None:
        self.parent = parent
        self.start = start
        self.end = end

    def report(self, done: int, total: int, stage: str) -> None:
        frac = 0.0 if total <= 0 else min(1.0, max(0.0, done / total))
        self.parent.report(int((self.start + (self.end - self.start) * frac) * 1000), 1000, stage)

    def check_cancelled(self) -> None:
        self.parent.check_cancelled()
