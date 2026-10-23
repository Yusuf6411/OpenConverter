#!/usr/bin/env python3
"""Checks OpenConverter's video containers against ffmpeg/ffprobe (test-time oracles only).
Usage: tools/video_interop.py [path/to/openconverter]"""
import json, os, subprocess, sys, tempfile

OC = sys.argv[1] if len(sys.argv) > 1 else "target/debug/openconverter"
tmp = tempfile.mkdtemp(prefix="ocvid")
fails = 0
total = 0


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def check(name, ok, detail=""):
    global fails, total
    total += 1
    if not ok:
        fails += 1
        print("FAIL", name, str(detail)[:600])


def ff(*a):
    r = sh("ffmpeg", "-v", "error", "-y", *a)
    if r.returncode:
        raise RuntimeError(r.stderr[-500:])


def oc(src, dst, *opts):
    r = sh(OC, "convert", src, dst, "--overwrite", *opts)
    check(f"oc {os.path.basename(src)} -> {os.path.basename(dst)}", r.returncode == 0, r.stderr)
    return r.returncode == 0


def probe(path):
    r = sh("ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", path)
    if r.returncode:
        return None
    return json.loads(r.stdout)


def packets(path, stream):
    """(pts_time, dts_time, size, keyframe) for each packet of stream index `stream` of one kind."""
    r = sh("ffprobe", "-v", "error", "-select_streams", stream, "-show_entries",
           "packet=pts_time,dts_time,size,flags", "-of", "json", path)
    out = []
    for p in json.loads(r.stdout).get("packets", []):
        out.append((round(float(p.get("pts_time", "nan")), 3), int(p["size"]), "K" in p["flags"]))
    return sorted(out)


def frame_hashes(path, kind="v"):
    """Per-frame md5 of the decoded stream, via ffmpeg's framemd5 muxer."""
    r = sh("ffmpeg", "-v", "error", "-i", path, "-map", f"0:{kind}:0", "-f", "framemd5", "-")
    lines = [l for l in r.stdout.splitlines() if l and not l.startswith("#")]
    return [l.split(",")[-1].strip() for l in lines]


def compare(src, dst, kinds=("v", "a")):
    for k in kinds:
        a, b = packets(src, k), packets(dst, k)
        same = len(a) == len(b) and all(abs(x[0] - y[0]) < 0.0011 and x[1:] == y[1:] for x, y in zip(a, b))
        diff = next((i for i, (x, y) in enumerate(zip(a, b)) if abs(x[0] - y[0]) >= 0.0011 or x[1:] != y[1:]), None)
        check(f"{os.path.basename(dst)} {k} packets", same and len(a) > 0,
              f"{len(a)} vs {len(b)} first diff at {diff}: {a[diff:diff+2] if diff is not None else ''} / {b[diff:diff+2] if diff is not None else ''}")
        if k == "v":
            ha, hb = frame_hashes(src), frame_hashes(dst)
            check(f"{os.path.basename(dst)} decoded frames", ha == hb and len(ha) > 0, f"{len(ha)} vs {len(hb)}")


def make():
    ff("-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30000/1001:duration=3", "-f", "lavfi", "-i",
       "sine=frequency=440:duration=3", "-c:v", "libx264", "-bf", "2", "-g", "24", "-pix_fmt", "yuv420p",
       "-c:a", "aac", "-shortest", f"{tmp}/h264.mp4")
    ff("-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=2", "-f", "lavfi", "-i",
       "sine=frequency=330:duration=2", "-c:v", "libvpx-vp9", "-b:v", "300k", "-c:a", "libopus", "-shortest",
       f"{tmp}/vp9.webm")
    ff("-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24:duration=2", "-c:v", "libvpx", "-b:v", "200k",
       f"{tmp}/vp8.webm")
    ff("-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24:duration=1", "-c:v", "mjpeg", "-q:v", "5",
       f"{tmp}/mjpeg.mkv")
    ff("-f", "lavfi", "-i", "testsrc2=size=320x240:rate=24:duration=2", "-c:v", "libx265", "-bf", "2",
       "-pix_fmt", "yuv420p", "-x265-params", "log-level=none", f"{tmp}/hevc.mp4")


make()
T = lambda n: f"{tmp}/{n}"

# H.264 + AAC with B-frames: MP4 -> MKV -> MP4 -> MOV keep every packet and decode the same.
oc(T("h264.mp4"), T("h264.mkv")); compare(T("h264.mp4"), T("h264.mkv"))
oc(T("h264.mkv"), T("h264_b.mp4")); compare(T("h264.mp4"), T("h264_b.mp4"))
oc(T("h264.mp4"), T("h264.mov"), "--set", "faststart=false"); compare(T("h264.mp4"), T("h264.mov"))
# HEVC
oc(T("hevc.mp4"), T("hevc.mkv")); compare(T("hevc.mp4"), T("hevc.mkv"), ("v",))
oc(T("hevc.mkv"), T("hevc_b.mp4")); compare(T("hevc.mp4"), T("hevc_b.mp4"), ("v",))
# VP9 + Opus: WebM -> MP4 -> MKV -> WebM.
oc(T("vp9.webm"), T("vp9.mp4")); compare(T("vp9.webm"), T("vp9.mp4"))
oc(T("vp9.mp4"), T("vp9_b.mkv")); compare(T("vp9.webm"), T("vp9_b.mkv"))
oc(T("vp9_b.mkv"), T("vp9_c.webm")); compare(T("vp9.webm"), T("vp9_c.webm"))
# VP8
oc(T("vp8.webm"), T("vp8.mp4")); compare(T("vp8.webm"), T("vp8.mp4"), ("v",))
oc(T("vp8.mp4"), T("vp8_b.mkv")); compare(T("vp8.webm"), T("vp8_b.mkv"), ("v",))
# MJPEG
oc(T("mjpeg.mkv"), T("mjpeg.mp4")); compare(T("mjpeg.mkv"), T("mjpeg.mp4"), ("v",))
oc(T("mjpeg.mkv"), T("mjpeg.mov")); compare(T("mjpeg.mkv"), T("mjpeg.mov"), ("v",))
# Streams that a container cannot hold are left out, not corrupted.
r = sh(OC, "convert", T("h264.mp4"), T("h264_x.webm"), "--overwrite")
check("webm refuses h264+aac with codec_not_in_build", r.returncode == 12, r.stderr)

print(f"{total - fails}/{total} checks passed")
sys.exit(1 if fails else 0)
