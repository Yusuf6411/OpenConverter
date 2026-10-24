#!/usr/bin/env python3
"""Writes tests/data/archives/*.cab: stored, MSZIP, LZX (verbatim, aligned, stored blocks).
The LZX encoder here is a small test-only greedy encoder; every output is also checked with
`cabextract -t` when available, which is the independent oracle."""
import heapq, os, random, struct, subprocess, sys, zlib

OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "oc-archive", "tests", "data", "archives")
os.makedirs(OUT, exist_ok=True)

def content():
    rnd = random.Random(42)
    words = [bytes(rnd.choices(b"abcdefghijklmnopqrstuvwxyz", k=rnd.randint(2, 9))) for _ in range(300)]
    text = b" ".join(rnd.choice(words) for _ in range(30000))
    binary = bytes(rnd.getrandbits(8) for _ in range(5000)) + bytes(range(256)) * 20 + b"\xe8\x01\x02\x03\x04" * 50
    return [("readme.txt", text), ("sub\\data.bin", binary), ("empty.txt", b""), ("sub\\deeper\\x.txt", b"hello cabinet\n" * 100)]

# ---------- cab container ----------
def csum(data, seed=0):
    c = seed
    n = len(data) // 4 * 4
    for (w,) in struct.iter_unpack("<I", data[:n]):
        c ^= w
    rem = data[n:]
    ul = 0
    if len(rem) == 3: ul = rem[0] << 16 | rem[1] << 8 | rem[2]
    elif len(rem) == 2: ul = rem[0] << 8 | rem[1]
    elif len(rem) == 1: ul = rem[0]
    return c ^ ul

def build_cab(files, blocks, method):
    """blocks: list of (compressed_bytes, uncompressed_len)."""
    nfiles = len(files)
    names = [n.encode() + b"\0" for n, _ in files]
    file_table_len = sum(16 + len(n) for n in names)
    coff_files = 36 + 8
    data_off = coff_files + file_table_len
    cfdata = b""
    for comp, ulen in blocks:
        hdr = struct.pack("<HH", len(comp), ulen)
        cfdata += struct.pack("<I", csum(hdr, csum(comp))) + hdr + comp
    total = data_off + len(cfdata)
    out = b"MSCF" + struct.pack("<IIIIIBBHHHHH", 0, total, 0, coff_files, 0, 3, 1, 1, nfiles, 0, 1234, 0)
    out += struct.pack("<IHH", data_off, len(blocks), method)
    off = 0
    for (n, d), nb in zip(files, names):
        out += struct.pack("<IIHHHH", len(d), off, 0, (2024 - 1980) << 9 | 5 << 5 | 17, 13 << 11 | 30 << 5 | 7, 0x20) + nb
        off += len(d)
    return out + cfdata

def frames(files):
    data = b"".join(d for _, d in files)
    return data, [data[i:i + 32768] for i in range(0, len(data), 32768)]

# ---------- LZX encoder ----------
EXTRA = []; BASE = []
j = 0; i = 0
EXTRA = [0] * 52
while i <= 50:
    EXTRA[i] = EXTRA[i + 1] = j
    if i != 0 and j < 17: j += 1
    i += 2
acc = 0
for i in range(52):
    BASE.append(acc); acc += 1 << EXTRA[i]
SLOTS = 30  # window 2^15

class W:
    def __init__(self): self.bits = []
    def w(self, v, n):
        for k in range(n - 1, -1, -1): self.bits.append((v >> k) & 1)
    def pad16(self):
        while len(self.bits) % 16: self.bits.append(0)
    def bytes(self):
        self.pad16(); out = bytearray()
        for k in range(0, len(self.bits), 16):
            v = 0
            for b in self.bits[k:k + 16]: v = v << 1 | b
            out += struct.pack("<H", v)
        return bytes(out)

