"""Server-Sent Events for job progress."""

import json
import queue
import time

HEARTBEAT_SECONDS = 15


def format_event(event: str, data: dict, event_id: int | None = None) -> bytes:
    lines = []
    if event_id is not None:
        lines.append(f"id: {event_id}")
    lines.append(f"event: {event}")
    payload = json.dumps(data, separators=(",", ":"))
    lines.extend(f"data: {line}" for line in payload.split("\n"))
    return ("\n".join(lines) + "\n\n").encode("utf-8")


def stream_job(write, jobs, job_id: str, stopping, heartbeat: float = HEARTBEAT_SECONDS) -> None:
    """Write job events until the job reaches a final state, the client goes
    away (write raises) or the server stops."""
    q = jobs.subscribe(job_id)
    try:
        write(b"retry: 2000\n\n")
        counter = 0
        last_beat = time.monotonic()
        while not stopping.is_set():
            try:
                kind, snap = q.get(timeout=1.0)
            except queue.Empty:
                if time.monotonic() - last_beat >= heartbeat:
                    write(b": heartbeat\n\n")
                    last_beat = time.monotonic()
                continue
            # Collapse a backlog of progress events into the newest one.
            while kind == "progress":
                try:
                    k2, s2 = q.get_nowait()
                except queue.Empty:
                    break
                kind, snap = k2, s2
            counter += 1
            write(format_event(kind, snap, counter))
            last_beat = time.monotonic()
            if snap["state"] in ("done", "failed", "cancelled"):
                return
    finally:
        jobs.unsubscribe(job_id, q)
