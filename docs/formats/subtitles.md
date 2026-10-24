# Subtitles: SRT, WebVTT, ASS/SSA, TTML

Crate: `oc-subtitle` (own code, uses `oc-xml` for TTML). Engine glue: `oc-engine::subtitle`.

All four formats read into one cue list (`Track`: start, end, text, alignment, style name, layer) and write from it. Text carries a small inline markup shared by every format: `<b> <i> <u> <s> <font color="#RRGGBB">` and `\n`. Each reader maps its own styling onto that and each writer maps it back, so bold, italic, underline, strike-out and colour survive a conversion; anything else is dropped with a note.

| Format | Read | Write | Notes |
| --- | --- | --- | --- |
| SRT | cues with or without index lines or blank lines, HTML-ish tags, `{\an8}` | `<b> <i> <u> <font>`, `{\anN}` for alignment | CRLF by default, any encoding |
| WebVTT | cue ids, settings (`align`, `line`), NOTE, `<v>`, `<c.colorRRGGBB>`/`<c.red>`, karaoke timestamps stripped | colour as `<c.colorRRGGBB>` plus a `STYLE` block (WebVTT has no `<font>`), strike-out dropped | UTF-8 only |
| ASS/SSA | `Format:` line honoured, `\b \i \u \s \c \an \N \h \r`, layer, style name | the original script header (styles, fonts, resolution) when the input was ASS, else a default `Default` style (Arial 64, 1920x1080) | other override tags (position, fade, karaoke, drawing) dropped |
| TTML | `p` with `begin`/`end`/`dur` as clock, frames (`ttp:frameRate`), `h m s ms f t` offsets (`ttp:tickRate`); `span` styling inline or through `style` references; `br`; `textAlign`/`displayAlign` | one `p` per cue, `tts:` styled `span`s | regions, animations, ruby not read |

Options on every subtitle output: input encoding (auto: BOM, UTF-8, else windows-1252), shift in milliseconds, retime from one frame rate to another (`fps_from / fps_to`), remove styling. SRT and ASS also take output encoding, BOM and line endings.

Cues are sorted by start, empty cues are removed and cues that end before they start are fixed; each of these reports a note.

## Checked against

- `tools/subtitle_interop.py`: ffmpeg reads our ASS and WebVTT and gets the same cues; we read ffmpeg's ASS, WebVTT and TTML. 12/12.
- Unit tests per format and an engine test that goes SRT to ASS to WebVTT to TTML to SRT with a shift and a frame-rate retime.
- Fuzz target `subtitle` (all four parsers and writers on arbitrary text).

## Not done

- Bitmap subtitles (PGS, VobSub, DVB) and OCR.
- SubViewer, MicroDVD, SAMI, EBU STL, SCC, iTT.
- ASS styles are not translated to other formats' styling (only emphasis and colour are), WebVTT `STYLE` and `REGION` blocks are not read, TTML regions are not read.
- Subtitle tracks inside MP4 and Matroska files are handled with the video containers (see [containers](containers.md)).
