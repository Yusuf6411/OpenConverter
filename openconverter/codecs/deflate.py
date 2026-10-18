"""DEFLATE (RFC 1951) and the zlib wrapper (RFC 1950), implemented from scratch.

Inflate supports stored, fixed and dynamic blocks and rejects every malformed
construct with CorruptFile. A caller-provided ``max_output`` bounds the output
(decompression-bomb guard -> LimitExceeded).

Deflate uses LZ77 with exact 3-byte hash chains over a 32 KiB window, lazy
matching for levels >= 4, and per-block choice of dynamic Huffman, fixed
Huffman or stored encoding.
"""

import heapq

from openconverter.codecs.checksums import adler32
from openconverter.errors import CorruptFile, LimitExceeded, UnsupportedVariant
from openconverter.models.progress import NULL_PROGRESS

# --------------------------------------------------------------------------
# shared tables
# --------------------------------------------------------------------------

LENGTH_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31,
               35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258]
LENGTH_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2,
                3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0]
DIST_BASE = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193,
             257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
             8193, 12289, 16385, 24577]
DIST_EXTRA = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6,
              7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13]
CODELEN_ORDER = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15]

FIXED_LITLEN_LENGTHS = [8] * 144 + [9] * 112 + [7] * 24 + [8] * 8
FIXED_DIST_LENGTHS = [5] * 30

WSIZE = 32768
WMASK = WSIZE - 1
HSIZE = 1 << 15
HMASK = HSIZE - 1
MAX_MATCH = 258
MIN_MATCH = 3


def _reverse(code: int, length: int) -> int:
    r = 0
    for _ in range(length):
        r = (r << 1) | (code & 1)
        code >>= 1
    return r


def canonical_codes(lengths):
    """RFC 1951 3.2.2: code lengths -> list of (code, length), MSB-first codes."""
    max_len = max(lengths, default=0)
    bl_count = [0] * (max_len + 1)
    for length in lengths:
        if length:
            bl_count[length] += 1
    next_code = [0] * (max_len + 1)
    code = 0
    for bits in range(1, max_len + 1):
        code = (code + bl_count[bits - 1]) << 1
        next_code[bits] = code
    out = []
    for length in lengths:
        if length:
            out.append((next_code[length], length))
            next_code[length] += 1
        else:
            out.append((0, 0))
    return out


# --------------------------------------------------------------------------
# inflate
# --------------------------------------------------------------------------

def _build_decode_table(lengths, kind: str):
    """Return (table, bits). table[peek] = (symbol << 4) | length, 0 = invalid."""
    max_len = max(lengths, default=0)
    if max_len == 0:
        return None, 0
    if max_len > 15:
        raise CorruptFile(f"Invalid DEFLATE {kind} code length.")
    count = [0] * 16
    for length in lengths:
        count[length] += 1
    count[0] = 0
    left = 1
    for bits in range(1, 16):
        left <<= 1
        left -= count[bits]
        if left < 0:
            raise CorruptFile(f"Over-subscribed DEFLATE {kind} Huffman code.")
    used = sum(count)
    if left > 0:
        # Incomplete code: only a single code of length 1 is permitted
        # (as zlib does), and never for the code-length alphabet.
        if kind == "code-length" or not (used == 1 and max_len == 1):
            raise CorruptFile(f"Incomplete DEFLATE {kind} Huffman code.")
    size = 1 << max_len
    table = [0] * size
    for sym, (code, length) in enumerate(canonical_codes(lengths)):
        if not length:
            continue
        rev = _reverse(code, length)
        entry = (sym << 4) | length
        for idx in range(rev, size, 1 << length):
            table[idx] = entry
    return table, max_len


_FIXED_LIT_TABLE = None
_FIXED_DIST_TABLE = None


def _fixed_tables():
    global _FIXED_LIT_TABLE, _FIXED_DIST_TABLE
    if _FIXED_LIT_TABLE is None:
        _FIXED_LIT_TABLE = _build_decode_table(FIXED_LITLEN_LENGTHS, "literal/length")
        # The fixed distance alphabet has 32 codes (30, 31 invalid); build it complete.
        _FIXED_DIST_TABLE = _build_decode_table([5] * 32, "distance")
    return _FIXED_LIT_TABLE, _FIXED_DIST_TABLE


