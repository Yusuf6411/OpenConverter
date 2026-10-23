# Matroska and WebM

Own EBML/Matroska reader and writer in `oc-container::matroska` (RFC 8794 and RFC 9559), audio mapping in `oc-audio::mka`, container-to-container copying in `oc-audio::copy`.

## Container
Reads: EBML header and DocType, Segment (also of unknown size), SeekHead (ignored, the file is scanned), Info, Tracks (audio, video and subtitle entries, with all the fields the remux phase needs), Clusters (also of unknown size), SimpleBlock and BlockGroup with Xiph, EBML and fixed-size lacing, header-stripping content compression, DiscardPadding, CodecDelay and SeekPreRoll, Cues (skipped), Tags, Chapters and Attachments. Damaged elements end the scan with what was read before them; no input makes the parser panic or allocate without bound (fuzz target `matroska`).
Writes: clusters of at most 5 s, SimpleBlock (BlockGroup where a block needs DiscardPadding or a duration), Cues, SeekHead, Tags, Chapters, Attachments, correct sizes (no unknown-size elements).

## Audio
`.mka` and `.weba`/`.webm` (audio only; the video side arrives in Phase 5a).
- **Read**: A_OPUS (OpusHead private data, pre-skip, DiscardPadding), A_VORBIS (Xiph-laced headers), A_FLAC, A_MPEG/L3, A_PCM/INT/LIT, A_PCM/INT/BIG and A_PCM/FLOAT/IEEE. Tags (any target level, Vorbis names), cover art (image attachments). The reader rebuilds the codec's own stream and hands it to that codec's decoder.
- **Write**: `codec=auto|flac|opus|vorbis|pcm` (WebM: opus or vorbis). `auto` keeps the source's codec when it can go in the container, and otherwise uses FLAC (Matroska) or Opus (WebM).
- **Stream copy**: Opus and Vorbis between Ogg, Matroska and WebM, and FLAC between `.flac` and Matroska, are moved packet by packet with no decoding or encoding, when no processing option (rate, channels, trim, fades, normalise, bit depth) and no encoder setting is given. Tags and pictures are carried across; WebM drops pictures with a note. The route line says "no re-encoding".

Checked by `tools/audio_interop.py`: ffmpeg decodes every file we write with the same duration, and our reader matches ffmpeg's decode of its own libopus/FLAC/Vorbis/PCM/MP3 files (FLAC and PCM exactly).

Not done: A_AAC, A_ALAC, A_AC3 and other codecs (typed error: `codec_not_in_build` for AAC and AC-3), Matroska 'Void' padding and CRC-32 elements on write, EBML lacing on write, per-codec seeking with Cues on read.