def huff_lengths(counts, limit):
    syms = [(c, s) for s, c in enumerate(counts) if c]
    if len(syms) == 0: return [0] * len(counts)
    if len(syms) == 1:
        syms.append((1, 0 if syms[0][1] != 0 else 1))
    heap = [(c, s, None, None) for c, s in syms]
    heapq.heapify(heap); n = len(counts); uid = n
    nodes = {}
    h = [(c, s, s) for c, s in syms]
    heapq.heapify(h)
    parent = {}
    while len(h) > 1:
        c1, t1, id1 = heapq.heappop(h); c2, t2, id2 = heapq.heappop(h)
        uid += 1
        parent[id1] = uid; parent[id2] = uid
        heapq.heappush(h, (c1 + c2, uid, uid))
    lens = [0] * n
    for c, s in syms:
        d = 0; x = s
        while x in parent: x = parent[x]; d += 1
        lens[s] = d
    assert max(lens) <= limit, "tree too deep for test encoder"
    return lens

def canon(lens):
    codes = [0] * len(lens)
    code = 0
    for l in range(1, 17):
        for s, sl in enumerate(lens):
            if sl == l:
                codes[s] = code; code += 1
        code <<= 1
    return codes

def put_sym(w, lens, codes, s): w.w(codes[s], lens[s])

def write_lengths(w, lens, prev, first, last):
    seq = []  # (pretree symbol, extra_bits_value, extra_bit_count)
    i = first
    while i < last:
        if lens[i] == 0:
            run = 1
            while i + run < last and lens[i + run] == 0: run += 1
            if run >= 20:
                n = min(run, 51); seq.append((18, n - 20, 5)); i += n; continue
            if run >= 4:
                n = min(run, 19); seq.append((17, n - 4, 4)); i += n; continue
        seq.append(((prev[i] - lens[i]) % 17, 0, 0)); i += 1
    counts = [0] * 20
    for s, _, _ in seq: counts[s] += 1
    pl = huff_lengths(counts, 15)
    pc = canon(pl)
    for l in pl: w.w(l, 4)
    for s, v, nb in seq:
        put_sym(w, pl, pc, s)
        if nb: w.w(v, nb)

def lz_parse(frame, history):
    """Greedy parse; returns list of ('lit', byte) / ('match', offset, length)."""
    buf = history + frame
    base = len(history)
    table = {}
    for k in range(max(0, base - 3000), base - 2):
        table.setdefault(buf[k:k + 3], []).append(k)
    out = []; i = base; end = len(buf)
    while i < end:
        best = (0, 0)
        if i + 3 <= end:
            for cand in reversed(table.get(buf[i:i + 3], [])[-16:]):
                d = i - cand
                if d > 32768 - 3: break
                l = 0
                while i + l < end and l < 257 and buf[cand + l] == buf[i + l]: l += 1
                if l > best[0]: best = (l, d)
        if best[0] >= 3:
            out.append(("match", best[1], best[0]))
            for k in range(i, i + best[0]): table.setdefault(buf[k:k + 3], []).append(k)
            i += best[0]
        else:
            out.append(("lit", buf[i]))
            table.setdefault(buf[i:i + 3], []).append(i); i += 1
    return out