class _Inflater:
    def __init__(self, data, max_output, progress):
        self.real_len = len(data)
        self.data = bytes(data) + bytes(16)
        self.pos = 0          # next byte to load into the bit buffer
        self.bitbuf = 0
        self.bitcnt = 0
        self.out = bytearray()
        self.block_types = []
        self.max_output = max_output
        self.progress = progress
        self.next_check = 1 << 16

    def _truncated(self):
        raise CorruptFile("DEFLATE stream is truncated.")

    def need(self, n):
        while self.bitcnt < n:
            if self.pos >= self.real_len + 8:
                self._truncated()
            self.bitbuf |= int.from_bytes(self.data[self.pos:self.pos + 8], "little") << self.bitcnt
            self.pos += 8
            self.bitcnt += 64

    def bits(self, n):
        self.need(n)
        v = self.bitbuf & ((1 << n) - 1)
        self.bitbuf >>= n
        self.bitcnt -= n
        return v

    def align_to_byte(self):
        drop = self.bitcnt & 7
        self.bitbuf >>= drop
        self.bitcnt -= drop

    def byte_position(self):
        """Index of the next unread whole byte (after align_to_byte)."""
        return self.pos - self.bitcnt // 8

    def reset_to(self, byte_pos):
        self.pos = byte_pos
        self.bitbuf = 0
        self.bitcnt = 0

    def decode_symbol(self, table, nbits, kind):
        self.need(nbits)
        entry = table[self.bitbuf & ((1 << nbits) - 1)]
        if not entry:
            raise CorruptFile(f"Invalid DEFLATE {kind} code.")
        length = entry & 15
        self.bitbuf >>= length
        self.bitcnt -= length
        return entry >> 4

    def run(self):
        while True:
            final = self.bits(1)
            btype = self.bits(2)
            self.block_types.append(btype)
            if btype == 0:
                self.stored_block()
            elif btype == 1:
                (lt, lb), (dt, db) = _fixed_tables()
                self.huffman_block(lt, lb, dt, db)
            elif btype == 2:
                lt, lb, dt, db = self.read_dynamic_header()
                self.huffman_block(lt, lb, dt, db)
            else:
                raise CorruptFile("Invalid DEFLATE block type 3.")
            if final:
                break
        consumed_bits = self.pos * 8 - self.bitcnt
        if consumed_bits > self.real_len * 8:
            self._truncated()
        return self.out

    def stored_block(self):
        self.align_to_byte()
        p = self.byte_position()
        self.reset_to(p)
        d = self.data
        if p + 4 > self.real_len:
            self._truncated()
        length = d[p] | (d[p + 1] << 8)
        nlength = d[p + 2] | (d[p + 3] << 8)
        if length != (~nlength & 0xFFFF):
            raise CorruptFile("DEFLATE stored block length check failed (LEN/NLEN mismatch).")
        p += 4
        if p + length > self.real_len:
            self._truncated()
        self.out += d[p:p + length]
        self.reset_to(p + length)
        if len(self.out) > self.max_output:
            raise LimitExceeded("Decompressed data exceeds the allowed size.")

    def read_dynamic_header(self):
        hlit = self.bits(5) + 257
        hdist = self.bits(5) + 1
        hclen = self.bits(4) + 4
        if hlit > 286 or hdist > 30:
            raise CorruptFile("DEFLATE dynamic block declares too many codes.")
        cl_lengths = [0] * 19
        for i in range(hclen):
            cl_lengths[CODELEN_ORDER[i]] = self.bits(3)
        cl_table, cl_bits = _build_decode_table(cl_lengths, "code-length")
        if cl_table is None:
            raise CorruptFile("DEFLATE code-length code is empty.")
        lengths = []
        total = hlit + hdist
        while len(lengths) < total:
            sym = self.decode_symbol(cl_table, cl_bits, "code-length")
            if sym < 16:
                lengths.append(sym)
            elif sym == 16:
                if not lengths:
                    raise CorruptFile("DEFLATE repeat code with no previous length.")
                lengths.extend([lengths[-1]] * (3 + self.bits(2)))
            elif sym == 17:
                lengths.extend([0] * (3 + self.bits(3)))
            else:
                lengths.extend([0] * (11 + self.bits(7)))
            if len(lengths) > total:
                raise CorruptFile("DEFLATE code lengths overrun the declared count.")
        lit_lengths = lengths[:hlit]
        dist_lengths = lengths[hlit:]
        if lit_lengths[256] == 0:
            raise CorruptFile("DEFLATE block has no end-of-block code.")
        lt, lb = _build_decode_table(lit_lengths, "literal/length")
        dt, db = _build_decode_table(dist_lengths, "distance")
        return lt, lb, dt, db

    def huffman_block(self, lit_table, lit_bits, dist_table, dist_bits):
        # Hot loop: everything bound to locals.
        data = self.data
        real_len = self.real_len
        out = self.out
        bitbuf = self.bitbuf
        bitcnt = self.bitcnt
        pos = self.pos
        lit_mask = (1 << lit_bits) - 1
        dist_mask = (1 << dist_bits) - 1 if dist_table else 0
        max_output = self.max_output
        length_base = LENGTH_BASE
        length_extra = LENGTH_EXTRA
        dist_base = DIST_BASE
        dist_extra = DIST_EXTRA
        from_bytes = int.from_bytes
        next_check = self.next_check
        progress = self.progress
        while True:
            if bitcnt < 48:
                if pos >= real_len + 8:
                    self._truncated()
                bitbuf |= from_bytes(data[pos:pos + 8], "little") << bitcnt
                pos += 8
                bitcnt += 64
                if pos >= next_check:
                    next_check = pos + (1 << 16)
                    if len(out) > max_output:
                        raise LimitExceeded("Decompressed data exceeds the allowed size.")
                    progress.check_cancelled()
                    progress.report(min(pos, real_len), real_len, "inflating")
            entry = lit_table[bitbuf & lit_mask]
            if not entry:
                raise CorruptFile("Invalid DEFLATE literal/length code.")
            n = entry & 15
            bitbuf >>= n
            bitcnt -= n
            sym = entry >> 4
            if sym < 256:
                out.append(sym)
                continue
            if sym == 256:
                break
            sym -= 257
            if sym >= 29:
                raise CorruptFile("Invalid DEFLATE length symbol.")
            eb = length_extra[sym]
            length = length_base[sym] + (bitbuf & ((1 << eb) - 1))
            bitbuf >>= eb
            bitcnt -= eb
            if dist_table is None:
                raise CorruptFile("DEFLATE match uses an empty distance code.")
            entry = dist_table[bitbuf & dist_mask]
            if not entry:
                raise CorruptFile("Invalid DEFLATE distance code.")
            n = entry & 15
            bitbuf >>= n
            bitcnt -= n
            dsym = entry >> 4
            if dsym >= 30:
                raise CorruptFile("Invalid DEFLATE distance symbol.")
            eb = dist_extra[dsym]
            if bitcnt < eb:
                # extremely rare: refill mid-symbol
                if pos >= real_len + 8:
                    self._truncated()
                bitbuf |= from_bytes(data[pos:pos + 8], "little") << bitcnt
                pos += 8
                bitcnt += 64
            dist = dist_base[dsym] + (bitbuf & ((1 << eb) - 1))
            bitbuf >>= eb
            bitcnt -= eb
            olen = len(out)
            if olen > max_output:
                raise LimitExceeded("Decompressed data exceeds the allowed size.")
            if dist > olen:
                raise CorruptFile("DEFLATE distance refers to data before the start of the output.")
            start = olen - dist
            if dist >= length:
                out += out[start:start + length]
            else:
                chunk = out[start:]
                reps, rem = divmod(length, dist)
                out += chunk * reps + chunk[:rem]
        self.bitbuf = bitbuf
        self.bitcnt = bitcnt
        self.pos = pos
        self.next_check = next_check
        if len(out) > max_output:
            raise LimitExceeded("Decompressed data exceeds the allowed size.")


