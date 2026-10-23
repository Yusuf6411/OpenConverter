# Video containers

All video containers meet in one packet-level model, `oc_video::Media`: streams plus packets in decode order with nanosecond times (decode time may be negative when frames are reordered). Codec setup travels in the Matroska `CodecPrivate` form (`avcC`, `hvcC`, AudioSpecificConfig, `OpusHead`, Xiph-laced Vorbis headers, `fLaC`), so a remux never touches compressed data. Interop checks against ffmpeg and ffprobe (oracles at test time only) are in `tools/video_interop.py`.

| Container | Read | Write | Streams carried |
|---|---|---|---|
| MP4, MOV | yes | yes, plain or fragmented (`fragmented=true`) | H.264, HEVC, VP8, VP9, AV1, MPEG-4 part 2, MJPEG; AAC, MP3, Opus, FLAC, PCM (MOV); SRT as tx3g |
| Matroska, WebM | yes | yes | the same plus Vorbis, AC-3, ASS, WebVTT |
| AVI | yes | yes | MJPEG, MPEG-4 part 2, VP8; PCM, MP3, AC-3 |
| MPEG-TS, M2TS | yes (188 and 192 byte packets) | yes | H.264, HEVC; AAC (ADTS), MP3, AC-3 |
| MPEG-PS (.mpg, .vob) | yes | no | H.264, HEVC; AAC, MP3, AC-3 |
| FLV | yes | no | H.264, HEVC (classic and enhanced tags); AAC, MP3, PCM |
| Y4M | yes | yes | raw 4:2:0, 4:2:2, 4:4:4, mono |
| Image sequence (`.frames.zip`) | yes | yes | ZIP of numbered PNG or JPEG pictures; read in natural name order, frame rate from the `fps` option (25 if absent) |

## What the engine does with them
- **Remux**: when the target container can hold a stream and no option changes the picture or the sound, packets are copied as they are. Streams a container cannot hold are left out with a note, or the conversion fails when nothing is left.
- **Re-encode**: pictures can be decoded for MJPEG, raw video and VP8, and encoded as VP8 (own encoder: inter prediction, split motion vectors, rate control, threading) or MJPEG. Audio decodes for PCM, Opus, Vorbis, FLAC and MP3.
- **Edits**: scale, crop, rotate, mirror, frame rate, trim, `pad` (keep proportions with black bars), `deinterlace` (line averaging), `audio_offset_ms`, `streams` (which streams to keep).
- **Subtitles** are rewritten between SRT, ASS and WebVTT as needed.
- **Pictures**: any decodable video to GIF, animated WebP and APNG, and back.

## Checked
- ProRes in MOV/MP4 is copied or decoded ([prores](prores.md)).
- Ogg video (Theora with Vorbis) is read ([theora](theora.md)); it can be copied into Matroska or decoded.
- MPEG-TS written here decodes to the same frames and samples in ffmpeg as the source, and passes `-err_detect explode`; FLV, M2TS, MPEG-TS from ffmpeg and PS built by a test script read back to the same pictures.
- Fragmented MP4 decodes identically to the plain file and passes strict decoding.
- Fuzz target `video` (readers, plus writers fed with whatever the readers accept).

## Not done
- AVI OpenDML (files over 1 GB), MPEG-1/2 video and MP2 audio, DTS, E-AC-3 in TS, AV1 and VP9 inside FLV, MP4 chapters and cover art.
- MPEG-PS times: a picture that begins inside a PES packet without its own time stamp gets one interpolated from its neighbours; with B-frames the presentation order of those pictures can be wrong.
- Colour matrices and pixel format conversion (everything is 8-bit 4:2:0 once decoded; MJPEG to YUV goes through RGB).
- Muxers for FLV and MPEG-PS.
- Opus end trimming does not survive MP4 (ffmpeg ignores it there too).
