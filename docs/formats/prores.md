# Apple ProRes

`oc-prores` decodes ProRes frames from SMPTE RDD 36:2022 (`docs/specs/smpte-rdd36.pdf`). Every
packet of a ProRes stream is one frame; there is no inter prediction.

## What is covered

- 4:2:2 and 4:4:4 frames (Proxy, LT, Standard, HQ, 4444, 4444 XQ), bitstream versions 0 and 1.
- Progressive and interlaced frames (two field pictures, either field first).
- Custom quantisation matrices, all slice sizes (1, 2, 4, 8 macroblocks) and frame sizes that are
  not a multiple of 16.
- 8-bit and 16-bit alpha channels (run-length and difference coded).

Not done: encoding, and bit depths above 10 on output (the decoder produces 10-bit samples,
which is what the stream carries).

## How it is checked

`tools/make_prores_fixtures.py` encodes frames with ffmpeg's `prores_ks` and decodes them with
ffmpeg's `prores` decoder. The Rust decoder agrees with it to within one level on every sample
(`crates/oc-prores/tests/ffmpeg.rs`); alpha, which is lossless, agrees exactly. The difference
comes from the inverse DCT: the specification allows any implementation that meets its accuracy
annex, and this one uses double precision where ffmpeg uses an integer transform.
Output samples are clamped to 4..1019, which keeps clear of the synchronisation levels, as
the specification allows and as ffmpeg does.

Truncated and bit-flipped frames give errors, never panics (`tests/robust.rs`, `fuzz/prores`).

## In conversions

MOV and MP4 `apco/apcs/apcn/apch/ap4h/ap4x` entries are read as codec `prores`. The stream can be
copied into another MOV unchanged, or decoded (to 8-bit 4:2:0, dropping alpha) and re-encoded
into a codec the target container holds.
