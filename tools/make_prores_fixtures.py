#!/usr/bin/env python3
"""Make the ProRes test streams: ffmpeg's prores_ks encodes them and ffmpeg's decoder gives the
planar 10-bit reference, which the Rust decoder must match to within one level.

    python3 tools/make_prores_fixtures.py crates/oc-prores/tests/data
"""
import subprocess, sys, os, tempfile

out = sys.argv[1] if len(sys.argv) > 1 else "crates/oc-prores/tests/data"
os.makedirs(out, exist_ok=True)
SRC = "testsrc2=size=50x34:rate=10:duration=0.3"
ALPHA = ",geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(gt(X,Y),X*3+Y,0)'"

# name, filter chain, profile, pix_fmt, extra encoder args
CASES = [
    ("prof0", f"{SRC},format=yuv422p10le", "0", "yuv422p10le", []),
    ("prof1", f"{SRC},format=yuv422p10le", "1", "yuv422p10le", []),
    ("prof2", f"{SRC},format=yuv422p10le", "2", "yuv422p10le", []),
    ("prof3", f"{SRC},format=yuv422p10le", "3", "yuv422p10le", []),
    ("yuv444", f"{SRC},format=yuv444p10le", "4444", "yuv444p10le", []),
    ("alpha", f"{SRC},format=yuva444p10le{ALPHA}", "4444", "yuva444p10le", []),
    ("il_tff", f"{SRC},format=yuv422p10le,setfield=tff", "3", "yuv422p10le", ["-flags", "+ildct"]),
    ("il_bff", f"{SRC},format=yuv422p10le,setfield=bff", "3", "yuv422p10le", ["-flags", "+ildct"]),
]

with tempfile.TemporaryDirectory() as tmp:
    for name, src, prof, pix, extra in CASES:
        mov = f"{tmp}/{name}.mov"
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", src, "-c:v", "prores_ks",
                        "-profile:v", prof, "-pix_fmt", pix, "-vendor", "apl0", *extra, mov], check=True)
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", mov, "-c", "copy", "-f", "rawvideo",
                        f"{out}/{name}.prores"], check=True)
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", mov, "-f", "rawvideo", "-pix_fmt", pix,
                        f"{out}/{name}.yuv"], check=True)
