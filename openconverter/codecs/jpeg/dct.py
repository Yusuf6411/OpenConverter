"""8x8 DCT kernels.

Inverse: a precomputed-cosine basis sum. Each non-zero coefficient adds its
64-sample basis image; photographic JPEG blocks are sparse, so this beats a
dense separable transform in pure Python. Exact up to float rounding.

Forward: the AAN float algorithm (as in IJG jfdctflt.c); its output scaling
is folded into the quantiser divisors (see ``aan_divisors``).
"""

import math

_C = [1 / math.sqrt(2)] + [1.0] * 7
# M[x][u] = C(u)/2 * cos((2x+1) u pi / 16)
M = [[_C[u] / 2 * math.cos((2 * x + 1) * u * math.pi / 16) for u in range(8)] for x in range(8)]

# BASIS[v*8+u] = 64 samples (row-major y, x) of coefficient (v, u)
BASIS = [[M[y][v] * M[x][u] for y in range(8) for x in range(8)] for v in range(8) for u in range(8)]
DC_SCALE = BASIS[0][0]  # == 1/8

# Clamp table: CLAMP[int(value + OFFSET)] gives the rounded, level-shifted sample.
_OFFSET = 1 << 15
_SIZE = 1 << 16
CLAMP = bytes(min(255, max(0, i - _OFFSET)) for i in range(_SIZE))
OFFSET_ROUND = _OFFSET + 128 + 0.5


def idct_block(coefs_nonzero, dc):
    """Inverse DCT.

    ``dc`` is the dequantised DC coefficient; ``coefs_nonzero`` lists
    (natural_index, dequantised_value) for non-zero AC coefficients.
    Returns 64 bytes of level-shifted, clamped samples (row-major).
    """
    base = dc * DC_SCALE + OFFSET_ROUND
    if not coefs_nonzero:
        v = int(base)
        if v < 0:
            v = 0
        elif v >= _SIZE:
            v = _SIZE - 1
        return bytes([CLAMP[v]]) * 64
    acc = None
    basis = BASIS
    for k, c in coefs_nonzero:
        b = basis[k]
        if acc is None:
            acc = [base + c * x for x in b]
        else:
            acc = [a + c * x for a, x in zip(acc, b)]
    clamp = CLAMP
    ints = [int(a) for a in acc]
    if min(ints) < 0 or max(ints) >= _SIZE:  # only for absurd (corrupt) coefficients
        ints = [min(_SIZE - 1, max(0, v)) for v in ints]
    return bytes([clamp[v] for v in ints])


def reference_idct(coefs):
    """Straightforward float IDCT (64 natural-order coefficients) for testing."""
    out = []
    for y in range(8):
        for x in range(8):
            s = 0.0
            for v in range(8):
                for u in range(8):
                    s += M[y][v] * M[x][u] * coefs[v * 8 + u]
            out.append(s)
    return out


# ---------------------------------------------------------------------------
# forward DCT (AAN, float)
# ---------------------------------------------------------------------------

_AAN = [1.0, 1.387039845, 1.306562965, 1.175875602, 1.0, 0.785694958, 0.541196100, 0.275899379]


def aan_divisors(qtable):
    """Reciprocal quantiser multipliers for AAN output (natural order)."""
    return [1.0 / (qtable[v * 8 + u] * _AAN[v] * _AAN[u] * 8.0) for v in range(8) for u in range(8)]


def fdct_aan(block):
    """In-place float AAN forward DCT of 64 level-shifted samples (list)."""
    d = block
    for p in range(0, 64, 8):
        tmp0 = d[p] + d[p + 7]
        tmp7 = d[p] - d[p + 7]
        tmp1 = d[p + 1] + d[p + 6]
        tmp6 = d[p + 1] - d[p + 6]
        tmp2 = d[p + 2] + d[p + 5]
        tmp5 = d[p + 2] - d[p + 5]
        tmp3 = d[p + 3] + d[p + 4]
        tmp4 = d[p + 3] - d[p + 4]
        tmp10 = tmp0 + tmp3
        tmp13 = tmp0 - tmp3
        tmp11 = tmp1 + tmp2
        tmp12 = tmp1 - tmp2
        d[p] = tmp10 + tmp11
        d[p + 4] = tmp10 - tmp11
        z1 = (tmp12 + tmp13) * 0.707106781
        d[p + 2] = tmp13 + z1
        d[p + 6] = tmp13 - z1
        tmp10 = tmp4 + tmp5
        tmp11 = tmp5 + tmp6
        tmp12 = tmp6 + tmp7
        z5 = (tmp10 - tmp12) * 0.382683433
        z2 = 0.541196100 * tmp10 + z5
        z4 = 1.306562965 * tmp12 + z5
        z3 = tmp11 * 0.707106781
        z11 = tmp7 + z3
        z13 = tmp7 - z3
        d[p + 5] = z13 + z2
        d[p + 3] = z13 - z2
        d[p + 1] = z11 + z4
        d[p + 7] = z11 - z4
    for p in range(8):
        tmp0 = d[p] + d[p + 56]
        tmp7 = d[p] - d[p + 56]
        tmp1 = d[p + 8] + d[p + 48]
        tmp6 = d[p + 8] - d[p + 48]
        tmp2 = d[p + 16] + d[p + 40]
        tmp5 = d[p + 16] - d[p + 40]
        tmp3 = d[p + 24] + d[p + 32]
        tmp4 = d[p + 24] - d[p + 32]
        tmp10 = tmp0 + tmp3
        tmp13 = tmp0 - tmp3
        tmp11 = tmp1 + tmp2
        tmp12 = tmp1 - tmp2
        d[p] = tmp10 + tmp11
        d[p + 32] = tmp10 - tmp11
        z1 = (tmp12 + tmp13) * 0.707106781
        d[p + 16] = tmp13 + z1
        d[p + 48] = tmp13 - z1
        tmp10 = tmp4 + tmp5
        tmp11 = tmp5 + tmp6
        tmp12 = tmp6 + tmp7
        z5 = (tmp10 - tmp12) * 0.382683433
        z2 = 0.541196100 * tmp10 + z5
        z4 = 1.306562965 * tmp12 + z5
        z3 = tmp11 * 0.707106781
        z11 = tmp7 + z3
        z13 = tmp7 - z3
        d[p + 40] = z13 + z2
        d[p + 24] = z13 - z2
        d[p + 8] = z11 + z4
        d[p + 56] = z11 - z4
    return d


def reference_fdct(samples):
    """Straightforward float FDCT (level-shifted samples) for testing."""
    out = []
    for v in range(8):
        for u in range(8):
            s = 0.0
            for y in range(8):
                for x in range(8):
                    s += M[y][v] * M[x][u] * samples[y * 8 + x]
            out.append(s)
    return out
