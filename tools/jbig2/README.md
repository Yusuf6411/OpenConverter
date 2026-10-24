# JBIG2 test tools

`jb2enc.py` and `jb2huff.py` are a small JBIG2 encoder (generic, refinement, symbol, text, pattern and halftone segments; arithmetic, Huffman and MMR) written only to make test streams for `crates/oc-jbig2`. `jbcases.py` defines the cases, and `jbfix.py DIR` writes them to `DIR` together with the reference page for each.

Each case is wrapped in a PDF and decoded by MuPDF (`pymupdf`) and pdfium (`pypdfium2`); the reference is the encoder's input when at least one of them reproduces it (they must agree with each other otherwise). Needs `pymupdf`, `pypdfium2`, `numpy` and `Pillow`.

    python3 tools/jbig2/jbfix.py crates/oc-jbig2/tests/data
