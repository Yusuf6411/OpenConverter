#!/usr/bin/env python3
"""Extract clean text from the AV1 specification PDF (docs/specs/av1-spec.pdf).

The PDF draws many table glyphs twice at almost the same position, which `pdftotext` turns into
doubled numbers; this drops the doubles. Needs `pdfplumber`.

    python3 tools/av1_spec_text.py docs/specs/av1-spec.pdf /tmp/av1-spec.txt
"""
import sys

import pdfplumber

src, out = sys.argv[1], sys.argv[2]
with pdfplumber.open(src) as pdf, open(out, "w", encoding="utf-8") as f:
    for page in pdf.pages:
        text = page.dedupe_chars(tolerance=0.5).extract_text(x_tolerance=2, y_tolerance=2) or ""
        keep = [
            line
            for line in text.split("\n")
            if not line.startswith("AV1 Bitstream & Decoding") and not line.startswith("Section:")
        ]
        f.write("\n".join(keep) + "\n")