def inflate(data, max_output: int, progress=NULL_PROGRESS) -> bytes:
    """Decompress a raw DEFLATE stream."""
    return bytes(_Inflater(data, max_output, progress).run())


def block_types(data, max_output: int = 1 << 30) -> list[int]:
    """Diagnostics: the BTYPE of every block in a raw DEFLATE stream."""
    inf = _Inflater(data, max_output, NULL_PROGRESS)
    inf.run()
    return inf.block_types


def _inflate_with_end(data, max_output, progress):
    inf = _Inflater(data, max_output, progress)
    out = inf.run()
    inf.align_to_byte()
    return out, inf.byte_position()


def zlib_decompress(data, max_output: int, progress=NULL_PROGRESS) -> bytes:
    """Decompress a zlib (RFC 1950) stream, verifying its Adler-32."""
    data = bytes(data)
    if len(data) < 2:
        raise CorruptFile("zlib stream is truncated.")
    cmf, flg = data[0], data[1]
    if cmf & 0x0F != 8:
        raise CorruptFile("zlib stream does not use the DEFLATE method.")
    if cmf >> 4 > 7:
        raise CorruptFile("zlib window size is invalid.")
    if ((cmf << 8) | flg) % 31:
        raise CorruptFile("zlib header check failed.")
    if flg & 0x20:
        raise UnsupportedVariant("zlib streams with a preset dictionary are not supported.")
    out, end = _inflate_with_end(data[2:], max_output, progress)
    end += 2
    if end + 4 > len(data):
        raise CorruptFile("zlib stream is missing its Adler-32 checksum.")
    expected = int.from_bytes(data[end:end + 4], "big")
    if adler32(out) != expected:
        raise CorruptFile("zlib Adler-32 checksum mismatch.")
    return bytes(out)


