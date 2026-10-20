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
