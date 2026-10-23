# PDF

Crates: `oc-pdf` (parser, decryption, content interpreter, renderer, text extraction, writer, page editing), `oc-engine/src/pdf.rs` (routes and options). Our own code throughout; the only dependencies are the RustCrypto hashes and ciphers needed for PDF encryption (see `deny.toml`).

## Routes
- PDF → PNG/JPEG/WebP/BMP/TIFF/GIF/QOI/ICO: the first chosen page, at `dpi` (default 150) or at `width`/`height`.
- PDF → TXT: reading-order text of the chosen pages.
- PDF → pages.zip: one file per page (`page_format` = png, jpeg, txt or pdf).
- PDF → PDF: keep/reorder `pages`, `rotate`, `compress`, `strip_metadata`, `password` (AES-256 output).
- Picture → PDF: one page per picture (animation frames each become a page); JPEG is stored without re-encoding. `page_size` fit/a4/letter, `margin`, `image_dpi`.
- Encrypted input: `source_password`. Owner permissions (copy, assemble, modify) are respected; a refusal names the missing permission.

## Supported
Cross-reference tables and streams, object streams, damaged-file recovery, RC4-40/128 and AES-128/256 (R2–R6), all common filters (Flate with predictors, LZW, ASCII85/Hex, RunLength, DCT, CCITT G3/G4), fonts (Type1/TrueType/CFF/Type0/Type3, standard 14 substituted from installed fonts), colour spaces (Device*, Cal*, Lab, ICCBased, Indexed, Separation, DeviceN), shadings types 1–7, tiling patterns, soft masks, transparency groups and blend modes, optional content, annotation appearances.

## How it was checked
Drawing is compared with Poppler (`pdftoppm`), text with `pdftotext` (F1 ≥ 0.94), and our writer/encryption output is opened by pikepdf and Poppler: `tools/pdf_cases.py`, `tools/pdf_interop.py`, `tools/pdf_text_interop.py`. A fuzz target is in `fuzz/fuzz_targets/pdf.rs`.

## Not done
- No OCR: pages that are only pictures give a warning and no text.
- JBIG2 and JPEG 2000 images are not decoded (a note is attached and the image is left out).
- Symbol and ZapfDingbats standard fonts draw nothing (no metrics shipped), with a note.
- Text extraction is geometric: tables and multi-column layouts follow reading order heuristics.
- Forms (AcroForm) are drawn only through their appearance streams; bookmarks, attachments and tagged structure are not carried by PDF → PDF.
- Output is not byte-identical to Poppler; edges differ by anti-aliasing.
