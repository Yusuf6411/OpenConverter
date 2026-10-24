#!/usr/bin/env python3
"""Make the Theora test streams: libtheora (via ffmpeg) encodes them and ffmpeg's decoder gives
the per-frame MD5 of the raw planes, which the Rust decoder must reproduce bit for bit.

    python3 tools/make_theora_fixtures.py crates/oc-theora/tests/data
"""
import subprocess, sys, os

out = sys.argv[1] if len(sys.argv) > 1 else "crates/oc-theora/tests/data"
os.makedirs(out, exist_ok=True)

# name, lavfi source, pix_fmt, extra encoder args
CASES = [
    ("420_testsrc", "testsrc2=size=64x48:rate=10:duration=2", "yuv420p", ["-q:v", "6", "-g", "6"]),
    ("420_odd", "testsrc2=size=70x50:rate=10:duration=2", "yuv420p", ["-q:v", "5", "-g", "8"]),
    ("420_hq", "testsrc2=size=64x64:rate=10:duration=1.5", "yuv420p", ["-q:v", "10", "-g", "30"]),
    ("420_lq", "testsrc2=size=96x64:rate=10:duration=2", "yuv420p", ["-q:v", "1", "-g", "12"]),
    ("422_testsrc", "testsrc2=size=64x48:rate=10:duration=2", "yuv422p", ["-q:v", "6", "-g", "6"]),
    ("444_testsrc", "testsrc2=size=64x48:rate=10:duration=2", "yuv444p", ["-q:v", "6", "-g", "6"]),
    ("420_mandel", "mandelbrot=size=96x64:rate=10", "yuv420p", ["-q:v", "6", "-g", "10", "-t", "2"]),
    ("420_life", "life=size=96x64:rate=10:mold=10:ratio=0.1:death_color=#C83232:life_color=#00ff00", "yuv420p", ["-q:v", "7", "-g", "10", "-t", "2"]),
    ("420_rgbtest", "rgbtestsrc=size=64x48:rate=10:duration=1.5", "yuv420p", ["-b:v", "300k", "-g", "4"]),
]

for name, src, pix, extra in CASES:
    ogv = f"{out}/{name}.ogv"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", src, "-c:v", "libtheora",
                    "-pix_fmt", pix, *extra, ogv], check=True)
    md5 = subprocess.run(["ffmpeg", "-v", "error", "-i", ogv, "-f", "framemd5", "-pix_fmt", pix, "-"],
                         check=True, capture_output=True, text=True).stdout
    lines = [l.split(",")[-1].strip() for l in md5.splitlines() if l and not l.startswith("#")]
    with open(f"{out}/{name}.md5", "w") as f:
        f.write("\n".join(lines) + "\n")
    print(name, len(lines), "frames", os.path.getsize(ogv), "bytes")