# --------------------------------------------------------------------------
# Huffman code construction (length-limited)
# --------------------------------------------------------------------------

def huffman_lengths(freqs, max_len: int):
    """Optimal prefix code lengths for ``freqs``, limited to ``max_len`` bits.

    Symbols with zero frequency get length 0. Always returns a complete code
    with at least two codes when two or more symbols exist.
    """
    n = len(freqs)
    used = [i for i in range(n) if freqs[i]]
    lengths = [0] * n
    if not used:
        return lengths
    if len(used) == 1:
        # Pair the lone symbol with a neighbour so the code is complete.
        other = 1 if used[0] == 0 else 0
        lengths[used[0]] = 1
        if other < n:
            lengths[other] = 1
        return lengths
    heap = [(freqs[i], i, i) for i in used]
    heapq.heapify(heap)
    parent = {}
    nid = n
    while len(heap) > 1:
        f1, _t1, a = heapq.heappop(heap)
        f2, _t2, b = heapq.heappop(heap)
        parent[a] = nid
        parent[b] = nid
        heapq.heappush(heap, (f1 + f2, nid, nid))
        nid += 1
    root = heap[0][2]
    depth_cache = {root: 0}

    def depth(node):
        chain = []
        while node not in depth_cache:
            chain.append(node)
            node = parent[node]
        d = depth_cache[node]
        for c in reversed(chain):
            d += 1
            depth_cache[c] = d
        return depth_cache[chain[0]] if chain else d

    for i in used:
        lengths[i] = depth(i)
    if max(lengths) > max_len:
        _limit_lengths(lengths, freqs, used, max_len)
    return lengths


def _limit_lengths(lengths, freqs, used, max_len):
    """Clamp to max_len and repair the Kraft inequality (zlib-style heuristic)."""
    for i in used:
        if lengths[i] > max_len:
            lengths[i] = max_len
    cap = 1 << max_len
    kraft = sum(1 << (max_len - lengths[i]) for i in used)
    # Lengthen the least frequent codes that are shorter than max_len.
    order = sorted(used, key=lambda i: (freqs[i], -lengths[i]))
    while kraft > cap:
        for i in order:
            if lengths[i] < max_len:
                kraft -= 1 << (max_len - lengths[i] - 1)
                lengths[i] += 1
                break
    # Use any freed space by shortening the most frequent codes.
    for i in sorted(used, key=lambda i: -freqs[i]):
        while lengths[i] > 1 and kraft + (1 << (max_len - lengths[i])) <= cap:
            kraft += 1 << (max_len - lengths[i])
            lengths[i] -= 1


