# JBIG2

Decoder in `crates/oc-jbig2`, wired in by `crates/oc-image/src/jbig2.rs` and the PDF `JBIG2Decode` filter (including `/JBIG2Globals`). Read only. Output is a bilevel picture, drawn as 8-bit grey.

**Source.** Written from ITU-T T.88 (ISO/IEC 14492). The standard prefix-code tables B.1 to B.15 are data from the standard (lists of prefix length, range length and range start); they were read from the table file of jbig2dec, which is the only place a free copy was found, and every table was then exercised: streams coded with B.1 to B.15 (and a custom table) decode correctly in pdfium or MuPDF.

**What is decoded.**

- Segments: page information (including unknown height with end-of-stripe segments), generic regions (templates 0 to 3, adaptive pixels, typical prediction, MMR, unknown segment length), generic refinement regions (both templates, typical prediction, reference from the page or from an intermediate region), symbol dictionaries, text regions, pattern dictionaries, halftone regions (with skip, grid rotation, MMR planes), custom Huffman tables, end of stripe, end of page.
- Symbol dictionaries: arithmetic and Huffman coding, refinement/aggregate symbols (single and multiple instances, arithmetic), context retention across dictionaries, export flags.
- Text regions: arithmetic and Huffman coding, all four reference corners, transposed, strips of 1 to 8 rows, refinement of individual symbols, all combination operators.
- Containers: embedded streams (PDF), standalone files in sequential and random-access organisation. Only the first page of a multi-page file is read.
- MMR (T.6) data is decoded through a callback; `oc-image` supplies its fax decoder, so `oc-jbig2` needs no fax tables. Halftone MMR bit planes need the consumed byte count, which `oc_image::ccitt::decode_mmr` returns.

**How it was checked.** No JBIG2 encoder was available, so `crates/oc-jbig2` was tested with an encoder written for the purpose (kept outside the repository; it is the mirror image of the decoder). Each of the 57 streams in `tests/data` (plus 10 standalone files in sequential and random-access layout) was decoded by MuPDF and by pdfium, through PDFs built around it. For 45 streams both match the encoder's input. For 8 only one does (pdfium alone on refinement with typical prediction, Huffman refinement, aggregate symbols, three Huffman tables and a custom table; MuPDF alone on one MMR collective bitmap) and the stored reference is the encoder's input. For the four transposed text regions both oracles agree with each other and with this decoder but not with the encoder, so those references are the oracles' output. The damage test (`JBIG2_FUZZ_ROUNDS`) flips bits and truncates the fixtures and requires errors, never panics or runaway allocation; the pixel limit of `Limits` applies to each bitmap and to all bitmaps of one decode together.

**Not done.** Encoding; Huffman-coded refinement/aggregate symbol dictionaries (typed unsupported-variant error); the extended 12-adaptive-pixel generic template of amendment 2; colour extensions; multi-page files beyond the first page; the profiles and extension segments are skipped.
