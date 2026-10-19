"""Reads the CCITT T.4/T.6 run-length code tables back from libtiff.

Modified Huffman TIFFs (compression 2) are byte-aligned per row and have no EOLs, so a
row of a white run, one black pixel and one white pixel (or a leading zero-length white
run, a black run and one white pixel) holds exactly the code to extract, followed by codes
that are known. The result is written as Rust source.

Run: python3 gen_ccitt_tables.py ../src/ccitt_tables.rs
"""
import io
import sys

import numpy as np
from PIL import Image

W0 = "00110101"  # white run 0
B0 = "0000110111"  # black run 0
W1 = "000111"  # white run 1
B1 = "010"  # black run 1
# Anchors to prove the extraction picks the right colours.
ANCHORS_W = {2: "0111", 3: "1000", 4: "1011", 8: "10011"}
ANCHORS_B = {2: "11", 3: "10", 4: "011", 8: "000101"}


def mh_bits(row):
    """Encode one row (True = a pixel of value 1, which libtiff codes as a black run) as Modified Huffman through libtiff; returns the bit string."""
    im = Image.fromarray(np.array([row], dtype=bool))
    buf = io.BytesIO()
    im.save(buf, format="TIFF", compression="tiff_ccitt")
    buf.seek(0)
    t = Image.open(buf)
    off = t.tag_v2[273][0]
    n = t.tag_v2[279][0]
    data = buf.getvalue()[off:off + n]
    bits = "".join(f"{b:08b}" for b in data)
    if t.tag_v2.get(266, 1) == 2:
        raise SystemExit("unexpected fill order")
    return bits


def strip_tail(bits, tail):
    stripped = bits.rstrip("0")
    assert stripped.endswith(tail), (bits, tail)
    return stripped[: -len(tail)]


def white_code(n):
    # n white pixels, then one black, then one white
    row = [False] * n + [True, False]
    bits = strip_tail(mh_bits(row), B1 + W1)
    return bits


def black_code(n):
    # (white 0) black n, then one white. A row cannot start with a zero-length run in the
    # array, so begin with a black pixel run.
    row = [True] * n + [False]
    bits = strip_tail(mh_bits(row), W1)
    assert bits.startswith(W0), bits
    return bits[len(W0):]


white, black = {}, {}
for n in range(0, 64):
    white[n] = white_code(n)
    black[n] = black_code(n) if n else B0  # a black run of 0 cannot be written on its own
for n in range(64, 2561, 64):
    w = white_code(n)
    assert w.endswith(W0), (n, w)
    white[n] = w[: -len(W0)]
    b = black_code(n)
    assert b.endswith(B0), (n, b)
    black[n] = b[: -len(B0)]

assert white[0] == W0 and black[0] == B0 and white[1] == W1 and black[1] == B1
for n, c in ANCHORS_W.items():
    assert white[n] == c, (n, white[n])
for n, c in ANCHORS_B.items():
    assert black[n] == c, (n, black[n])


def prefix_free(tab):
    codes = sorted(tab.values())
    for a, b in zip(codes, codes[1:]):
        assert not b.startswith(a), (a, b)
    assert len(set(codes)) == len(codes)


prefix_free(white)
prefix_free(black)
# Extended make-up codes (1792..2560) are shared by both colours.
for n in range(1792, 2561, 64):
    assert white[n] == black[n], n

with open(sys.argv[1], "w") as f:
    f.write("//! CCITT T.4/T.6 run-length codes, read back from libtiff by tools/gen_ccitt_tables.py.\n")
    f.write("//! Each entry is (code bits, code length, run length).\n\n")
    for name, tab in (("WHITE", white), ("BLACK", black)):
        f.write(f"pub const {name}: [(u16, u8, u16); {len(tab)}] = [\n")
        for n in sorted(tab):
            c = tab[n]
            f.write(f"    (0b{c}, {len(c)}, {n}),\n")
        f.write("];\n\n")
print(len(white), len(black))
