# Tags and cover art

Every audio reader fills `Audio.tags` (ordered, repeatable KEY=value pairs in Vorbis vocabulary, upper case) and `Audio.pictures`. Every writer maps them to its own system and reports what it cannot keep as a loss note (`tags.dropped`).

| System | Where | Mapping |
| --- | --- | --- |
| Vorbis comments | FLAC, Ogg Vorbis, Opus | identity; pictures as FLAC PICTURE blocks / `METADATA_BLOCK_PICTURE` |
| ID3v2.2/2.3/2.4, ID3v1 | MP3 | text frames to keys (TIT2 to TITLE, TPE1 to ARTIST, TRCK to TRACKNUMBER, TDRC/TYER to DATE, ...), TXXX to its description, COMM, APIC; v1 only when nothing else is present |
| iTunes atoms | M4A | `©nam`, `©ART`, `©alb`, `aART`, `trkn`, `disk`, `covr`, freeform `----` |
| APEv2 | MP3 (trailing) | Title/Artist/Album/Genre/Comment as is; Year to DATE, Track to TRACKNUMBER, Disc to DISCNUMBER, Album Artist to ALBUMARTIST; `Cover Art (Front)` binary items to pictures; repeated values separated by NUL (`oc-audio::ape` reads and writes them) |
| Matroska Tags | MKA/WebM | SimpleTags by name at any target level (track-level tags win); pictures from image attachments, `cover.*` is the front cover; written as one track-level Tag (TargetTypeValue 30); WebM keeps tags but has no attachments |
| WAV INFO / AIFF text | WAV, AIFF | a small fixed set of keys; others are dropped with a note |
