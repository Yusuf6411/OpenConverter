# WAV, AIFF, CAF (PCM)

Own readers and writers in `oc-audio` (`wav.rs`, `aiff.rs`, `caf.rs`).

- **WAV**: RIFF, RF64 and BW64; PCM at 8 to 32 bits, IEEE float, A-law, mu-law, `WAVE_FORMAT_EXTENSIBLE` with the speaker mask; LIST/INFO tags; odd-sized chunks, a `data` chunk that claims more than the file holds, and sizes of zero or 0xFFFFFFFF (streamed files) are handled. Writing picks float, PCM or the companded encodings (`encoding`); files that do not fit RIFF's 32-bit sizes become RF64.
- **AIFF / AIFF-C**: big-endian PCM, little-endian `sowt` (16-bit only, noted otherwise), `fl32`, `fl64`, `alaw`, `ulaw`; the 80-bit extended sample rate is converted exactly; NAME, AUTH, (c) and ANNO text chunks as tags.
- **CAF**: `lpcm` in either byte order, integer and float, and ALAC (see [alac](alac.md)).

Checked against ffmpeg both ways by `tools/audio_interop.py` (bit-identical samples for every integer depth). The PCM paths never go through floating point, so 24- and 32-bit conversions are lossless.

Not done: ADPCM and other WAVE codecs (a typed error names the format tag), AIFF `ima4`, CAF chunks other than `desc`, `data`, `pakt`, `kuki` and `info` (channel layouts are not read).
