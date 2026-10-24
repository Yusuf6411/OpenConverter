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

# Subtitle tracks: SRT text in Matroska <-> tx3g in MP4, extraction to SRT, ASS in Matroska.
open(T("s.srt"), "w").write("1\n00:00:00,500 --> 00:00:01,500\n<i>Hello</i> there\n\n2\n00:00:02,000 --> 00:00:02,800\nSecond & last\n")
ff("-i", T("h264.mp4"), "-i", T("s.srt"), "-map", "0", "-map", "1", "-c", "copy", "-c:s", "srt", T("subs.mkv"))


def cue_texts(path, idx="0:s:0"):
    r = sh("ffmpeg", "-v", "error", "-i", path, "-map", idx, "-f", "srt", "-")
    import re
    blocks = [b for b in re.split(r"\n\n+", r.stdout.strip()) if b]
    out = []
    for b in blocks:
        lines = b.split("\n")
        out.append(re.sub(r"<font[^>]*>|</font>", "", "\n".join(lines[2:])))
    return out


oc(T("subs.mkv"), T("subs.mp4")); check("tx3g text", cue_texts(T("subs.mp4")) == cue_texts(T("subs.mkv")), cue_texts(T("subs.mp4")))
oc(T("subs.mp4"), T("subs_b.mkv")); check("srt back in mkv", cue_texts(T("subs_b.mkv")) == cue_texts(T("subs.mkv")))
oc(T("subs.mkv"), T("subs_x.srt"))
open(T("ref.srt"), "w").write(sh("ffmpeg", "-v", "error", "-i", T("subs.mkv"), "-map", "0:s:0", "-f", "srt", "-").stdout)
import re
norm = lambda p: re.sub(r"\s+", " ", re.sub(r"<[^>]+>", "", open(p).read()))
check("extracted srt text", norm(T("subs_x.srt")) == norm(T("ref.srt")), norm(T("subs_x.srt")))
ff("-i", T("s.srt"), T("s.ass"))
ff("-i", T("h264.mp4"), "-i", T("s.ass"), "-map", "0", "-map", "1", "-c", "copy", T("ass.mkv"))
oc(T("ass.mkv"), T("ass_x.ass")); check("extracted ass parses in ffmpeg", sh("ffmpeg", "-v", "error", "-i", T("ass_x.ass"), "-f", "srt", "-").stdout.count("-->") == 2)
oc(T("ass.mkv"), T("ass.mp4")); check("ass becomes tx3g text", cue_texts(T("ass.mp4")) == cue_texts(T("ass.mkv")), cue_texts(T("ass.mp4")))

# Audio tracks pulled out of a video: lossless copy where the codec allows it.
oc(T("vp9.webm"), T("a.opus"))
d = float(probe(T("a.opus"))["format"]["duration"])
check("opus extracted", abs(d - 2.0) < 0.05, d)
oc(T("vp9.webm"), T("a.wav"))
d = float(probe(T("a.wav"))["format"]["duration"])
check("opus decoded to wav", abs(d - 2.0) < 0.05, d)
r = sh(OC, "convert", T("h264.mp4"), T("a.flac"), "--overwrite")
check("aac extraction refused as codec_not_in_build", r.returncode == 12, r.stderr)

# AVI: Motion JPEG with PCM, MPEG-4 part 2 with MP3, VP8.
ff("-f", "lavfi", "-i", "testsrc2=size=160x120:rate=24:duration=1", "-f", "lavfi", "-i", "sine=frequency=500:duration=1",
   "-c:v", "mjpeg", "-q:v", "5", "-c:a", "pcm_s16le", T("mjpeg_pcm.mkv"))
