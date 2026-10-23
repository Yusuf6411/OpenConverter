# Apple Lossless (ALAC)

Own encoder and decoder (`oc-audio::alac`), containers M4A (`m4a.rs`, on the shared ISO-BMFF reader in `oc-container::isobmff`) and CAF (`caf.rs`).

- **Decode/encode**: 16, 20, 24 and 32 bits, 1 to 8 channels (single, pair and multi-element frames; the channel mask is not stored), adaptive-Golomb coding, the stage-1 adaptive predictor and stage-2 LMS filters, mixed and verbatim ("uncompressed escape") frames. The decoder and encoder are bit-exact with ffmpeg's `alac`: lossless round trips in both directions on every fixture.
- **M4A**: `ftyp` (`M4A `), `moov`/`trak`/`stbl` with `alac` sample entries, iTunes `ilst` tags and `covr` pictures (written and read). The sample count comes from the sample table; edit lists are not applied. AAC and AC-3 in an `.m4a` raise `codec_not_in_build` (ADR-0006); other codecs raise `unsupported_variant`.

Not done: AAC, fragmented MP4 audio, chapters (`chpl`), `meta` atoms other than iTunes keys.