# --------------------------------------------------------------------------
# deflate
# --------------------------------------------------------------------------

# (good_length, max_lazy, nice_length, max_chain). Same shape as zlib's table,
# but chain/nice limits are scaled down because each hash-chain step costs far
# more in pure Python; measured trade-offs are documented in docs/FORMATS.md.
LEVELS = {
    1: (4, 4, 8, 4), 2: (4, 5, 16, 8), 3: (4, 6, 32, 16),
    4: (4, 4, 16, 8), 5: (8, 16, 32, 12), 6: (8, 16, 64, 16),
    7: (8, 32, 128, 64), 8: (32, 128, 258, 256), 9: (32, 258, 258, 1024),
}
BLOCK_TOKENS = 1 << 15


def _build_length_tables():
    code = [0] * 259
    extra_bits = [0] * 259
    extra_val = [0] * 259
    for sym in range(29):
        base = LENGTH_BASE[sym]
        top = base + (1 << LENGTH_EXTRA[sym])
        if sym == 28:
            top = 259
        for length in range(base, min(top, 259)):
            if sym == 27 and length == 258:
                break
            code[length] = 257 + sym
            extra_bits[length] = LENGTH_EXTRA[sym]
            extra_val[length] = length - base
    code[258] = 285
    extra_bits[258] = 0
    extra_val[258] = 0
    return code, extra_bits, extra_val


def _build_dist_tables():
    code = [0] * (WSIZE + 1)
    for sym in range(30):
        base = DIST_BASE[sym]
        for d in range(base, min(base + (1 << DIST_EXTRA[sym]), WSIZE + 1)):
            code[d] = sym
    return code


LEN_CODE, LEN_XBITS, LEN_XVAL = _build_length_tables()
DIST_CODE = _build_dist_tables()


class _BitWriter:
    __slots__ = ("out", "acc", "n")

    def __init__(self):
        self.out = bytearray()
        self.acc = 0
        self.n = 0

    def write(self, value, nbits):
        self.acc |= value << self.n
        self.n += nbits
        if self.n >= 64:
            self.out += (self.acc & 0xFFFFFFFFFFFFFFFF).to_bytes(8, "little")
            self.acc >>= 64
            self.n -= 64

    def align(self):
        nbytes = (self.n + 7) // 8
        self.out += self.acc.to_bytes(nbytes, "little") if nbytes else b""
        self.acc = 0
        self.n = 0

    def getvalue(self):
        self.align()
        return bytes(self.out)


def _encode_table(lengths):
    """lengths -> list of (reversed_code, length) for LSB-first emission."""
    return [(_reverse(c, l), l) for c, l in canonical_codes(lengths)]


_FIXED_LIT_ENC = None
_FIXED_DIST_ENC = None


def _fixed_enc():
    global _FIXED_LIT_ENC, _FIXED_DIST_ENC
    if _FIXED_LIT_ENC is None:
        _FIXED_LIT_ENC = _encode_table(FIXED_LITLEN_LENGTHS)
        _FIXED_DIST_ENC = _encode_table(FIXED_DIST_LENGTHS)
    return _FIXED_LIT_ENC, _FIXED_DIST_ENC


def _rle_code_lengths(lengths):
    """Run-length encode code lengths with symbols 16/17/18 -> [(sym, extra_bits, extra_val)]."""
    out = []
    i = 0
    n = len(lengths)
    while i < n:
        v = lengths[i]
        j = i
        while j < n and lengths[j] == v:
            j += 1
        run = j - i
        if v == 0:
            while run >= 11:
                r = min(run, 138)
                out.append((18, 7, r - 11))
                run -= r
            if run >= 3:
                out.append((17, 3, run - 3))
                run = 0
            out.extend([(0, 0, 0)] * run)
        else:
            out.append((v, 0, 0))
            run -= 1
            while run >= 3:
                r = min(run, 6)
                out.append((16, 2, r - 3))
                run -= r
            out.extend([(v, 0, 0)] * run)
        i = j
    return out


