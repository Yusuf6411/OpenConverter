# MP3

Own decoder and encoder (`oc-audio::mp3`): MPEG-1, 2 and 2.5 layer III.

- **Decode**: long, short and mixed blocks, mid/side and intensity stereo, the bit reservoir, free-format is rejected. The Xing/Info/LAME header gives gapless trimming. Output matches ffmpeg and libmpg123 to 3e-6 on LAME streams at every sample rate. Tags: ID3v2 (2.2, 2.3, 2.4 with unsynchronisation and pictures), ID3v1 and APEv2 (see [tags](tags.md)).
- **Encode**: constant bit rate (8 to 320 kbit/s) or VBR quality, long blocks only, joint stereo, bit reservoir, Xing/LAME header and ID3v2.4 tags (UTF-8). It is tuned for SNR, not for a psychoacoustic model, so it is not a replacement for LAME at low rates. ffmpeg and libmpg123 decode its output to the exact length.

The encoder's LAME header carries the marker `LAMEOC2.2` instead of a LAME version so that no player mistakes it for LAME-encoded audio (ADR-0007). MP3 patents have expired (the last in 2017), so there is no licensing reason to leave it out.

Not done: short-block switching in the encoder, a psychoacoustic model, MPEG layers I and II.
