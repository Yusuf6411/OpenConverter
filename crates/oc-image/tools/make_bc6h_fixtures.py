"""BC6H fixtures: random blocks of every mode and the half-floats bcdec decodes from them.

Dev-time only. Needs bcdec.h (github.com/iOrange/bcdec) and a C compiler:
    python3 tools/make_bc6h_fixtures.py path/to/bcdec.h
`<name>.blocks` holds N blocks of 16 bytes, `<name>.half` the 16 x 3 half-float bit
patterns (u16 LE) of each block. Names starting `uf16` are unsigned, `sf16` signed.
"""
import os
import random
import struct
import shutil
import subprocess
import sys
import tempfile

out = os.path.join(os.path.dirname(__file__), "..", "tests", "data", "bc6h")
os.makedirs(out, exist_ok=True)
rng = random.Random(11)

# (selector value, selector bit count)
MODES = [(0, 2), (1, 2), (2, 5), (6, 5), (10, 5), (14, 5), (18, 5), (22, 5), (26, 5),
         (30, 5), (3, 5), (7, 5), (11, 5), (15, 5)]


def block(sel, n):
    v = rng.getrandbits(128)
    v &= ~((1 << n) - 1)
    return (v | sel).to_bytes(16, "little")


here = os.path.dirname(os.path.abspath(__file__))
work = tempfile.mkdtemp()
shutil.copy(sys.argv[1], os.path.join(work, "bcdec.h"))
oracle = os.path.join(work, "oracle")
subprocess.run(["cc", "-O1", "-o", oracle, os.path.join(here, "bc6h_oracle.c"), "-I", work], check=True)


def write(name, blocks, signed):
    data = b"".join(blocks)
    halves = subprocess.run([oracle, "1" if signed else "0"], input=data, capture_output=True, check=True).stdout
    open(os.path.join(out, name + ".blocks"), "wb").write(data)
    open(os.path.join(out, name + ".half"), "wb").write(halves)
    print(name, len(blocks))


for kind, signed in (("uf16", False), ("sf16", True)):
    for sel, n in MODES:
        write(f"{kind}_m{sel}", [block(sel, n) for _ in range(96)], signed)
    write(f"{kind}_mixed", [block(*rng.choice(MODES)) for _ in range(512)], signed)
