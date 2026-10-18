"""CRC-32 (IEEE 802.3, reflected, as used by PNG/zlib/gzip) and Adler-32."""

import sys
from array import array
from itertools import accumulate

_WORD = next(code for code in ("I", "L") if array(code).itemsize == 4)


def _make_crc_tables():
    t0 = []
    for n in range(256):
        c = n
        for _ in range(8):
            c = (0xEDB88320 ^ (c >> 1)) if c & 1 else (c >> 1)
        t0.append(c)
    # Slicing-by-4 tables: t[k][n] = CRC of byte n followed by k zero bytes.
    tables = [t0]
    for _ in range(3):
        prev = tables[-1]
        tables.append([(prev[n] >> 8) ^ t0[prev[n] & 0xFF] for n in range(256)])
    return tables


_T0, _T1, _T2, _T3 = _make_crc_tables()
CRC_TABLE = tuple(_T0)


def crc32(data, crc: int = 0) -> int:
    """Incremental CRC-32: ``crc32(b, crc32(a)) == crc32(a + b)``."""
    t0, t1, t2, t3 = _T0, _T1, _T2, _T3
    c = crc ^ 0xFFFFFFFF
    mv = memoryview(data).cast("B")
    n = len(mv)
    head = n - (n % 4)
    if head:
        words = array(_WORD)
        words.frombytes(mv[:head])
        if sys.byteorder != "little":
            words.byteswap()
        for w in words:
            c ^= w
            c = t3[c & 0xFF] ^ t2[(c >> 8) & 0xFF] ^ t1[(c >> 16) & 0xFF] ^ t0[c >> 24]
    for byte in bytes(mv[head:]):
        c = t0[(c ^ byte) & 0xFF] ^ (c >> 8)
    return c ^ 0xFFFFFFFF


class CRC32:
    """Streaming helper."""

    def __init__(self) -> None:
        self.value = 0

    def update(self, data) -> "CRC32":
        self.value = crc32(data, self.value)
        return self


_ADLER_MOD = 65521
_ADLER_CHUNK = 1 << 20  # deferred modulo: Python ints never overflow, so reduce per MiB


def adler32(data, value: int = 1) -> int:
    """Incremental Adler-32: ``adler32(b, adler32(a)) == adler32(a + b)``."""
    s1 = value & 0xFFFF
    s2 = (value >> 16) & 0xFFFF
    mv = memoryview(data).cast("B")
    for off in range(0, len(mv), _ADLER_CHUNK):
        chunk = mv[off:off + _ADLER_CHUNK]
        # s2 accumulates every running value of s1 after each byte.
        sums = sum(accumulate(chunk, initial=s1))
        s2 = (s2 + sums - s1) % _ADLER_MOD
        s1 = (s1 + sum(chunk)) % _ADLER_MOD
    return (s2 << 16) | s1
