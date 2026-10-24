#!/usr/bin/env python3
"""Writes .Z fixtures with a small compress(1) clone (checked with the system `uncompress`)."""
import os, random, subprocess

def compress(data, maxbits=16, block=True):
    out = bytearray(b"\x1f\x9d" + bytes([maxbits | (0x80 if block else 0)]))
    nbits, bitbuf, nb_in = 9, 0, 0   # bit accumulator
    codes_in_group = 0
    region = 0  # bit position where the current code width began
    def emit(code):
        nonlocal bitbuf, nb_in, codes_in_group
        bitbuf |= code << nb_in; nb_in += nbits; codes_in_group += 1
        while nb_in >= 8:
            out.append(bitbuf & 255); bitbuf >>= 8; nb_in -= 8
    def pad_group():
        nonlocal bitbuf, nb_in, codes_in_group, region
        # finish the current group of 8 codes with zero bits
        total = (len(out) - 3) * 8 + nb_in
        group = nbits * 8
        target = region + -(-(total - region) // group) * group
        pad = target - total
        while pad > 0:
            take = min(pad, 8 - nb_in if nb_in else 8)
            nb_in += take; pad -= take
            if nb_in >= 8:
                out.append(bitbuf & 255); bitbuf >>= 8; nb_in -= 8
        region = (len(out) - 3) * 8 + nb_in
    first_free = 257 if block else 256
    table = {bytes([i]): i for i in range(256)}
    free = first_free
    maxcode = (1 << nbits) - 1
    maxmax = 1 << maxbits
    w = b""
    for c in data:
        wc = w + bytes([c])
        if wc in table:
            w = wc; continue
        emit(table[w])
        # compress(1) widens the code after writing a code once the table is full for the width
        if free > maxcode and nbits < maxbits:
            pad_group(); nbits += 1
            maxcode = maxmax if nbits == maxbits else (1 << nbits) - 1
        if free < maxmax:
            table[wc] = free; free += 1
        w = bytes([c])
    if w:
        emit(table[w])
    if nb_in:
        out.append(bitbuf & 255)
    return bytes(out)

root = os.path.join(os.path.dirname(__file__), "..", "tests", "data", "compress")
os.makedirs(root, exist_ok=True)
r = random.Random(3)
words = [bytes(r.choice(b"abcdefgh") for _ in range(r.randint(2, 8))) for _ in range(300)]
samples = {"text": b" ".join(r.choice(words) for _ in range(20000)), "short": b"abababababab", "noise": bytes(r.randrange(256) for _ in range(5000))}
for name, raw in samples.items():
    for bits in (12, 16):
        z = compress(raw, bits)
        open(os.path.join(root, f"{name}-{bits}.Z"), "wb").write(z)
        open(os.path.join(root, f"{name}-{bits}.raw"), "wb").write(raw)
        back = subprocess.run(["uncompress", "-c"], input=z, capture_output=True)
        print(name, bits, len(raw), "->", len(z), "uncompress ok:", back.stdout == raw)