oc(T("mjpeg_pcm.mkv"), T("mjpeg_pcm.avi")); compare(T("mjpeg_pcm.mkv"), T("mjpeg_pcm.avi"))
check("avi has two streams", len(probe(T("mjpeg_pcm.avi"))["streams"]) == 2)
oc(T("mjpeg_pcm.avi"), T("mjpeg_pcm_b.mkv")); compare(T("mjpeg_pcm.mkv"), T("mjpeg_pcm_b.mkv"))
ff("-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25:duration=1", "-f", "lavfi", "-i", "sine=frequency=500:duration=1",
   "-c:v", "mpeg4", "-q:v", "4", "-c:a", "libmp3lame", T("mp4v_mp3.avi"))
oc(T("mp4v_mp3.avi"), T("mp4v_mp3.mkv")); compare(T("mp4v_mp3.avi"), T("mp4v_mp3.mkv"))
oc(T("mp4v_mp3.mkv"), T("mp4v_mp3_b.avi")); compare(T("mp4v_mp3.avi"), T("mp4v_mp3_b.avi"))
oc(T("mp4v_mp3.avi"), T("mp4v_mp3.mp4")); compare(T("mp4v_mp3.avi"), T("mp4v_mp3.mp4"))
ff("-i", T("vp8.webm"), "-c", "copy", T("vp8.avi"))
oc(T("vp8.avi"), T("vp8_c.mkv")); compare(T("vp8.webm"), T("vp8_c.mkv"), ("v",))
oc(T("vp8.webm"), T("vp8_d.avi")); compare(T("vp8.webm"), T("vp8_d.avi"), ("v",))

