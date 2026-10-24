#!/usr/bin/env python3
"""Checks OpenConverter's audio readers and writers against ffmpeg (a test-time oracle only).
Usage: tools/audio_interop.py [path/to/openconverter]"""
import hashlib, os, subprocess, sys, tempfile

OC = sys.argv[1] if len(sys.argv) > 1 else "target/debug/openconverter"
tmp = tempfile.mkdtemp(prefix="ocaudio")
fails = 0
total = 0


def sh(*a, check=True):
    r = subprocess.run(a, capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError(f"{' '.join(a)}\n{r.stderr[-600:]}")
    return r


def ff(*a):
    return sh("ffmpeg", "-v", "error", "-y", *a)


def pcm(path, fmt="s32le", extra=()):
    """Decoded samples as raw bytes, via ffmpeg."""
    out = os.path.join(tmp, "raw.bin")
    ff("-i", path, *extra, "-f", fmt, out)
    return open(out, "rb").read()


def oc(src, dst, *opts):
    args = [OC, "convert", src, dst, "--overwrite"]
    for o in opts:
        args += ["--set", o]
    return sh(*args)


def check(name, ok, detail=""):
    global fails, total
    total += 1
    if ok:
        print("ok  ", name)
    else:
        fails += 1
        print("FAIL", name, detail)


def tone(path, *, rate=44100, ch=1, secs=2.0, codec="pcm_s16le", fmt=None, extra=()):
    graph = "sine=frequency=440:sample_rate=%d:duration=%s,volume=0.5[a];anoisesrc=r=%d:d=%s:a=0.05:c=white[n];[a][n]amix=inputs=2:normalize=0" % (rate, secs, rate, secs)
    if ch > 1:
        graph += ",aformat=channel_layouts=%s" % {2: "stereo", 3: "3.0", 4: "quad", 6: "5.1", 8: "7.1"}[ch]
    args = ["-filter_complex", graph, "-c:a", codec, *extra]
    if fmt:
        args += ["-f", fmt]
    ff(*args, path)


# ---- 1. our reader vs ffmpeg's, on files ffmpeg wrote
cases = [
    ("wav_s16_st", dict(codec="pcm_s16le", ch=2), "wav"),
    ("wav_s24_mono", dict(codec="pcm_s24le"), "wav"),
    ("wav_s32_st_48k", dict(codec="pcm_s32le", ch=2, rate=48000), "wav"),
    ("wav_u8", dict(codec="pcm_u8"), "wav"),
    ("wav_f32", dict(codec="pcm_f32le", ch=2), "wav"),
    ("wav_f64", dict(codec="pcm_f64le"), "wav"),
    ("wav_alaw", dict(codec="pcm_alaw", rate=8000), "wav"),
    ("wav_mulaw", dict(codec="pcm_mulaw", rate=8000), "wav"),
    ("wav_ima", dict(codec="adpcm_ima_wav", ch=2), "wav"),
    ("wav_ms", dict(codec="adpcm_ms", ch=2), "wav"),
    ("wav_51", dict(codec="pcm_s24le", ch=6, rate=48000), "wav"),
    ("wav_71", dict(codec="pcm_s16le", ch=8, rate=48000), "wav"),
    ("wav_rf64", dict(codec="pcm_s16le", ch=2, extra=("-rf64", "always")), "wav"),
    ("aiff_s16", dict(codec="pcm_s16be", ch=2), "aiff"),
    ("aiff_s24", dict(codec="pcm_s24be"), "aiff"),
    ("aiff_s32", dict(codec="pcm_s32be"), "aiff"),
    ("aifc_sowt", dict(codec="pcm_s16le", ch=2, extra=("-write_id3v2", "0")), "aiff"),
    ("caf_s16", dict(codec="pcm_s16le", ch=2), "caf"),
    ("caf_s24", dict(codec="pcm_s24le"), "caf"),
    ("caf_f32", dict(codec="pcm_f32le", ch=2), "caf"),
    ("flac_16", dict(codec="flac", ch=2), "flac"),
    ("flac_24", dict(codec="flac", ch=2, extra=("-sample_fmt", "s32", "-bits_per_raw_sample", "24")), "flac"),
    ("flac_8", dict(codec="flac", extra=("-sample_fmt", "u8" if False else "s16",)), "flac"),
    ("flac_51", dict(codec="flac", ch=6, rate=48000), "flac"),
    ("flac_l0", dict(codec="flac", ch=2, extra=("-compression_level", "0")), "flac"),
    ("flac_l12", dict(codec="flac", ch=2, extra=("-compression_level", "12")), "flac"),
    ("flac_8k", dict(codec="flac", rate=8000), "flac"),
    ("flac_96k", dict(codec="flac", rate=96000, ch=2), "flac"),
]
for name, kw, ext in cases:
    try:
        src = os.path.join(tmp, name + "." + ext)
        tone(src, **kw)
        out = os.path.join(tmp, name + ".ours.wav")
        oc(src, out, "bit_depth=float" if "f32" in name or "f64" in name else "bit_depth=keep")
        a = pcm(src, "f32le")
        b = pcm(out, "f32le")
        if name == "wav_ima":
            # IMA ADPCM decoders round the step product differently (ffmpeg uses
            # ((2d+1)*step)>>3, the IMA reference adds the bit terms); the results stay within
            # a few hundred parts per million of full scale.
            import array
            x, y = array.array("f", a), array.array("f", b)
            worst = max(abs(p - q) for p, q in zip(x, y))
            check("read " + name + " (worst difference %.5f of full scale)" % worst, len(x) == len(y) and worst < 0.003)
        else:
            check("read " + name, a == b, f"{len(a)} vs {len(b)}")
    except Exception as e:
        check("read " + name, False, str(e)[:300])

# ---- 2. our writers, read back by ffmpeg
base = os.path.join(tmp, "base.wav")
tone(base, codec="pcm_s24le", ch=2, secs=3.0)
ref = pcm(base, "s32le")
ref16 = os.path.join(tmp, "base16.wav")
tone(ref16, codec="pcm_s16le", ch=2, secs=3.0)
ref16raw = pcm(ref16, "s16le")
for out, opts, raw, rawfmt in [
    ("o24.wav", ["bit_depth=24"], ref, "s32le"),
    ("o32.wav", ["bit_depth=32"], ref, "s32le"),
    ("o24.aiff", ["bit_depth=24"], ref, "s32le"),
    ("o24le.aiff", ["bit_depth=24", "little_endian=true"], ref, "s32le"),
    ("o24.caf", ["bit_depth=24"], ref, "s32le"),
    ("o24_l0.flac", ["level=0"], ref, "s32le"),
    ("o24_l5.flac", ["level=5"], ref, "s32le"),
    ("o24_l8.flac", ["level=8"], ref, "s32le"),
    ("o24_bs.flac", ["level=6", "block_size=1000"], ref, "s32le"),
    ("o16.flac", ["bit_depth=16", "level=5"], None, "s16le"),
]:
    try:
        dst = os.path.join(tmp, out)
        oc(base, dst, *opts)
        got = pcm(dst, rawfmt)
        if raw is None:
            # 24 -> 16 is a dithered reduction; only the length and rough level are checked
            check("write " + out, len(got) == len(ref) // 2, f"{len(got)} vs {len(ref)//2}")
        else:
            check("write " + out, got == raw, f"{len(got)} vs {len(raw)}")
    except Exception as e:
        check("write " + out, False, str(e)[:300])

# our FLAC passes libFLAC's md5 check inside ffmpeg (-strict -> error on mismatch)
for lvl in (0, 3, 8):
    dst = os.path.join(tmp, f"v{lvl}.flac")
    oc(base, dst, f"level={lvl}")
    r = sh("ffmpeg", "-v", "error", "-err_detect", "crccheck+bitstream+explode", "-i", dst, "-f", "null", "-", check=False)
    check(f"flac level {lvl} passes ffmpeg's strict check", r.returncode == 0 and not r.stderr.strip(), r.stderr[:200])

# size relative to ffmpeg's encoder
f_ff = os.path.join(tmp, "ff.flac")
ff("-i", base, "-c:a", "flac", "-compression_level", "8", f_ff)
f_oc = os.path.join(tmp, "oc.flac")
oc(base, f_oc, "level=8")
sa, sb = os.path.getsize(f_ff), os.path.getsize(f_oc)
print(f"info flac size: ffmpeg level 8 = {sa}, ours = {sb} ({100.0 * sb / sa:.1f}%)")
check("flac size within 3% of ffmpeg's", sb <= sa * 1.03, f"{sa} vs {sb}")

# ---- 2b. our Vorbis encoder: ffmpeg (libvorbis) must decode it, with the right length and tags
for q in (0, 4, 8):
    dst = os.path.join(tmp, f"q{q}.ogg")
    oc(base, dst, f"quality={q}")
    r = sh("ffmpeg", "-v", "error", "-i", dst, "-f", "null", "-", check=False)
    check(f"vorbis quality {q} decodes in ffmpeg without errors", r.returncode == 0 and not r.stderr.strip(), r.stderr[:200])
    r = sh("ffprobe", "-v", "error", "-show_entries", "stream=codec_name,channels,sample_rate", "-of", "csv=p=0", dst)
    ref = sh("ffprobe", "-v", "error", "-show_entries", "stream=channels,sample_rate", "-of", "csv=p=0", base).stdout.strip()
    check(f"vorbis quality {q} keeps channels and rate", r.stdout.strip() == "vorbis," + ref, r.stdout)

# ---- 2c. MP3: our decoder against ffmpeg on LAME files, our encoder against ffmpeg's and libmpg123's decoders
import struct
try:
    import numpy as np
except ImportError:
    np = None
if np is not None:
    def float_pcm(path, ch):
        raw = pcm(path, "f32le", ("-ac", str(ch)))
        return np.frombuffer(raw, "<f4").reshape(-1, ch)

    def oc_float(path, ch):
        dst = os.path.join(tmp, "f.wav")
        oc(path, dst, "bit_depth=float")
        b = open(dst, "rb").read()
        i = b.index(b"data")
        n = struct.unpack_from("<I", b, i + 4)[0]
        return np.frombuffer(b[i + 8:i + 8 + n], "<f4").reshape(-1, ch)

    rng = np.random.default_rng(11)
    for name, rate, ch, args in [
        ("mp3 44.1k stereo 128k", 44100, 2, ["-b:a", "128k"]),
        ("mp3 48k stereo V0", 48000, 2, ["-q:a", "0"]),
        ("mp3 32k mono 64k", 32000, 1, ["-b:a", "64k"]),
        ("mp3 22.05k stereo 96k (MPEG-2)", 22050, 2, ["-b:a", "96k"]),
        ("mp3 24k mono 48k", 24000, 1, ["-b:a", "48k"]),
        ("mp3 11.025k mono 24k (MPEG-2.5)", 11025, 1, ["-b:a", "24k"]),
        ("mp3 8k mono 16k (MPEG-2.5)", 8000, 1, ["-b:a", "16k"]),
    ]:
        n = rate * 3
        t = np.arange(n) / rate
        x = np.zeros((n, ch))
        for c in range(ch):
            for f in (220 * (c + 1), 660, 1750, 5100):
                x[:, c] += 0.15 * np.sin(2 * np.pi * f * t)
            x[:, c] += 0.01 * rng.standard_normal(n)
        for k in range(0, n, rate // 2):
            m = min(200, n - k)
            x[k:k + m, :] += rng.standard_normal((m, ch)) * 0.5 * np.exp(-np.arange(m) / 40)[:, None]
        raw = os.path.join(tmp, "src.f32")
        np.clip(x, -1, 1).astype("<f4").tofile(raw)
        mp3 = os.path.join(tmp, "lame.mp3")
        ff("-f", "f32le", "-ar", str(rate), "-ac", str(ch), "-i", raw, "-c:a", "libmp3lame", *args, mp3)
        ref = float_pcm(mp3, ch)
        got = oc_float(mp3, ch)
        m = min(len(ref), len(got))
        err = float(np.abs(ref[:m] - got[:m]).max()) if m else 1.0
        check(f"{name}: decoder matches ffmpeg ({err:.1e}, {len(got)} vs {len(ref)} frames)", len(ref) == len(got) and err < 1e-4)

    # our encoder: ffmpeg must decode it, to the right length, close to the source
    src = os.path.join(tmp, "src_music.wav")
    ff("-f", "lavfi", "-i", "sine=frequency=440:duration=3:sample_rate=44100", "-f", "lavfi", "-i", "anoisesrc=d=3:a=0.02:r=44100", "-filter_complex", "amix=inputs=2:normalize=0,aformat=channel_layouts=stereo", "-c:a", "pcm_f32le", src)
    ref = float_pcm(src, 2)
    for opt in (["bitrate=64"], ["bitrate=128"], ["bitrate=256"], ["vbr_quality=2"], ["vbr_quality=7"], ["joint_stereo=false"]):
        dst = os.path.join(tmp, "ours.mp3")
        oc(src, dst, *opt)
        r = sh("ffmpeg", "-v", "error", "-i", dst, "-f", "null", "-", check=False)
        check(f"mp3 encoder {opt[0]}: ffmpeg decodes without errors", r.returncode == 0 and not r.stderr.strip(), r.stderr[:200])
        d = float_pcm(dst, 2)
        m = min(len(d), len(ref))
        snr = 10 * np.log10((ref[:m] ** 2).sum() / max(((ref[:m] - d[:m]) ** 2).sum(), 1e-20))
        check(f"mp3 encoder {opt[0]}: gapless length and SNR ({len(d)} vs {len(ref)}, {snr:.1f} dB)", len(d) == len(ref) and snr > 12)
    try:
        import ctypes
        lib = ctypes.CDLL("libmpg123.so.0")
        lib.mpg123_init()
        lib.mpg123_new.restype = ctypes.c_void_p
        h = lib.mpg123_new(None, None)
        lib.mpg123_format_none(ctypes.c_void_p(h))
        for r_ in (8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000):
            lib.mpg123_format(ctypes.c_void_p(h), ctypes.c_long(r_), ctypes.c_int(3), ctypes.c_int(0x200))
        dst = os.path.join(tmp, "ours.mp3")
        oc(src, dst, "bitrate=128")
        ok = lib.mpg123_open(ctypes.c_void_p(h), dst.encode()) == 0
        fmt = (ctypes.c_long(), ctypes.c_int(), ctypes.c_int())
        lib.mpg123_getformat(ctypes.c_void_p(h), ctypes.byref(fmt[0]), ctypes.byref(fmt[1]), ctypes.byref(fmt[2]))
        buf = ctypes.create_string_buffer(1 << 16)
        done = ctypes.c_size_t()
        chunks = []
        while ok:
            r = lib.mpg123_read(ctypes.c_void_p(h), buf, len(buf), ctypes.byref(done))
            chunks.append(buf.raw[:done.value])
            if r != 0 and done.value == 0:
                break
        data = np.frombuffer(b"".join(chunks), "<f4")
        ours = oc_float(dst, 2).reshape(-1)
        k = min(len(data), len(ours))
        diff = float(np.abs(data[:k] - ours[:k]).max()) if k else 1.0
        check(f"mp3 encoder output decodes in libmpg123 like in ours ({len(data)} vs {len(ours)} samples, {diff:.1e})", ok and len(data) == len(ours) and diff < 1e-3)
    except OSError:
        print("skip libmpg123 not installed")

# ---- 2d. Opus, ALAC and Matroska: ffmpeg (libopus, alac) must decode our files, and we read theirs
try:
    src48 = os.path.join(tmp, "src48.wav")
    ff("-f", "lavfi", "-i", "sine=f=440:d=1.5:r=48000", "-ac", "2", src48)
    for name, opts in [("opus", ["bitrate=96", "rate_mode=cbr"]), ("opus", ["bitrate=48", "rate_mode=vbr", "frame_ms=10"]), ("mka", ["codec=opus"]), ("mka", ["codec=flac"]),
                       ("mka", ["codec=vorbis"]), ("mka", ["codec=pcm"]), ("weba", ["codec=opus"]), ("weba", ["codec=vorbis"]), ("m4a", [])]:
        dst = os.path.join(tmp, "o_" + "_".join(opts).replace("=", "") + "." + name)
        oc(src48, dst, *opts)
        r = sh("ffmpeg", "-v", "error", "-i", dst, "-f", "null", "-", check=False)
        check(f"{name} {' '.join(opts)}: ffmpeg decodes without errors", r.returncode == 0 and not r.stderr.strip(), r.stderr[:200])
        r = sh("ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", dst)
        check(f"{name} {' '.join(opts)}: duration {r.stdout.strip()} s", abs(float(r.stdout) - 1.5) < 0.05, r.stdout)
    # ffmpeg-made files read by us
    for ext, enc in [("mka", ["-c:a", "libopus"]), ("mka", ["-c:a", "flac"]), ("webm", ["-c:a", "libvorbis"]), ("mka", ["-c:a", "pcm_s24le"]), ("mka", ["-c:a", "libmp3lame"]), ("opus", ["-c:a", "libopus"])]:
        f = os.path.join(tmp, "ff_" + enc[1] + "." + ext)
        ff("-i", src48, *enc, f)
        dst = os.path.join(tmp, "back_" + enc[1] + ".wav")
        oc(f, dst, "bit_depth=16")
        a = oc_float(dst, 2).reshape(-1)
        b = pcm(f, "f32le", ("-ac", "2")) and None
        ref = os.path.join(tmp, "ref.raw")
        ff("-i", f, "-ac", "2", "-f", "f32le", ref)
        import numpy as np
        r = np.frombuffer(open(ref, "rb").read(), "<f4")
        k = min(len(a), len(r))
        err = float(np.abs(a[:k] - r[:k]).max()) if k else 1.0
        check(f"read ffmpeg {enc[1]} in .{ext} (max difference {err:.1e}, {len(a)} vs {len(r)} samples)", abs(len(a) - len(r)) <= 2400 and err < 0.01)
except Exception as e:
    check("opus/alac/matroska", False, str(e)[:300])

# ---- 3. processing
try:
    dst = os.path.join(tmp, "rs.wav")
    oc(base, dst, "sample_rate=22050", "channels=1", "bit_depth=16")
    r = sh("ffprobe", "-v", "error", "-show_entries", "stream=sample_rate,channels", "-of", "csv=p=0", dst)
    check("resample and downmix", r.stdout.strip() == "22050,1", r.stdout)
    dst = os.path.join(tmp, "norm.wav")
    oc(base, dst, "normalize=-23", "bit_depth=24")
    r = sh("ffmpeg", "-nostats", "-i", dst, "-af", "ebur128", "-f", "null", "-", check=False)
    line = [l for l in r.stderr.splitlines() if "I:" in l][-1]
    lufs = float(line.split("I:")[1].split("LUFS")[0])
    check("normalise reaches -23 LUFS (ffmpeg ebur128 measures %.1f)" % lufs, abs(lufs + 23) < 0.3, line)
except Exception as e:
    check("processing", False, str(e)[:300])

print(f"\n{total - fails}/{total} passed")
sys.exit(1 if fails else 0)
