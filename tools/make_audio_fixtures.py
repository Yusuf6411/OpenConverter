#!/usr/bin/env python3
"""Writes the audio test fixtures under crates/oc-audio/tests/data using ffmpeg (a test-time
tool only): small encoded files and the s16le PCM that ffmpeg's own decoder gives for them."""
import os, subprocess, sys

out = os.path.join(os.path.dirname(__file__), "..", "crates", "oc-audio", "tests", "data")
os.makedirs(out, exist_ok=True)


def ff(*a):
    r = subprocess.run(["ffmpeg", "-v", "error", "-y", *a], capture_output=True, text=True)
    if r.returncode:
        sys.exit(r.stderr)


def src(rate, layout, secs):
    return ["-f", "lavfi", "-i", f"sine=frequency=440:duration={secs}:sample_rate={rate},volume=0.5",
            "-f", "lavfi", "-i", f"anoisesrc=d={secs}:a=0.05:r={rate}",
            "-filter_complex", f"amix=inputs=2:normalize=0,aformat=channel_layouts={layout}"]


def make(name, rate, layout, secs, enc):
    path = os.path.join(out, name)
    ff(*src(rate, layout, secs), *enc, path)
    ff("-i", path, "-f", "s16le", "-acodec", "pcm_s16le", path + ".pcm")


make("vorbis_stereo_q5.ogg", 44100, "stereo", 0.4, ["-c:a", "libvorbis", "-q:a", "5"])
make("vorbis_mono_q2.ogg", 22050, "mono", 0.4, ["-c:a", "libvorbis", "-q:a", "2"])
make("vorbis_51_q4.ogg", 48000, "5.1", 0.3, ["-c:a", "libvorbis", "-q:a", "4"])
make("vorbis_stereo_qm1.ogg", 44100, "stereo", 0.4, ["-c:a", "libvorbis", "-q:a", "-1"])


# ---- MP3: LAME encodes (a tone, noise and clicks so that short blocks and joint stereo occur)
def mp3_src(rate, layout, secs):
    return ["-f", "lavfi", "-i", f"sine=frequency=440:duration={secs}:sample_rate={rate},volume=0.4",
            "-f", "lavfi", "-i", f"anoisesrc=d={secs}:a=0.03:r={rate}",
            "-f", "lavfi", "-i", f"aevalsrc='if(lt(mod(t,0.1),0.002),random(0)-0.5,0)':d={secs}:s={rate}",
            "-filter_complex", f"amix=inputs=3:normalize=0,aformat=channel_layouts={layout}"]


def mp3_make(name, rate, layout, secs, enc, extra=()):
    path = os.path.join(out, name)
    ff(*mp3_src(rate, layout, secs), *enc, *extra, path)
    ff("-i", path, "-f", "s16le", "-acodec", "pcm_s16le", path + ".pcm")


mp3_make("mp3_stereo_44k.mp3", 44100, "stereo", 0.4, ["-c:a", "libmp3lame", "-b:a", "128k"])
mp3_make("mp3_mono_22k.mp3", 22050, "mono", 0.4, ["-c:a", "libmp3lame", "-b:a", "48k"])
mp3_make("mp3_mono_8k.mp3", 8000, "mono", 0.5, ["-c:a", "libmp3lame", "-b:a", "16k"])
mp3_make("mp3_tagged_48k.mp3", 48000, "stereo", 0.3, ["-c:a", "libmp3lame", "-q:a", "2"],
         ["-id3v2_version", "3", "-write_id3v1", "1", "-metadata", "title=Test Title", "-metadata", "artist=Some Artist",
          "-metadata", "album=An Album", "-metadata", "track=3/12", "-metadata", "date=2020", "-metadata", "genre=Rock"])


# ---- ALAC: ffmpeg's encoder (M4A), with the PCM its decoder gives back (s32le keeps 24 bit)
def alac_make(name, rate, layout, secs, fmt, extra=()):
    path = os.path.join(out, name)
    ff(*mp3_src(rate, layout, secs), "-c:a", "alac", "-sample_fmt", fmt, *extra, path)
    ff("-i", path, "-f", "s32le", "-acodec", "pcm_s32le", path + ".pcm")


alac_make("alac_stereo_16.m4a", 44100, "stereo", 0.5, "s16p")
alac_make("alac_mono_16.m4a", 22050, "mono", 0.4, "s16p")
alac_make("alac_stereo_24.m4a", 48000, "stereo", 0.4, "s32p", ["-bits_per_raw_sample", "24"])
alac_make("alac_51_16.m4a", 48000, "5.1", 0.3, "s16p")

ff(*mp3_src(44100, "stereo", 0.4), "-c:a", "alac", "-sample_fmt", "s16p", os.path.join(out, "alac_stereo_16.caf"))
ff("-i", os.path.join(out, "alac_stereo_16.caf"), "-f", "s32le", "-acodec", "pcm_s32le", os.path.join(out, "alac_stereo_16.caf.pcm"))


# ---- Opus: libopus encodes (CELT-only, low-delay application) with libopus's own float decode
def opus_make(name, rate, layout, secs, extra=()):
    path = os.path.join(out, name)
    ff(*mp3_src(rate, layout, secs), "-c:a", "libopus", "-application", "lowdelay", *extra, path)
    ff("-c:a", "libopus", "-i", path, "-f", "f32le", "-acodec", "pcm_f32le", path + ".pcm")


opus_make("opus_celt_mono_20ms.opus", 48000, "mono", 0.4, ["-frame_duration", "20", "-b:a", "48k"])
opus_make("opus_celt_stereo_20ms.opus", 48000, "stereo", 0.4, ["-frame_duration", "20", "-b:a", "96k"])
opus_make("opus_celt_stereo_10ms.opus", 48000, "stereo", 0.4, ["-frame_duration", "10", "-b:a", "64k"])
opus_make("opus_celt_stereo_5ms.opus", 48000, "stereo", 0.3, ["-frame_duration", "5", "-b:a", "128k"])
opus_make("opus_celt_mono_2_5ms.opus", 48000, "mono", 0.3, ["-frame_duration", "2.5", "-b:a", "64k"])
opus_make("opus_celt_stereo_60ms.opus", 48000, "stereo", 0.5, ["-frame_duration", "60", "-b:a", "64k"])
opus_make("opus_celt_stereo_vbr_wb.opus", 16000, "stereo", 0.4, ["-frame_duration", "20", "-b:a", "32k", "-cutoff", "8000"])
