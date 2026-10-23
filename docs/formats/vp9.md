# VP9

Decoder in `crates/oc-vp9`, written from the VP9 Bitstream and Decoding Process Specification v0.6 (`docs/specs/vp9-bitstream-specification-v0.6.pdf`). Decoding only: VP9 is a read-only video input (WebM, MP4, Matroska); the pictures can be written to any image or animation format, or re-encoded as VP8.

**Decoder.** Key and inter frames, hidden frames and `show_existing_frame`, superframes, all four profiles (8, 10 and 12 bit; 4:2:0, 4:2:2, 4:4:0, 4:4:4), tiles in both directions, segmentation, lossless mode (WHT), compound prediction, all transform types and sizes, the loop filter, backward probability adaptation, error-resilient and frame-parallel modes, motion-vector prediction from the previous frame, and scaled reference frames (the reference may differ in size from the frame). The probability, scan, and filter tables are generated from the specification text by `tools/gen_vp9_tables.py`, so none was copied from another decoder. Transforms are written as the specification's butterfly steps in 64-bit integers.

**How it was checked.** `tools/make_vp9_fixtures.py` encodes 14 streams with libvpx (through ffmpeg, and through libvpx directly for the resize case) and records a hash of each decoded frame from ffmpeg's native VP9 decoder, which is bit-exact with libvpx. `crates/oc-vp9/tests/ffmpeg.rs` requires every frame of every stream to match: key frames, inter frames, altref (hidden) frames, odd sizes, tiles, lossless, 4:4:4, 4:2:2, 10-bit, segmentation, error-resilient, frame-parallel and a stream that changes size mid-stream. `tests/robust.rs` damages each stream (flipped bytes, truncation) and requires errors, never panics. Fuzz target `vp9`.

**Where the specification and libvpx differ.** The decoder follows libvpx wherever the two disagree, since libvpx defines what real files need: key frames use the key-frame partition probabilities, loop-filter deltas are applied only when enabled, the reference sign bias is not reset by `setup_past_independence`, and high-precision motion-vector counts are updated even when high precision is off.

**Video.** `oc_video::vp9` hands each packet to the decoder and keeps the last picture it shows. Higher bit depths are rounded to 8 bit and 4:2:2, 4:4:0 and 4:4:4 chroma is averaged to 4:2:0, as for every other video here.

**Not done.** VP9 encoding; alpha (VP9 alpha is a separate stream in WebM, ignored); SVC spatial layers other than the last shown picture; 12-bit output beyond rounding to 8 bit.
