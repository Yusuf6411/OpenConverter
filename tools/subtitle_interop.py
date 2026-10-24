#!/usr/bin/env python3
"""Checks OpenConverter's subtitle readers and writers against ffmpeg (a test-time oracle only).
Usage: tools/subtitle_interop.py [path/to/openconverter]"""
import os, re, subprocess, sys, tempfile

OC = sys.argv[1] if len(sys.argv) > 1 else "target/debug/openconverter"
tmp = tempfile.mkdtemp(prefix="ocsub")
fails = 0
total = 0

SRT = """1
00:00:01,000 --> 00:00:02,500
<i>Hello</i> there
second line

2
00:01:03,250 --> 00:01:05,000
<b>Bold</b> & <u>under</u>

3
01:00:00,000 --> 01:00:01,500
Late cue
"""


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def check(name, ok, detail=""):
    global fails, total
    total += 1
    if not ok:
        fails += 1
        print("FAIL", name, detail[:300])


def cues(path):
    """(start, end, plain text) from an SRT file, tags removed."""
    text = open(path, encoding="utf-8").read().replace("\r\n", "\n")
    out = []
    for block in re.split(r"\n\n+", text.strip()):
        lines = block.split("\n")
        i = next(k for k, l in enumerate(lines) if "-->" in l)
        a, b = [x.strip() for x in lines[i].split("-->")]
        body = re.sub(r"<[^>]+>", "", "\n".join(lines[i + 1:]))
        out.append((a, b, body.strip()))
    return out


src = os.path.join(tmp, "in.srt")
open(src, "w").write(SRT)
want = cues(src)

for ext in ("ass", "vtt"):
    ours = os.path.join(tmp, "ours." + ext)
    r = sh(OC, "convert", src, ours, "--overwrite")
    check(f"oc srt->{ext}", r.returncode == 0, r.stderr)
    back = os.path.join(tmp, f"ff_{ext}.srt")
    r = sh("ffmpeg", "-v", "error", "-y", "-i", ours, back)
    check(f"ffmpeg reads our {ext}", r.returncode == 0, r.stderr)
    if r.returncode == 0:
        check(f"{ext} cues match", cues(back) == want, f"{cues(back)} != {want}")
    theirs = os.path.join(tmp, "ff." + ext)
    sh("ffmpeg", "-v", "error", "-y", "-i", src, theirs)
    mine = os.path.join(tmp, f"oc_{ext}.srt")
    r = sh(OC, "convert", theirs, mine, "--overwrite")
    check(f"oc reads ffmpeg {ext}", r.returncode == 0, r.stderr)
    if r.returncode == 0:
        check(f"{ext} cues from ffmpeg match", cues(mine) == want, f"{cues(mine)} != {want}")

# TTML: ffmpeg can write it, so check we read that.
theirs = os.path.join(tmp, "ff.ttml")
r = sh("ffmpeg", "-v", "error", "-y", "-i", src, theirs)
if r.returncode == 0:
    mine = os.path.join(tmp, "oc_ttml.srt")
    r = sh(OC, "convert", theirs, mine, "--overwrite")
    check("oc reads ffmpeg ttml", r.returncode == 0, r.stderr)
    if r.returncode == 0:
        check("ttml cues from ffmpeg match", cues(mine) == want, f"{cues(mine)} != {want}")

print(f"{total - fails}/{total} checks passed")
sys.exit(1 if fails else 0)
