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
