"""JPEG Huffman tables: decoding lookups, encoding maps and optimal table
construction (ITU-T T.81 Annex K.2)."""

from openconverter.errors import CorruptFile


def code_lengths_and_codes(bits, vals):
    """Annex C: generate (symbol, code, length) triples, validating the table."""
    if len(bits) != 16:
        raise CorruptFile("JPEG Huffman table must list 16 code-length counts.")
    total = sum(bits)
    if total > 256 or total != len(vals):
        raise CorruptFile("JPEG Huffman table has an invalid number of symbols.")
    out = []
    code = 0
    k = 0
    for length in range(1, 17):
        for _ in range(bits[length - 1]):
            out.append((vals[k], code, length))
            code += 1
            k += 1
        if code > (1 << length):
            raise CorruptFile("JPEG Huffman table is over-subscribed.")
        code <<= 1
    return out


def build_decode_table(bits, vals):
    """16-bit look-ahead table: table[peek16] = (symbol << 5) | length; 0 = invalid."""
    table = [0] * 65536
    for sym, code, length in code_lengths_and_codes(bits, vals):
        start = code << (16 - length)
        count = 1 << (16 - length)
        if start + count > 65536:
            raise CorruptFile("JPEG Huffman table is over-subscribed.")
        table[start:start + count] = [(sym << 5) | length] * count
    return table


def build_encode_table(bits, vals):
    """symbol -> (code, length)."""
    enc = {}
    for sym, code, length in code_lengths_and_codes(bits, vals):
        enc[sym] = (code, length)
    return enc


def optimal_table(freqs):
    """Annex K.2: build (BITS, HUFFVAL) from symbol frequencies (256 entries).

    A reserved symbol 256 with frequency 1 guarantees that no code consists
    entirely of 1-bits; code lengths are then limited to 16 bits (Figure K.3).
    """
    freq = list(freqs) + [1]          # symbol 256 is the reserved code point
    codesize = [0] * 257
    others = [-1] * 257
    while True:
        # V1: least frequency > 0 (ties: largest symbol value); V2: next least.
        v1 = v2 = -1
        for i in range(257):
            if freq[i] > 0:
                if v1 < 0 or freq[i] <= freq[v1]:
                    v1 = i
        for i in range(257):
            if freq[i] > 0 and i != v1:
                if v2 < 0 or freq[i] <= freq[v2]:
                    v2 = i
        if v2 < 0:
            break
        freq[v1] += freq[v2]
        freq[v2] = 0
        codesize[v1] += 1
        while others[v1] >= 0:
            v1 = others[v1]
            codesize[v1] += 1
        others[v1] = v2
        codesize[v2] += 1
        while others[v2] >= 0:
            v2 = others[v2]
            codesize[v2] += 1
    bits = [0] * 33
    for i in range(257):
        if codesize[i]:
            bits[codesize[i]] += 1
    # Figure K.3: adjust BITS so no code is longer than 16 bits.
    i = 32
    while i > 16:
        while bits[i] > 0:
            j = i - 2
            while bits[j] == 0:
                j -= 1
            bits[i] -= 2
            bits[i - 1] += 1
            bits[j + 1] += 2
            bits[j] -= 1
        i -= 1
    while bits[i] == 0:
        i -= 1
    bits[i] -= 1                      # remove the reserved code point
    huffval = []
    for size in range(1, 33):
        for sym in range(256):
            if codesize[sym] == size:
                huffval.append(sym)
    return bits[1:17], huffval