def encode_block(w, frame, history, state, aligned):
    ops = lz_parse(frame, history)
    R = state["R"]
    syms = []  # (kind, ...)
    main_counts = [0] * (256 + SLOTS * 8); len_counts = [0] * 249; al_counts = [0] * 8
    for op in ops:
        if op[0] == "lit":
            main_counts[op[1]] += 1; syms.append(("lit", op[1])); continue
        _, off, ln = op
        if off == R[0]: slot = 0
        elif off == R[1]: slot = 1; R[0], R[1] = R[1], R[0]
        elif off == R[2]: slot = 2; R[0], R[2] = R[2], R[0]
        else:
            f = off + 2
            slot = max(s for s in range(SLOTS) if BASE[s] <= f)
            R[2], R[1], R[0] = R[1], R[0], off
        if slot == 0: pass
        lh = min(ln - 2, 7)
        main_counts[256 + slot * 8 + lh] += 1
        lf = None
        if lh == 7: lf = ln - 2 - 7; len_counts[lf] += 1
        vbits = None
        if slot >= 3:
            f = off + 2 - BASE[slot]; eb = EXTRA[slot]
            if aligned and eb >= 3:
                vbits = (f >> 3, eb - 3, f & 7); al_counts[f & 7] += 1
            else:
                vbits = (f, eb, None)
        syms.append(("match", slot, lh, lf, vbits))
    state["R"] = R
    kind = 2 if aligned else 1
    w.w(kind, 3); w.w(len(frame) >> 8, 16); w.w(len(frame) & 255, 8)
    if aligned:
        al_lens = huff_lengths(al_counts, 7)
        # the aligned tree must be complete over 8 symbols with 3-bit lengths max
        for l in al_lens: w.w(l, 3)
        al_codes = canon(al_lens)
    main_lens = huff_lengths(main_counts, 16); main_codes = canon(main_lens)
    len_lens = huff_lengths(len_counts, 16); len_codes = canon(len_lens)
    write_lengths(w, main_lens, state["main"], 0, 256)
    write_lengths(w, main_lens, state["main"], 256, 256 + SLOTS * 8)
    write_lengths(w, len_lens, state["len"], 0, 249)
    state["main"] = main_lens; state["len"] = len_lens
    for s in syms:
        if s[0] == "lit": put_sym(w, main_lens, main_codes, s[1]); continue
        _, slot, lh, lf, vb = s
        put_sym(w, main_lens, main_codes, 256 + slot * 8 + lh)
        if lf is not None: put_sym(w, len_lens, len_codes, lf)
        if vb:
            v, nb, al = vb
            if nb: w.w(v, nb)
            if al is not None: put_sym(w, al_lens, al_codes, al)

def lzx_blocks(fr, mode):
    state = {"R": [1, 1, 1], "main": [0] * (256 + SLOTS * 8), "len": [0] * 249}
    blocks = []; history = b""
    for n, frame in enumerate(fr):
        w = W()
        if n == 0: w.w(0, 1)  # no E8 translation
        if mode == "stored":
            w.w(3, 3); w.w(len(frame) >> 8, 16); w.w(len(frame) & 255, 8)
            w.pad16()
            if len(w.bits) % 16 == 0 and False: pass
            hdr_bits = len(w.bits)
            raw = w.bytes()
            # position was already padded to 16; the encoder adds a whole word when aligned
            raw = raw if hdr_bits % 16 else raw
            body = raw + struct.pack("<III", *state["R"]) + frame + (b"\0" if len(frame) & 1 else b"")
            blocks.append((body, len(frame)))
        else:
            encode_block(w, frame, history[-32000:], state, mode == "aligned")
            blocks.append((w.bytes(), len(frame)))
        history += frame
    return blocks

def mszip_blocks(fr):
    blocks = []; hist = b""
    for f in fr:
        co = zlib.compressobj(6, zlib.DEFLATED, -15, zdict=hist[-32768:]) if hist else zlib.compressobj(6, zlib.DEFLATED, -15)
        blocks.append((b"CK" + co.compress(f) + co.flush(), len(f)))
        hist += f
    return blocks

files = content()
data, fr = frames(files)
variants = {
    "stored.cab": (build_cab(files, [(f, len(f)) for f in fr], 0)),
    "mszip.cab": build_cab(files, mszip_blocks(fr), 1),
    "lzx_verbatim.cab": build_cab(files, lzx_blocks(fr, "verbatim"), 3 | 15 << 8),
    "lzx_aligned.cab": build_cab(files, lzx_blocks(fr, "aligned"), 3 | 15 << 8),
    "lzx_stored.cab": build_cab(files, lzx_blocks(fr, "stored"), 3 | 15 << 8),
}
with open(os.path.join(OUT, "cab_expected.txt"), "w") as f:
    for n, d in files:
        f.write(f"{n.replace(chr(92), '/')} {len(d)} {zlib.crc32(d)}\n")
for name, blob in variants.items():
    p = os.path.join(OUT, name); open(p, "wb").write(blob)
    r = subprocess.run(["cabextract", "-t", p], capture_output=True, text=True) if subprocess.run(["which", "cabextract"], capture_output=True).returncode == 0 else None
    print(name, len(blob), "cabextract:", (r.returncode if r else "n/a"), (r.stdout.strip().splitlines()[-1] if r and r.stdout else r.stderr.strip()[:200] if r else ""))
