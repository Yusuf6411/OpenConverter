"""Reads the QM-coder state table out of the system libjpeg (jpeg_aritab, 114 entries of
8 bytes) and prints it as Rust. Dev-time only; the result is committed as
src/jpeg/arith_table.rs."""
import struct, sys

path = sys.argv[1] if len(sys.argv) > 1 else "/usr/lib/x86_64-linux-gnu/libjpeg.so.8.2.2"
d = open(path, "rb").read()
i = d.find(struct.pack("<I", 0x5A1D0181))  # first entry: Qe 0x5a1d, MPS -> 1, LPS -> 1 + switch
vals = struct.unpack("<114Q", d[i:i + 114 * 8])
for k in range(0, 114, 6):
    print("    " + ", ".join(f"0x{v:08x}" for v in vals[k:k + 6]) + ",")
