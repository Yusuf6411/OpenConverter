# VP8

Decoder and encoder in `crates/oc-vp8`, written from RFC 6386; constant tables are in ADR-0004.

**Decoder** (key and inter frames): three reference buffers with golden/altref sign bias, segmentation, loop-filter deltas, probability save and restore, all four split-MV shapes, sixtap and bilinear prediction (versions 0 to 3), the loop filter with its key/inter threshold difference, odd sizes, empty packets (repeat the last frame) and non-shown frames. Output is bit-exact with libvpx on streams made with altref, profiles 1-3, error-resilient and realtime settings (`tools/video_interop.py`, six cases).

**Encoder** (`VideoEncoder`): key frames and inter frames from the last and golden references; candidate modes ZERO/NEAREST/NEAR/NEW and split shapes, scored by rate-distortion; motion search in parallel by macroblock row (three-step search on SAD, then sub-pixel on SSE); quantiser steered to a bit rate with a debt term and key-frame re-encode on overshoot; golden refresh every 10 frames; token partitions; loop-filter level search. The decoder's output equals the encoder's reconstruction frame for frame (Rust test), and ffmpeg's native decoder, libvpx and this decoder agree on every stream written (4 sources x 3 option sets). Quality is about 47 dB at 185 KB where libvpx gives 47.3 dB at 176 KB on the test clip; a 300 kbit/s target gave 319 kbit/s.

Split motion vectors stay within 4 pixels of the frame edge (`EDGE_SLACK`) because ffmpeg's native decoder differs from libvpx for vectors pointing far outside.

**Not done**: altref frames, motion-vector probability updates, segmentation in the encoder, versions other than 0, two-pass encoding. Threading covers motion search and filter search only, so speed-up is modest.