def _token_freqs(tokens):
    lit = [0] * 286
    dist = [0] * 30
    extra = 0
    len_code = LEN_CODE
    dist_code = DIST_CODE
    for t in tokens:
        if t < 256:
            lit[t] += 1
        else:
            length = t >> 16
            d = t & 0xFFFF
            lit[len_code[length]] += 1
            ds = dist_code[d]
            dist[ds] += 1
            extra += LEN_XBITS[length] + DIST_EXTRA[ds]
    lit[256] += 1
    return lit, dist, extra


def _write_block(bw, tokens, raw, final):
    lit_f, dist_f, extra_bits = _token_freqs(tokens)
    # --- dynamic ---
    lit_len = huffman_lengths(lit_f, 15)
    dist_len = huffman_lengths(dist_f, 15)
    if not any(dist_len):
        dist_len[0] = 1
        dist_len[1] = 1
    hlit = 286
    while hlit > 257 and lit_len[hlit - 1] == 0:
        hlit -= 1
    hdist = 30
    while hdist > 1 and dist_len[hdist - 1] == 0:
        hdist -= 1
    rle = _rle_code_lengths(lit_len[:hlit] + dist_len[:hdist])
    cl_f = [0] * 19
    for sym, _, _ in rle:
        cl_f[sym] += 1
    cl_len = huffman_lengths(cl_f, 7)
    hclen = 19
    while hclen > 4 and cl_len[CODELEN_ORDER[hclen - 1]] == 0:
        hclen -= 1
    dyn_bits = 3 + 5 + 5 + 4 + 3 * hclen
    dyn_bits += sum(cl_len[s] + xb for s, xb, _ in rle)
    dyn_bits += sum(f * l for f, l in zip(lit_f, lit_len)) + sum(f * l for f, l in zip(dist_f, dist_len))
    dyn_bits += extra_bits
    # --- fixed ---
    fix_bits = 3 + sum(f * l for f, l in zip(lit_f, FIXED_LITLEN_LENGTHS)) + sum(dist_f) * 5 + extra_bits
    # --- stored ---
    nchunks = max(1, (len(raw) + 65534) // 65535)
    stored_bits = (len(raw) + 5 * nchunks) * 8 + 7
    best = min(dyn_bits, fix_bits, stored_bits)
    if best == stored_bits:
        _write_stored(bw, raw, final)
        return
    if best == fix_bits:
        bw.write(1 if final else 0, 1)
        bw.write(1, 2)
        lit_enc, dist_enc = _fixed_enc()
    else:
        bw.write(1 if final else 0, 1)
        bw.write(2, 2)
        bw.write(hlit - 257, 5)
        bw.write(hdist - 1, 5)
        bw.write(hclen - 4, 4)
        for i in range(hclen):
            bw.write(cl_len[CODELEN_ORDER[i]], 3)
        cl_enc = _encode_table(cl_len)
        for sym, xb, xv in rle:
            code, length = cl_enc[sym]
            bw.write(code, length)
            if xb:
                bw.write(xv, xb)
        lit_enc = _encode_table(lit_len)
        dist_enc = _encode_table(dist_len + [0] * (30 - len(dist_len)))
    _write_tokens(bw, tokens, lit_enc, dist_enc)


def _write_tokens(bw, tokens, lit_enc, dist_enc):
    # Precombine length symbol + extra bits, and distance symbol + extra bits.
    len_combo = [None] * 259
    for length in range(3, 259):
        code, cl = lit_enc[LEN_CODE[length]]
        xb = LEN_XBITS[length]
        len_combo[length] = (code | (LEN_XVAL[length] << cl), cl + xb)
    dist_code = DIST_CODE
    dist_base = DIST_BASE
    dist_extra = DIST_EXTRA
    out = bw.out
    acc = bw.acc
    n = bw.n
    for t in tokens:
        if t < 256:
            v, nb = lit_enc[t]
        else:
            length = t >> 16
            d = t & 0xFFFF
            v, nb = len_combo[length]
            ds = dist_code[d]
            dc, dl = dist_enc[ds]
            v |= (dc | ((d - dist_base[ds]) << dl)) << nb
            nb += dl + dist_extra[ds]
        acc |= v << n
        n += nb
        if n >= 64:
            out += (acc & 0xFFFFFFFFFFFFFFFF).to_bytes(8, "little")
            acc >>= 64
            n -= 64
    bw.acc = acc
    bw.n = n
    bw.write(*lit_enc[256])


def _write_stored(bw, raw, final):
    if not raw:
        bw.write(1 if final else 0, 1)
        bw.write(0, 2)
        bw.align()
        bw.out += b"\x00\x00\xff\xff"
        return
    for off in range(0, len(raw), 65535):
        chunk = raw[off:off + 65535]
        last = final and off + 65535 >= len(raw)
        bw.write(1 if last else 0, 1)
        bw.write(0, 2)
        bw.align()
        ln = len(chunk)
        bw.out += bytes((ln & 255, ln >> 8, ~ln & 255, (~ln >> 8) & 255))
        bw.out += chunk


def _lz77(data, good, max_lazy, nice, chain_limit, lazy, progress):
    """Yield (tokens, end_position) blocks. A token is a literal byte (< 256)
    or ``(length << 16) | distance`` for a match."""
    n = len(data)
    # Hash chains: head[h] is the most recent position whose 3-byte hash is h,
    # prev[p & WMASK] links to the previous position with the same hash.
    # The hash is zlib's rolling 15-bit hash; candidates are verified byte-wise.
    head = [-1] * HSIZE
    prev = [-1] * WSIZE
    tokens = []
    append = tokens.append
    i = 0
    end = n - 2  # last position where a 3-byte key exists
    check_step = 1 << 14
    next_check = check_step

    # The match finder is inlined in both loops below: function calls per
    # input byte would dominate the run time in pure Python.
    h = ((data[0] << 5) ^ data[1]) & HMASK if n >= 2 else 0
    if not lazy:
        while i < end:
            h = ((h << 5) ^ data[i + 2]) & HMASK
            cand = head[h]
            prev[i & WMASK] = cand
            head[h] = i
            best = 2
            if cand >= 0:
                limit = i - WSIZE if i > WSIZE else -1
                maxlen = n - i if n - i < MAX_MATCH else MAX_MATCH
                chain = chain_limit
                ib = data[i + 2]
                while cand > limit:
                    # Cheap reject on the byte that would extend the best match,
                    # then verify the prefix with one slice comparison.
                    if data[cand + best] == ib and data[cand:cand + best] == data[i:i + best]:
                        m = best + 1
                        while m + 8 <= maxlen and data[cand + m:cand + m + 8] == data[i + m:i + m + 8]:
                            m += 8
                        while m < maxlen and data[cand + m] == data[i + m]:
                            m += 1
                        best = m
                        dist = i - cand
                        if m >= nice or m >= maxlen:
                            break
                        ib = data[i + m]
                    chain -= 1
                    if not chain:
                        break
                    cand = prev[cand & WMASK]
            if best >= MIN_MATCH:
                append((best << 16) | dist)
                stop = i + best
                if best <= max_lazy:
                    for q in range(i + 1, stop if stop < end else end):
                        h = ((h << 5) ^ data[q + 2]) & HMASK
                        prev[q & WMASK] = head[h]
                        head[h] = q
                elif stop < end:
                    h = ((data[stop - 1] << 10) ^ (data[stop] << 5) ^ data[stop + 1]) & HMASK
                i = stop
            else:
                append(data[i])
                i += 1
            if i >= next_check:
                next_check = i + check_step
                progress.check_cancelled()
                progress.report(i, n, "compressing")
                if len(tokens) >= BLOCK_TOKENS:
                    yield tokens, i
                    tokens = []
                    append = tokens.append
    else:
        prev_len = 0
        prev_dist = 0
        have_prev = False
        while i < end:
            h = ((h << 5) ^ data[i + 2]) & HMASK
            cand = head[h]
            prev[i & WMASK] = cand
            head[h] = i
            ln = 0
            if cand >= 0 and prev_len < max_lazy:
                maxlen = n - i if n - i < MAX_MATCH else MAX_MATCH
                best = prev_len if prev_len >= MIN_MATCH else 2
                if best < maxlen:
                    limit = i - WSIZE if i > WSIZE else -1
                    chain = chain_limit >> 2 if prev_len >= good else chain_limit
                    dist = 0
                    ib = data[i + best]
                    while cand > limit:
                        if data[cand + best] == ib and data[cand:cand + best] == data[i:i + best]:
                            m = best + 1
                            while m + 8 <= maxlen and data[cand + m:cand + m + 8] == data[i + m:i + m + 8]:
                                m += 8
                            while m < maxlen and data[cand + m] == data[i + m]:
                                m += 1
                            best = m
                            dist = i - cand
                            if m >= nice or m >= maxlen:
                                break
                            ib = data[i + m]
                        chain -= 1
                        if not chain:
                            break
                        cand = prev[cand & WMASK]
                    if dist:
                        ln = best
            if prev_len >= MIN_MATCH and ln <= prev_len:
                # Emit the previous match, which starts at i - 1.
                append((prev_len << 16) | prev_dist)
                stop = i - 1 + prev_len
                for q in range(i + 1, stop if stop < end else end):
                    h = ((h << 5) ^ data[q + 2]) & HMASK
                    prev[q & WMASK] = head[h]
                    head[h] = q
                i = stop
                prev_len = 0
                have_prev = False
            else:
                if have_prev:
                    append(data[i - 1])
                prev_len = ln
                prev_dist = dist if ln else 0
                have_prev = True
                i += 1
            if i >= next_check:
                next_check = i + check_step
                progress.check_cancelled()
                progress.report(i, n, "compressing")
                if len(tokens) >= BLOCK_TOKENS:
                    # A pending (not yet emitted) position at i - 1 belongs to the next block.
                    yield tokens, (i - 1 if have_prev else i)
                    tokens = []
                    append = tokens.append
        if have_prev:
            if prev_len >= MIN_MATCH:
                append((prev_len << 16) | prev_dist)
                i = i - 1 + prev_len
            else:
                append(data[i - 1])
    while i < n:
        append(data[i])
        i += 1
    yield tokens, n


def deflate(data, level: int = 6, progress=NULL_PROGRESS, max_chain: int | None = None) -> bytes:
    """Compress ``data`` to a raw DEFLATE stream at ``level`` 0..9."""
    data = bytes(data)
    if not 0 <= level <= 9:
        raise ValueError("level must be 0..9")
    bw = _BitWriter()
    n = len(data)
    if level == 0:
        for off in range(0, max(n, 1), 1 << 20):
            _write_stored(bw, data[off:off + (1 << 20)], off + (1 << 20) >= n)
            progress.check_cancelled()
            progress.report(min(off + (1 << 20), n), max(n, 1), "compressing")
        return bw.getvalue()
    if n == 0:
        bw.write(1, 1)
        bw.write(1, 2)
        bw.write(0, 7)  # fixed-code end of block
        return bw.getvalue()
    good, max_lazy, nice, chain_limit = LEVELS[level]
    if max_chain is not None:
        chain_limit = max(1, min(chain_limit, max_chain))
    start = 0
    pending = None
    for tokens, upto in _lz77(data, good, max_lazy, nice, chain_limit, level >= 4, progress):
        if pending is not None:
            _write_block(bw, pending[0], data[pending[1]:pending[2]], False)
        pending = (tokens, start, upto)
        start = upto
    _write_block(bw, pending[0], data[pending[1]:pending[2]], True)
    progress.report(n, n, "compressing")
    return bw.getvalue()


def zlib_compress(data, level: int = 6, progress=NULL_PROGRESS, max_chain: int | None = None) -> bytes:
    cmf = 0x78
    flevel = 0 if level <= 1 else 1 if level <= 5 else 2 if level == 6 else 3
    flg = flevel << 6
    flg |= 31 - ((cmf << 8) | flg) % 31
    body = deflate(data, level, progress, max_chain)
    return bytes((cmf, flg)) + body + adler32(data).to_bytes(4, "big")