# Animated images as video sources and sinks.
ff("-f", "lavfi", "-i", "testsrc=size=96x64:rate=10:duration=1.5", T("anim.gif"))
for ext in ("mp4", "webm", "mkv", "y4m"):
    if oc(T("anim.gif"), T("anim_g." + ext)):
        v = probe(T("anim_g." + ext))
        st = v["streams"][0] if v else {}
        check(f"gif -> {ext} size", (st.get("width"), st.get("height")) == (96, 64), st)
        n = sh("ffprobe", "-v", "error", "-count_frames", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", T("anim_g." + ext))
        check(f"gif -> {ext} 15 frames", n.stdout.strip() == "15", n.stdout)
for ext in ("gif", "webp"):
    if oc(T("anim_g.webm"), T("anim_b." + ext)) and ext == "gif":
        n = sh("ffprobe", "-v", "error", "-count_frames", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", T("anim_b.gif"))
        check("webm -> gif 15 frames", n.stdout.strip() == "15", n.stdout)

# VP8 inter-frame decoding: our pictures must equal ffmpeg's for libvpx streams.
def yuv_md5(path):
    r = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-pix_fmt", "yuv420p", "-f", "md5", "-"], capture_output=True, text=True)
    return r.stdout.strip()

vp8_cases = {
    "plain": ["-f", "lavfi", "-i", "testsrc2=size=176x144:rate=25:duration=2", "-c:v", "libvpx", "-b:v", "400k", "-g", "30"],
    "altref": ["-f", "lavfi", "-i", "testsrc2=size=176x144:rate=25:duration=2", "-c:v", "libvpx", "-b:v", "300k", "-g", "50", "-auto-alt-ref", "1", "-lag-in-frames", "16"],
    "odd30": ["-f", "lavfi", "-i", "mandelbrot=size=203x117:rate=30", "-t", "2", "-c:v", "libvpx", "-b:v", "500k", "-threads", "4"],
    "rt": ["-f", "lavfi", "-i", "mandelbrot=size=320x240:rate=30", "-t", "2", "-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "6", "-b:v", "120k"],
    "prof3": ["-f", "lavfi", "-i", "testsrc2=size=176x144:rate=25:duration=1", "-c:v", "libvpx", "-profile:v", "3", "-b:v", "300k"],
    "errres": ["-f", "lavfi", "-i", "mandelbrot=size=203x117:rate=25", "-t", "2", "-c:v", "libvpx", "-b:v", "400k", "-error-resilient", "1"],
}
for name, args in vp8_cases.items():
    ff(*args, T(f"vp8_{name}.webm"))
    if oc(T(f"vp8_{name}.webm"), T(f"vp8_{name}.y4m")):
        check(f"vp8 {name} decodes like ffmpeg", yuv_md5(T(f"vp8_{name}.y4m")) == yuv_md5(T(f"vp8_{name}.webm")))

# VP8 encoding: ffmpeg's native decoder, libvpx and ours must all decode our streams alike.
def md5_of(path, *pre):
    r = subprocess.run(["ffmpeg", "-v", "error", *pre, "-i", path, "-f", "md5", "-pix_fmt", "yuv420p", "-"], capture_output=True, text=True)
    return r.stdout.strip()

enc_sources = {
    "noise": ["-f", "lavfi", "-i", "nullsrc=size=203x117:rate=20,geq=random(1)*255:128:128", "-t", "2"],
    "mandel": ["-f", "lavfi", "-i", "mandelbrot=size=256x192:rate=30", "-t", "2"],
    "odd": ["-f", "lavfi", "-i", "testsrc2=size=177x145:rate=24", "-t", "2"],
    "tiny": ["-f", "lavfi", "-i", "testsrc2=size=8x8:rate=24", "-t", "1"],
}
for name, args in enc_sources.items():
    ff(*args, "-pix_fmt", "yuv420p", T(f"enc_{name}.y4m"))
    for label, opts in (("default", []), ("fast", ["--set", "effort=0"]), ("rate", ["--set", "effort=5", "--set", "bitrate_kbps=150", "--set", "keyframe_interval=10"])):
        out = T(f"enc_{name}_{label}.webm")
        if oc(T(f"enc_{name}.y4m"), out, *opts):
            a, b = md5_of(out), md5_of(out, "-c:v", "libvpx")
            oc(out, T(f"enc_{name}_{label}.y4m"))
            c = md5_of(T(f"enc_{name}_{label}.y4m"))
            check(f"vp8 encode {name}/{label}: native == libvpx == ours", a == b == c, (a, b, c))
r = sh("ffmpeg", "-v", "info", "-i", T("enc_mandel_default.webm"), "-i", T("enc_mandel.y4m"), "-lavfi", "psnr", "-f", "null", "-")
import re
m = re.search(r"average:([0-9.]+)", r.stderr)
check("vp8 encode keeps quality", bool(m) and float(m.group(1)) > 30, r.stderr[-300:])

# MPEG transport streams: demux ffmpeg's files, mux ours, and compare what decodes.
def same_decode(a, b, name, video=True, audio=True):
    if video:
        ha, hb = frame_hashes(a), frame_hashes(b)
        check(f"{name} video frames", ha == hb and len(ha) > 0, f"{len(ha)} vs {len(hb)}")
    if audio:
        check(f"{name} audio", md5_of(a, "-vn") == md5_of(b, "-vn") and md5_of(a, "-vn") != "", (md5_of(a, "-vn"), md5_of(b, "-vn")))


def strict_ok(path):
    r = sh("ffmpeg", "-v", "warning", "-err_detect", "explode", "-i", path, "-f", "null", "-")
    return r.returncode == 0 and not r.stderr.strip()


ff("-i", T("h264.mp4"), "-c", "copy", T("h264.ts"))
ff("-i", T("hevc.mp4"), "-c", "copy", T("hevc.ts"))
ff("-f", "lavfi", "-i", "sine=frequency=300:duration=2", "-c:a", "ac3", T("ac3.ts"))
ff("-f", "lavfi", "-i", "sine=frequency=300:duration=2", "-c:a", "libmp3lame", T("mp3.ts"))
ff("-i", T("h264.mp4"), "-c", "copy", "-f", "mpegts", "-mpegts_m2ts_mode", "1", T("h264.m2ts"))
for name, kinds in (("h264", (1, 1)), ("hevc", (1, 0)), ("ac3", (0, 1)), ("mp3", (0, 1))):
    src = T(name + ".ts")
    if oc(src, T(name + "_t.mkv")):
        same_decode(src, T(name + "_t.mkv"), f"ts {name} -> mkv", bool(kinds[0]), bool(kinds[1]))
        if oc(T(name + "_t.mkv"), T(name + "_o.ts")):
            same_decode(src, T(name + "_o.ts"), f"mkv -> ts {name}", bool(kinds[0]), bool(kinds[1]))
            check(f"{name} ts passes ffmpeg's strict decode", strict_ok(T(name + "_o.ts")))
            check(f"{name} ts is whole 188-byte packets", os.path.getsize(T(name + "_o.ts")) % 188 == 0)
if oc(T("h264.m2ts"), T("h264_m.mp4")):
    same_decode(T("h264.ts"), T("h264_m.mp4"), "m2ts -> mp4")
if oc(T("h264.mp4"), T("h264_d.ts")):
    same_decode(T("h264.ts"), T("h264_d.ts"), "mp4 -> ts")
    check("ts keeps the frame rate", probe(T("h264_d.ts"))["streams"][0]["r_frame_rate"] == "30000/1001", probe(T("h264_d.ts"))["streams"][0])
r = sh(OC, "convert", T("vp8.webm"), T("vp8.ts"), "--overwrite")
check("ts refuses a codec it cannot carry", r.returncode != 0, r.stderr)

# FLV (ffmpeg writes classic AVC and enhanced HEVC tags) and program streams (built here).
ff("-i", T("h264.mp4"), "-c", "copy", T("h264.flv"))
ff("-i", T("hevc.mp4"), "-c", "copy", T("hevc.flv"))
ff("-f", "lavfi", "-i", "sine=frequency=300:duration=2", "-c:a", "libmp3lame", T("mp3.flv"))
for name, kinds in (("h264", (1, 0)), ("hevc", (1, 0)), ("mp3", (0, 1))):
    if oc(T(name + ".flv"), T(name + "_f.mkv")):
        same_decode(T(name + ".flv"), T(name + "_f.mkv"), f"flv {name} -> mkv", bool(kinds[0]), bool(kinds[1]))
r = sh(OC, "convert", T("h264.mp4"), T("x.flv"), "--overwrite")
check("flv is read only", r.returncode != 0, r.stderr)


def make_ps(es_path, times, out_path, chunk=2000):
    """MPEG-2 program stream of one video stream: 2048-byte packs, PES cut every `chunk`
    bytes regardless of picture boundaries, and a stamp only on the PES where a picture starts."""
    import struct
    data = open(es_path, "rb").read()
    starts, i = [], 0
    while True:
        i = data.find(b"\x00\x00\x01\x09", i)
        if i < 0:
            break
        starts.append(i)
        i += 4
    stamp = lambda m, t: bytes([(m << 4) | (((t >> 30) & 7) << 1) | 1, (t >> 22) & 255, (((t >> 15) & 127) << 1) | 1, (t >> 7) & 255, ((t & 127) << 1) | 1])
    out = bytearray()
    for off in range(0, len(data), chunk):
        part = data[off:off + chunk]
        first = next((k for k, s in enumerate(starts) if off <= s < off + chunk), None)
        out += b"\x00\x00\x01\xba\x44\x00\x04\x00\x04\x01\x01\x89\xc3\xf8"
        if first is not None:
            pts, dts = times[first]
            hdr = bytes([0x80, 0xC0 if dts != pts else 0x80, 10 if dts != pts else 5]) + stamp(3 if dts != pts else 2, pts) + (stamp(1, dts) if dts != pts else b"")
        else:
            hdr = bytes([0x80, 0x00, 0])
        out += b"\x00\x00\x01\xe0" + struct.pack(">H", len(hdr) + len(part)) + hdr + part
    open(out_path, "wb").write(out + b"\x00\x00\x01\xb9")


ff("-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=2", "-c:v", "libx264", "-bf", "0", "-g", "25", "-pix_fmt", "yuv420p", T("nb.mp4"))
ff("-i", T("nb.mp4"), "-c", "copy", "-bsf:v", "h264_mp4toannexb,h264_metadata=aud=insert", "-f", "h264", T("nb.h264"))
pk = json.loads(sh("ffprobe", "-v", "error", "-show_entries", "packet=pts,dts", "-of", "json", T("nb.mp4")).stdout)["packets"]
make_ps(T("nb.h264"), [(int(p["pts"]) * 90000 // 12800 + 9000, int(p["dts"]) * 90000 // 12800 + 9000) for p in pk], T("nb.mpg"))
if oc(T("nb.mpg"), T("nb_p.mkv")):
    same_decode(T("nb.mp4"), T("nb_p.mkv"), "program stream -> mkv", True, False)
    check("program stream frame rate", probe(T("nb_p.mkv"))["streams"][0]["r_frame_rate"] == "25/1", probe(T("nb_p.mkv"))["streams"][0])

# Fragmented MP4: moof/mdat pairs after an empty moov, decoding the same.
# (Opus in MP4 keeps its end padding in either form, so VP9/Opus is compared with the plain MP4.)
for src, kinds in (("h264.mp4", (1, 1)), ("vp9.mp4", (1, 1)), ("hevc.mp4", (1, 0))):
    out = T("frag_" + src.split(".")[0] + ".mp4")
    if oc(T(src), out, "--set", "fragmented=true"):
        raw = open(out, "rb").read()
        check(f"{src} fragmented has moof boxes", raw.count(b"moof") >= 1 and b"mvex" in raw)
        same_decode(T(src), out, f"fragmented {src}", bool(kinds[0]), bool(kinds[1]))
        check(f"{src} fragmented passes strict decode", strict_ok(out))

# Image sequences: a ZIP of numbered pictures, both ways.
import zipfile
ff("-f", "lavfi", "-i", "testsrc2=size=160x120:rate=20:duration=2", "-c:v", "libvpx", "-b:v", "300k", T("seq_src.webm"))
if oc(T("seq_src.webm"), T("seq.frames.zip"), "--set", "fps=10"):
    names = zipfile.ZipFile(T("seq.frames.zip")).namelist()
    check("sequence has 20 numbered frames", names == [f"frame_{i:06d}.png" for i in range(1, 21)], names[:3])
    zipfile.ZipFile(T("seq.frames.zip")).extract("frame_000001.png", T("seq_dir"))
    ff("-i", T("seq_src.webm"), "-frames:v", "1", T("seq_ref.png"))
    r = sh("ffmpeg", "-v", "info", "-i", T("seq_dir/frame_000001.png"), "-i", T("seq_ref.png"), "-lavfi", "psnr", "-f", "null", "-")
    m = re.search(r"average:([0-9.inf]+)", r.stderr)
    check("sequence picture matches ffmpeg's", bool(m) and (m.group(1) == "inf" or float(m.group(1)) > 35), r.stderr[-200:])
    for ext in ("mp4", "webm", "mkv"):
        if oc(T("seq.frames.zip"), T("seq_back." + ext), "--set", "fps=10"):
            n = sh("ffprobe", "-v", "error", "-count_frames", "-show_entries", "stream=nb_read_frames,r_frame_rate", "-of", "csv=p=0", T("seq_back." + ext)).stdout.strip()
            check(f"sequence -> {ext}: 20 frames at 10 fps", n in ("10/1,20", "20,10/1") or n.replace(",", " ").split() == ["10/1", "20"], n)
    if oc(T("seq_src.webm"), T("seq_j.frames.zip"), "--set", "frame_format=jpeg", "--set", "quality=85"):
        check("jpeg frames", all(n.endswith(".jpg") for n in zipfile.ZipFile(T("seq_j.frames.zip")).namelist()))
        oc(T("seq_j.frames.zip"), T("seq_j.mp4"), "--set", "fps=20")

print(f"{total - fails}/{total} checks passed")
sys.exit(1 if fails else 0)
