# FLAC

Own encoder and decoder (`oc-audio::flac`), RFC 9639.

- **Decode**: all block sizes, constant, verbatim, fixed and LPC subframes, the four stereo modes, 4 to 32 bits, Rice partitions (both parameter widths), STREAMINFO, VORBIS_COMMENT, PICTURE and SEEKTABLE blocks, the MD5 of the audio is verified and a mismatch is a warning. ID3v2 tags before `fLaC` are skipped. A damaged frame ends decoding with what came before and a warning.
- **Encode**: levels 0 to 8 (`level`), block size override, fixed and LPC prediction with quantised coefficients, mid/side choice per frame, Rice partition search, a seek table. Output passes libFLAC's MD5 check at every level and is within 3% of ffmpeg's size.

Not done: variable block size, CUESHEET and APPLICATION blocks (dropped on read).
