# Opus

Own range coder, CELT layer, MDCT and Ogg encapsulation (`oc-audio::opus`), RFC 6716 and RFC 7845.

## Decoder
CELT frames of 2.5, 5, 10 and 20 ms (and packets of up to 120 ms made of several frames, codes 0 to 3), mono and stereo, all bandwidths, transient (short-block) frames, tf changes, spreading, the pitch post-filter, intensity and dual stereo, anti-collapse, de-emphasis. Multistream (mapping families 1 and 255) with self-delimited packets, pre-skip, output gain and granule trimming. Mapping families 0, 1 and 255 are accepted; families 2 and 3 (ambisonics) are a typed error.

It is checked against libopus (through ffmpeg and `libopus.so` via ctypes) on twelve streams (all four frame sizes, stereo and mono, 60 ms packets, VBR/CBR, narrow to full bandwidth). The range-coder state after every frame (`final_range`) is identical to libopus's, which proves that the bitstream parse is exact; the decoded audio differs from libopus's float output by less than -70 dB (SNR 70 to 85 dB; libopus's float approximations of `exp2`, `cos` and friends account for the rest).

## Encoder
CELT-only, long blocks, 48 kHz output (other rates are resampled first, noted as `opus.resampled`). CBR, VBR and constrained VBR (`rate_mode`), 6 to 510 kbit/s, 2.5 to 20 ms frames, band energy with coarse and fine quantisation, PVQ with a greedy pulse search, spreading 2, allocation by the standard tables with dynamic-allocation off, dual stereo (L/R coded independently), multichannel via mapping family 1. Both our decoder and libopus decode the output (84 to 86 dB agreement between them).

The encoder is basic: it has no transient detector, no tf selection, no pitch pre-filter and no mid/side stereo, so it needs more bits than libopus for the same quality, especially on speech and on stereo with correlated channels (ADR-0008).

## Not done
- **SILK and hybrid frames**: decoding them raises `unsupported_variant` for that packet (the rest of the file is still decoded, with a warning). SILK needs about 40 KB of codebook tables and the RFC text was not reachable from the build environment; the tables cannot be recovered from a stripped libopus without symbols (ADR-0008). Opus files from VoIP encoders and `opusenc --speech` at low rates use SILK.
- SILK/hybrid encoding, in-band FEC, DTX and packet loss concealment, ambisonics mapping, the custom-mode API.
